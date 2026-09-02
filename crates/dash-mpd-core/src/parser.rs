use bounded_xml_reader::{BoundedXmlReader, XmlBudgets, XmlElement, XmlEvent};

use crate::error::{DashMpdError, DashMpdErrorKind};
use crate::model::{
    DashAdaptationSet, DashAddressing, DashAudioChannelConfiguration, DashBaseUrl,
    DashColorMetadata, DashContainer, DashFrameRate, DashInitialization, DashMediaKind, DashMpd,
    DashPeriod, DashPresentationDuration, DashRepresentation, DashSegmentBase, DashSegmentList,
    DashSegmentListEntry, DashSegmentTemplate, DashTimelineEntry, DashUrlReference, IndexRange,
};
use crate::template::DashTemplateString;

mod attributes;
mod metadata;
mod text_adaptation;

pub(super) use attributes::{
    bounded_optional_attribute, is_name, optional_attribute, require_name, validate_attributes,
};
use attributes::{
    optional_positive_ratio_attribute, optional_positive_u32_attribute, optional_u64_attribute,
    read_text_leaf, required_bounded_attribute, validate_attributes_with_namespaced_allowlist,
};
use metadata::*;
use text_adaptation::{consume_non_playback_text_adaptation_set, is_declared_text_adaptation_set};

/// Narrow static profile allowlist, доказанный checked-in S34 matrix.
pub(super) const SUPPORTED_DASH_PROFILES: &[&str] = &[
    "urn:mpeg:dash:profile:full:2011",
    "urn:mpeg:dash:profile:isoff-on-demand:2011",
    "urn:mpeg:dash:profile:isoff-live:2011",
    "urn:mpeg:dash:profile:isoff-main:2011",
    "urn:mpeg:dash:profile:webm-on-demand:2012",
    "http://dashif.org/guidelines/dash-if-simple",
];

/// Schema/model caps, которые caller выбирает независимо от XML budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashMpdLimits {
    /// Максимум Period.
    pub maximum_periods: usize,
    /// Максимум AdaptationSet внутри Period.
    pub maximum_adaptation_sets_per_period: usize,
    /// Максимум Representation внутри AdaptationSet.
    pub maximum_representations_per_adaptation_set: usize,
    /// Максимум SegmentURL внутри SegmentList.
    pub maximum_segments_per_list: usize,
    /// Максимум `S` внутри SegmentTimeline.
    pub maximum_timeline_entries: usize,
    /// Максимум bytes одного schema string/text.
    pub maximum_schema_string_bytes: usize,
}

impl DashMpdLimits {
    /// Проверяет, что ни один model cap не отключён нулём.
    pub(super) fn validate(self) -> Result<Self, DashMpdError> {
        let values = [
            self.maximum_periods,
            self.maximum_adaptation_sets_per_period,
            self.maximum_representations_per_adaptation_set,
            self.maximum_segments_per_list,
            self.maximum_timeline_entries,
            self.maximum_schema_string_bytes,
        ];
        if values.contains(&0) {
            return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded));
        }
        Ok(self)
    }
}

/// Complete pure parse request.
pub struct DashMpdParseRequest<'document> {
    /// Caller-owned MPD bytes.
    pub document_bytes: &'document [u8],
    /// Complete hardened XML budgets.
    pub xml_budgets: XmlBudgets,
    /// DASH schema/model caps.
    pub limits: DashMpdLimits,
}

/// Небольшой cursor централизует XML error mapping.
pub(super) struct EventCursor<'document> {
    /// Hardened project-owned reader.
    pub(super) reader: BoundedXmlReader<'document>,
}

/// Period до вычисления omitted start/duration.
pub(super) struct ParsedPeriod {
    /// Optional schema identifier.
    pub(super) id: Option<String>,
    /// Optional explicit start.
    pub(super) start_milliseconds: Option<u64>,
    /// Optional explicit duration.
    pub(super) duration_milliseconds: Option<u64>,
    /// Period BaseURL.
    pub(super) base_url: Option<DashBaseUrl>,
    /// Parsed adaptations.
    pub(super) adaptation_sets: Box<[DashAdaptationSet]>,
}

