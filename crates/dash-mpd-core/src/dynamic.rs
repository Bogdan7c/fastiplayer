//! Строгий pure-профиль dynamic MPD для S35.
//!
//! Модуль не знает сеть или локальные часы. Он только доказывает, что XML
//! содержит все timing inputs, необходимые runtime-у для точного live/DVR окна.
//!
//! S35 v2 contract:
//! - clock model — inline `UTCTiming` direct либо bounded HTTP XSDATE descriptor;
//! - `availabilityStartTime`, `publishTime`, positive `minimumUpdatePeriod` и
//!   positive `suggestedPresentationDelay` обязательны;
//! - отсутствие `timeShiftBufferDepth` означает окно от `availabilityStartTime`;
//! - каждый Period имеет stable `id` и explicit `start`; последний может быть open-ended;
//! - Representation selection остаётся однозначным без обязательного `AdaptationSet@id`;
//! - единственная addressing-модель — complete `SegmentTemplate` +
//!   explicit `SegmentTimeline`;
//! - `availabilityTimeOffset` хранится exact signed nanoseconds, а
//!   `availabilityTimeComplete` допускает только default/explicit `true`;
//! - `INF`, `availabilityTimeComplete=false` и прочие partial/LL semantics
//!   возвращают typed `ProfileExcluded`.

use std::fmt;

use bounded_xml_reader::{BoundedXmlReader, XmlElement, XmlEvent};

use crate::error::{DashMpdError, DashMpdErrorKind};
use crate::model::{DashAddressing, DashMpd, DashPeriod, DashPresentationDuration};
use crate::parser::{
    DashMpdParseRequest, EventCursor, ParsedPeriod, bounded_optional_attribute, is_name,
    optional_attribute, optional_duration_attribute, parse_base_url, parse_period, require_name,
    set_single_base_url, validate_attributes, validate_profiles,
};

mod clock;
pub use clock::{
    DASH_DIRECT_UTC_SCHEME, DASH_HTTP_XSDATE_UTC_SCHEME, DashUtcTimestamp,
    DashUtcTimestampParseError, DashUtcTiming, DashUtcTimingResource,
};

/// Typed причина fail-closed исключения из dynamic S35 v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashDynamicProfileExclusion {
    /// MPD не является dynamic.
    NotDynamic,
    /// Объявленный DASH profile не входит в exact доказанный allowlist.
    UnsupportedDeclaredProfile,
    /// Clock model отсутствует, неоднозначен или не поддержан.
    UnsupportedClockModel,
    /// Обязательный wall-clock timestamp отсутствует или неверен.
    MissingOrInvalidUtcTimestamp,
    /// Обязательный positive refresh cadence отсутствует или неверен.
    MissingOrInvalidMinimumUpdatePeriod,
    /// Suggested presentation delay отсутствует, равен нулю или ломает DVR.
    MissingOrInvalidSuggestedPresentationDelay,
    /// Availability start time отсутствует или неверен.
    MissingOrInvalidAvailabilityStartTime,
    /// Period identity/start либо соседние bounds недостаточны для refresh continuity.
    MissingPeriodTiming,
    /// Dynamic addressing не является SegmentTemplate + SegmentTimeline.
    UnsupportedAddressing,
    /// LL-DASH partial/chunked availability явно запрошена.
    PartialSegmentAvailability,
    /// Update identity недостаточна для ordering.
    MissingPublishTime,
    /// Неизвестная timing-changing конструкция присутствует.
    UnsupportedTimingConstruct,
}

