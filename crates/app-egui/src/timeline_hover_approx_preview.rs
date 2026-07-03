//! App-owned approximate hover preview slot.
//!
//! Этот модуль намеренно живёт в `app-egui`, а не в `frame-server-core`:
//! approximate keyframe является только визуальным fallback-ом hover preview.
//! Shared working set остаётся exact-only и продолжает хранить только кадры,
//! которые прошли `FrameExactnessPolicy::TargetOrAfter`.

use frame_server_core::TimelineHoverPrepareSessionEndReleaseReason;
use media_core::TrackTimestamp;
use tracing::trace;
use video_core::HostUploadBackpressureReason;
use video_present_core::VideoFrameLease;

use crate::timeline_hover_prepare::{DecodeDependencySpan, TimelineHoverPrepareSpanId};

/// Причина очистки approximate слота.
///
/// Значения нужны не для UI, а для typed lifecycle trace-ов: caller видит,
/// почему keyframe lease больше не удерживается визуальным fallback-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverApproximatePreviewClearReason {
    /// Новый dependency span заменяет предыдущий visual keyframe.
    NewSpanStarted { span_id: TimelineHoverPrepareSpanId },

    /// Текущий span отменён: leave, supersede или другой controller cancel.
    SpanCancelled { span_id: TimelineHoverPrepareSpanId },

    /// Exact target-or-after кадр готов, approximate больше не нужен.
    ExactPreparedHit { span_id: TimelineHoverPrepareSpanId },

    /// Hover source/backend/config потеряли актуальность для старого lease-а.
    SourceLost,

    /// Timeline hover session завершилась: leave grace или non-timeline action.
    SessionEnded {
        reason: TimelineHoverPrepareSessionEndReleaseReason,
    },

    /// Provider поменялся; старые resource handles нельзя материализовать.
    SourceOrBackendSwitched,

    /// Approximate lease уступает слот пула продолжению exact decode.
    ExactDecodePoolPressure {
        span_id: TimelineHoverPrepareSpanId,
        pressure: HostUploadBackpressureReason,
    },
}

/// Результат очистки approximate слота без двусмысленного `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverApproximatePreviewClearOutcome {
    /// В слоте был lease; drop запускает обычный RAII release path.
    Cleared,

    /// Слот уже был пуст, повторный release не выполнялся.
    AlreadyEmpty,
}

impl TimelineHoverApproximatePreviewClearOutcome {
    /// Удобный read-only helper для diagnostics/tests.
    #[must_use]
    pub(crate) const fn cleared_slot(self) -> bool {
        matches!(self, Self::Cleared)
    }
}

/// Результат публикации первого pre-target кадра span-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverApproximatePreviewPublishOutcome {
    /// Слот принял keyframe lease для текущего span-а.
    Published,

    /// Старый slot был заменён новым span-ом.
    ReplacedPreviousSpan,

    /// Для этого span-а approximate уже был опубликован; новый lease dropped.
    AlreadyPublishedForSpan,
}

/// Clone-borrow approximate lease-а для preview materialization.
///
/// Borrow не даёт caller-у права удалять slot: он получает только clone lease-а,
/// а исходный owner остаётся в `TimelineHoverApproximatePreviewSlot`.
#[derive(Clone)]
pub(crate) struct TimelineHoverApproximatePreviewBorrow {
    lease: VideoFrameLease,
    actual_pts: TrackTimestamp,
}

impl TimelineHoverApproximatePreviewBorrow {
    /// Собирает borrow из уже клонированного lease-а.
    ///
    /// Span-принадлежность keyframe-а валидируется owner-ом slot-а при выдаче
    /// borrow-а (`borrow_for_span`); сам borrow span не переносит, потому что
    /// visual слой показывает кадр по latest-only replace policy.
    #[must_use]
    pub(crate) const fn new(lease: VideoFrameLease, actual_pts: TrackTimestamp) -> Self {
        Self { lease, actual_pts }
    }