/// Наследуемые media hints AdaptationSet.
#[derive(Clone, Default)]
struct MediaHints {
    /// MIME type.
    mime_type: Option<String>,
    /// DASH contentType.
    content_type: Option<String>,
    /// Codec list.
    codecs: Option<String>,
}

impl EventCursor<'_> {
    /// Возвращает следующий project-owned event.
    pub(super) fn next_event(&mut self) -> Result<Option<XmlEvent>, DashMpdError> {
        self.reader.next_event().map_err(DashMpdError::from_xml)
    }
}

/// Единственный static DASH MPD entry point.
pub fn parse_dash_mpd(request: DashMpdParseRequest<'_>) -> Result<DashMpd, DashMpdError> {
    let limits = request.limits.validate()?;
    let reader = BoundedXmlReader::new(request.document_bytes, request.xml_budgets)
        .map_err(DashMpdError::from_xml)?;
    let mut cursor = EventCursor { reader };
    let root = match cursor.next_event()? {
        Some(XmlEvent::StartElement(element)) => element,
        _ => return Err(DashMpdError::new(DashMpdErrorKind::InvalidRoot)),
    };
    require_name(root.name(), "MPD", DashMpdErrorKind::InvalidRoot)?;
    let presentation_duration = optional_duration_attribute(&root, "mediaPresentationDuration")?;
    let presentation_type = optional_attribute(&root, "type")?.unwrap_or("static");
    if presentation_type != "static" {
        return Err(DashMpdError::new(DashMpdErrorKind::DynamicPresentation));
    }
    validate_profiles(optional_attribute(&root, "profiles")?)?;
    validate_attributes_with_namespaced_allowlist(
        &root,
        &[
            "id",
            "type",
            "profiles",
            "minBufferTime",
            "mediaPresentationDuration",
            "maxSegmentDuration",
        ],
        &[(
            "http://www.w3.org/2001/XMLSchema-instance",
            "schemaLocation",
        )],
    )?;

    let mut base_url = None;
    let mut periods = Vec::new();
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(element)) if is_name(element.name(), "BaseURL") => {
                set_single_base_url(&mut base_url, parse_base_url(&mut cursor, element, limits)?)?;
            }
            Some(XmlEvent::StartElement(element)) if is_name(element.name(), "Period") => {
                if periods.len() >= limits.maximum_periods {
                    return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded));
                }
                periods.push(parse_period(&mut cursor, element, limits)?);
            }
            Some(XmlEvent::StartElement(element))
                if is_name(element.name(), "ContentProtection") =>
            {
                return Err(DashMpdError::new(DashMpdErrorKind::ContentProtection));
            }
            Some(XmlEvent::EmptyElement(element))
                if is_name(element.name(), "ContentProtection") =>
            {
                return Err(DashMpdError::new(DashMpdErrorKind::ContentProtection));
            }
            Some(XmlEvent::EndElement(name)) if is_name(&name, "MPD") => break,
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(_) | None => {
                return Err(DashMpdError::new(DashMpdErrorKind::UnsupportedConstruct));
            }
        }
    }
    if cursor.next_event()?.is_some() || periods.is_empty() {
        return Err(DashMpdError::new(DashMpdErrorKind::InvalidRoot));
    }
    let (periods, total_duration) = finalize_periods(periods, presentation_duration)?;
    Ok(DashMpd {
        media_presentation_duration: DashPresentationDuration::FiniteMilliseconds(total_duration),
        base_url,
        periods,
    })
}

/// Проверяет каждый comma-separated profile как exact allowlisted identifier.
pub(super) fn validate_profiles(profiles: Option<&str>) -> Result<(), DashMpdError> {
    let Some(profiles) = profiles else {
        return Ok(());
    };
    for profile in profiles.split(',').map(str::trim) {
        if profile.is_empty() || !SUPPORTED_DASH_PROFILES.contains(&profile) {
            return Err(DashMpdError::new(DashMpdErrorKind::UnsupportedProfile));
        }
    }
    Ok(())
}

