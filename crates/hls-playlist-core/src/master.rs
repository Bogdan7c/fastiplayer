use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;

use url::Url;

use crate::{
    ClosedCaptionsReference, ExactReference, HlsFrameRate, HlsLineNumber, HlsParseError,
    HlsParseErrorKind, HlsVideoRange, MediaRendition, MediaRenditionType, VariantStream,
    attribute::Attributes,
    parser::{parse_u64, validate_reference},
};

/// Временные поля variant до обязательной следующей URI line.
pub(crate) struct VariantFields {
    bandwidth: u64,
    average_bandwidth: Option<u64>,
    codecs: Option<Box<str>>,
    resolution: Option<(u32, u32)>,
    frame_rate: Option<HlsFrameRate>,
    video_range: Option<HlsVideoRange>,
    audio_group: Option<Box<str>>,
    video_group: Option<Box<str>>,
    subtitle_group: Option<Box<str>>,
    closed_captions: Option<ClosedCaptionsReference>,
    requires_output_protection: bool,
}

impl VariantFields {
    /// Закрывает pending `EXT-X-STREAM-INF` exact URI-reference.
    pub(crate) fn finish(self, uri: ExactReference) -> VariantStream {
        VariantStream {
            uri,
            bandwidth: self.bandwidth,
            average_bandwidth: self.average_bandwidth,
            codecs: self.codecs,
            resolution: self.resolution,
            frame_rate: self.frame_rate,
            video_range: self.video_range,
            audio_group: self.audio_group,
            video_group: self.video_group,
            subtitle_group: self.subtitle_group,
            closed_captions: self.closed_captions,
            requires_output_protection: self.requires_output_protection,
        }
    }
}

/// Разбирает все известные `EXT-X-STREAM-INF` attributes.
pub(crate) fn parse_variant(
    attributes: &Attributes<'_>,
    line: HlsLineNumber,
) -> Result<VariantFields, HlsParseError> {
    let closed_captions = match attributes.raw("CLOSED-CAPTIONS") {
        None => None,
        Some("NONE") => Some(ClosedCaptionsReference::None),
        Some(_) => Some(ClosedCaptionsReference::Group(
            required_quoted(attributes, "CLOSED-CAPTIONS", line)?.into(),
        )),
    };
    let requires_output_protection = match attributes.raw("HDCP-LEVEL") {
        None | Some("NONE") => false,
        Some("TYPE-0") => true,
        Some(_) => return Err(syntax(line)),
    };
    Ok(VariantFields {
        bandwidth: parse_u64(
            attributes.raw("BANDWIDTH").ok_or_else(|| syntax(line))?,
            line,
        )?,
        average_bandwidth: attributes
            .raw("AVERAGE-BANDWIDTH")
            .map(|value| parse_u64(value, line))
            .transpose()?,
        codecs: optional_quoted(attributes, "CODECS", line)?.map(Into::into),
        resolution: attributes
            .raw("RESOLUTION")
            .map(|value| parse_resolution(value, line))
            .transpose()?,
        frame_rate: attributes
            .raw("FRAME-RATE")
            .map(|value| parse_frame_rate(value, line))
            .transpose()?,
        video_range: attributes
            .raw("VIDEO-RANGE")
            .map(|value| parse_video_range(value, line))
            .transpose()?,
        audio_group: optional_quoted(attributes, "AUDIO", line)?.map(Into::into),
        video_group: optional_quoted(attributes, "VIDEO", line)?.map(Into::into),
        subtitle_group: optional_quoted(attributes, "SUBTITLES", line)?.map(Into::into),
        closed_captions,
        requires_output_protection,
    })
}

/// HLS задаёт FRAME-RATE как decimal value, округлённое до трёх знаков после точки.
fn parse_frame_rate(value: &str, line: HlsLineNumber) -> Result<HlsFrameRate, HlsParseError> {
    let (whole, fractional) = match value.split_once('.') {
        Some((whole, fractional)) => {
            if whole.is_empty()
                || fractional.is_empty()
                || fractional.len() > 3
                || fractional.contains('.')
            {
                return Err(syntax(line));
            }
            (whole, Some(fractional))
        }
        None => (value, None),
    };
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fractional.is_some_and(|digits| !digits.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(syntax(line));
    }

    let denominator = fractional.map_or(1_u64, |digits| 10_u64.pow(digits.len() as u32));
    let whole = whole.parse::<u64>().map_err(|_| syntax(line))?;
    let fractional = fractional
        .map(|digits| digits.parse::<u64>().map_err(|_| syntax(line)))
        .transpose()?
        .unwrap_or(0);
    let numerator = whole
        .checked_mul(denominator)
        .and_then(|scaled| scaled.checked_add(fractional))
        .ok_or_else(|| syntax(line))?;
    let divisor = greatest_common_divisor(numerator, denominator);
    let reduced_denominator = NonZeroU64::new(denominator / divisor).ok_or_else(|| syntax(line))?;
    Ok(HlsFrameRate::new(numerator / divisor, reduced_denominator))
}

