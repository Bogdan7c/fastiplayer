//! Structural parser узкого bounded Smooth Streaming VOD client manifest profile.

use bounded_xml_reader::{BoundedXmlReader, XmlBudgets, XmlElement, XmlEvent, XmlExpandedName};

use crate::error::{
    SmoothManifestError, SmoothProfileIncompatibility, SmoothSchemaField, SmoothTimelineError,
    SmoothUnsupportedConstruct,
};
use crate::limits::{SmoothManifestLimitKind, SmoothManifestLimits};
use crate::model::{
    SmoothDeclaredQualityCount, SmoothDeclaredStreamCount, SmoothManifest, SmoothManifestVersion,
    SmoothStream, SmoothStreamConstruction, SmoothStreamIdentityMetadata, SmoothStreamKind,
    SmoothStreamLanguage, SmoothStreamName,
};
use crate::parser_quality::{parse_empty_quality, parse_quality};
use crate::parser_values::{
    is_unqualified_name, optional_attribute, parse_bool, parse_positive_u16, parse_positive_u64,
    parse_u64, require_unqualified_name, required_attribute, unsupported_child,
    validate_attributes, validate_schema_string,
};
use crate::template::SmoothFragmentUrlTemplate;
use crate::time::{SmoothTime, SmoothTimescale};
use crate::timeline::SmoothManifestTimelineBudget;
use crate::timeline_input::{
    SmoothChunkDuration, SmoothChunkEntry, SmoothChunkRepeat, SmoothChunkStart,
    SmoothDeclaredFragmentCount,
};

/// MS-SSTR default root clock применяется только при отсутствии `TimeScale`.
pub const SMOOTH_STREAMING_DEFAULT_TIMESCALE_TICKS_PER_SECOND: u64 = 10_000_000;

/// Caller-owned input и оба независимых budget набора одного parse.
#[derive(Clone)]
pub struct SmoothManifestParseRequest<'document> {
    pub document_bytes: &'document [u8],
    pub xml_budgets: XmlBudgets,
    pub limits: SmoothManifestLimits,
}

/// Обычный entry point является точной never-cancelled делегацией.
pub fn parse_vod_client_manifest(
    request: SmoothManifestParseRequest<'_>,
) -> Result<SmoothManifest, SmoothManifestError> {
    parse_vod_client_manifest_cancellable(request, &mut || false)
}

/// Cancellation-aware entry point никогда не публикует частичный manifest.
pub fn parse_vod_client_manifest_cancellable(
    request: SmoothManifestParseRequest<'_>,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> Result<SmoothManifest, SmoothManifestError> {
    check_cancelled(is_cancelled)?;
    let reader = BoundedXmlReader::new(request.document_bytes, request.xml_budgets)
        .map_err(|source| SmoothManifestError::Xml { source })?;
    let mut cursor = EventCursor {
        reader,
        is_cancelled,
    };
    let root = match cursor.next_event()? {
        Some(XmlEvent::StartElement(element)) => element,
        Some(XmlEvent::EmptyElement(element)) => {
            require_unqualified_name(
                element.name(),
                "SmoothStreamingMedia",
                SmoothSchemaField::Root,
            )?;
            return Err(SmoothManifestError::ProfileIncompatible {
                reason: SmoothProfileIncompatibility::MissingRequiredStream,
            });
        }
        _ => {
            return Err(SmoothManifestError::MalformedSchema {
                field: SmoothSchemaField::Root,
            });
        }
    };
    require_unqualified_name(root.name(), "SmoothStreamingMedia", SmoothSchemaField::Root)?;
    let root_metadata = parse_root_metadata(&root)?;
    let mut timeline_budget = SmoothManifestTimelineBudget::new(&request.limits);
    let mut streams = Vec::new();
    let mut accepted_quality_count = 0usize;
    let mut accepted_custom_attribute_count = 0usize;

    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(element))
                if is_unqualified_name(element.name(), "StreamIndex") =>
            {
                enforce_limit(
                    streams.len().saturating_add(1),
                    request.limits.maximum_streams(),
                    SmoothManifestLimitKind::Streams,
                )?;
                let stream = parse_stream(
                    &mut cursor,
                    element,
                    root_metadata.version,
                    root_metadata.timescale,
                    &request.limits,
                    &mut timeline_budget,
                    &mut accepted_custom_attribute_count,
                )?;
                accepted_quality_count = accepted_quality_count
                    .checked_add(stream.qualities().len())
                    .ok_or(SmoothManifestError::LimitExceeded {
                        limit: SmoothManifestLimitKind::TotalQualities,
                        maximum: request.limits.maximum_total_qualities(),
                    })?;
                enforce_limit(
                    accepted_quality_count,
                    request.limits.maximum_total_qualities(),
                    SmoothManifestLimitKind::TotalQualities,
                )?;
                streams.push(stream);
            }
            Some(XmlEvent::EmptyElement(element))
                if is_unqualified_name(element.name(), "StreamIndex") =>
            {
                return Err(SmoothManifestError::MalformedSchema {
                    field: SmoothSchemaField::StreamIndex,
                });
            }
            Some(XmlEvent::StartElement(element) | XmlEvent::EmptyElement(element))
                if is_drm_name(element.name()) =>
            {
                return Err(SmoothManifestError::DrmProtected);
            }
            Some(XmlEvent::EndElement(name))
                if is_unqualified_name(&name, "SmoothStreamingMedia") =>
            {
                break;
            }
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(XmlEvent::StartElement(element) | XmlEvent::EmptyElement(element)) => {
                return Err(unsupported_child(element.name()));
            }
            Some(XmlEvent::EndElement(name)) => return Err(unsupported_child(&name)),
            Some(XmlEvent::Text(_)) | None => {
                return Err(SmoothManifestError::MalformedSchema {
                    field: SmoothSchemaField::Root,
                });
            }
        }
    }
    if cursor.next_event()?.is_some() {
        return Err(SmoothManifestError::MalformedSchema {
            field: SmoothSchemaField::Root,
        });
    }
    check_cancelled(cursor.is_cancelled)?;
    SmoothManifest::new_vod(
        root_metadata.version,
        SmoothTime::new(root_metadata.duration_ticks, root_metadata.timescale),
        streams,
        root_metadata.declared_stream_count,
        &request.limits,
    )
}