/// Разбирает Period без предположений о следующем Period.
pub(super) fn parse_period(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    limits: DashMpdLimits,
) -> Result<ParsedPeriod, DashMpdError> {
    validate_attributes(&element, &["id", "start", "duration"])?;
    let id = bounded_optional_attribute(&element, "id", limits)?;
    let start_milliseconds = optional_duration_attribute(&element, "start")?;
    let duration_milliseconds = optional_duration_attribute(&element, "duration")?;
    let mut base_url = None;
    let mut adaptation_sets = Vec::new();
    let mut encountered_adaptation_set_count = 0_usize;
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "BaseURL") => {
                set_single_base_url(&mut base_url, parse_base_url(cursor, child, limits)?)?;
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "AdaptationSet") => {
                if encountered_adaptation_set_count >= limits.maximum_adaptation_sets_per_period {
                    return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded));
                }
                encountered_adaptation_set_count += 1;
                if is_declared_text_adaptation_set(&child, limits)? {
                    consume_non_playback_text_adaptation_set(cursor, &child, limits)?;
                } else {
                    adaptation_sets.push(parse_adaptation_set(cursor, child, limits)?);
                }
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "ContentProtection") => {
                return Err(DashMpdError::new(DashMpdErrorKind::ContentProtection));
            }
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "ContentProtection") => {
                return Err(DashMpdError::new(DashMpdErrorKind::ContentProtection));
            }
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "ContentProtection") => {
                return Err(DashMpdError::new(DashMpdErrorKind::ContentProtection));
            }
            Some(XmlEvent::EndElement(name)) if is_name(&name, "Period") => break,
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(_) | None => {
                return Err(DashMpdError::new(DashMpdErrorKind::UnsupportedConstruct));
            }
        }
    }
    if adaptation_sets.is_empty() {
        return Err(DashMpdError::new(DashMpdErrorKind::MalformedSchema));
    }
    Ok(ParsedPeriod {
        id,
        start_milliseconds,
        duration_milliseconds,
        base_url,
        adaptation_sets: adaptation_sets.into_boxed_slice(),
    })
}