fn greatest_common_divisor(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}

fn parse_video_range(value: &str, line: HlsLineNumber) -> Result<HlsVideoRange, HlsParseError> {
    match value {
        "SDR" => Ok(HlsVideoRange::Sdr),
        "HLG" => Ok(HlsVideoRange::Hlg),
        "PQ" => Ok(HlsVideoRange::Pq),
        _ => Err(syntax(line)),
    }
}

/// Разбирает и проверяет RFC MUST-инварианты одного `EXT-X-MEDIA`.
pub(crate) fn parse_rendition(
    attributes: &Attributes<'_>,
    line: HlsLineNumber,
    base: Option<&Url>,
) -> Result<MediaRendition, HlsParseError> {
    let rendition_type = match attributes.raw("TYPE").ok_or_else(|| syntax(line))? {
        "AUDIO" => MediaRenditionType::Audio,
        "VIDEO" => MediaRenditionType::Video,
        "SUBTITLES" => MediaRenditionType::Subtitles,
        "CLOSED-CAPTIONS" => MediaRenditionType::ClosedCaptions,
        _ => return Err(syntax(line)),
    };
    let uri = optional_quoted(attributes, "URI", line)?
        .map(|reference| {
            validate_reference(reference, line, base)?;
            Ok(ExactReference::new(reference))
        })
        .transpose()?;
    let is_subtitles = matches!(rendition_type, MediaRenditionType::Subtitles);
    let is_closed_captions = matches!(rendition_type, MediaRenditionType::ClosedCaptions);
    if is_subtitles && uri.is_none() || is_closed_captions && uri.is_some() {
        return Err(required(line));
    }

    let default = parse_yes_no(attributes.raw("DEFAULT"), line)?.unwrap_or(false);
    let autoselect = parse_yes_no(attributes.raw("AUTOSELECT"), line)?;
    if default && autoselect == Some(false) {
        return Err(required(line));
    }
    let forced = parse_yes_no(attributes.raw("FORCED"), line)?;
    if forced.is_some() && !is_subtitles {
        return Err(required(line));
    }
    let instream_id = optional_quoted(attributes, "INSTREAM-ID", line)?;
    if is_closed_captions {
        validate_instream_id(instream_id.ok_or_else(|| required(line))?, line)?;
    } else if instream_id.is_some() {
        return Err(required(line));
    }
    let associated_language = optional_quoted(attributes, "ASSOC-LANGUAGE", line)?;
    let characteristics = optional_quoted(attributes, "CHARACTERISTICS", line)?;
    let channels = optional_quoted(attributes, "CHANNELS", line)?;
    let channel_count = match (rendition_type, channels) {
        (MediaRenditionType::Audio, Some(channel_description)) => {
            Some(parse_channel_description(channel_description, line)?)
        }
        (MediaRenditionType::Audio, None) => None,
        (_, Some(_)) => return Err(required(line)),
        (_, None) => None,
    };

    Ok(MediaRendition {
        rendition_type,
        group_id: required_quoted(attributes, "GROUP-ID", line)?.into(),
        name: required_quoted(attributes, "NAME", line)?.into(),
        uri,
        language: optional_quoted(attributes, "LANGUAGE", line)?.map(Into::into),
        associated_language: associated_language.map(Into::into),
        characteristics: characteristics.map(Into::into),
        channel_count,
        channels: channels.map(Into::into),
        is_default: default,
        autoselect: autoselect.unwrap_or(false),
        forced: forced.unwrap_or(false),
    })
}

/// Проверяет непустой slash-separated `CHANNELS` и возвращает его primary count.
fn parse_channel_description(
    channel_description: &str,
    line: HlsLineNumber,
) -> Result<NonZeroU64, HlsParseError> {
    let mut parameters = channel_description.split('/');
    let primary = parameters.next().ok_or_else(|| syntax(line))?;
    let channel_count = NonZeroU64::new(parse_u64(primary, line)?).ok_or_else(|| syntax(line))?;
    if parameters.any(str::is_empty) {
        return Err(syntax(line));
    }
    Ok(channel_count)
}

