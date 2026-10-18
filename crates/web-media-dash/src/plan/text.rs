//! Планирование текстовых (субтитровых) Representation одного Period.
//!
//! Использует те же планировщики адресации, что и A/V (`plan_template`, `plan_list`,
//! `resolve_optional_base`), но результат другой: не поток для демукса, а список ресурсов
//! с правилом перевода внутреннего времени ресурса в шкалу презентации. Поведение A/V
//! планировщика этот модуль не меняет.

use std::num::NonZeroUsize;
use std::time::Duration;

use dash_mpd_core::{
    DashAddressing, DashMpd, DashPeriod, DashPresentationDuration, DashTextAdaptationSet,
    DashTextKind, DashTextRepresentation,
};
use source_core::HttpRequestTarget;

use super::resources::{plan_list, plan_template, resolve_optional_base, validate_segment_base};
use super::timeline::{DashPeriodTimelineBound, DashTimelinePlanningIntent};
use super::{DashPlanError, DashPlannedResource};
use crate::request::DashSerializedFragmentKind;

/// Как время ВНУТРИ ресурса связано со шкалой презентации (MPD) этого Period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DashTextTimeBasis {
    /// Время внутри ресурса отсчитывается от начала Period (отдельные TTML/WebVTT-файлы).
    PeriodTimeline,
    /// ISOBMFF: время на media timeline дорожки; презентация = время - PTO + начало Period.
    /// Это то же преобразование, что у A/V (`globalize_packet_timestamp`).
    MediaTimeline {
        /// `presentationTimeOffset` адресации (ноль, если не задан).
        media_time_origin: Duration,
    },
    /// ISOBMFF без известного PTO (SegmentList): первый сэмпл сегмента (`tfdt`) считается
    /// началом сегмента на шкале Period (как `NormalizeAtFirstPacket` у A/V).
    NormalizedAtSegmentStart,
}

/// План текстовой Representation в одном Period.
#[derive(Clone)]
pub(crate) struct DashTextPeriodPlan {
    /// Формат ресурсов.
    pub kind: DashTextKind,
    /// Идентификатор Representation: часть идентичности init-сегмента (кэш транспорта).
    pub representation_id: Box<str>,
    /// Начало Period на шкале презентации (MPD).
    pub period_start: Duration,
    /// Правило перевода времени внутри ресурса.
    pub basis: DashTextTimeBasis,
    /// Отдельный init-ресурс (SegmentTemplate/SegmentList); `None`, если init внутри ресурса.
    pub initialization: Option<DashPlannedResource>,
    /// Медиаресурсы по порядку; `timeline_start`/`duration` заданы и отсчитываются от Period.
    pub segments: Vec<DashPlannedResource>,
}