/// Разбирает AdaptationSet и применяет его addressing к Representation без собственного.
fn parse_adaptation_set(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    limits: DashMpdLimits,
) -> Result<DashAdaptationSet, DashMpdError> {
    validate_attributes(
        &element,
        &[
            "id",
            "mimeType",
            "contentType",
            "codecs",
            "lang",
            "width",
            "height",
            "frameRate",
            "audioSamplingRate",
            "segmentAlignment",
            "subsegmentAlignment",
            "startWithSAP",
            "par",
            "minWidth",
            "maxWidth",
            "minHeight",
            "maxHeight",
            "maxFrameRate",
        ],
    )?;
    let declared_picture_aspect_ratio = validate_adaptation_constraints(&element, limits)?;
    let id = bounded_optional_attribute(&element, "id", limits)?;
    let hints = media_hints(&element, limits)?;
    let mut metadata = representation_metadata(&element, limits)?;
    let mut base_url = None;
    let mut inherited_addressing = None;
    let mut representations = Vec::new();
    let mut observed_unsupported_media_representation = false;
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "Role") => {
                validate_main_role(&child, limits)?;
                consume_descriptor_body(cursor, "Role")?;
            }
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "Role") => {
                validate_main_role(&child, limits)?;
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "BaseURL") => {
                set_single_base_url(&mut base_url, parse_base_url(cursor, child, limits)?)?;
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "SegmentTemplate") => {
                set_single_addressing(
                    &mut inherited_addressing,
                    DashAddressing::Template(parse_segment_template(cursor, child, limits)?),
                )?;
            }
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "SegmentTemplate") => {
                set_single_addressing(
                    &mut inherited_addressing,
                    DashAddressing::Template(parse_empty_segment_template_leaf(child, limits)?),
                )?;
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "SegmentList") => {
                set_single_addressing(
                    &mut inherited_addressing,
                    DashAddressing::List(parse_segment_list(cursor, child, limits)?),
                )?;
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "SegmentBase") => {
                set_single_addressing(
                    &mut inherited_addressing,
                    DashAddressing::Base(parse_segment_base(cursor, child, limits)?),
                )?;
            }
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "SegmentBase") => {
                set_single_addressing(
                    &mut inherited_addressing,
                    DashAddressing::Base(parse_empty_segment_base(child)?),
                )?;
            }
            Some(XmlEvent::StartElement(child))
                if is_name(child.name(), "AudioChannelConfiguration") =>
            {
                let configuration = parse_audio_channel_configuration(&child, limits)?;
                consume_descriptor_body(cursor, "AudioChannelConfiguration")?;
                set_optional_metadata(&mut metadata.audio_channel_configuration, configuration)?;
            }
            Some(XmlEvent::EmptyElement(child))
                if is_name(child.name(), "AudioChannelConfiguration") =>
            {
                let configuration = parse_audio_channel_configuration(&child, limits)?;
                set_optional_metadata(&mut metadata.audio_channel_configuration, configuration)?;
            }
            Some(XmlEvent::StartElement(child))
                if is_name(child.name(), "EssentialProperty")
                    || is_name(child.name(), "SupplementalProperty") =>
            {
                let essential = is_name(child.name(), "EssentialProperty");
                let name = if essential {
                    "EssentialProperty"
                } else {
                    "SupplementalProperty"
                };
                apply_color_descriptor(&child, limits, essential, &mut metadata.color)?;
                consume_descriptor_body(cursor, name)?;
            }
            Some(XmlEvent::EmptyElement(child))
                if is_name(child.name(), "EssentialProperty")
                    || is_name(child.name(), "SupplementalProperty") =>
            {
                let essential = is_name(child.name(), "EssentialProperty");
                apply_color_descriptor(&child, limits, essential, &mut metadata.color)?;
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "Representation") => {
                if representations.len() >= limits.maximum_representations_per_adaptation_set {
                    return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded));
                }
                match parse_representation(
                    cursor,
                    child,
                    limits,
                    &hints,
                    &metadata,
                    inherited_addressing.clone(),
                ) {
                    Ok(representation) => representations.push(representation),
                    Err(error) if error.kind() == DashMpdErrorKind::UnsupportedMediaEvidence => {
                        observed_unsupported_media_representation = true;
                    }
                    Err(error) => return Err(error),
                }
            }
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "Representation") => {
                if representations.len() >= limits.maximum_representations_per_adaptation_set {
                    return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded));
                }
                match parse_empty_representation(
                    child,
                    limits,
                    &hints,
                    &metadata,
                    inherited_addressing.clone(),
                ) {
                    Ok(representation) => representations.push(representation),
                    Err(error) if error.kind() == DashMpdErrorKind::UnsupportedMediaEvidence => {
                        observed_unsupported_media_representation = true;
                    }
                    Err(error) => return Err(error),
                }
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "ContentProtection") => {
                return Err(DashMpdError::new(DashMpdErrorKind::ContentProtection));
            }
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "ContentProtection") => {
                return Err(DashMpdError::new(DashMpdErrorKind::ContentProtection));
            }
            Some(XmlEvent::EndElement(name)) if is_name(&name, "AdaptationSet") => break,
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(_) | None => {
                return Err(DashMpdError::new(DashMpdErrorKind::UnsupportedConstruct));
            }
        }
    }
    if representations.is_empty() {
        if observed_unsupported_media_representation {
            return Err(DashMpdError::new(
                DashMpdErrorKind::UnsupportedMediaEvidence,
            ));
        }
        return Err(DashMpdError::new(DashMpdErrorKind::MalformedSchema));
    }
    validate_representation_picture_aspect_ratio(declared_picture_aspect_ratio, &representations)?;
    Ok(DashAdaptationSet {
        id,
        base_url,
        representations: representations.into_boxed_slice(),
    })
}

