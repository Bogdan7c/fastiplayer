//! RepresentationBase metadata parsing and inheritance.

use super::*;

/// Наследуемая RepresentationBase metadata с independent field override.
#[derive(Clone, Default)]
pub(super) struct RepresentationMetadata {
    pub(super) width: Option<u32>,
    pub(super) height: Option<u32>,
    pub(super) frame_rate: Option<DashFrameRate>,
    pub(super) audio_sampling_rate: Option<u32>,
    pub(super) audio_channel_configuration: Option<DashAudioChannelConfiguration>,
    pub(super) language: Option<String>,
    pub(super) color: DashColorMetadata,
}

/// Читает точный MPD `FrameRateType`: positive integer либо positive rational.
fn optional_frame_rate_attribute(
    element: &XmlElement,
    name: &str,
) -> Result<Option<DashFrameRate>, DashMpdError> {
    optional_attribute(element, name)?
        .map(|value| {
            let (numerator, denominator) = value.split_once('/').unwrap_or((value, "1"));
            if numerator.is_empty()
                || denominator.is_empty()
                || !numerator.bytes().all(|byte| byte.is_ascii_digit())
                || !denominator.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(DashMpdError::new(DashMpdErrorKind::InvalidAttribute));
            }
            let numerator = numerator
                .parse::<u32>()
                .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?;
            let denominator = denominator
                .parse::<u32>()
                .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?;
            if numerator == 0 || denominator == 0 {
                return Err(DashMpdError::new(DashMpdErrorKind::InvalidAttribute));
            }
            Ok(DashFrameRate {
                numerator,
                denominator,
            })
        })
        .transpose()
}

/// Извлекает scalar RepresentationBase metadata одного уровня.
pub(super) fn representation_metadata(
    element: &XmlElement,
    limits: DashMpdLimits,
) -> Result<RepresentationMetadata, DashMpdError> {
    Ok(RepresentationMetadata {
        width: optional_positive_u32_attribute(element, "width")?,
        height: optional_positive_u32_attribute(element, "height")?,
        frame_rate: optional_frame_rate_attribute(element, "frameRate")?,
        audio_sampling_rate: optional_positive_u32_attribute(element, "audioSamplingRate")?,
        language: bounded_optional_attribute(element, "lang", limits)?,
        ..RepresentationMetadata::default()
    })
}

/// Representation scalar/descriptor fields override AdaptationSet independently.
pub(super) fn merge_representation_metadata(
    parent: &RepresentationMetadata,
    child: RepresentationMetadata,
) -> RepresentationMetadata {
    RepresentationMetadata {
        width: child.width.or(parent.width),
        height: child.height.or(parent.height),
        frame_rate: child.frame_rate.or(parent.frame_rate),
        audio_sampling_rate: child.audio_sampling_rate.or(parent.audio_sampling_rate),
        audio_channel_configuration: child
            .audio_channel_configuration
            .or(parent.audio_channel_configuration),
        language: child.language.or_else(|| parent.language.clone()),
        color: DashColorMetadata {
            colour_primaries: child
                .color
                .colour_primaries
                .or(parent.color.colour_primaries),
            transfer_characteristics: child
                .color
                .transfer_characteristics
                .or(parent.color.transfer_characteristics),
            matrix_coefficients: child
                .color
                .matrix_coefficients
                .or(parent.color.matrix_coefficients),
            video_full_range: child
                .color
                .video_full_range
                .or(parent.color.video_full_range),
        },
    }
}

/// Не допускает противоречивые duplicate descriptors на одном inheritance level.
pub(super) fn set_optional_metadata<T: Copy + PartialEq>(
    slot: &mut Option<T>,
    value: T,
) -> Result<(), DashMpdError> {
    if slot.is_some_and(|existing| existing != value) {
        return Err(DashMpdError::new(DashMpdErrorKind::InvalidAttribute));
    }
    *slot = Some(value);
    Ok(())
}

/// Разбирает только две MPEG standardized channel-configuration schemes.
pub(super) fn parse_audio_channel_configuration(
    element: &XmlElement,
    limits: DashMpdLimits,
) -> Result<DashAudioChannelConfiguration, DashMpdError> {
    validate_attributes(element, &["schemeIdUri", "value", "id"])?;
    let scheme = required_bounded_attribute(element, "schemeIdUri", limits)?;
    Ok(match scheme.as_str() {
        "urn:mpeg:mpegB:cicp:ChannelConfiguration" => DashAudioChannelConfiguration::MpegCicp(
            required_bounded_attribute(element, "value", limits)?
                .parse::<u16>()
                .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?,
        ),
        "urn:mpeg:dash:23003:3:audio_channel_configuration:2011" => {
            DashAudioChannelConfiguration::Mpeg23003_3(
                required_bounded_attribute(element, "value", limits)?
                    .parse::<u16>()
                    .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?,
            )
        }
        _ => DashAudioChannelConfiguration::Unsupported,
    })
}

/// Применяет один standardized CICP Essential/Supplemental descriptor.
pub(super) fn apply_color_descriptor(
    element: &XmlElement,
    limits: DashMpdLimits,
    essential: bool,
    color: &mut DashColorMetadata,
) -> Result<(), DashMpdError> {
    validate_attributes(element, &["schemeIdUri", "value", "id"])?;
    let scheme = required_bounded_attribute(element, "schemeIdUri", limits)?;
    match scheme.as_str() {
        "urn:mpeg:mpegB:cicp:ColourPrimaries" => set_optional_metadata(
            &mut color.colour_primaries,
            required_bounded_attribute(element, "value", limits)?
                .parse::<u8>()
                .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?,
        ),
        "urn:mpeg:mpegB:cicp:TransferCharacteristics" => set_optional_metadata(
            &mut color.transfer_characteristics,
            required_bounded_attribute(element, "value", limits)?
                .parse::<u8>()
                .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?,
        ),
        "urn:mpeg:mpegB:cicp:MatrixCoefficients" => set_optional_metadata(
            &mut color.matrix_coefficients,
            required_bounded_attribute(element, "value", limits)?
                .parse::<u8>()
                .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?,
        ),
        "urn:mpeg:mpegB:cicp:VideoFullRangeFlag" => set_optional_metadata(
            &mut color.video_full_range,
            match required_bounded_attribute(element, "value", limits)?.as_str() {
                "0" => false,
                "1" => true,
                _ => return Err(DashMpdError::new(DashMpdErrorKind::InvalidAttribute)),
            },
        ),
        _ if essential => Err(DashMpdError::new(DashMpdErrorKind::UnsupportedConstruct)),
        _ => Ok(()),
    }
}

/// DescriptorType extensions не входят в профиль; text-only пустое body допустимо.
pub(super) fn consume_descriptor_body(
    cursor: &mut EventCursor<'_>,
    element_name: &str,
) -> Result<(), DashMpdError> {
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::EndElement(name)) if is_name(&name, element_name) => return Ok(()),
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(_) | None => {
                return Err(DashMpdError::new(DashMpdErrorKind::UnsupportedConstruct));
            }
        }
    }
}