/// Строит план одной текстовой Representation внутри Period.
///
/// Всегда использует правила «живого снимка» (допускает неполное покрытие Period): тексту
/// не нужно непрерывное покрытие, как видео, а лишний отказ закрыл бы дорожку зря.
pub(crate) fn plan_text_period(
    mpd: &DashMpd,
    manifest_base: &HttpRequestTarget,
    period: &DashPeriod,
    adaptation: &DashTextAdaptationSet,
    representation: &DashTextRepresentation,
    maximum_segments: NonZeroUsize,
) -> Result<DashTextPeriodPlan, DashPlanError> {
    let mpd_base = resolve_optional_base(manifest_base, mpd.base_url.as_ref())?;
    let period_base = resolve_optional_base(&mpd_base, period.base_url.as_ref())?;
    let adaptation_base = resolve_optional_base(&period_base, adaptation.base_url.as_ref())?;
    let representation_base =
        resolve_optional_base(&adaptation_base, representation.base_url.as_ref())?;
    let period_bound = match period.duration {
        DashPresentationDuration::FiniteMilliseconds(milliseconds) => {
            DashPeriodTimelineBound::Finite(Duration::from_millis(milliseconds))
        }
        DashPresentationDuration::OpenEnded => DashPeriodTimelineBound::OpenEnded,
    };
    let isobmff = matches!(
        representation.kind,
        DashTextKind::SegmentedStpp | DashTextKind::SegmentedWvtt
    );
    let (initialization, segments, basis) = match &representation.addressing {
        DashAddressing::Template(template) => {
            let (resources, media_time_origin, _) = plan_template(
                &representation.id,
                representation.bandwidth,
                template,
                &representation_base,
                period_bound,
                maximum_segments,
                DashTimelinePlanningIntent::DynamicSnapshot,
            )?;
            let basis = if isobmff {
                DashTextTimeBasis::MediaTimeline { media_time_origin }
            } else {
                DashTextTimeBasis::PeriodTimeline
            };
            let (initialization, segments) = split_initialization(resources);
            (initialization, segments, basis)
        }
        DashAddressing::List(list) => {
            let duration = finite_duration(period_bound)?;
            let resources = plan_list(list, &representation_base, duration, maximum_segments)?;
            let basis = if isobmff {
                DashTextTimeBasis::NormalizedAtSegmentStart
            } else {
                DashTextTimeBasis::PeriodTimeline
            };
            let (initialization, segments) = split_initialization(resources);
            (initialization, segments, basis)
        }
        DashAddressing::Base(segment_base) => {
            validate_segment_base(segment_base)?;
            (
                None,
                vec![whole_resource(representation_base, period_bound)?],
                whole_resource_basis(isobmff),
            )
        }
        DashAddressing::SingleResource => (
            None,
            vec![whole_resource(representation_base, period_bound)?],
            whole_resource_basis(isobmff),
        ),
    };
    if segments.is_empty() {
        return Err(DashPlanError::InvalidSerializedLifecycle);
    }
    Ok(DashTextPeriodPlan {
        kind: representation.kind,
        representation_id: representation.id.as_str().into(),
        period_start: Duration::from_millis(period.start_milliseconds),
        basis,
        initialization,
        segments,
    })
}

/// Один ресурс на весь Period (файл целиком; ISOBMFF-файл несёт `moov` и фрагменты сам).
fn whole_resource(
    target: HttpRequestTarget,
    period_bound: DashPeriodTimelineBound,
) -> Result<DashPlannedResource, DashPlanError> {
    Ok(DashPlannedResource {
        kind: DashSerializedFragmentKind::Media,
        target,
        byte_range: None,
        timeline_start: Some(Duration::ZERO),
        duration: Some(finite_duration(period_bound)?),
    })
}

/// Файл целиком: время ISOBMFF на media timeline (PTO у `SegmentBase` не применяется, как у A/V).
fn whole_resource_basis(isobmff: bool) -> DashTextTimeBasis {
    if isobmff {
        DashTextTimeBasis::MediaTimeline {
            media_time_origin: Duration::ZERO,
        }
    } else {
        DashTextTimeBasis::PeriodTimeline
    }
}

/// Отделяет init-ресурс (если есть) от медиаресурсов.
fn split_initialization(
    resources: Vec<DashPlannedResource>,
) -> (Option<DashPlannedResource>, Vec<DashPlannedResource>) {
    let mut initialization = None;
    let mut segments = Vec::with_capacity(resources.len());
    for resource in resources {
        match resource.kind {
            DashSerializedFragmentKind::Initialization => initialization = Some(resource),
            DashSerializedFragmentKind::Media => segments.push(resource),
        }
    }
    (initialization, segments)
}

/// Файл целиком и SegmentList требуют конечной длительности Period.
fn finite_duration(period_bound: DashPeriodTimelineBound) -> Result<Duration, DashPlanError> {
    match period_bound {
        DashPeriodTimelineBound::Finite(duration) => Ok(duration),
        DashPeriodTimelineBound::OpenEnded => {
            Err(DashPlanError::OpenEndedPeriodRequiresExplicitTimeline)
        }
    }
}
