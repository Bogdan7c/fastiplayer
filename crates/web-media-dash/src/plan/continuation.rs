//! Typed boundary между consumed immutable DASH plan и следующим live snapshot-ом.

use std::time::Duration;

use thiserror::Error;

use super::{
    DashComponentPlan, DashPeriodInputPlan, DashPresentationPlan, DashSerializedFragmentKind,
};

/// Последний media fragment, полностью принадлежащий установленному component plan-у.
///
/// Runtime использует position, а не endpoint URL: при refresh подписанный URL может
/// измениться, тогда как presentation position остаётся стабильной identity сегмента.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DashComponentContinuationPoint {
    /// Global presentation start последнего media fragment-а.
    last_media_start: Duration,
}

/// Shape-aware continuation point не позволяет перепутать muxed и separate A/V plans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DashPresentationContinuationPoint {
    /// Один muxed либо single-kind component.
    Single(DashComponentContinuationPoint),
    /// Независимые границы aligned video/audio components.
    Separate {
        /// Последний video fragment установленного snapshot-а.
        video: DashComponentContinuationPoint,
        /// Последний audio fragment установленного snapshot-а.
        audio: DashComponentContinuationPoint,
    },
}

/// Dynamic continuation требует explicit ordered timeline resources.
#[derive(Debug, Error)]
pub(crate) enum DashContinuationPlanError {
    /// Range addressing не предоставляет immutable fragment identity.
    #[error("DASH live continuation requires ordered media resources")]
    RangeAddressing,
    /// Empty component нельзя считать успешно установленным live snapshot-ом.
    #[error("DASH live continuation plan has no media fragments")]
    MissingMediaFragment,
    /// Dynamic planner обязан сохранить explicit timeline start каждого fragment-а.
    #[error("DASH live continuation media fragment has no timeline start")]
    MissingMediaTimelineStart,
    /// Global Period + fragment position не помещается в Duration.
    #[error("DASH live continuation position overflow")]
    PositionOverflow,
}

impl DashComponentPlan {
    /// Фиксирует последний media fragment immutable plan-а после успешного open.
    pub(crate) fn continuation_point(
        &self,
    ) -> Result<DashComponentContinuationPoint, DashContinuationPlanError> {
        let mut last_media_start = None;
        for period in &self.periods {
            let DashPeriodInputPlan::Ordered { resources, .. } = &period.input else {
                return Err(DashContinuationPlanError::RangeAddressing);
            };
            for resource in resources
                .iter()
                .filter(|resource| resource.kind == DashSerializedFragmentKind::Media)
            {
                let resource_start = resource
                    .timeline_start
                    .ok_or(DashContinuationPlanError::MissingMediaTimelineStart)?;
                let global_start = period
                    .timeline_start
                    .checked_add(resource_start)
                    .ok_or(DashContinuationPlanError::PositionOverflow)?;
                last_media_start = Some(
                    last_media_start
                        .map_or(global_start, |current: Duration| current.max(global_start)),
                );
            }
        }
        last_media_start
            .map(|last_media_start| DashComponentContinuationPoint { last_media_start })
            .ok_or(DashContinuationPlanError::MissingMediaFragment)
    }

    /// Находит первый media fragment строго после уже consumed plan boundary.
    ///
    /// Возвращаемый media index считается только среди media resources — это exact
    /// контракт `DashOrderedSegmentSource::new`, который всегда добавляет init отдельно.
    pub(crate) fn first_media_after(
        &self,
        point: DashComponentContinuationPoint,
    ) -> Result<Option<(usize, usize)>, DashContinuationPlanError> {
        let mut first = None;
        for (period_index, period) in self.periods.iter().enumerate() {
            let DashPeriodInputPlan::Ordered { resources, .. } = &period.input else {
                return Err(DashContinuationPlanError::RangeAddressing);
            };
            for (media_index, resource) in resources
                .iter()
                .filter(|resource| resource.kind == DashSerializedFragmentKind::Media)
                .enumerate()
            {
                let resource_start = resource
                    .timeline_start
                    .ok_or(DashContinuationPlanError::MissingMediaTimelineStart)?;
                let global_start = period
                    .timeline_start
                    .checked_add(resource_start)
                    .ok_or(DashContinuationPlanError::PositionOverflow)?;
                if global_start <= point.last_media_start {
                    continue;
                }
                match first {
                    Some((first_start, _, _)) if first_start <= global_start => {}
                    _ => first = Some((global_start, period_index, media_index)),
                }
            }
        }
        Ok(first.map(|(_, period_index, media_index)| (period_index, media_index)))
    }
}

impl DashPresentationPlan {
    /// Сохраняет component shape вместе с consumed fragment boundaries.
    pub(crate) fn continuation_point(
        &self,
    ) -> Result<DashPresentationContinuationPoint, DashContinuationPlanError> {
        match self {
            Self::Single(component) => component
                .continuation_point()
                .map(DashPresentationContinuationPoint::Single),
            Self::Separate { video, audio } => Ok(DashPresentationContinuationPoint::Separate {
                video: video.continuation_point()?,
                audio: audio.continuation_point()?,
            }),
        }
    }
}
