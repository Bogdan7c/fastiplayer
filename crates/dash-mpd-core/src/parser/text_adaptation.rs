//! Разбор текстовых (субтитровых) AdaptationSet в отдельную модель, не затрагивающую A/V.
//!
//! Правила допуска:
//! - текстовым считается набор с `contentType="text"`, либо с `mimeType` `application/ttml+xml`
//!   / `text/vtt`, либо с `application/mp4` и кодеком `stpp*`/`wvtt` (на уровне набора);
//! - поддерживаются четыре формата ([`DashTextKind`]); Representation, чей MIME/кодек не
//!   опознан, отбрасывается, набор без поддерживаемых Representation пропускается;
//! - `ContentProtection` (DRM) внутри текстового набора пропускает ТОЛЬКО этот набор;
//! - ни одно содержимое текстового набора не роняет весь MPD и не прячет A/V: набор либо
//!   разбирается, либо пропускается и считается в счётчик Period (прежний инвариант
//!   «текст не блокирует A/V» сохранён). Ошибки самого XML (битая разметка, превышение
//!   бюджетов) по-прежнему фатальны для всего документа.
//!
//! Чтобы ошибка внутри набора не оставила cursor посреди элемента, поддерево набора сначала
//! целиком вычитывается в буфер (границы задают бюджеты XML), а затем разбирается из буфера.
//!
//! Доказательства текста могут стоять только на `Representation`: тогда набор (без явного
//! `contentType` video/audio и с `mimeType` либо пустым, либо `application/mp4`) читается в
//! буфер, и он считается текстовым ТОЛЬКО если ВСЕ его Representation имеют текстовые
//! доказательства (`classify_text_kind`). Смешанный набор остаётся A/V с прежней семантикой
//! ошибок: буфер просто отдаётся A/V-разбору.

use bounded_xml_reader::{XmlElement, XmlEvent};

use super::{
    DashMpdError, DashMpdErrorKind, DashMpdLimits, EventCursor, bounded_optional_attribute,
    consume_descriptor_body, is_name, optional_u64_attribute, parse_base_url,
    parse_empty_segment_base, parse_empty_segment_template_leaf, parse_segment_base,
    parse_segment_list, parse_segment_template, read_text_leaf, required_bounded_attribute,
    set_single_addressing, set_single_base_url,
};
use crate::model::{DashAddressing, DashBaseUrl};
use crate::text_model::{
    DashTextAdaptationSet, DashTextKind, DashTextRepresentation, DashTextRole,
};

/// Схема ролей DASH (`Role` и `Accessibility` с этой схемой дают [`DashTextRole`]).
const ROLE_SCHEME: &str = "urn:mpeg:dash:role:2011";

/// Итог разбора одного текстового набора.
pub(super) enum TextAdaptationOutcome {
    /// Набор разобран и содержит хотя бы одну поддерживаемую Representation.
    Parsed(DashTextAdaptationSet),
    /// Набор пропущен (DRM, неизвестный формат, битое описание): A/V это не затрагивает.
    Skipped,
}

/// Признак текстового набора по его собственным атрибутам (см. правила в шапке модуля).
pub(super) fn is_declared_text_adaptation_set(
    element: &XmlElement,
    limits: DashMpdLimits,
) -> Result<bool, DashMpdError> {
    if bounded_optional_attribute(element, "contentType", limits)?.as_deref() == Some("text") {
        return Ok(true);
    }
    let mime_type = bounded_optional_attribute(element, "mimeType", limits)?;
    let codecs = bounded_optional_attribute(element, "codecs", limits)?;
    Ok(match mime_type.as_deref() {
        Some("application/ttml+xml" | "text/vtt") => true,
        Some("application/mp4") | None => codecs.as_deref().is_some_and(declares_text_codec),
        Some(_) => false,
    })
}

/// Набор без текстовых признаков на своём уровне, но способный нести их в Representation.
pub(super) fn may_hold_text_representations(
    element: &XmlElement,
    limits: DashMpdLimits,
) -> Result<bool, DashMpdError> {
    let content_type = bounded_optional_attribute(element, "contentType", limits)?;
    if matches!(content_type.as_deref(), Some("video" | "audio")) {
        return Ok(false);
    }
    let mime_type = bounded_optional_attribute(element, "mimeType", limits)?;
    Ok(matches!(
        mime_type.as_deref(),
        None | Some("application/mp4")
    ))
}

