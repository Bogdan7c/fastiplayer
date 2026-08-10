/// Разбирает non-empty SegmentTemplate с optional SegmentTimeline.
fn parse_segment_template(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    limits: DashMpdLimits,
) -> Result<DashSegmentTemplate, DashMpdError> {
    let mut template = parse_empty_segment_template(element, limits)?;
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "SegmentTimeline") => {
                if !template.timeline.is_empty() {
                    return Err(DashMpdError::new(
                        DashMpdErrorKind::InvalidAddressing,
                    ));
                }
                template.timeline = parse_segment_timeline(cursor, child, limits)?;
            }
            Some(XmlEvent::EndElement(name)) if is_name(&name, "SegmentTemplate") => break,
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(_) | None => {
                return Err(DashMpdError::new(
                    DashMpdErrorKind::UnsupportedConstruct,
                ));
            }
        }
    }
    validate_template_timing(&template)?;
    Ok(template)
}

/// Разбирает attributes SegmentTemplate.
fn parse_empty_segment_template(
    element: XmlElement,
    limits: DashMpdLimits,
) -> Result<DashSegmentTemplate, DashMpdError> {
    validate_attributes(
        &element,
        &[
            "timescale",
            "duration",
            "startNumber",
            "presentationTimeOffset",
            "media",
            "initialization",
            "availabilityTimeOffset",
            "availabilityTimeComplete",
        ],
    )?;
    let media = required_bounded_attribute(&element, "media", limits)?;
    let initialization = bounded_optional_attribute(&element, "initialization", limits)?
        .map(DashTemplateString::parse)
        .transpose()
        .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAddressing))?;
    Ok(DashSegmentTemplate {
        timescale: positive_u64_attribute(&element, "timescale", 1)?,
        start_number: optional_u64_attribute(&element, "startNumber")?.unwrap_or(1),
        presentation_time_offset: optional_u64_attribute(&element, "presentationTimeOffset")?
            .unwrap_or(0),
        media: DashTemplateString::parse(media)
            .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAddressing))?,
        initialization,
        duration: optional_u64_attribute(&element, "duration")?,
        timeline: Box::new([]),
        availability_time_offset_nanoseconds:
            optional_decimal_seconds_nanoseconds_attribute(&element, "availabilityTimeOffset")?,
        availability_time_complete:
            optional_boolean_attribute(&element, "availabilityTimeComplete")?,
    })
}

/// Empty SegmentTemplate не может получить timeline позже.
fn parse_empty_segment_template_leaf(
    element: XmlElement,
    limits: DashMpdLimits,
) -> Result<DashSegmentTemplate, DashMpdError> {
    let template = parse_empty_segment_template(element, limits)?;
    validate_template_timing(&template)?;
    Ok(template)
}

/// SegmentTemplate требует ровно один timing mode.
fn validate_template_timing(template: &DashSegmentTemplate) -> Result<(), DashMpdError> {
    match (template.duration, template.timeline.is_empty()) {
        (Some(duration), true) if duration > 0 => Ok(()),
        (None, false) => Ok(()),
        _ => Err(DashMpdError::new(
            DashMpdErrorKind::InvalidAddressing,
        )),
    }
}

/// Разбирает bounded SegmentTimeline.
fn parse_segment_timeline(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    limits: DashMpdLimits,
) -> Result<Box<[DashTimelineEntry]>, DashMpdError> {
    validate_attributes(&element, &[])?;
    let mut entries = Vec::new();
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "S") => {
                if entries.len() >= limits.maximum_timeline_entries {
                    return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded));
                }
                entries.push(parse_timeline_entry(&child)?);
                consume_descriptor_body(cursor, "S")?;
            }
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "S") => {
                if entries.len() >= limits.maximum_timeline_entries {
                    return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded));
                }
                entries.push(parse_timeline_entry(&child)?);
            }
            Some(XmlEvent::EndElement(name)) if is_name(&name, "SegmentTimeline") => break,
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(_) | None => {
                return Err(DashMpdError::new(DashMpdErrorKind::MalformedSchema));
            }
        }
    }
    if entries.is_empty() {
        return Err(DashMpdError::new(DashMpdErrorKind::InvalidAddressing));
    }
    Ok(entries.into_boxed_slice())
}

/// Разбирает один атрибутивный `S`; paired/empty XML формы имеют одну семантику.
fn parse_timeline_entry(element: &XmlElement) -> Result<DashTimelineEntry, DashMpdError> {
    validate_attributes(element, &["t", "d", "r"])?;
    let duration = optional_u64_attribute(element, "d")?
        .filter(|duration| *duration > 0)
        .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidAddressing))?;
    let repeat = optional_attribute(element, "r")?
        .map(|value| {
            value
                .parse::<i64>()
                .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAddressing))
        })
        .transpose()?
        .unwrap_or(0);
    if repeat < -1 {
        return Err(DashMpdError::new(
            DashMpdErrorKind::InvalidAddressing,
        ));
    }
    Ok(DashTimelineEntry {
        start_time: optional_u64_attribute(element, "t")?,
        duration,
        repeat,
    })
}