/// Разбирает Representation и доказывает container/component shape.
fn parse_representation(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    limits: DashMpdLimits,
    inherited_hints: &MediaHints,
    inherited_metadata: &RepresentationMetadata,
    inherited_addressing: Option<DashAddressing>,
) -> Result<DashRepresentation, DashMpdError> {
    validate_attributes(
        &element,
        &[
            "id",
            "bandwidth",
            "mimeType",
            "contentType",
            "codecs",
            "width",
            "height",
            "frameRate",
            "audioSamplingRate",
            "startWithSAP",
            "sar",
        ],
    )?;
    let id = required_bounded_attribute(&element, "id", limits)?;
    let bandwidth = optional_u64_attribute(&element, "bandwidth")?;
    let own_hints = media_hints(&element, limits)?;
    let effective_hints = merge_hints(inherited_hints, own_hints);
    let mut own_metadata = representation_metadata(&element, limits)?;
    let mut base_url = None;
    let mut own_addressing = None;
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "BaseURL") => {
                set_single_base_url(&mut base_url, parse_base_url(cursor, child, limits)?)?;
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "SegmentTemplate") => {
                set_single_addressing(
                    &mut own_addressing,
                    DashAddressing::Template(parse_segment_template(cursor, child, limits)?),
                )?;
            }
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "SegmentTemplate") => {
                set_single_addressing(
                    &mut own_addressing,
                    DashAddressing::Template(parse_empty_segment_template_leaf(child, limits)?),
                )?;
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "SegmentList") => {
                set_single_addressing(
                    &mut own_addressing,
                    DashAddressing::List(parse_segment_list(cursor, child, limits)?),
                )?;
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "SegmentBase") => {
                set_single_addressing(
                    &mut own_addressing,
                    DashAddressing::Base(parse_segment_base(cursor, child, limits)?),
                )?;
            }
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "SegmentBase") => {
                set_single_addressing(
                    &mut own_addressing,
                    DashAddressing::Base(parse_empty_segment_base(child)?),
                )?;
            }
            Some(XmlEvent::StartElement(child))
                if is_name(child.name(), "AudioChannelConfiguration") =>
            {
                let configuration = parse_audio_channel_configuration(&child, limits)?;
                consume_descriptor_body(cursor, "AudioChannelConfiguration")?;
                set_optional_metadata(
                    &mut own_metadata.audio_channel_configuration,
                    configuration,
                )?;
            }
            Some(XmlEvent::EmptyElement(child))
                if is_name(child.name(), "AudioChannelConfiguration") =>
            {
                let configuration = parse_audio_channel_configuration(&child, limits)?;
                set_optional_metadata(
                    &mut own_metadata.audio_channel_configuration,
                    configuration,
                )?;
            }
            Some(XmlEvent::StartElement(child))
                if is_name(child.name(), "EssentialProperty")
                    || is_name(child.name(), "SupplementalProperty") =>
            {
                let essential = is_name(child.name(), "EssentialProperty");
                let name = if essential {
                    "EssentialProperty"
                } else {
                    "SupplementalProperty"
                };
                apply_color_descriptor(&child, limits, essential, &mut own_metadata.color)?;
                consume_descriptor_body(cursor, name)?;
            }
            Some(XmlEvent::EmptyElement(child))
                if is_name(child.name(), "EssentialProperty")
                    || is_name(child.name(), "SupplementalProperty") =>
            {
                let essential = is_name(child.name(), "EssentialProperty");
                apply_color_descriptor(&child, limits, essential, &mut own_metadata.color)?;
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "ContentProtection") => {
                return Err(DashMpdError::new(DashMpdErrorKind::ContentProtection));
            }
            Some(XmlEvent::EndElement(name)) if is_name(&name, "Representation") => break,
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(_) | None => {
                return Err(DashMpdError::new(DashMpdErrorKind::UnsupportedConstruct));
            }
        }
    }
    validate_square_sample_aspect_ratio(&element, limits)?;
    let (container, media_kind, codecs) = classify_media(&effective_hints)?;
    let metadata = merge_representation_metadata(inherited_metadata, own_metadata);
    Ok(DashRepresentation {
        id,
        bandwidth,
        width: metadata.width,
        height: metadata.height,
        frame_rate: metadata.frame_rate,
        audio_sampling_rate: metadata.audio_sampling_rate,
        audio_channel_configuration: metadata.audio_channel_configuration,
        language: metadata.language,
        color: metadata.color,
        container,
        media_kind,
        codecs,
        base_url,
        addressing: own_addressing
            .or(inherited_addressing)
            .unwrap_or(DashAddressing::SingleResource),
    })
}