/// Вычитывает поддерево набора в буфер повторного чтения и сообщает, текстовые ли ВСЕ его
/// Representation. Буфер остаётся в `cursor.replay` для того разбора, который выберет вызывающий.
pub(super) fn peek_all_representations_are_text(
    cursor: &mut EventCursor<'_>,
    adaptation: &XmlElement,
    limits: DashMpdLimits,
) -> Result<bool, DashMpdError> {
    let buffered = buffer_current_element(cursor)?;
    let inherited_mime = bounded_optional_attribute(adaptation, "mimeType", limits)?;
    let inherited_codecs = bounded_optional_attribute(adaptation, "codecs", limits)?;
    let mut depth = 1_usize;
    let mut representation_count = 0_usize;
    let mut all_text = true;
    for event in &buffered {
        match event {
            XmlEvent::StartElement(child) | XmlEvent::EmptyElement(child) => {
                if depth == 1 && is_name(child.name(), "Representation") {
                    representation_count += 1;
                    let mime = bounded_optional_attribute(child, "mimeType", limits)
                        .ok()
                        .flatten()
                        .or_else(|| inherited_mime.clone());
                    let codecs = bounded_optional_attribute(child, "codecs", limits)
                        .ok()
                        .flatten()
                        .or_else(|| inherited_codecs.clone());
                    all_text &= classify_text_kind(mime.as_deref(), codecs.as_deref()).is_some();
                }
                if matches!(event, XmlEvent::StartElement(_)) {
                    depth += 1;
                }
            }
            XmlEvent::EndElement(_) => depth = depth.saturating_sub(1),
            XmlEvent::Text(_) => {}
        }
    }
    cursor.replay = buffered;
    Ok(representation_count > 0 && all_text)
}

/// Разбирает текстовый набор: вычитывает поддерево, затем разбирает из буфера.
///
/// `Err` возвращается только для ошибок самого XML; всё остальное - `Skipped`.
pub(super) fn parse_text_adaptation_set(
    cursor: &mut EventCursor<'_>,
    adaptation: &XmlElement,
    limits: DashMpdLimits,
) -> Result<TextAdaptationOutcome, DashMpdError> {
    let buffered = buffer_current_element(cursor)?;
    cursor.replay = buffered;
    let parsed = parse_buffered_text_set(cursor, adaptation, limits);
    // Разбор мог оборваться посередине: остаток буфера не должен «протечь» в следующий набор.
    cursor.replay.clear();
    Ok(match parsed {
        Ok(Some(set)) => TextAdaptationOutcome::Parsed(set),
        Ok(None) | Err(_) => TextAdaptationOutcome::Skipped,
    })
}

/// Вычитывает события до закрывающего тега текущего (уже открытого) элемента включительно.
fn buffer_current_element(
    cursor: &mut EventCursor<'_>,
) -> Result<std::collections::VecDeque<XmlEvent>, DashMpdError> {
    let mut events = std::collections::VecDeque::new();
    let mut open_elements = 1_usize;
    while open_elements > 0 {
        let event = cursor
            .next_event()?
            .ok_or(DashMpdError::new(DashMpdErrorKind::MalformedSchema))?;
        match &event {
            XmlEvent::StartElement(_) => {
                open_elements = open_elements
                    .checked_add(1)
                    .ok_or(DashMpdError::new(DashMpdErrorKind::LimitExceeded))?;
            }
            XmlEvent::EndElement(_) => open_elements -= 1,
            XmlEvent::EmptyElement(_) | XmlEvent::Text(_) => {}
        }
        events.push_back(event);
    }
    Ok(events)
}

/// Данные уровня набора, наследуемые Representation.
struct InheritedTextContext {
    language: Option<String>,
    mime_type: Option<String>,
    codecs: Option<String>,
    label: Option<String>,
    addressing: Option<DashAddressing>,
}

