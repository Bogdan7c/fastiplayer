//! Structural parsing и codec proof одного standard QualityLevel subtree.

use bounded_xml_reader::{XmlElement, XmlEvent};

use crate::codec::{parse_aac_lc_configuration, parse_h264_configuration};
use crate::custom_attributes::{
    SmoothCustomAttribute, SmoothCustomAttributeName, SmoothCustomAttributeSet,
    SmoothCustomAttributeValue,
};
use crate::error::{SmoothManifestError, SmoothProfileIncompatibility, SmoothSchemaField};
use crate::limits::{SmoothManifestLimitKind, SmoothManifestLimits};
use crate::model::SmoothStreamKind;
use crate::parser::EventCursor;
use crate::parser_values::{
    is_unqualified_name, optional_attribute, parse_positive_u16, parse_positive_u32,
    parse_positive_u64, parse_u64, required_attribute, unsupported_child, validate_attributes,
};
use crate::quality::{
    SmoothAudioQuality, SmoothQualityIndex, SmoothQualityLevel, SmoothVideoQuality,
};

/// Читает non-empty QualityLevel, разрешая только один CustomAttributes child.
pub(super) fn parse_quality(
    cursor: &mut EventCursor<'_, '_>,
    element: XmlElement,
    kind: SmoothStreamKind,
    inherited_four_cc: Option<&str>,
    limits: &SmoothManifestLimits,
    accepted_custom_attribute_count: &mut usize,
) -> Result<SmoothQualityLevel, SmoothManifestError> {
    let mut custom_attributes = None;
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(child))
                if is_unqualified_name(child.name(), "CustomAttributes") =>
            {
                if custom_attributes.is_some() {
                    return Err(SmoothManifestError::MalformedSchema {
                        field: SmoothSchemaField::CustomAttributes,
                    });
                }
                custom_attributes = Some(parse_custom_attributes(cursor, child, limits)?);
            }
            Some(XmlEvent::EmptyElement(child))
                if is_unqualified_name(child.name(), "CustomAttributes") =>
            {
                validate_attributes(&child, &[])?;
                if custom_attributes
                    .replace(SmoothCustomAttributeSet::new(Vec::new(), limits)?)
                    .is_some()
                {
                    return Err(SmoothManifestError::MalformedSchema {
                        field: SmoothSchemaField::CustomAttributes,
                    });
                }
            }
            Some(XmlEvent::EndElement(name)) if is_unqualified_name(&name, "QualityLevel") => break,
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(XmlEvent::StartElement(child) | XmlEvent::EmptyElement(child)) => {
                return Err(unsupported_child(child.name()));
            }
            Some(XmlEvent::EndElement(name)) => return Err(unsupported_child(&name)),
            Some(XmlEvent::Text(_)) | None => {
                return Err(SmoothManifestError::MalformedSchema {
                    field: SmoothSchemaField::QualityLevel,
                });
            }
        }
    }
    let custom_attributes =
        custom_attributes.unwrap_or(SmoothCustomAttributeSet::new(Vec::new(), limits)?);
    let custom_attribute_count = custom_attributes.len();
    let quality = build_quality(
        &element,
        kind,
        inherited_four_cc,
        custom_attributes,
        limits,
        cursor.is_cancelled,
    )?;
    commit_total_custom_attributes(
        accepted_custom_attribute_count,
        custom_attribute_count,
        limits,
    )?;
    Ok(quality)
}

/// Empty QualityLevel получает тот же validation path с пустым attribute set.
pub(super) fn parse_empty_quality(
    element: XmlElement,
    kind: SmoothStreamKind,
    inherited_four_cc: Option<&str>,
    limits: &SmoothManifestLimits,
    is_cancelled: &mut dyn FnMut() -> bool,
    accepted_custom_attribute_count: &mut usize,
) -> Result<SmoothQualityLevel, SmoothManifestError> {
    let quality = build_quality(
        &element,
        kind,
        inherited_four_cc,
        SmoothCustomAttributeSet::new(Vec::new(), limits)?,
        limits,
        is_cancelled,
    )?;
    commit_total_custom_attributes(accepted_custom_attribute_count, 0, limits)?;
    Ok(quality)
}