/// Разбирает attributes-only Representation с inherited addressing.
fn parse_empty_representation(
    element: XmlElement,
    limits: DashMpdLimits,
    inherited_hints: &MediaHints,
    inherited_metadata: &RepresentationMetadata,
    inherited_addressing: Option<DashAddressing>,
) -> Result<DashRepresentation, DashMpdError> {
    validate_attributes(
        &element,
        &[
            "id",
            "bandwidth",
            "mimeType",
            "contentType",
            "codecs",
            "width",
            "height",
            "frameRate",
            "audioSamplingRate",
            "startWithSAP",
            "sar",
        ],
    )?;
    validate_square_sample_aspect_ratio(&element, limits)?;
    let id = required_bounded_attribute(&element, "id", limits)?;
    let bandwidth = optional_u64_attribute(&element, "bandwidth")?;
    let effective_hints = merge_hints(inherited_hints, media_hints(&element, limits)?);
    let metadata = merge_representation_metadata(
        inherited_metadata,
        representation_metadata(&element, limits)?,
    );
    let (container, media_kind, codecs) = classify_media(&effective_hints)?;
    Ok(DashRepresentation {
        id,
        bandwidth,
        width: metadata.width,
        height: metadata.height,
        frame_rate: metadata.frame_rate,
        audio_sampling_rate: metadata.audio_sampling_rate,
        audio_channel_configuration: metadata.audio_channel_configuration,
        language: metadata.language,
        color: metadata.color,
        container,
        media_kind,
        codecs,
        base_url: None,
        addressing: inherited_addressing.unwrap_or(DashAddressing::SingleResource),
    })
}

/// Проверяет известные non-playback constraints и возвращает optional picture ratio.
fn validate_adaptation_constraints(
    element: &XmlElement,
    limits: DashMpdLimits,
) -> Result<Option<(u32, u32)>, DashMpdError> {
    optional_boolean_attribute(element, "segmentAlignment")?;
    optional_boolean_attribute(element, "subsegmentAlignment")?;
    let minimum_width = optional_positive_u32_attribute(element, "minWidth")?;
    let maximum_width = optional_positive_u32_attribute(element, "maxWidth")?;
    let minimum_height = optional_positive_u32_attribute(element, "minHeight")?;
    let maximum_height = optional_positive_u32_attribute(element, "maxHeight")?;
    if minimum_width
        .zip(maximum_width)
        .is_some_and(|(minimum, maximum)| minimum > maximum)
        || minimum_height
            .zip(maximum_height)
            .is_some_and(|(minimum, maximum)| minimum > maximum)
    {
        return Err(DashMpdError::new(DashMpdErrorKind::InvalidAttribute));
    }
    optional_frame_rate_attribute(element, "maxFrameRate")?;
    optional_positive_ratio_attribute(element, "par", ':', limits)
}

/// Для пока не моделируемого SAR допускает только square pixels без silent distortion.
fn validate_square_sample_aspect_ratio(
    element: &XmlElement,
    limits: DashMpdLimits,
) -> Result<(), DashMpdError> {
    if optional_positive_ratio_attribute(element, "sar", ':', limits)?
        .is_some_and(|sample_aspect_ratio| sample_aspect_ratio != (1, 1))
    {
        return Err(DashMpdError::new(
            DashMpdErrorKind::UnsupportedMediaEvidence,
        ));
    }
    Ok(())
}

/// Square-pixel Representation dimensions обязаны совпадать с AdaptationSet `par`.
fn validate_representation_picture_aspect_ratio(
    picture_aspect_ratio: Option<(u32, u32)>,
    representations: &[DashRepresentation],
) -> Result<(), DashMpdError> {
    let Some((picture_width, picture_height)) = picture_aspect_ratio else {
        return Ok(());
    };
    for representation in representations {
        let (Some(width), Some(height)) = (representation.width, representation.height) else {
            return Err(DashMpdError::new(
                DashMpdErrorKind::UnsupportedMediaEvidence,
            ));
        };
        if u64::from(width) * u64::from(picture_height)
            != u64::from(height) * u64::from(picture_width)
        {
            return Err(DashMpdError::new(
                DashMpdErrorKind::UnsupportedMediaEvidence,
            ));
        }
    }
    Ok(())
}

