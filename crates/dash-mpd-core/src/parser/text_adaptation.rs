//! Узкий admission boundary для subtitle AdaptationSet, не имеющих playback consumer-а.

use bounded_xml_reader::{XmlElement, XmlEvent};

use super::{
    DashMpdError, DashMpdErrorKind, DashMpdLimits, EventCursor, bounded_optional_attribute, is_name,
};

/// Доказывает explicit non-playback subtitle AdaptationSet.
///
/// `contentType` — authoritative DASH evidence этого уровня. Если provider дополнительно
/// объявил inherited MIME/codec, они обязаны совпасть с узким subtitle allowlist; противоречивый
/// document продолжает завершаться fail-closed через обычный media classifier.
pub(super) fn is_non_playback_text_adaptation_set(
    element: &XmlElement,
    limits: DashMpdLimits,
) -> Result<bool, DashMpdError> {
    if bounded_optional_attribute(element, "contentType", limits)?.as_deref() != Some("text") {
        return Ok(false);
    }
    if let Some(mime_type) = bounded_optional_attribute(element, "mimeType", limits)?
        && !matches!(mime_type.as_str(), "application/mp4" | "text/vtt")
    {
        return Ok(false);
    }
    let Some(codecs) = bounded_optional_attribute(element, "codecs", limits)? else {
        return Ok(true);
    };
    let mut observed_codec = false;
    for codec in codecs.split(',').map(str::trim) {
        if codec.is_empty() || !(codec == "wvtt" || codec.starts_with("stpp")) {
            return Ok(false);
        }
        observed_codec = true;
    }
    Ok(observed_codec)
}

/// Пропускает доказанный subtitle subtree под общими hardened XML budgets.
///
/// DRM marker остаётся terminal даже в неиспользуемой дорожке: это сохраняет прежний
/// manifest-wide fail-closed policy и не расширяет DRM/auth surface плеера.
pub(super) fn consume_non_playback_text_adaptation_set(
    cursor: &mut EventCursor<'_>,
) -> Result<(), DashMpdError> {
    let mut open_element_count = 1_usize;
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(child) | XmlEvent::EmptyElement(child))
                if is_name(child.name(), "ContentProtection") =>
            {
                return Err(DashMpdError::new(DashMpdErrorKind::ContentProtection));
            }
            Some(XmlEvent::StartElement(_)) => {
                open_element_count = open_element_count
                    .checked_add(1)
                    .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::LimitExceeded))?;
            }
            Some(XmlEvent::EndElement(_)) => {
                open_element_count = open_element_count
                    .checked_sub(1)
                    .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::MalformedSchema))?;
                if open_element_count == 0 {
                    return Ok(());
                }
            }
            Some(XmlEvent::EmptyElement(_) | XmlEvent::Text(_)) => {}
            None => return Err(DashMpdError::new(DashMpdErrorKind::MalformedSchema)),
        }
    }
}