/// Dynamic parse отделяет schema failure от намеренного profile exclusion.
#[derive(Debug, thiserror::Error)]
pub enum DashDynamicMpdError {
    /// XML/schema/model нарушены.
    #[error("dynamic DASH schema is invalid")]
    Schema(#[from] DashMpdError),
    /// Валидная DASH возможность не входит в строгий S35 v1.
    #[error("dynamic DASH profile is excluded: {0:?}")]
    ProfileExcluded(DashDynamicProfileExclusion),
}

/// Проверенный dynamic MPD snapshot без runtime clock state.
#[derive(Clone, PartialEq, Eq)]
pub struct DashDynamicMpd {
    /// Announced periods/resources текущего snapshot-а с явным open tail.
    pub presentation: DashMpd,
    /// Wall-clock anchor MPD timeline.
    pub availability_start_time: DashUtcTimestamp,
    /// Pure UTC timing descriptor; network resolution остаётся runtime-owned.
    pub utc_timing: DashUtcTiming,
    /// Monotonic MPD update identity.
    pub publish_time: DashUtcTimestamp,
    /// Positive refresh cadence.
    pub minimum_update_period_milliseconds: u64,
    /// Optional DVR depth; `None` означает историю от AST.
    pub time_shift_buffer_depth_milliseconds: Option<u64>,
    /// Required safe presentation delay.
    pub suggested_presentation_delay_milliseconds: u64,
}

impl DashDynamicMpd {
    /// Возвращает inline UTC sample только для pure timing tests/consumers.
    #[must_use]
    pub const fn direct_utc_time(&self) -> Option<DashUtcTimestamp> {
        self.utc_timing.direct_timestamp()
    }
}

impl fmt::Debug for DashDynamicMpd {
    /// Не раскрывает BaseURL/template/UTC payload.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DashDynamicMpd")
            .field("period_count", &self.presentation.periods.len())
            .field(
                "minimum_update_period_milliseconds",
                &self.minimum_update_period_milliseconds,
            )
            .field(
                "time_shift_buffer_depth_milliseconds",
                &self.time_shift_buffer_depth_milliseconds,
            )
            .field(
                "suggested_presentation_delay_milliseconds",
                &self.suggested_presentation_delay_milliseconds,
            )
            .finish_non_exhaustive()
    }
}

/// Разбирает только утверждённый dynamic S35 v2 contract.
pub fn parse_dynamic_dash_mpd(
    request: DashMpdParseRequest<'_>,
) -> Result<DashDynamicMpd, DashDynamicMpdError> {
    let limits = request.limits.validate()?;
    let reader = BoundedXmlReader::new(request.document_bytes, request.xml_budgets)
        .map_err(DashMpdError::from_xml)?;
    let mut cursor = EventCursor { reader };
    let root = match cursor.next_event()? {
        Some(XmlEvent::StartElement(element)) => element,
        _ => return Err(DashMpdError::new(DashMpdErrorKind::InvalidRoot).into()),
    };
    require_name(root.name(), "MPD", DashMpdErrorKind::InvalidRoot)?;
    if optional_attribute(&root, "type")? != Some("dynamic") {
        return Err(excluded(DashDynamicProfileExclusion::NotDynamic));
    }
    validate_profiles(optional_attribute(&root, "profiles")?).map_err(map_dynamic_schema_error)?;
    validate_dynamic_root_attributes(&root, limits)?;
    let availability_start_time = required_utc_attribute(
        &root,
        "availabilityStartTime",
        DashDynamicProfileExclusion::MissingOrInvalidAvailabilityStartTime,
    )?;
    let publish_time = required_utc_attribute(
        &root,
        "publishTime",
        DashDynamicProfileExclusion::MissingPublishTime,
    )?;
    let minimum_update_period_milliseconds =
        required_positive_duration(&root, "minimumUpdatePeriod").map_err(|_| {
            excluded(DashDynamicProfileExclusion::MissingOrInvalidMinimumUpdatePeriod)
        })?;
    let suggested_presentation_delay_milliseconds =
        required_positive_duration(&root, "suggestedPresentationDelay").map_err(|_| {
            excluded(DashDynamicProfileExclusion::MissingOrInvalidSuggestedPresentationDelay)
        })?;
    let time_shift_buffer_depth_milliseconds =
        optional_duration_attribute(&root, "timeShiftBufferDepth")?;
    if time_shift_buffer_depth_milliseconds
        .is_some_and(|depth| depth <= suggested_presentation_delay_milliseconds)
    {
        return Err(excluded(
            DashDynamicProfileExclusion::MissingOrInvalidSuggestedPresentationDelay,
        ));
    }

    let mut base_url = None;
    let mut periods = Vec::new();
    let mut utc_timing = None;
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(element)) if is_name(element.name(), "BaseURL") => {
                let parsed = parse_base_url(&mut cursor, element, limits)
                    .map_err(map_dynamic_schema_error)?;
                set_single_base_url(&mut base_url, parsed).map_err(map_dynamic_schema_error)?;
            }
            Some(XmlEvent::StartElement(element))
                if is_name(element.name(), "ProgramInformation") =>
            {
                consume_informational_element(&mut cursor)?;
            }
            Some(XmlEvent::EmptyElement(element))
                if is_name(element.name(), "ProgramInformation") => {}
            Some(XmlEvent::EmptyElement(element)) if is_name(element.name(), "UTCTiming") => {
                if utc_timing.is_some() {
                    return Err(excluded(DashDynamicProfileExclusion::UnsupportedClockModel));
                }
                utc_timing = Some(parse_utc_timing(&element, limits)?);
            }
            Some(XmlEvent::StartElement(element)) if is_name(element.name(), "UTCTiming") => {
                if utc_timing.is_some() {
                    return Err(excluded(DashDynamicProfileExclusion::UnsupportedClockModel));
                }
                let parsed = parse_utc_timing(&element, limits)?;
                consume_empty_utc_timing_body(&mut cursor)?;
                utc_timing = Some(parsed);
            }
            Some(XmlEvent::StartElement(element)) if is_name(element.name(), "Period") => {
                if periods.len() >= limits.maximum_periods {
                    return Err(DashMpdError::new(DashMpdErrorKind::LimitExceeded).into());
                }
                periods.push(
                    parse_period(&mut cursor, element, limits).map_err(map_dynamic_schema_error)?,
                );
            }
            Some(XmlEvent::EndElement(name)) if is_name(&name, "MPD") => break,
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(_) | None => {
                return Err(excluded(
                    DashDynamicProfileExclusion::UnsupportedTimingConstruct,
                ));
            }
        }
    }
    if cursor.next_event()?.is_some() || periods.is_empty() {
        return Err(DashMpdError::new(DashMpdErrorKind::InvalidRoot).into());
    }
    let (periods, media_presentation_duration) = finalize_dynamic_periods(periods)?;
    let presentation = DashMpd {
        media_presentation_duration,
        base_url,
        periods,
    };
    validate_dynamic_addressing(&presentation)?;
    Ok(DashDynamicMpd {
        presentation,
        availability_start_time,
        utc_timing: utc_timing
            .ok_or_else(|| excluded(DashDynamicProfileExclusion::UnsupportedClockModel))?,
        publish_time,
        minimum_update_period_milliseconds,
        time_shift_buffer_depth_milliseconds,
        suggested_presentation_delay_milliseconds,
    })
}