/// Проверяет обязательные attributes standalone I-frame variant.
pub(crate) fn validate_i_frame_variant(
    attributes: &Attributes<'_>,
    line: HlsLineNumber,
    base: Option<&Url>,
) -> Result<(), HlsParseError> {
    parse_u64(
        attributes.raw("BANDWIDTH").ok_or_else(|| required(line))?,
        line,
    )?;
    let reference = required_quoted(attributes, "URI", line)?;
    validate_reference(reference, line, base)?;
    optional_quoted(attributes, "VIDEO", line)?;
    Ok(())
}

/// Borrowed identity нужна только для RFC duplicate detection без копирования secrets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SessionKeyIdentity<'a> {
    method: &'a str,
    uri: &'a str,
    iv: Option<&'a str>,
    key_format: Option<&'a str>,
    key_format_versions: Option<&'a str>,
}

/// Создаёт exact RFC duplicate identity `EXT-X-SESSION-KEY`.
pub(crate) fn session_key_identity<'a>(
    attributes: &Attributes<'a>,
    line: HlsLineNumber,
) -> Result<SessionKeyIdentity<'a>, HlsParseError> {
    Ok(SessionKeyIdentity {
        method: attributes.raw("METHOD").ok_or_else(|| required(line))?,
        uri: required_quoted(attributes, "URI", line)?,
        iv: attributes.raw("IV"),
        key_format: optional_quoted(attributes, "KEYFORMAT", line)?,
        key_format_versions: optional_quoted(attributes, "KEYFORMATVERSIONS", line)?,
    })
}

/// Проверяет safe-to-ignore `EXT-X-SESSION-DATA` и возвращает duplicate identity.
pub(crate) fn validate_session_data<'a>(
    attributes: &Attributes<'a>,
    line: HlsLineNumber,
    base: Option<&Url>,
) -> Result<(&'a str, Option<&'a str>), HlsParseError> {
    let data_id = required_quoted(attributes, "DATA-ID", line)?;
    let value = optional_quoted(attributes, "VALUE", line)?;
    let uri = optional_quoted(attributes, "URI", line)?;
    if value.is_some() == uri.is_some() {
        return Err(required(line));
    }
    if let Some(reference) = uri {
        validate_reference(reference, line, base)?;
    }
    let language = optional_quoted(attributes, "LANGUAGE", line)?;
    Ok((data_id, language))
}

/// Проверяет group membership после materialization всего master playlist.
pub(crate) fn validate_master_relations(
    variants: &[VariantStream],
    variant_lines: &[HlsLineNumber],
    renditions: &[MediaRendition],
    rendition_lines: &[HlsLineNumber],
) -> Result<(), HlsParseError> {
    let mut names = HashSet::new();
    let mut default_counts = HashMap::<(&MediaRenditionType, &str), usize>::new();
    for (index, rendition) in renditions.iter().enumerate() {
        let group = (&rendition.rendition_type, rendition.group_id.as_ref());
        if !names.insert((group.0, group.1, rendition.name.as_ref())) {
            return Err(required(rendition_lines[index]));
        }
        if rendition.is_default {
            let count = default_counts.entry(group).or_default();
            *count += 1;
            if *count > 1 {
                return Err(required(rendition_lines[index]));
            }
        }
    }
    validate_same_type_group_members(renditions, rendition_lines)?;
    for (index, variant) in variants.iter().enumerate() {
        validate_group_reference(
            variant.audio_group.as_deref(),
            MediaRenditionType::Audio,
            renditions,
            variant_lines[index],
        )?;
        validate_group_reference(
            variant.video_group.as_deref(),
            MediaRenditionType::Video,
            renditions,
            variant_lines[index],
        )?;
        validate_group_reference(
            variant.subtitle_group.as_deref(),
            MediaRenditionType::Subtitles,
            renditions,
            variant_lines[index],
        )?;
        if let Some(ClosedCaptionsReference::Group(group_id)) = &variant.closed_captions {
            validate_group_reference(
                Some(group_id.as_ref()),
                MediaRenditionType::ClosedCaptions,
                renditions,
                variant_lines[index],
            )?;
        }
    }
    validate_closed_captions_none_consistency(variants, variant_lines)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RenditionSignature<'a> {
    language: Option<&'a str>,
    associated_language: Option<&'a str>,
    characteristics: Option<&'a str>,
    is_default: bool,
    autoselect: bool,
    forced: bool,
}