/// Shape-typed quality constructor выбирает codec proof по stream axis.
fn build_quality(
    element: &XmlElement,
    kind: SmoothStreamKind,
    inherited_four_cc: Option<&str>,
    custom_attributes: SmoothCustomAttributeSet,
    limits: &SmoothManifestLimits,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> Result<SmoothQualityLevel, SmoothManifestError> {
    match kind {
        SmoothStreamKind::Video => build_video_quality(
            element,
            inherited_four_cc,
            custom_attributes,
            limits,
            is_cancelled,
        )
        .map(SmoothQualityLevel::Video),
        SmoothStreamKind::Audio => build_audio_quality(
            element,
            inherited_four_cc,
            custom_attributes,
            limits,
            is_cancelled,
        )
        .map(SmoothQualityLevel::Audio),
    }
}

/// Валидирует все обязательные H.264 video fields.
fn build_video_quality(
    element: &XmlElement,
    inherited_four_cc: Option<&str>,
    custom_attributes: SmoothCustomAttributeSet,
    limits: &SmoothManifestLimits,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> Result<SmoothVideoQuality, SmoothManifestError> {
    validate_attributes(
        element,
        &[
            "Index",
            "Bitrate",
            "FourCC",
            "MaxWidth",
            "MaxHeight",
            "CodecPrivateData",
            "NALUnitLengthField",
        ],
    )?;
    let index = parse_quality_index(element)?;
    if optional_attribute(element, "NALUnitLengthField")?.is_some_and(|value| value != "4") {
        return Err(profile_error(
            SmoothProfileIncompatibility::UnsupportedCodecProfile,
        ));
    }
    let four_cc = optional_attribute(element, "FourCC")?
        .or(inherited_four_cc)
        .ok_or(SmoothManifestError::MalformedSchema {
            field: SmoothSchemaField::QualityLevel,
        })?;
    let codec_private =
        required_attribute(element, "CodecPrivateData", SmoothSchemaField::QualityLevel)?;
    let (codec, codec_configuration) =
        parse_h264_configuration(four_cc, codec_private, limits, is_cancelled)?;
    SmoothVideoQuality::new(
        index,
        parse_positive_u64(
            required_attribute(element, "Bitrate", SmoothSchemaField::QualityLevel)?,
            SmoothSchemaField::QualityLevel,
        )?,
        parse_positive_u32(
            required_attribute(element, "MaxWidth", SmoothSchemaField::QualityLevel)?,
            SmoothSchemaField::QualityLevel,
        )?,
        parse_positive_u32(
            required_attribute(element, "MaxHeight", SmoothSchemaField::QualityLevel)?,
            SmoothSchemaField::QualityLevel,
        )?,
        codec,
        codec_configuration,
        custom_attributes,
    )
}

/// Валидирует все обязательные AAC-LC fields и exact AudioTag=255.
fn build_audio_quality(
    element: &XmlElement,
    inherited_four_cc: Option<&str>,
    custom_attributes: SmoothCustomAttributeSet,
    limits: &SmoothManifestLimits,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> Result<SmoothAudioQuality, SmoothManifestError> {
    validate_attributes(
        element,
        &[
            "Index",
            "Bitrate",
            "FourCC",
            "SamplingRate",
            "Channels",
            "BitsPerSample",
            "PacketSize",
            "AudioTag",
            "CodecPrivateData",
        ],
    )?;
    let index = parse_quality_index(element)?;
    let sampling_rate = parse_positive_u32(
        required_attribute(element, "SamplingRate", SmoothSchemaField::QualityLevel)?,
        SmoothSchemaField::QualityLevel,
    )?;
    let channels = parse_positive_u16(
        required_attribute(element, "Channels", SmoothSchemaField::QualityLevel)?,
        SmoothSchemaField::QualityLevel,
    )?;
    let audio_tag_u64 = parse_u64(
        required_attribute(element, "AudioTag", SmoothSchemaField::QualityLevel)?,
        SmoothSchemaField::QualityLevel,
    )?;
    let audio_tag =
        u16::try_from(audio_tag_u64).map_err(|_| SmoothManifestError::MalformedSchema {
            field: SmoothSchemaField::QualityLevel,
        })?;
    let four_cc = optional_attribute(element, "FourCC")?
        .or(inherited_four_cc)
        .ok_or(SmoothManifestError::MalformedSchema {
            field: SmoothSchemaField::QualityLevel,
        })?;
    let (codec, codec_configuration) = parse_aac_lc_configuration(
        four_cc,
        audio_tag,
        sampling_rate,
        channels,
        optional_attribute(element, "CodecPrivateData")?,
        limits,
        is_cancelled,
    )?;
    SmoothAudioQuality::new(
        index,
        parse_positive_u64(
            required_attribute(element, "Bitrate", SmoothSchemaField::QualityLevel)?,
            SmoothSchemaField::QualityLevel,
        )?,
        sampling_rate,
        channels,
        parse_positive_u16(
            required_attribute(element, "BitsPerSample", SmoothSchemaField::QualityLevel)?,
            SmoothSchemaField::QualityLevel,
        )?,
        parse_positive_u16(
            required_attribute(element, "PacketSize", SmoothSchemaField::QualityLevel)?,
            SmoothSchemaField::QualityLevel,
        )?,
        audio_tag,
        codec,
        codec_configuration,
        custom_attributes,
    )
}

/// Index обязателен, но не становится скрытой selection identity.
fn parse_quality_index(element: &XmlElement) -> Result<SmoothQualityIndex, SmoothManifestError> {
    let value = parse_u64(
        required_attribute(element, "Index", SmoothSchemaField::QualityLevel)?,
        SmoothSchemaField::QualityLevel,
    )?;
    Ok(SmoothQualityIndex::new(value))
}

/// Читает bounded standard CustomAttributes list без duplicate names.
fn parse_custom_attributes(
    cursor: &mut EventCursor<'_, '_>,
    element: XmlElement,
    limits: &SmoothManifestLimits,
) -> Result<SmoothCustomAttributeSet, SmoothManifestError> {
    validate_attributes(&element, &[])?;
    let mut attributes = Vec::new();
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::EmptyElement(child))
                if is_unqualified_name(child.name(), "Attribute") =>
            {
                enforce_limit(
                    attributes.len().saturating_add(1),
                    limits.maximum_custom_attributes_per_quality(),
                    SmoothManifestLimitKind::CustomAttributesPerQuality,
                )?;
                attributes.push(parse_custom_attribute(&child, limits)?);
            }
            Some(XmlEvent::StartElement(child))
                if is_unqualified_name(child.name(), "Attribute") =>
            {
                return Err(SmoothManifestError::MalformedSchema {
                    field: SmoothSchemaField::CustomAttributes,
                });
            }
            Some(XmlEvent::EndElement(name)) if is_unqualified_name(&name, "CustomAttributes") => {
                break;
            }
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(XmlEvent::StartElement(child) | XmlEvent::EmptyElement(child)) => {
                return Err(unsupported_child(child.name()));
            }
            Some(XmlEvent::EndElement(name)) => return Err(unsupported_child(&name)),
            Some(XmlEvent::Text(_)) | None => {
                return Err(SmoothManifestError::MalformedSchema {
                    field: SmoothSchemaField::CustomAttributes,
                });
            }
        }
    }
    SmoothCustomAttributeSet::new(attributes, limits)
}