/// Root допускает exact XSI schema hint, который не влияет на playback semantics.
fn validate_dynamic_root_attributes(
    root: &XmlElement,
    limits: crate::parser::DashMpdLimits,
) -> Result<(), DashDynamicMpdError> {
    const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";
    const ALLOWED_UNQUALIFIED_ATTRIBUTES: &[&str] = &[
        "id",
        "type",
        "profiles",
        "minBufferTime",
        "maxSegmentDuration",
        "availabilityStartTime",
        "publishTime",
        "minimumUpdatePeriod",
        "timeShiftBufferDepth",
        "suggestedPresentationDelay",
    ];
    for attribute in root.attributes() {
        let attribute_name = attribute.name();
        let unqualified_is_allowed = attribute_name.namespace_uri().is_none()
            && ALLOWED_UNQUALIFIED_ATTRIBUTES.contains(&attribute_name.local_name());
        let xsi_schema_hint_is_allowed = attribute_name.namespace_uri() == Some(XSI_NAMESPACE)
            && attribute_name.local_name() == "schemaLocation"
            && !attribute.value().is_empty()
            && attribute.value().len() <= limits.maximum_schema_string_bytes;
        if !unqualified_is_allowed && !xsi_schema_hint_is_allowed {
            return Err(excluded(
                DashDynamicProfileExclusion::UnsupportedTimingConstruct,
            ));
        }
    }
    Ok(())
}

