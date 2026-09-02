//! Узкий admission boundary для subtitle AdaptationSet, не имеющих playback consumer-а.

use bounded_xml_reader::{XmlElement, XmlEvent};

use super::{
    DashMpdError, DashMpdErrorKind, DashMpdLimits, EventCursor, bounded_optional_attribute,
    is_name, validate_adaptation_constraints, validate_attributes,
};

/// Узнаёт explicit text AdaptationSet, доказательство которого завершается по representation-ам.
///
/// Один `contentType="text"` определяет назначение строки, но ещё не разрешает пропустить subtree:
/// MIME/codec могут находиться только у дочернего `Representation` и проверяются consumer-ом ниже.
pub(super) fn is_declared_text_adaptation_set(
    element: &XmlElement,
    limits: DashMpdLimits,
) -> Result<bool, DashMpdError> {
    Ok(bounded_optional_attribute(element, "contentType", limits)?.as_deref() == Some("text"))
}

/// Доказывает и пропускает subtitle subtree под общими hardened XML budgets.
///
/// DRM marker остаётся terminal даже в неиспользуемой дорожке: это сохраняет прежний
/// manifest-wide fail-closed policy и не расширяет DRM/auth surface плеера.
pub(super) fn consume_non_playback_text_adaptation_set(
    cursor: &mut EventCursor<'_>,
    adaptation: &XmlElement,
    limits: DashMpdLimits,
) -> Result<(), DashMpdError> {
    validate_attributes(
        adaptation,
        &[
            "id",
            "mimeType",
            "contentType",
            "codecs",
            "lang",
            "segmentAlignment",
            "subsegmentAlignment",
            "startWithSAP",
        ],
    )?;
    validate_adaptation_constraints(adaptation, limits)?;
    let inherited_mime_type = bounded_optional_attribute(adaptation, "mimeType", limits)?;
    let inherited_codecs = bounded_optional_attribute(adaptation, "codecs", limits)?;
    let mut open_element_count = 1_usize;
    let mut representation_count = 0_usize;
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(child) | XmlEvent::EmptyElement(child))
                if is_name(child.name(), "ContentProtection") =>
            {
                return Err(DashMpdError::new(DashMpdErrorKind::ContentProtection));
            }
            Some(XmlEvent::StartElement(child)) => {
                if open_element_count == 1 && is_name(child.name(), "Representation") {
                    representation_count = representation_count
                        .checked_add(1)
                        .ok_or(DashMpdError::new(DashMpdErrorKind::LimitExceeded))?;
                    validate_text_representation(
                        &child,
                        inherited_mime_type.as_deref(),
                        inherited_codecs.as_deref(),
                        representation_count,
                        limits,
                    )?;
                }
                open_element_count = open_element_count
                    .checked_add(1)
                    .ok_or(DashMpdError::new(DashMpdErrorKind::LimitExceeded))?;
            }
            Some(XmlEvent::EmptyElement(child)) => {
                if open_element_count == 1 && is_name(child.name(), "Representation") {
                    representation_count = representation_count
                        .checked_add(1)
                        .ok_or(DashMpdError::new(DashMpdErrorKind::LimitExceeded))?;
                    validate_text_representation(
                        &child,
                        inherited_mime_type.as_deref(),
                        inherited_codecs.as_deref(),
                        representation_count,
                        limits,
                    )?;
                }
            }
            Some(XmlEvent::EndElement(_)) => {
                open_element_count = open_element_count
                    .checked_sub(1)
                    .ok_or(DashMpdError::new(DashMpdErrorKind::MalformedSchema))?;
                if open_element_count == 0 {
                    return if representation_count == 0 {
                        Err(DashMpdError::new(DashMpdErrorKind::MalformedSchema))
                    } else {
                        Ok(())
                    };
                }
            }
            Some(XmlEvent::Text(_)) => {}
            None => return Err(DashMpdError::new(DashMpdErrorKind::MalformedSchema)),
        }
    }
}

/// Проверяет direct representation, прежде чем не-playback subtree будет отброшен.
fn validate_text_representation(
    representation: &XmlElement,
    inherited_mime_type: Option<&str>,
    inherited_codecs: Option<&str>,
    representation_count: usize,
    limits: DashMpdLimits,
) -> Result<(), DashMpdError> {
    if representation_count > limits.maximum_representations_per_adaptation_set {
        return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded));
    }
    validate_attributes(
        representation,
        &[
            "id",
            "bandwidth",
            "mimeType",
            "contentType",
            "codecs",
            "lang",
        ],
    )?;
    if let Some(content_type) = bounded_optional_attribute(representation, "contentType", limits)?
        && content_type != "text"
    {
        return Err(DashMpdError::new(
            DashMpdErrorKind::UnsupportedMediaEvidence,
        ));
    }
    let representation_mime_type = bounded_optional_attribute(representation, "mimeType", limits)?;
    let representation_codecs = bounded_optional_attribute(representation, "codecs", limits)?;
    let mime_type = representation_mime_type.as_deref().or(inherited_mime_type);
    let codecs = representation_codecs.as_deref().or(inherited_codecs);
    if !is_known_text_evidence(mime_type, codecs) {
        return Err(DashMpdError::new(
            DashMpdErrorKind::UnsupportedMediaEvidence,
        ));
    }
    Ok(())
}

/// Разрешает только text/VTT либо ISO-BMFF subtitle codecs из утверждённого профиля.
fn is_known_text_evidence(mime_type: Option<&str>, codecs: Option<&str>) -> bool {
    match mime_type {
        Some("text/vtt") => codecs.is_none(),
        Some("application/mp4") => codecs.is_some_and(|codecs| {
            let mut observed_codec = false;
            for codec in codecs.split(',').map(str::trim) {
                if codec.is_empty() || !(codec == "wvtt" || codec.starts_with("stpp")) {
                    return false;
                }
                observed_codec = true;
            }
            observed_codec
        }),
        Some(_) | None => false,
    }
}