    /// Возвращает lease, который будет материализован shared helper-ом.
    #[must_use]
    pub(crate) const fn lease(&self) -> &VideoFrameLease {
        &self.lease
    }

    /// Actual PTS keyframe-а; это diagnostics metadata, не exact target proof.
    #[must_use]
    pub(crate) const fn actual_pts(&self) -> TrackTimestamp {
        self.actual_pts
    }
}

/// Единственный visual approximate slot hover preview-а.
#[derive(Default)]
pub(crate) struct TimelineHoverApproximatePreviewSlot {
    frame: Option<TimelineHoverApproximatePreviewFrame>,
}

impl TimelineHoverApproximatePreviewSlot {
    /// Публикует первый decoded pre-target кадр текущего span-а.
    ///
    /// Lease остаётся только в app-egui visual slot-е; working set его не видит.
    pub(crate) fn publish_first_frame(
        &mut self,
        span_id: TimelineHoverPrepareSpanId,
        span: DecodeDependencySpan,
        lease: VideoFrameLease,
        actual_pts: TrackTimestamp,
    ) -> TimelineHoverApproximatePreviewPublishOutcome {
        if self
            .frame
            .as_ref()
            .is_some_and(|frame| frame.span_id == span_id)
        {
            trace!(
                ?span_id,
                ?actual_pts,
                "Dropping duplicate approximate hover preview frame for the same span"
            );
            drop(lease);
            return TimelineHoverApproximatePreviewPublishOutcome::AlreadyPublishedForSpan;
        }

        let replaced_previous_span = self.frame.replace(TimelineHoverApproximatePreviewFrame {
            span_id,
            span,
            lease,
            actual_pts,
        });

        let outcome = if replaced_previous_span.is_some() {
            TimelineHoverApproximatePreviewPublishOutcome::ReplacedPreviousSpan
        } else {
            TimelineHoverApproximatePreviewPublishOutcome::Published
        };

        trace!(
            ?span_id,
            ?actual_pts,
            ?outcome,
            "Published approximate hover preview keyframe"
        );
        outcome
    }

    /// Возвращает clone lease-а, если active span имеет тот же decode-safe anchor.
    #[must_use]
    pub(crate) fn borrow_for_span(
        &self,
        active_span: DecodeDependencySpan,
    ) -> Option<TimelineHoverApproximatePreviewBorrow> {
        let frame = self.frame.as_ref()?;
        if !frame.span.matches_approximate_preview_anchor(active_span) {
            return None;
        }

        Some(TimelineHoverApproximatePreviewBorrow::new(
            frame.lease.clone(),
            frame.actual_pts,
        ))
    }

    /// Освобождает slot-owned lease через обычный drop path.
    pub(crate) fn clear(
        &mut self,
        reason: TimelineHoverApproximatePreviewClearReason,
    ) -> TimelineHoverApproximatePreviewClearOutcome {
        let Some(frame) = self.frame.take() else {
            trace!(?reason, "Approximate hover preview slot already empty");
            return TimelineHoverApproximatePreviewClearOutcome::AlreadyEmpty;
        };

        trace!(
            ?reason,
            span_id = ?frame.span_id,
            actual_pts = ?frame.actual_pts,
            "Clearing approximate hover preview slot"
        );
        drop(frame);
        TimelineHoverApproximatePreviewClearOutcome::Cleared
    }

    /// Проверяет, удерживает ли slot какой-либо keyframe lease.
    #[must_use]
    pub(crate) const fn is_occupied(&self) -> bool {
        self.frame.is_some()
    }
}

/// Owned payload visual approximate слота.
struct TimelineHoverApproximatePreviewFrame {
    /// Span, в котором keyframe был первым decoded frame-ом.
    span_id: TimelineHoverPrepareSpanId,

    /// Resolved dependency span; match идёт по context + decode_safe_start.
    span: DecodeDependencySpan,

    /// Lease удерживает decoder/provider frame pool slot до drop-а.
    lease: VideoFrameLease,

    /// PTS keyframe-а, который показывается как approximate preview.
    actual_pts: TrackTimestamp,
}