/// Root metadata остаётся локальным parser state до полной проверки документа.
#[derive(Debug, Clone, Copy)]
struct RootMetadata {
    version: SmoothManifestVersion,
    timescale: SmoothTimescale,
    duration_ticks: u64,
    declared_stream_count: SmoothDeclaredStreamCount,
}

/// Валидирует exact VOD root attributes и исключения до чтения body.
fn parse_root_metadata(root: &XmlElement) -> Result<RootMetadata, SmoothManifestError> {
    validate_attributes(
        root,
        &[
            "MajorVersion",
            "MinorVersion",
            "TimeScale",
            "Duration",
            "IsLive",
            "LookAheadFragmentCount",
            "DVRWindowLength",
            "StreamIndexCount",
        ],
    )?;
    if optional_attribute(root, "LookAheadFragmentCount")?.is_some() {
        return Err(SmoothManifestError::UnsupportedConstruct {
            construct: SmoothUnsupportedConstruct::LookAheadFragments,
        });
    }
    if optional_attribute(root, "DVRWindowLength")?.is_some() {
        return Err(SmoothManifestError::UnsupportedConstruct {
            construct: SmoothUnsupportedConstruct::DvrWindow,
        });
    }
    if optional_attribute(root, "IsLive")?
        .map(|value| parse_bool(value, SmoothSchemaField::Root))
        .transpose()?
        .unwrap_or(false)
    {
        return Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::LiveManifest,
        });
    }
    let major = parse_positive_u16(
        required_attribute(root, "MajorVersion", SmoothSchemaField::MajorVersion)?,
        SmoothSchemaField::MajorVersion,
    )?;
    let minor = u16::try_from(parse_u64(
        required_attribute(root, "MinorVersion", SmoothSchemaField::MinorVersion)?,
        SmoothSchemaField::MinorVersion,
    )?)
    .map_err(|_| SmoothManifestError::MalformedSchema {
        field: SmoothSchemaField::MinorVersion,
    })?;
    let timescale = optional_attribute(root, "TimeScale")?
        .map(|value| parse_positive_u64(value, SmoothSchemaField::TimeScale))
        .transpose()?
        .map(SmoothTimescale::new)
        .transpose()
        .map_err(|_| SmoothManifestError::MalformedSchema {
            field: SmoothSchemaField::TimeScale,
        })?
        .unwrap_or_else(default_root_timescale);
    let duration_ticks = parse_positive_u64(
        required_attribute(root, "Duration", SmoothSchemaField::Duration)?,
        SmoothSchemaField::Duration,
    )?;
    let declared_stream_count = optional_attribute(root, "StreamIndexCount")?
        .map(|value| parse_u64(value, SmoothSchemaField::Root))
        .transpose()?
        .map_or(
            SmoothDeclaredStreamCount::Unspecified,
            SmoothDeclaredStreamCount::Exact,
        );
    Ok(RootMetadata {
        version: SmoothManifestVersion::from_major_minor(major, minor)?,
        timescale,
        duration_ticks,
        declared_stream_count,
    })
}