/// Разбирает набор из буфера; `Ok(None)` - набор осознанно пропускается.
fn parse_buffered_text_set(
    cursor: &mut EventCursor<'_>,
    adaptation: &XmlElement,
    limits: DashMpdLimits,
) -> Result<Option<DashTextAdaptationSet>, DashMpdError> {
    let id = bounded_optional_attribute(adaptation, "id", limits)?;
    let mut context = InheritedTextContext {
        language: bounded_optional_attribute(adaptation, "lang", limits)?,
        mime_type: bounded_optional_attribute(adaptation, "mimeType", limits)?,
        codecs: bounded_optional_attribute(adaptation, "codecs", limits)?,
        label: None,
        addressing: None,
    };
    let mut roles = Vec::new();
    let mut base_url = None;
    // Representation разбираются после чтения всего набора: Segment*/Label могут стоять и
    // после них, а наследование должно видеть набор целиком. Поэтому буферизуем их события.
    let mut representation_blocks: Vec<(XmlElement, Vec<XmlEvent>)> = Vec::new();
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(child) | XmlEvent::EmptyElement(child))
                if is_name(child.name(), "ContentProtection") =>
            {
                return Ok(None);
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "Representation") => {
                if representation_blocks.len() >= limits.maximum_representations_per_adaptation_set
                {
                    return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded));
                }
                let body = collect_element_body(cursor)?;
                representation_blocks.push((child, body));
            }
            Some(XmlEvent::EmptyElement(child)) if is_name(child.name(), "Representation") => {
                if representation_blocks.len() >= limits.maximum_representations_per_adaptation_set
                {
                    return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded));
                }
                representation_blocks.push((child, Vec::new()));
            }
            Some(XmlEvent::StartElement(child)) if is_role_descriptor(&child) => {
                push_role(&mut roles, &child, limits);
                consume_descriptor_body(cursor, local_name_of(&child))?;
            }
            Some(XmlEvent::EmptyElement(child)) if is_role_descriptor(&child) => {
                push_role(&mut roles, &child, limits);
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "Label") => {
                context.label = Some(read_text_leaf(cursor, "Label", limits)?);
            }
            Some(XmlEvent::StartElement(child)) if is_name(child.name(), "BaseURL") => {
                set_single_base_url(&mut base_url, parse_base_url(cursor, child, limits)?)?;
            }
            Some(XmlEvent::StartElement(child)) if is_segment_addressing(&child) => {
                set_single_addressing(
                    &mut context.addressing,
                    parse_addressing_start(cursor, child, limits)?,
                )?;
            }
            Some(XmlEvent::EmptyElement(child)) if is_segment_addressing(&child) => {
                set_single_addressing(
                    &mut context.addressing,
                    parse_addressing_empty(child, limits)?,
                )?;
            }
            Some(XmlEvent::StartElement(_)) => skip_current_element(cursor)?,
            Some(XmlEvent::EndElement(name)) if is_name(&name, "AdaptationSet") => break,
            Some(XmlEvent::EndElement(_) | XmlEvent::EmptyElement(_) | XmlEvent::Text(_)) => {}
            None => return Err(DashMpdError::new(DashMpdErrorKind::MalformedSchema)),
        }
    }
    let mut representations = Vec::new();
    for (element, body) in representation_blocks {
        cursor.replay = body.into();
        let parsed = parse_text_representation(cursor, &element, &context, limits);
        cursor.replay.clear();
        match parsed? {
            RepresentationOutcome::Supported(representation) => {
                representations.push(*representation);
            }
            RepresentationOutcome::Unsupported => {}
            RepresentationOutcome::ProtectedSet => return Ok(None),
        }
    }
    if representations.is_empty() {
        return Ok(None);
    }
    Ok(Some(DashTextAdaptationSet {
        id,
        roles: roles.into_boxed_slice(),
        base_url,
        representations: representations.into_boxed_slice(),
    }))
}

/// Итог разбора одной текстовой Representation.
enum RepresentationOutcome {
    Supported(Box<DashTextRepresentation>),
    /// MIME/кодек не опознан: Representation молча отбрасывается.
    Unsupported,
    /// Внутри Representation найден `ContentProtection`: пропускается весь набор.
    ProtectedSet,
}

