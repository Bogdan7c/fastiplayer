//! Пропуск описательных (informational) поддеревьев корня MPD.
//!
//! ISO 23009-1 допускает под `MPD` элементы, которые не влияют на воспроизведение:
//! `ProgramInformation` (§5.7, Title/Source/Copyright) и `ServiceDescription`
//! (§5.15, подсказки Latency/PlaybackRate/Scope). Их пишет, например, `ffmpeg -f dash`.
//! Static и dynamic парсеры используют один и тот же пропуск, чтобы список таких
//! элементов и лимиты не расходились.

use bounded_xml_reader::{XmlElement, XmlEvent};

use crate::error::{DashMpdError, DashMpdErrorKind};

use super::{EventCursor, is_name};

/// Локальные имена детей `MPD`, чьё содержимое не влияет на воспроизведение.
const INFORMATIONAL_ROOT_ELEMENTS: &[&str] = &["ProgramInformation", "ServiceDescription"];

/// Является ли элемент одним из описательных детей корня (точное имя + DASH namespace).
pub(crate) fn is_informational_root_element(element: &XmlElement) -> bool {
    INFORMATIONAL_ROOT_ELEMENTS
        .iter()
        .any(|local_name| is_name(element.name(), local_name))
}

/// Пропускает тело уже открытого описательного элемента до его закрывающего тега.
///
/// Чтение идёт через тот же `EventCursor`, поэтому глубина, число элементов и размер
/// документа по-прежнему ограничены XML budgets. По схеме внутри этих элементов
/// `ContentProtection` невозможен; если он всё же встретился, это нарушение схемы,
/// скрывающее DRM, и мы закрываемся с `ContentProtection`, а не молча пропускаем.
pub(crate) fn skip_informational_element_body(
    cursor: &mut EventCursor<'_>,
) -> Result<(), DashMpdError> {
    // Один открытый элемент: тот, чей start-tag уже прочитан вызывающим кодом.
    let mut open_element_count = 1_usize;
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(child)) => {
                reject_hidden_content_protection(&child)?;
                open_element_count = open_element_count
                    .checked_add(1)
                    .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::LimitExceeded))?;
            }
            Some(XmlEvent::EmptyElement(child)) => reject_hidden_content_protection(&child)?,
            Some(XmlEvent::EndElement(_)) => {
                open_element_count = open_element_count
                    .checked_sub(1)
                    .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::MalformedSchema))?;
                if open_element_count == 0 {
                    return Ok(());
                }
            }
            Some(XmlEvent::Text(_)) => {}
            // Документ оборвался до закрытия элемента.
            None => return Err(DashMpdError::new(DashMpdErrorKind::MalformedSchema)),
        }
    }
}

/// DRM-дескриптор внутри пропускаемого поддерева не допускается ни в каком виде.
fn reject_hidden_content_protection(element: &XmlElement) -> Result<(), DashMpdError> {
    if is_name(element.name(), "ContentProtection") {
        return Err(DashMpdError::new(DashMpdErrorKind::ContentProtection));
    }
    Ok(())
}