/// Разбирает finite explicit SegmentList.
fn parse_segment_list(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    limits: DashMpdLimits,
) -> Result<DashSegmentList, DashMpdError> {
    validate_attributes(
        &element,
        &["timescale", "duration", "presentationTimeOffset"],
    )?;
    if optional_u64_attribute(&element, "presentationTimeOffset")?.unwrap_or(0) != 0 {
        return Err(DashMpdError::new(
            DashMpdErrorKind::UnsupportedConstruct,
        ));
    }
    let timescale = positive_u64_attribute(&element, "timescale", 1)?;
    let duration = optional_u64_attribute(&element, "duration")?;
    if duration == Some(0) {
        return Err(DashMpdError::new(
            DashMpdErrorKind::InvalidAddressing,
        ));
    }
    let mut initialization = None;
    let mut segments = Vec::new();
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "Initialization") => {
                if initialization.is_some() {
                    return Err(DashMpdError::new(
                        DashMpdErrorKind::InvalidAddressing,
                    ));
                }
                initialization = Some(parse_initialization(child, limits)?);
            }
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "SegmentURL") => {
                if segments.len() >= limits.maximum_segments_per_list {
                    return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded));
                }
                segments.push(parse_segment_url(child, limits)?);
            }
            Some(XmlEvent::EndElement(name)) if is_name(&name, "SegmentList") => break,
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(_) | None => {
                return Err(DashMpdError::new(
                    DashMpdErrorKind::UnsupportedConstruct,
                ));
            }
        }
    }
    if segments.is_empty() || duration.is_none() {
        return Err(DashMpdError::new(
            DashMpdErrorKind::InvalidAddressing,
        ));
    }
    Ok(DashSegmentList {
        timescale,
        duration,
        initialization,
        segments: segments.into_boxed_slice(),
    })
}

/// Разбирает один SegmentURL.
fn parse_segment_url(
    element: XmlElement,
    limits: DashMpdLimits,
) -> Result<DashSegmentListEntry, DashMpdError> {
    validate_attributes(&element, &["media", "mediaRange", "index", "indexRange"])?;
    Ok(DashSegmentListEntry {
        media: DashUrlReference::new(required_bounded_attribute(&element, "media", limits)?),
        media_range: optional_range_attribute(&element, "mediaRange")?,
        index: bounded_optional_attribute(&element, "index", limits)?.map(DashUrlReference::new),
        index_range: optional_range_attribute(&element, "indexRange")?,
    })
}

/// Разбирает SegmentBase с optional Initialization.
fn parse_segment_base(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    limits: DashMpdLimits,
) -> Result<DashSegmentBase, DashMpdError> {
    let mut base = parse_empty_segment_base(element)?;
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "Initialization") => {
                if base.initialization.is_some() {
                    return Err(DashMpdError::new(
                        DashMpdErrorKind::InvalidAddressing,
                    ));
                }
                base.initialization = Some(parse_initialization(child, limits)?);
            }
            Some(XmlEvent::EndElement(name)) if is_name(&name, "SegmentBase") => break,
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(_) | None => {
                return Err(DashMpdError::new(
                    DashMpdErrorKind::UnsupportedConstruct,
                ));
            }
        }
    }
    Ok(base)
}

/// Разбирает attributes-only SegmentBase.
fn parse_empty_segment_base(element: XmlElement) -> Result<DashSegmentBase, DashMpdError> {
    validate_attributes(
        &element,
        &["timescale", "presentationTimeOffset", "indexRange"],
    )?;
    Ok(DashSegmentBase {
        index_range: optional_range_attribute(&element, "indexRange")?,
        initialization: None,
        presentation_time_offset: optional_u64_attribute(&element, "presentationTimeOffset")?
            .unwrap_or(0),
        timescale: positive_u64_attribute(&element, "timescale", 1)?,
    })
}

/// Разбирает Initialization descriptor.
fn parse_initialization(
    element: XmlElement,
    limits: DashMpdLimits,
) -> Result<DashInitialization, DashMpdError> {
    validate_attributes(&element, &["sourceURL", "range"])?;
    let source_url =
        bounded_optional_attribute(&element, "sourceURL", limits)?.map(DashUrlReference::new);
    let byte_range = optional_range_attribute(&element, "range")?;
    if source_url.is_none() && byte_range.is_none() {
        return Err(DashMpdError::new(
            DashMpdErrorKind::InvalidAddressing,
        ));
    }
    Ok(DashInitialization {
        source_url,
        byte_range,
    })
}

/// Читает inclusive `start-end`.
fn optional_range_attribute(
    element: &XmlElement,
    name: &str,
) -> Result<Option<IndexRange>, DashMpdError> {
    optional_attribute(element, name)?
        .map(|value| {
            let (start, end) = value
                .split_once('-')
                .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?;
            let start = start
                .parse::<u64>()
                .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?;
            let end = end
                .parse::<u64>()
                .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?;
            IndexRange::new(start, end)
                .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))
        })
        .transpose()
}