/// Parser cursor совмещает event boundary и обязательный cancellation poll.
pub(super) struct EventCursor<'input, 'cancel> {
    reader: BoundedXmlReader<'input>,
    pub(super) is_cancelled: &'cancel mut dyn FnMut() -> bool,
}

impl EventCursor<'_, '_> {
    /// Сохраняет исходный `XmlReadError` без перекодирования taxonomy.
    pub(super) fn next_event(&mut self) -> Result<Option<XmlEvent>, SmoothManifestError> {
        check_cancelled(self.is_cancelled)?;
        self.reader
            .next_event()
            .map_err(|source| SmoothManifestError::Xml { source })
    }
}

/// Parser-local StreamIndex attributes после lexical validation.
struct StreamMetadata {
    kind: SmoothStreamKind,
    identity_metadata: SmoothStreamIdentityMetadata,
    timescale: SmoothTimescale,
    url_template: SmoothFragmentUrlTemplate,
    inherited_four_cc: Option<String>,
    declared_quality_count: SmoothDeclaredQualityCount,
    declared_fragment_count: SmoothDeclaredFragmentCount,
}

/// Читает один StreamIndex и транзакционно добавляет его timeline accounting.
fn parse_stream(
    cursor: &mut EventCursor<'_, '_>,
    element: XmlElement,
    version: SmoothManifestVersion,
    inherited_timescale: SmoothTimescale,
    limits: &SmoothManifestLimits,
    timeline_budget: &mut SmoothManifestTimelineBudget<'_>,
    accepted_custom_attribute_count: &mut usize,
) -> Result<SmoothStream, SmoothManifestError> {
    let metadata = parse_stream_metadata(&element, inherited_timescale, limits)?;
    let mut qualities = Vec::new();
    let mut entries = Vec::new();
    let mut timeline_started = false;
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(child))
                if is_unqualified_name(child.name(), "QualityLevel") && !timeline_started =>
            {
                enforce_limit(
                    qualities.len().saturating_add(1),
                    limits.maximum_qualities_per_stream(),
                    SmoothManifestLimitKind::QualitiesPerStream,
                )?;
                qualities.push(parse_quality(
                    cursor,
                    child,
                    metadata.kind,
                    metadata.inherited_four_cc.as_deref(),
                    limits,
                    accepted_custom_attribute_count,
                )?);
            }
            Some(XmlEvent::EmptyElement(child))
                if is_unqualified_name(child.name(), "QualityLevel") && !timeline_started =>
            {
                enforce_limit(
                    qualities.len().saturating_add(1),
                    limits.maximum_qualities_per_stream(),
                    SmoothManifestLimitKind::QualitiesPerStream,
                )?;
                qualities.push(parse_empty_quality(
                    child,
                    metadata.kind,
                    metadata.inherited_four_cc.as_deref(),
                    limits,
                    cursor.is_cancelled,
                    accepted_custom_attribute_count,
                )?);
            }
            Some(XmlEvent::EmptyElement(child)) if is_unqualified_name(child.name(), "c") => {
                timeline_started = true;
                enforce_limit(
                    entries.len().saturating_add(1),
                    limits.maximum_timeline_entries_per_stream(),
                    SmoothManifestLimitKind::TimelineEntriesPerStream,
                )?;
                entries.push(parse_chunk(&child)?);
            }
            Some(XmlEvent::StartElement(child)) if is_unqualified_name(child.name(), "c") => {
                return Err(SmoothManifestError::MalformedSchema {
                    field: SmoothSchemaField::Timeline,
                });
            }
            Some(XmlEvent::StartElement(child) | XmlEvent::EmptyElement(child))
                if is_drm_name(child.name()) =>
            {
                return Err(SmoothManifestError::DrmProtected);
            }
            Some(XmlEvent::EndElement(name)) if is_unqualified_name(&name, "StreamIndex") => break,
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(XmlEvent::StartElement(child) | XmlEvent::EmptyElement(child)) => {
                return Err(unsupported_child(child.name()));
            }
            Some(XmlEvent::EndElement(name)) => return Err(unsupported_child(&name)),
            Some(XmlEvent::Text(_)) | None => {
                return Err(SmoothManifestError::MalformedSchema {
                    field: SmoothSchemaField::StreamIndex,
                });
            }
        }
    }
    let timeline = timeline_budget.build_stream_timeline_cancellable(
        version,
        metadata.timescale,
        &entries,
        metadata.declared_fragment_count,
        cursor.is_cancelled,
    )?;
    SmoothStream::new(
        SmoothStreamConstruction {
            kind: metadata.kind,
            identity_metadata: metadata.identity_metadata,
            timescale: metadata.timescale,
            url_template: metadata.url_template,
            qualities,
            timeline,
            declared_quality_count: metadata.declared_quality_count,
        },
        limits,
    )
}

