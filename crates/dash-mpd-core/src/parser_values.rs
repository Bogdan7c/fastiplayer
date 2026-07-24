/// Читает positive unsigned integer либо default.
fn positive_u64_attribute(
    element: &XmlElement,
    name: &str,
    default: u64,
) -> Result<u64, DashMpdError> {
    let value = optional_u64_attribute(element, name)?.unwrap_or(default);
    if value == 0 {
        return Err(DashMpdError::new(DashMpdErrorKind::InvalidAttribute));
    }
    Ok(value)
}

/// Читает restricted ISO 8601 duration.
fn optional_duration_attribute(
    element: &XmlElement,
    name: &str,
) -> Result<Option<u64>, DashMpdError> {
    optional_attribute(element, name)?
        .map(parse_iso8601_duration_milliseconds)
        .transpose()
}

/// Поддерживает date-free `PT#H#M#S` с millisecond precision.
fn parse_iso8601_duration_milliseconds(value: &str) -> Result<u64, DashMpdError> {
    let body = value
        .strip_prefix("PT")
        .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?;
    if body.is_empty() {
        return Err(DashMpdError::new(DashMpdErrorKind::InvalidAttribute));
    }
    let mut total = 0_u64;
    let mut number_start = 0_usize;
    let mut last_unit_rank = 0_u8;
    for (index, character) in body.char_indices() {
        if !matches!(character, 'H' | 'M' | 'S') {
            continue;
        }
        let unit_rank = match character {
            'H' => 1,
            'M' => 2,
            'S' => 3,
            _ => unreachable!(),
        };
        if unit_rank <= last_unit_rank {
            return Err(DashMpdError::new(DashMpdErrorKind::InvalidAttribute));
        }
        let number = &body[number_start..index];
        if number.is_empty() {
            return Err(DashMpdError::new(DashMpdErrorKind::InvalidAttribute));
        }
        let milliseconds = match character {
            'H' => parse_whole_unit(number, 3_600_000)?,
            'M' => parse_whole_unit(number, 60_000)?,
            'S' => parse_seconds(number)?,
            _ => unreachable!(),
        };
        total = total
            .checked_add(milliseconds)
            .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?;
        number_start = index + character.len_utf8();
        last_unit_rank = unit_rank;
    }
    if number_start != body.len() {
        return Err(DashMpdError::new(DashMpdErrorKind::InvalidAttribute));
    }
    Ok(total)
}

/// Парсит целые часы/минуты.
fn parse_whole_unit(value: &str, multiplier: u64) -> Result<u64, DashMpdError> {
    value
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(multiplier))
        .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))
}

/// Парсит секунды с максимум тремя значимыми decimal digits.
fn parse_seconds(value: &str) -> Result<u64, DashMpdError> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if fraction.len() > 3 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(DashMpdError::new(DashMpdErrorKind::InvalidAttribute));
    }
    let whole_milliseconds = parse_whole_unit(whole, 1_000)?;
    let mut fraction_text = fraction.to_owned();
    while fraction_text.len() < 3 {
        fraction_text.push('0');
    }
    let fraction_milliseconds = if fraction_text.is_empty() {
        0
    } else {
        fraction_text
            .parse::<u64>()
            .map_err(|_| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))?
    };
    whole_milliseconds
        .checked_add(fraction_milliseconds)
        .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))
}

/// Извлекает media hints одного уровня.
fn media_hints(
    element: &XmlElement,
    limits: DashMpdLimits,
) -> Result<MediaHints, DashMpdError> {
    Ok(MediaHints {
        mime_type: bounded_optional_attribute(element, "mimeType", limits)?,
        content_type: bounded_optional_attribute(element, "contentType", limits)?,
        codecs: bounded_optional_attribute(element, "codecs", limits)?,
    })
}

/// Representation override-ит каждый AdaptationSet hint независимо.
fn merge_hints(parent: &MediaHints, child: MediaHints) -> MediaHints {
    MediaHints {
        mime_type: child.mime_type.or_else(|| parent.mime_type.clone()),
        content_type: child.content_type.or_else(|| parent.content_type.clone()),
        codecs: child.codecs.or_else(|| parent.codecs.clone()),
    }
}

/// Доказывает fMP4/WebM и audio/video/muxed shape.
fn classify_media(hints: &MediaHints) -> Result<(DashContainer, DashMediaKind, String), DashMpdError> {
    let mime_type = hints
        .mime_type
        .as_deref()
        .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::UnsupportedMediaEvidence))?;
    let container = match mime_type {
        "video/mp4" | "audio/mp4" | "application/mp4" => DashContainer::IsoBmff,
        "video/webm" | "audio/webm" => DashContainer::WebM,
        _ => {
            return Err(DashMpdError::new(
                DashMpdErrorKind::UnsupportedMediaEvidence,
            ));
        }
    };
    let codecs = hints
        .codecs
        .clone()
        .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::UnsupportedMediaEvidence))?;
    let mut has_video = false;
    let mut has_audio = false;
    for codec in codecs.split(',').map(str::trim) {
        let (codec_is_video, codec_is_audio) = match container {
            DashContainer::IsoBmff => (
                codec.starts_with("avc1")
                    || codec.starts_with("hvc1")
                    || codec.starts_with("hev1")
                    || codec.starts_with("av01"),
                codec.starts_with("mp4a"),
            ),
            DashContainer::WebM => (
                codec.eq_ignore_ascii_case("vp8")
                    || codec.eq_ignore_ascii_case("vp9")
                    || codec.eq_ignore_ascii_case("av1")
                    || codec.starts_with("vp08")
                    || codec.starts_with("vp09")
                    || codec.starts_with("av01"),
                codec.eq_ignore_ascii_case("opus") || codec.eq_ignore_ascii_case("vorbis"),
            ),
        };
        if codec_is_video {
            has_video = true;
        } else if codec_is_audio {
            has_audio = true;
        } else {
            return Err(DashMpdError::new(
                DashMpdErrorKind::UnsupportedMediaEvidence,
            ));
        }
    }
    let media_kind = match (has_video, has_audio) {
        (true, true) => DashMediaKind::Muxed,
        (true, false) => DashMediaKind::Video,
        (false, true) => DashMediaKind::Audio,
        (false, false) => {
            return Err(DashMpdError::new(
                DashMpdErrorKind::UnsupportedMediaEvidence,
            ));
        }
    };
    if let Some(content_type) = hints.content_type.as_deref() {
        let matches = matches!(
            (content_type, media_kind),
            ("video", DashMediaKind::Video | DashMediaKind::Muxed)
                | ("audio", DashMediaKind::Audio)
                | ("application", DashMediaKind::Muxed)
        );
        if !matches {
            return Err(DashMpdError::new(
                DashMpdErrorKind::UnsupportedMediaEvidence,
            ));
        }
    }
    if (mime_type.starts_with("audio/") && media_kind != DashMediaKind::Audio)
        || (mime_type.starts_with("video/") && media_kind == DashMediaKind::Audio)
    {
        return Err(DashMpdError::new(
            DashMpdErrorKind::UnsupportedMediaEvidence,
        ));
    }
    Ok((container, media_kind, codecs))
}