/// Завершает contiguous dynamic timeline, сохраняя open tail отдельным типом.
fn finalize_dynamic_periods(
    parsed_periods: Vec<ParsedPeriod>,
) -> Result<(Box<[DashPeriod]>, DashPresentationDuration), DashDynamicMpdError> {
    let mut finalized_periods = Vec::with_capacity(parsed_periods.len());
    let mut expected_start_milliseconds = None;
    for (period_index, parsed_period) in parsed_periods.iter().enumerate() {
        let period_id_is_missing = parsed_period.id.as_deref().is_none_or(str::is_empty);
        let period_start_milliseconds = parsed_period
            .start_milliseconds
            .ok_or_else(|| excluded(DashDynamicProfileExclusion::MissingPeriodTiming))?;
        if period_id_is_missing
            || expected_start_milliseconds
                .is_some_and(|expected_start| expected_start != period_start_milliseconds)
        {
            return Err(excluded(DashDynamicProfileExclusion::MissingPeriodTiming));
        }

        let next_period_start_milliseconds = parsed_periods
            .get(period_index + 1)
            .and_then(|next_period| next_period.start_milliseconds);
        let period_duration = match (
            parsed_period.duration_milliseconds,
            next_period_start_milliseconds,
        ) {
            (Some(0), _) => {
                return Err(excluded(DashDynamicProfileExclusion::MissingPeriodTiming));
            }
            (Some(duration_milliseconds), next_start) => {
                let period_end_milliseconds = period_start_milliseconds
                    .checked_add(duration_milliseconds)
                    .ok_or_else(|| excluded(DashDynamicProfileExclusion::MissingPeriodTiming))?;
                if next_start.is_some_and(|next_start| next_start != period_end_milliseconds) {
                    return Err(excluded(DashDynamicProfileExclusion::MissingPeriodTiming));
                }
                expected_start_milliseconds = Some(period_end_milliseconds);
                DashPresentationDuration::FiniteMilliseconds(duration_milliseconds)
            }
            (None, Some(next_start_milliseconds)) => {
                let duration_milliseconds = next_start_milliseconds
                    .checked_sub(period_start_milliseconds)
                    .filter(|duration| *duration > 0)
                    .ok_or_else(|| excluded(DashDynamicProfileExclusion::MissingPeriodTiming))?;
                expected_start_milliseconds = Some(next_start_milliseconds);
                DashPresentationDuration::FiniteMilliseconds(duration_milliseconds)
            }
            (None, None) => {
                expected_start_milliseconds = None;
                DashPresentationDuration::OpenEnded
            }
        };
        finalized_periods.push(DashPeriod {
            id: parsed_period.id.clone(),
            start_milliseconds: period_start_milliseconds,
            duration: period_duration,
            base_url: parsed_period.base_url.clone(),
            adaptation_sets: parsed_period.adaptation_sets.clone(),
        });
    }

    let last_period = finalized_periods
        .last()
        .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidRoot))?;
    let presentation_duration = match last_period.duration {
        DashPresentationDuration::FiniteMilliseconds(duration_milliseconds) => {
            DashPresentationDuration::FiniteMilliseconds(
                last_period
                    .start_milliseconds
                    .checked_add(duration_milliseconds)
                    .ok_or_else(|| excluded(DashDynamicProfileExclusion::MissingPeriodTiming))?,
            )
        }
        DashPresentationDuration::OpenEnded => DashPresentationDuration::OpenEnded,
    };
    Ok((finalized_periods.into_boxed_slice(), presentation_duration))
}

/// S35 v2 допускает только explicit SegmentTimeline.
fn validate_dynamic_addressing(mpd: &DashMpd) -> Result<(), DashDynamicMpdError> {
    reject_partial_base_url(mpd.base_url.as_ref())?;
    for period in &mpd.periods {
        reject_partial_base_url(period.base_url.as_ref())?;
        for adaptation in &period.adaptation_sets {
            reject_partial_base_url(adaptation.base_url.as_ref())?;
            for representation in &adaptation.representations {
                reject_partial_base_url(representation.base_url.as_ref())?;
                match &representation.addressing {
                    DashAddressing::Template(template)
                        if template.duration.is_none() && !template.timeline.is_empty() =>
                    {
                        if template.availability_time_complete == Some(false) {
                            return Err(excluded(
                                DashDynamicProfileExclusion::PartialSegmentAvailability,
                            ));
                        }
                    }
                    _ => {
                        return Err(excluded(DashDynamicProfileExclusion::UnsupportedAddressing));
                    }
                }
            }
        }
    }
    Ok(())
}

/// `availabilityTimeComplete=false` является LL-DASH semantics и исключается.
fn reject_partial_base_url(
    base_url: Option<&crate::model::DashBaseUrl>,
) -> Result<(), DashDynamicMpdError> {
    if base_url.is_some_and(|base_url| base_url.availability_time_complete == Some(false)) {
        return Err(excluded(
            DashDynamicProfileExclusion::PartialSegmentAvailability,
        ));
    }
    Ok(())
}