/// DASH role пока не участвует в selection; безопасно принимается только exact `main`.
fn validate_main_role(element: &XmlElement, limits: DashMpdLimits) -> Result<(), DashMpdError> {
    validate_attributes(element, &["schemeIdUri", "value"])?;
    let scheme = bounded_optional_attribute(element, "schemeIdUri", limits)?;
    let value = bounded_optional_attribute(element, "value", limits)?;
    if scheme.as_deref() != Some("urn:mpeg:dash:role:2011") || value.as_deref() != Some("main") {
        return Err(DashMpdError::new(DashMpdErrorKind::UnsupportedConstruct));
    }
    Ok(())
}

/// Вычисляет exact contiguous Period timeline.
pub(super) fn finalize_periods(
    parsed: Vec<ParsedPeriod>,
    presentation_duration: Option<u64>,
) -> Result<(Box<[DashPeriod]>, u64), DashMpdError> {
    let mut periods = Vec::with_capacity(parsed.len());
    let mut expected_start = 0_u64;
    for (index, period) in parsed.iter().enumerate() {
        let start = period.start_milliseconds.unwrap_or(expected_start);
        if start != expected_start {
            return Err(DashMpdError::new(DashMpdErrorKind::InvalidPeriodTimeline));
        }
        let duration = period
            .duration_milliseconds
            .or_else(|| {
                parsed
                    .get(index + 1)
                    .and_then(|next| next.start_milliseconds)
                    .and_then(|next_start| next_start.checked_sub(start))
            })
            .or_else(|| presentation_duration.and_then(|total| total.checked_sub(start)))
            .filter(|duration| *duration > 0)
            .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidPeriodTimeline))?;
        expected_start = start
            .checked_add(duration)
            .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidPeriodTimeline))?;
        periods.push(DashPeriod {
            id: period.id.clone(),
            start_milliseconds: start,
            duration: DashPresentationDuration::FiniteMilliseconds(duration),
            base_url: period.base_url.clone(),
            adaptation_sets: period.adaptation_sets.clone(),
        });
    }
    if let Some(declared) = presentation_duration
        && declared != expected_start
    {
        return Err(DashMpdError::new(DashMpdErrorKind::InvalidPeriodTimeline));
    }
    Ok((periods.into_boxed_slice(), expected_start))
}

/// Парсит BaseURL как text-only leaf.
pub(super) fn parse_base_url(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    limits: DashMpdLimits,
) -> Result<DashBaseUrl, DashMpdError> {
    validate_attributes(
        &element,
        &[
            "serviceLocation",
            "availabilityTimeOffset",
            "availabilityTimeComplete",
        ],
    )?;
    let availability_time_offset_nanoseconds =
        optional_decimal_seconds_nanoseconds_attribute(&element, "availabilityTimeOffset")?;
    let availability_time_complete =
        optional_boolean_attribute(&element, "availabilityTimeComplete")?;
    let text = read_text_leaf(cursor, "BaseURL", limits)?;
    Ok(DashBaseUrl::with_availability(
        DashUrlReference::new(text),
        availability_time_offset_nanoseconds,
        availability_time_complete,
    ))
}

/// Сохраняет cardinality 0..1 BaseURL на одном уровне.
pub(super) fn set_single_base_url(
    slot: &mut Option<DashBaseUrl>,
    value: DashBaseUrl,
) -> Result<(), DashMpdError> {
    if slot.replace(value).is_some() {
        return Err(DashMpdError::new(DashMpdErrorKind::MultipleBaseUrls));
    }
    Ok(())
}

/// Сохраняет ровно один addressing mode на уровне.
fn set_single_addressing(
    slot: &mut Option<DashAddressing>,
    value: DashAddressing,
) -> Result<(), DashMpdError> {
    if slot.replace(value).is_some() {
        return Err(DashMpdError::new(DashMpdErrorKind::InvalidAddressing));
    }
    Ok(())
}

include!("parser_values.rs");
include!("parser_addressing.rs");