fn validate_same_type_group_members(
    renditions: &[MediaRendition],
    rendition_lines: &[HlsLineNumber],
) -> Result<(), HlsParseError> {
    let mut groups =
        HashMap::<MediaRenditionType, HashMap<&str, HashMap<&str, RenditionSignature<'_>>>>::new();
    for rendition in renditions.iter().filter(|rendition| {
        matches!(
            rendition.rendition_type,
            MediaRenditionType::Audio | MediaRenditionType::Subtitles
        )
    }) {
        groups
            .entry(rendition.rendition_type)
            .or_default()
            .entry(rendition.group_id.as_ref())
            .or_default()
            .insert(
                rendition.name.as_ref(),
                RenditionSignature {
                    language: rendition.language.as_deref(),
                    associated_language: rendition.associated_language.as_deref(),
                    characteristics: rendition.characteristics.as_deref(),
                    is_default: rendition.is_default,
                    autoselect: rendition.autoselect,
                    forced: rendition.forced,
                },
            );
    }

    let mut canonical_group = HashMap::<MediaRenditionType, &str>::new();
    let mut visited_groups = HashSet::<(MediaRenditionType, &str)>::new();
    for (index, rendition) in renditions.iter().enumerate() {
        if !matches!(
            rendition.rendition_type,
            MediaRenditionType::Audio | MediaRenditionType::Subtitles
        ) || !visited_groups.insert((rendition.rendition_type, rendition.group_id.as_ref()))
        {
            continue;
        }
        let group_id = rendition.group_id.as_ref();
        let first_group = canonical_group
            .entry(rendition.rendition_type)
            .or_insert(group_id);
        if groups[&rendition.rendition_type][*first_group]
            != groups[&rendition.rendition_type][group_id]
        {
            return Err(required(rendition_lines[index]));
        }
    }
    Ok(())
}

fn validate_group_reference(
    group_id: Option<&str>,
    rendition_type: MediaRenditionType,
    renditions: &[MediaRendition],
    line: HlsLineNumber,
) -> Result<(), HlsParseError> {
    let Some(group_id) = group_id else {
        return Ok(());
    };
    if renditions.iter().any(|rendition| {
        rendition.rendition_type == rendition_type && rendition.group_id.as_ref() == group_id
    }) {
        Ok(())
    } else {
        Err(required(line))
    }
}

fn validate_closed_captions_none_consistency(
    variants: &[VariantStream],
    variant_lines: &[HlsLineNumber],
) -> Result<(), HlsParseError> {
    if !variants
        .iter()
        .any(|variant| variant.closed_captions == Some(ClosedCaptionsReference::None))
    {
        return Ok(());
    }
    variants
        .iter()
        .position(|variant| variant.closed_captions != Some(ClosedCaptionsReference::None))
        .map_or(Ok(()), |index| Err(required(variant_lines[index])))
}

fn optional_quoted<'a>(
    attributes: &Attributes<'a>,
    name: &str,
    line: HlsLineNumber,
) -> Result<Option<&'a str>, HlsParseError> {
    match attributes.raw(name) {
        None => Ok(None),
        Some(_) => attributes
            .quoted(name)
            .map(Some)
            .ok_or_else(|| syntax(line)),
    }
}

fn required_quoted<'a>(
    attributes: &Attributes<'a>,
    name: &str,
    line: HlsLineNumber,
) -> Result<&'a str, HlsParseError> {
    optional_quoted(attributes, name, line)?.ok_or_else(|| required(line))
}

fn parse_resolution(value: &str, line: HlsLineNumber) -> Result<(u32, u32), HlsParseError> {
    let (width, height) = value.split_once('x').ok_or_else(|| syntax(line))?;
    let width = width.parse::<u32>().map_err(|_| syntax(line))?;
    let height = height.parse::<u32>().map_err(|_| syntax(line))?;
    if width == 0 || height == 0 {
        return Err(syntax(line));
    }
    Ok((width, height))
}

fn parse_yes_no(value: Option<&str>, line: HlsLineNumber) -> Result<Option<bool>, HlsParseError> {
    value
        .map(|value| match value {
            "YES" => Ok(true),
            "NO" => Ok(false),
            _ => Err(syntax(line)),
        })
        .transpose()
}

fn validate_instream_id(value: &str, line: HlsLineNumber) -> Result<(), HlsParseError> {
    if matches!(value, "CC1" | "CC2" | "CC3" | "CC4") {
        return Ok(());
    }
    let service = value
        .strip_prefix("SERVICE")
        .ok_or_else(|| syntax(line))
        .and_then(|number| parse_u64(number, line))?;
    if (1..=63).contains(&service) {
        Ok(())
    } else {
        Err(syntax(line))
    }
}

fn syntax(line: HlsLineNumber) -> HlsParseError {
    HlsParseError::new(HlsParseErrorKind::InvalidTagSyntax { line })
}

fn required(line: HlsLineNumber) -> HlsParseError {
    HlsParseError::new(HlsParseErrorKind::InvalidRequiredStructure { line })
}
