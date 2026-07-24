//! Строгий pure-профиль dynamic MPD для S35.
//!
//! Модуль не знает сеть или локальные часы. Он только доказывает, что XML
//! содержит все timing inputs, необходимые runtime-у для точного live/DVR окна.
//!
//! S35 v1 contract:
//! - единственный clock model — `UTCTiming` direct 2014 с exact offset timestamp;
//! - `availabilityStartTime`, `publishTime`, positive `minimumUpdatePeriod` и
//!   positive `suggestedPresentationDelay` обязательны;
//! - отсутствие `timeShiftBufferDepth` означает окно от `availabilityStartTime`;
//! - каждый Period имеет stable `id`, explicit `start` и `duration`;
//! - каждый AdaptationSet/Representation имеет stable identity;
//! - единственная addressing-модель — complete `SegmentTemplate` +
//!   explicit `SegmentTimeline`;
//! - `availabilityTimeOffset` хранится exact signed nanoseconds, а
//!   `availabilityTimeComplete` допускает только default/explicit `true`;
//! - `INF`, `availabilityTimeComplete=false` и прочие partial/LL semantics
//!   возвращают typed `ProfileExcluded`.

use std::fmt;

use bounded_xml_reader::{BoundedXmlReader, XmlElement, XmlEvent};
use time::OffsetDateTime;
use time::format_description::well_known::Iso8601;

use crate::error::{DashMpdError, DashMpdErrorKind};
use crate::model::{DashAddressing, DashMpd};
use crate::parser::{
    DashMpdParseRequest, EventCursor, finalize_periods, is_name, optional_attribute,
    optional_duration_attribute, parse_base_url, parse_period, require_name, set_single_base_url,
    validate_attributes, validate_profiles,
};

/// Единственный UTC synchronization scheme строгого S35 v1.
pub const DASH_DIRECT_UTC_SCHEME: &str = "urn:mpeg:dash:utc:direct:2014";

/// UTC timestamp без исходного XML текста и без floating-point арифметики.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DashUtcTimestamp {
    unix_nanoseconds: i128,
}

impl DashUtcTimestamp {
    /// Создаёт timestamp из clock/test boundary.
    #[must_use]
    pub const fn from_unix_nanoseconds(unix_nanoseconds: i128) -> Self {
        Self { unix_nanoseconds }
    }

    /// Возвращает точное число nanoseconds относительно Unix epoch.
    #[must_use]
    pub const fn unix_nanoseconds(self) -> i128 {
        self.unix_nanoseconds
    }
}

impl fmt::Debug for DashUtcTimestamp {
    /// Не отражает исходный UTCTiming payload в diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DashUtcTimestamp(<redacted>)")
    }
}

/// Typed причина fail-closed исключения из dynamic S35 v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashDynamicProfileExclusion {
    /// MPD не является dynamic.
    NotDynamic,
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
    /// Period identity/start/duration недостаточны для refresh continuity.
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
    /// Finite announced periods/resources текущего snapshot-а.
    pub presentation: DashMpd,
    /// Wall-clock anchor MPD timeline.
    pub availability_start_time: DashUtcTimestamp,
    /// Direct UTC evidence текущего response-а.
    pub direct_utc_time: DashUtcTimestamp,
    /// Monotonic MPD update identity.
    pub publish_time: DashUtcTimestamp,
    /// Positive refresh cadence.
    pub minimum_update_period_milliseconds: u64,
    /// Optional DVR depth; `None` означает историю от AST.
    pub time_shift_buffer_depth_milliseconds: Option<u64>,
    /// Required safe presentation delay.
    pub suggested_presentation_delay_milliseconds: u64,
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