/// Разбирает pure clock descriptor без network I/O внутри schema owner-а.
fn parse_utc_timing(
    element: &XmlElement,
    limits: crate::parser::DashMpdLimits,
) -> Result<DashUtcTiming, DashDynamicMpdError> {
    validate_attributes(element, &["schemeIdUri", "value"])?;
    let timing_scheme = bounded_optional_attribute(element, "schemeIdUri", limits)?
        .ok_or_else(|| excluded(DashDynamicProfileExclusion::UnsupportedClockModel))?;
    let timing_value = bounded_optional_attribute(element, "value", limits)?
        .ok_or_else(|| excluded(DashDynamicProfileExclusion::MissingOrInvalidUtcTimestamp))?;
    match timing_scheme.as_str() {
        DASH_DIRECT_UTC_SCHEME => DashUtcTimestamp::parse_iso8601(&timing_value)
            .map(DashUtcTiming::Direct)
            .map_err(|_| excluded(DashDynamicProfileExclusion::MissingOrInvalidUtcTimestamp)),
        DASH_HTTP_XSDATE_UTC_SCHEME => DashUtcTimingResource::new(timing_value)
            .map(DashUtcTiming::HttpXsDate)
            .map_err(|_| excluded(DashDynamicProfileExclusion::MissingOrInvalidUtcTimestamp)),
        _ => Err(excluded(DashDynamicProfileExclusion::UnsupportedClockModel)),
    }
}

/// Пропускает только exact informational subtree под общими XML budgets.
fn consume_informational_element(cursor: &mut EventCursor<'_>) -> Result<(), DashDynamicMpdError> {
    let mut open_element_count = 1_usize;
    loop {
        match cursor.next_event()? {
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
            None => {
                return Err(excluded(
                    DashDynamicProfileExclusion::UnsupportedTimingConstruct,
                ));
            }
        }
    }
}

/// Paired `UTCTiming` допускает только whitespace между start/end tags.
fn consume_empty_utc_timing_body(cursor: &mut EventCursor<'_>) -> Result<(), DashDynamicMpdError> {
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::Text(text)) if text.content().trim().is_empty() => {}
            Some(XmlEvent::EndElement(name)) if is_name(&name, "UTCTiming") => return Ok(()),
            Some(_) | None => {
                return Err(excluded(DashDynamicProfileExclusion::UnsupportedClockModel));
            }
        }
    }
}

/// Читает required positive ISO duration.
fn required_positive_duration(element: &XmlElement, name: &str) -> Result<u64, DashMpdError> {
    optional_duration_attribute(element, name)?
        .filter(|duration| *duration > 0)
        .ok_or_else(|| DashMpdError::new(DashMpdErrorKind::InvalidAttribute))
}

/// Использует общий audited UTC parser вместо самописного Gregorian calendar.
fn required_utc_attribute(
    element: &XmlElement,
    name: &str,
    exclusion: DashDynamicProfileExclusion,
) -> Result<DashUtcTimestamp, DashDynamicMpdError> {
    let value = optional_attribute(element, name)?.ok_or_else(|| excluded(exclusion))?;
    DashUtcTimestamp::parse_iso8601(value).map_err(|_| excluded(exclusion))
}

/// Короткий constructor сохраняет typed profile category.
const fn excluded(reason: DashDynamicProfileExclusion) -> DashDynamicMpdError {
    DashDynamicMpdError::ProfileExcluded(reason)
}

/// Переводит валидные, но недоказанные DASH возможности в typed ProfileExcluded.
fn map_dynamic_schema_error(error: DashMpdError) -> DashDynamicMpdError {
    match error.kind() {
        DashMpdErrorKind::UnsupportedProfile => {
            excluded(DashDynamicProfileExclusion::UnsupportedDeclaredProfile)
        }
        DashMpdErrorKind::UnsupportedAvailabilityOffset => {
            excluded(DashDynamicProfileExclusion::PartialSegmentAvailability)
        }
        DashMpdErrorKind::UnsupportedConstruct => {
            excluded(DashDynamicProfileExclusion::UnsupportedTimingConstruct)
        }
        DashMpdErrorKind::InvalidPeriodTimeline => {
            excluded(DashDynamicProfileExclusion::MissingPeriodTiming)
        }
        _ => DashDynamicMpdError::Schema(error),
    }
}