/// Валидирует standard StreamIndex vocabulary и explicit excluded stream shapes.
fn parse_stream_metadata(
    element: &XmlElement,
    inherited_timescale: SmoothTimescale,
    limits: &SmoothManifestLimits,
) -> Result<StreamMetadata, SmoothManifestError> {
    validate_attributes(
        element,
        &[
            "Type",
            "Name",
            "Subtype",
            "Chunks",
            "TimeScale",
            "QualityLevels",
            "Url",
            "Language",
            "FourCC",
            "MaxWidth",
            "MaxHeight",
            "DisplayWidth",
            "DisplayHeight",
            "ParentStreamIndex",
            "ManifestOutput",
        ],
    )?;
    if optional_attribute(element, "ParentStreamIndex")?.is_some() {
        return Err(profile_error(SmoothProfileIncompatibility::EmbeddedStream));
    }
    if optional_attribute(element, "ManifestOutput")?.is_some() {
        return Err(profile_error(SmoothProfileIncompatibility::CompositeStream));
    }
    if optional_attribute(element, "Subtype")?
        .is_some_and(|value| value.eq_ignore_ascii_case("TRICKMODE"))
    {
        return Err(profile_error(SmoothProfileIncompatibility::TrickModeStream));
    }
    for attribute_name in ["Name", "Language", "Subtype"] {
        if let Some(value) = optional_attribute(element, attribute_name)? {
            validate_schema_string(value, limits, SmoothSchemaField::StreamIndex)?;
        }
    }
    let name = optional_attribute(element, "Name")?
        .map(|value| {
            validate_schema_string(value, limits, SmoothSchemaField::StreamIndex)?;
            Ok(SmoothStreamName::from_validated(value.to_owned()))
        })
        .transpose()?;
    let language = optional_attribute(element, "Language")?
        .map(|value| {
            validate_schema_string(value, limits, SmoothSchemaField::StreamIndex)?;
            Ok(SmoothStreamLanguage::from_validated(value.to_owned()))
        })
        .transpose()?;
    let kind = match required_attribute(element, "Type", SmoothSchemaField::StreamIndex)? {
        "video" | "Video" => SmoothStreamKind::Video,
        "audio" | "Audio" => SmoothStreamKind::Audio,
        "text" | "Text" => return Err(profile_error(SmoothProfileIncompatibility::TextStream)),
        "sparse" | "Sparse" => {
            return Err(profile_error(SmoothProfileIncompatibility::SparseStream));
        }
        "embedded" | "Embedded" => {
            return Err(profile_error(SmoothProfileIncompatibility::EmbeddedStream));
        }
        "composite" | "Composite" => {
            return Err(profile_error(SmoothProfileIncompatibility::CompositeStream));
        }
        "trickmode" | "TrickMode" => {
            return Err(profile_error(SmoothProfileIncompatibility::TrickModeStream));
        }
        _ => {
            return Err(profile_error(
                SmoothProfileIncompatibility::UnsupportedStreamKind,
            ));
        }
    };
    if optional_attribute(element, "Subtype")?.is_some() {
        return Err(profile_error(SmoothProfileIncompatibility::VendorExtension));
    }
    for dimension_name in ["MaxWidth", "MaxHeight", "DisplayWidth", "DisplayHeight"] {
        if let Some(value) = optional_attribute(element, dimension_name)? {
            crate::parser_values::parse_positive_u32(value, SmoothSchemaField::StreamIndex)?;
        }
    }
    let timescale = optional_attribute(element, "TimeScale")?
        .map(|value| parse_positive_u64(value, SmoothSchemaField::TimeScale))
        .transpose()?
        .map(SmoothTimescale::new)
        .transpose()
        .map_err(|_| SmoothManifestError::MalformedSchema {
            field: SmoothSchemaField::TimeScale,
        })?
        .unwrap_or(inherited_timescale);
    let url = required_attribute(element, "Url", SmoothSchemaField::Url)?;
    let url_template = SmoothFragmentUrlTemplate::parse(url, limits)?;
    let inherited_four_cc = optional_attribute(element, "FourCC")?.map(str::to_owned);
    let declared_quality_count = SmoothDeclaredQualityCount::Exact(parse_u64(
        required_attribute(element, "QualityLevels", SmoothSchemaField::StreamIndex)?,
        SmoothSchemaField::StreamIndex,
    )?);
    let declared_fragment_count = SmoothDeclaredFragmentCount::Exact(parse_u64(
        required_attribute(element, "Chunks", SmoothSchemaField::StreamIndex)?,
        SmoothSchemaField::StreamIndex,
    )?);
    Ok(StreamMetadata {
        kind,
        identity_metadata: SmoothStreamIdentityMetadata::new(name, language),
        timescale,
        url_template,
        inherited_four_cc,
        declared_quality_count,
        declared_fragment_count,
    })
}