/// Разбирает Representation по её буферизованному телу (события лежат в `cursor.replay`).
fn parse_text_representation(
    cursor: &mut EventCursor<'_>,
    element: &XmlElement,
    inherited: &InheritedTextContext,
    limits: DashMpdLimits,
) -> Result<RepresentationOutcome, DashMpdError> {
    let id = required_bounded_attribute(element, "id", limits)?;
    let bandwidth = optional_u64_attribute(element, "bandwidth")?;
    let mime_type = bounded_optional_attribute(element, "mimeType", limits)?
        .or_else(|| inherited.mime_type.clone());
    let codecs =
        bounded_optional_attribute(element, "codecs", limits)?.or_else(|| inherited.codecs.clone());
    let language =
        bounded_optional_attribute(element, "lang", limits)?.or_else(|| inherited.language.clone());
    let mut label = inherited.label.clone();
    let mut base_url: Option<DashBaseUrl> = None;
    let mut own_addressing = None;
    while let Some(event) = cursor.next_buffered_event() {
        match event {
            XmlEvent::StartElement(child) | XmlEvent::EmptyElement(child)
                if is_name(child.name(), "ContentProtection") =>
            {
                return Ok(RepresentationOutcome::ProtectedSet);
            }
            XmlEvent::StartElement(child) if is_name(child.name(), "Label") => {
                label = Some(read_text_leaf(cursor, "Label", limits)?);
            }
            XmlEvent::StartElement(child) if is_name(child.name(), "BaseURL") => {
                set_single_base_url(&mut base_url, parse_base_url(cursor, child, limits)?)?;
            }
            XmlEvent::StartElement(child) if is_segment_addressing(&child) => {
                set_single_addressing(
                    &mut own_addressing,
                    parse_addressing_start(cursor, child, limits)?,
                )?;
            }
            XmlEvent::EmptyElement(child) if is_segment_addressing(&child) => {
                set_single_addressing(&mut own_addressing, parse_addressing_empty(child, limits)?)?;
            }
            XmlEvent::StartElement(_) => skip_current_element(cursor)?,
            XmlEvent::EndElement(_) | XmlEvent::EmptyElement(_) | XmlEvent::Text(_) => {}
        }
    }
    let Some(kind) = classify_text_kind(mime_type.as_deref(), codecs.as_deref()) else {
        return Ok(RepresentationOutcome::Unsupported);
    };
    Ok(RepresentationOutcome::Supported(Box::new(
        DashTextRepresentation {
            id,
            bandwidth,
            language,
            label,
            kind,
            base_url,
            addressing: own_addressing
                .or_else(|| inherited.addressing.clone())
                .unwrap_or(DashAddressing::SingleResource),
        },
    )))
}

/// Формат текстовой Representation по эффективным `mimeType` и `codecs`.
fn classify_text_kind(mime_type: Option<&str>, codecs: Option<&str>) -> Option<DashTextKind> {
    match mime_type? {
        "application/ttml+xml" => Some(DashTextKind::SidecarTtml),
        "text/vtt" => Some(DashTextKind::SidecarWebVtt),
        "application/mp4" => {
            let mut kind = None;
            for codec in codecs?.split(',').map(str::trim) {
                let current = if codec == "wvtt" {
                    DashTextKind::SegmentedWvtt
                } else if codec.starts_with("stpp") {
                    DashTextKind::SegmentedStpp
                } else {
                    return None;
                };
                // Смесь stpp и wvtt в одной Representation не поддерживается.
                if kind.is_some_and(|known| known != current) {
                    return None;
                }
                kind = Some(current);
            }
            kind
        }
        _ => None,
    }
}

/// Список кодеков объявляет текстовый ISOBMFF-кодек (`stpp*` либо `wvtt`).
fn declares_text_codec(codecs: &str) -> bool {
    codecs
        .split(',')
        .map(str::trim)
        .any(|codec| codec == "wvtt" || codec.starts_with("stpp"))
}

/// `Role` либо `Accessibility`: оба могут нести значение из схемы ролей DASH.
fn is_role_descriptor(element: &XmlElement) -> bool {
    is_name(element.name(), "Role") || is_name(element.name(), "Accessibility")
}