/// Разбирает только утверждённый dynamic S35 v1 contract.
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
    validate_profiles(optional_attribute(&root, "profiles")?)?;
    validate_attributes(
        &root,
        &[
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
        ],
    )
    .map_err(|_| excluded(DashDynamicProfileExclusion::UnsupportedTimingConstruct))?;
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
    let mut direct_utc_time = None;
    loop {
        match cursor.next_event()? {
            Some(XmlEvent::StartElement(element)) if is_name(element.name(), "BaseURL") => {
                let parsed = parse_base_url(&mut cursor, element, limits)
                    .map_err(map_dynamic_schema_error)?;
                set_single_base_url(&mut base_url, parsed).map_err(map_dynamic_schema_error)?;
            }
            Some(XmlEvent::EmptyElement(element)) if is_name(element.name(), "UTCTiming") => {
                if direct_utc_time.is_some() {
                    return Err(excluded(DashDynamicProfileExclusion::UnsupportedClockModel));
                }
                direct_utc_time = Some(parse_direct_utc_timing(&element)?);
            }
            Some(XmlEvent::StartElement(element)) if is_name(element.name(), "UTCTiming") => {
                if direct_utc_time.is_some() {
                    return Err(excluded(DashDynamicProfileExclusion::UnsupportedClockModel));
                }
                let parsed = parse_direct_utc_timing(&element)?;
                consume_empty_utc_timing_body(&mut cursor)?;
                direct_utc_time = Some(parsed);
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
    validate_dynamic_periods(&periods)?;
    let (periods, total_duration) = finalize_periods(periods, None)?;
    let presentation = DashMpd {
        media_presentation_duration_milliseconds: total_duration,
        base_url,
        periods,
    };
    validate_dynamic_addressing(&presentation)?;
    Ok(DashDynamicMpd {
        presentation,
        availability_start_time,
        direct_utc_time: direct_utc_time
            .ok_or_else(|| excluded(DashDynamicProfileExclusion::UnsupportedClockModel))?,
        publish_time,
        minimum_update_period_milliseconds,
        time_shift_buffer_depth_milliseconds,
        suggested_presentation_delay_milliseconds,
    })
}

/// Dynamic continuity требует stable explicit Period identity и finite timing.
fn validate_dynamic_periods(
    periods: &[crate::parser::ParsedPeriod],
) -> Result<(), DashDynamicMpdError> {
    for period in periods {
        if period.id.as_deref().is_none_or(str::is_empty)
            || period.start_milliseconds.is_none()
            || period.duration_milliseconds.is_none()
        {
            return Err(excluded(DashDynamicProfileExclusion::MissingPeriodTiming));
        }
    }
    Ok(())
}

/// S35 v1 допускает только explicit SegmentTimeline.
fn validate_dynamic_addressing(mpd: &DashMpd) -> Result<(), DashDynamicMpdError> {
    reject_partial_base_url(mpd.base_url.as_ref())?;
    for period in &mpd.periods {
        reject_partial_base_url(period.base_url.as_ref())?;
        for adaptation in &period.adaptation_sets {
            if adaptation.id.as_deref().is_none_or(str::is_empty) {
                return Err(excluded(DashDynamicProfileExclusion::MissingPeriodTiming));
            }
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

/// UTCTiming payload остаётся только parsed timestamp-ом.
fn parse_direct_utc_timing(element: &XmlElement) -> Result<DashUtcTimestamp, DashDynamicMpdError> {
    validate_attributes(element, &["schemeIdUri", "value"])?;
    if optional_attribute(element, "schemeIdUri")? != Some(DASH_DIRECT_UTC_SCHEME) {
        return Err(excluded(DashDynamicProfileExclusion::UnsupportedClockModel));
    }
    required_utc_attribute(
        element,
        "value",
        DashDynamicProfileExclusion::MissingOrInvalidUtcTimestamp,
    )
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

/// Использует audited `time` parser вместо самописного Gregorian calendar.
fn required_utc_attribute(
    element: &XmlElement,
    name: &str,
    exclusion: DashDynamicProfileExclusion,
) -> Result<DashUtcTimestamp, DashDynamicMpdError> {
    let value = optional_attribute(element, name)?.ok_or_else(|| excluded(exclusion))?;
    let parsed =
        OffsetDateTime::parse(value, &Iso8601::DEFAULT).map_err(|_| excluded(exclusion))?;
    Ok(DashUtcTimestamp::from_unix_nanoseconds(
        parsed.unix_timestamp_nanos(),
    ))
}

/// Короткий constructor сохраняет typed profile category.
const fn excluded(reason: DashDynamicProfileExclusion) -> DashDynamicMpdError {
    DashDynamicMpdError::ProfileExcluded(reason)
}

/// Переводит standard LL availability marker в обязательный typed ProfileExcluded.
fn map_dynamic_schema_error(error: DashMpdError) -> DashDynamicMpdError {
    if error.kind() == DashMpdErrorKind::UnsupportedAvailabilityOffset {
        excluded(DashDynamicProfileExclusion::PartialSegmentAvailability)
    } else {
        DashDynamicMpdError::Schema(error)
    }
}
