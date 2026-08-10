//! Period lifecycle projection для finite VOD и open live snapshot-ов.

use std::time::Duration;

use dash_mpd_core::DashPresentationDuration;

use super::{
    DashComponentPeriodPlan, DashComponentPlan, DashPeriodInputPlan, DashPeriodLifecycle,
    DashPlanError, DashPresentationPlan,
};

impl DashPresentationPlan {
    /// Возвращает самую раннюю доказанную media boundary выбранной presentation.
    ///
    /// Live runtime использует её как стабильный session origin, не заглядывая
    /// напрямую во внутреннее устройство component/Period/resource plan-ов.
    pub(crate) fn earliest_planned_media_start(&self) -> Result<Duration, DashPlanError> {
        match self {
            Self::Single(component) => earliest_component_media_start(component),
            Self::Separate { video, audio } => {
                Ok(earliest_component_media_start(video)?
                    .min(earliest_component_media_start(audio)?))
            }
        }
    }
}

/// Находит global start первой ordered media boundary одного component-а.
fn earliest_component_media_start(
    component: &DashComponentPlan,
) -> Result<Duration, DashPlanError> {
    let mut earliest = None;
    for period in &component.periods {
        let DashPeriodInputPlan::Ordered { resources, .. } = &period.input else {
            continue;
        };
        for local_start in resources
            .iter()
            .filter_map(|resource| resource.timeline_start)
        {
            let global_start = period
                .timeline_start
                .checked_add(local_start)
                .ok_or(DashPlanError::TimelineOverflow)?;
            earliest =
                Some(earliest.map_or(global_start, |current: Duration| current.min(global_start)));
        }
    }
    earliest.ok_or(DashPlanError::InvalidSerializedLifecycle)
}

/// Вычисляет operational horizon, не превращая open lifecycle в fake duration.
pub(super) fn component_snapshot_duration(
    declared_duration: DashPresentationDuration,
    periods: &[DashComponentPeriodPlan],
) -> Result<Duration, DashPlanError> {
    match declared_duration {
        DashPresentationDuration::FiniteMilliseconds(duration_milliseconds) => {
            Ok(Duration::from_millis(duration_milliseconds))
        }
        DashPresentationDuration::OpenEnded => {
            let last_period = periods
                .last()
                .ok_or(DashPlanError::InvalidSerializedLifecycle)?;
            last_period
                .timeline_start
                .checked_add(last_period.duration)
                .ok_or(DashPlanError::TimelineOverflow)
        }
    }
}

/// Проверяет declared Period boundaries pair-а, не требуя общего fake live end-а.
pub(super) fn validate_period_alignment(
    video: &DashComponentPlan,
    audio: &DashComponentPlan,
) -> Result<(), DashPlanError> {
    let periods_are_misaligned = video.periods.len() != audio.periods.len()
        || video
            .periods
            .iter()
            .zip(&audio.periods)
            .any(|(video_period, audio_period)| {
                video_period.timeline_start != audio_period.timeline_start
                    || video_period.declared_lifecycle != audio_period.declared_lifecycle
                    || matches!(
                        video_period.declared_lifecycle,
                        DashPeriodLifecycle::Finite(_)
                    ) && video_period.duration != audio_period.duration
            });
    let finite_presentation_durations_differ = video.periods.last().is_some_and(|last_period| {
        matches!(
            last_period.declared_lifecycle,
            DashPeriodLifecycle::Finite(_)
        ) && video.duration != audio.duration
    });
    if periods_are_misaligned || finite_presentation_durations_differ {
        return Err(DashPlanError::ComponentAlignmentMismatch);
    }
    Ok(())
}