/// Локальное имя элемента для `consume_descriptor_body`.
fn local_name_of(element: &XmlElement) -> &'static str {
    if is_name(element.name(), "Role") {
        "Role"
    } else {
        "Accessibility"
    }
}

/// Добавляет роль, если дескриптор использует схему ролей DASH; прочие схемы игнорируются.
fn push_role(roles: &mut Vec<DashTextRole>, element: &XmlElement, limits: DashMpdLimits) {
    let scheme = bounded_optional_attribute(element, "schemeIdUri", limits)
        .ok()
        .flatten();
    if scheme.as_deref() != Some(ROLE_SCHEME) {
        return;
    }
    let value = bounded_optional_attribute(element, "value", limits)
        .ok()
        .flatten();
    let role = match value.as_deref() {
        Some("main") => DashTextRole::Main,
        Some("alternate") => DashTextRole::Alternate,
        Some("supplementary") => DashTextRole::Supplementary,
        Some("commentary") => DashTextRole::Commentary,
        Some("caption") => DashTextRole::Caption,
        Some("subtitle") => DashTextRole::Subtitle,
        Some("forced-subtitle") => DashTextRole::ForcedSubtitle,
        Some("description") => DashTextRole::Description,
        _ => DashTextRole::Other,
    };
    if !roles.contains(&role) {
        roles.push(role);
    }
}

/// Элемент задаёт адресацию сегментов.
fn is_segment_addressing(element: &XmlElement) -> bool {
    is_name(element.name(), "SegmentTemplate")
        || is_name(element.name(), "SegmentList")
        || is_name(element.name(), "SegmentBase")
}

/// Разбирает открытый элемент адресации (с телом).
fn parse_addressing_start(
    cursor: &mut EventCursor<'_>,
    element: XmlElement,
    limits: DashMpdLimits,
) -> Result<DashAddressing, DashMpdError> {
    if is_name(element.name(), "SegmentTemplate") {
        Ok(DashAddressing::Template(parse_segment_template(
            cursor, element, limits,
        )?))
    } else if is_name(element.name(), "SegmentList") {
        Ok(DashAddressing::List(parse_segment_list(
            cursor, element, limits,
        )?))
    } else {
        Ok(DashAddressing::Base(parse_segment_base(
            cursor, element, limits,
        )?))
    }
}

/// Разбирает пустой элемент адресации (только атрибуты).
fn parse_addressing_empty(
    element: XmlElement,
    limits: DashMpdLimits,
) -> Result<DashAddressing, DashMpdError> {
    if is_name(element.name(), "SegmentTemplate") {
        Ok(DashAddressing::Template(parse_empty_segment_template_leaf(
            element, limits,
        )?))
    } else if is_name(element.name(), "SegmentBase") {
        Ok(DashAddressing::Base(parse_empty_segment_base(element)?))
    } else {
        // Пустой SegmentList не задаёт ни одного сегмента.
        Err(DashMpdError::new(DashMpdErrorKind::InvalidAddressing))
    }
}

/// Вычитывает тело только что открытого элемента (без его закрывающего тега).
fn collect_element_body(cursor: &mut EventCursor<'_>) -> Result<Vec<XmlEvent>, DashMpdError> {
    let mut body = Vec::new();
    let mut open_elements = 1_usize;
    loop {
        let event = cursor
            .next_event()?
            .ok_or(DashMpdError::new(DashMpdErrorKind::MalformedSchema))?;
        match &event {
            XmlEvent::StartElement(_) => {
                open_elements = open_elements
                    .checked_add(1)
                    .ok_or(DashMpdError::new(DashMpdErrorKind::LimitExceeded))?;
            }
            XmlEvent::EndElement(_) => {
                open_elements -= 1;
                if open_elements == 0 {
                    return Ok(body);
                }
            }
            XmlEvent::EmptyElement(_) | XmlEvent::Text(_) => {}
        }
        body.push(event);
    }
}

/// Пропускает тело только что открытого элемента неизвестного типа.
fn skip_current_element(cursor: &mut EventCursor<'_>) -> Result<(), DashMpdError> {
    collect_element_body(cursor).map(|_| ())
}