/// Материализует одну safe grammar name/value пару.
fn parse_custom_attribute(
    element: &XmlElement,
    limits: &SmoothManifestLimits,
) -> Result<SmoothCustomAttribute, SmoothManifestError> {
    validate_attributes(element, &["Name", "Value"])?;
    Ok(SmoothCustomAttribute::new(
        SmoothCustomAttributeName::new(
            required_attribute(element, "Name", SmoothSchemaField::CustomAttributes)?,
            limits,
        )?,
        SmoothCustomAttributeValue::new(
            required_attribute(element, "Value", SmoothSchemaField::CustomAttributes)?,
            limits,
        )?,
    ))
}

/// Формирует profile rejection без string matching.
const fn profile_error(reason: SmoothProfileIncompatibility) -> SmoothManifestError {
    SmoothManifestError::ProfileIncompatible { reason }
}

/// Применяет quality-local counters до allocation/push.
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

/// Total budget коммитится только после полной проверки quality row.
fn commit_total_custom_attributes(
    accepted_custom_attribute_count: &mut usize,
    quality_attribute_count: usize,
    limits: &SmoothManifestLimits,
) -> Result<(), SmoothManifestError> {
    let candidate = accepted_custom_attribute_count
        .checked_add(quality_attribute_count)
        .ok_or(SmoothManifestError::LimitExceeded {
            limit: SmoothManifestLimitKind::TotalCustomAttributes,
            maximum: limits.maximum_total_custom_attributes(),
        })?;
    enforce_limit(
        candidate,
        limits.maximum_total_custom_attributes(),
        SmoothManifestLimitKind::TotalCustomAttributes,
    )?;
    *accepted_custom_attribute_count = candidate;
    Ok(())
}