/// Named function делает protocol default видимым и type-checked.
fn default_root_timescale() -> SmoothTimescale {
    SmoothTimescale::new(SMOOTH_STREAMING_DEFAULT_TIMESCALE_TICKS_PER_SECOND)
        .expect("MS-SSTR default timescale ненулевой")
}

/// Парсит raw `<c>` intent, сохраняя absent и distinct negative repeat.
fn parse_chunk(element: &XmlElement) -> Result<SmoothChunkEntry, SmoothManifestError> {
    validate_attributes(element, &["t", "d", "r", "n"])?;
    if optional_attribute(element, "n")?.is_some() {
        return Err(SmoothManifestError::UnsupportedConstruct {
            construct: SmoothUnsupportedConstruct::SparseTimeline,
        });
    }
    let start = optional_attribute(element, "t")?
        .map(|value| parse_u64(value, SmoothSchemaField::Timeline))
        .transpose()?
        .map_or(SmoothChunkStart::Inferred, SmoothChunkStart::Explicit);
    let duration = optional_attribute(element, "d")?
        .map(|value| parse_u64(value, SmoothSchemaField::Timeline))
        .transpose()?
        .map_or(
            SmoothChunkDuration::InferFromNextExplicitStart,
            SmoothChunkDuration::Explicit,
        );
    let repeat = match optional_attribute(element, "r")? {
        None => SmoothChunkRepeat::ImplicitSingle,
        Some(value) if value.starts_with('-') => {
            return Err(SmoothManifestError::InvalidTimeline {
                reason: SmoothTimelineError::NegativeRepeat,
            });
        }
        Some(value) => SmoothChunkRepeat::Declared(parse_u64(value, SmoothSchemaField::Timeline)?),
    };
    Ok(SmoothChunkEntry::new(start, duration, repeat))
}

/// DRM vocabulary имеет приоритет над decoding body.
fn is_drm_name(name: &XmlExpandedName) -> bool {
    name.namespace_uri().is_none() && matches!(name.local_name(), "Protection" | "ProtectionHeader")
}

/// Формирует profile rejection без string matching.
const fn profile_error(reason: SmoothProfileIncompatibility) -> SmoothManifestError {
    SmoothManifestError::ProfileIncompatible { reason }
}

/// Применяет parser-owned counters до allocation/push.
fn enforce_limit(
    observed: usize,
    maximum: usize,
    limit: SmoothManifestLimitKind,
) -> Result<(), SmoothManifestError> {
    if observed > maximum {
        Err(SmoothManifestError::LimitExceeded { limit, maximum })
    } else {
        Ok(())
    }
}

/// Последний poll используется перед финальным immutable construction.
fn check_cancelled(is_cancelled: &mut dyn FnMut() -> bool) -> Result<(), SmoothManifestError> {
    if is_cancelled() {
        Err(SmoothManifestError::Cancelled)
    } else {
        Ok(())
    }
}
