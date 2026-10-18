//! Period: разбор элемента, разделение A/V и текстовых AdaptationSet, вычисление таймлайна.

use bounded_xml_reader::{XmlElement, XmlEvent};

use super::text_adaptation::{
    TextAdaptationOutcome, is_declared_text_adaptation_set, may_hold_text_representations,
    parse_text_adaptation_set, peek_all_representations_are_text,
};
use super::{
    DashMpdLimits, EventCursor, bounded_optional_attribute, is_name, optional_duration_attribute,
    parse_adaptation_set, parse_base_url, set_single_base_url, validate_attributes,
};
use crate::error::{DashMpdError, DashMpdErrorKind};
use crate::model::{DashAdaptationSet, DashBaseUrl, DashPeriod, DashPresentationDuration};
use crate::text_model::DashTextAdaptationSet;

/// Period до вычисления omitted start/duration.
pub(crate) struct ParsedPeriod {
    /// Optional schema identifier.
    pub(crate) id: Option<String>,
    /// Optional explicit start.
    pub(crate) start_milliseconds: Option<u64>,
    /// Optional explicit duration.
    pub(crate) duration_milliseconds: Option<u64>,
    /// Period BaseURL.
    pub(crate) base_url: Option<DashBaseUrl>,
    /// Parsed A/V adaptations.
    pub(crate) adaptation_sets: Box<[DashAdaptationSet]>,
    /// Разобранные текстовые (субтитровые) наборы: отдельно от A/V, чтобы не трогать каталог A/V.
    pub(crate) text_adaptation_sets: Box<[DashTextAdaptationSet]>,
    /// Сколько текстовых наборов пропущено как неподдерживаемые (DRM, неизвестный формат и т. п.).
    pub(crate) skipped_text_adaptation_set_count: usize,
}

/// Разбирает Period без предположений о следующем Period.
pub(crate) fn parse_period(
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
    let mut text_adaptation_sets = Vec::new();
    let mut skipped_text_adaptation_set_count = 0_usize;
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
                // Текст по признакам набора либо (только Representation-уровень) по ВСЕМ его
                // Representation; смешанный набор остаётся A/V.
                let is_text = is_declared_text_adaptation_set(&child, limits)?
                    || (may_hold_text_representations(&child, limits)?
                        && peek_all_representations_are_text(cursor, &child, limits)?);
                if is_text {
                    // Текстовый набор никогда не валит MPD из-за своего содержимого: либо
                    // разобран, либо пропущен со счётчиком (A/V от него не зависит).
                    match parse_text_adaptation_set(cursor, &child, limits)? {
                        TextAdaptationOutcome::Parsed(text_set) => {
                            text_adaptation_sets.push(text_set)
                        }
                        TextAdaptationOutcome::Skipped => {
                            skipped_text_adaptation_set_count =
                                skipped_text_adaptation_set_count.saturating_add(1);
                        }
                    }
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
        text_adaptation_sets: text_adaptation_sets.into_boxed_slice(),
        skipped_text_adaptation_set_count,
    })
}

/// Вычисляет exact contiguous Period timeline.
pub(crate) fn finalize_periods(
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
            text_adaptation_sets: period.text_adaptation_sets.clone(),
            skipped_text_adaptation_set_count: period.skipped_text_adaptation_set_count,
        });
    }
    if let Some(declared) = presentation_duration
        && declared != expected_start
    {
        return Err(DashMpdError::new(DashMpdErrorKind::InvalidPeriodTimeline));
    }
    Ok((periods.into_boxed_slice(), expected_start))
}
