use std::time::Duration;

use frame_server_core::{
    ScrubDiagnosticsRecorder, ScrubDiagnosticsSnapshot, ScrubHoverDependencySpanIncompleteReason,
    ScrubHoverDependencySpanOutcome, ScrubHoverDependencySpanProgress, ScrubHoverNetworkState,
    ScrubRequestKind, TimelineHoverPrepareSessionEndReleaseOutcome,
};

use super::timeline_hover_leave_grace::TimelineHoverLeaveGraceReleaseReason;
use crate::frame_prepare::{
    TimelineHoverPreviewLoadState, TimelineHoverPreviewRenderDiagnosticsSnapshot,
    TimelineHoverPreviewUpdateOutcome,
};
use crate::frame_server_budget::FrameServerHoverBudgetDiagnosticsSnapshot;
use crate::timeline_hover_network::TimelineHoverNetworkOpenDiagnosticsSnapshot;
use crate::timeline_hover_prepare::{
    TimelineHoverPrepareCancellationReason, TimelineHoverPrepareCompletionOutcome,
    TimelineHoverPrepareControllerTransition, TimelineHoverPrepareExecutorNoOpReason,
    TimelineHoverPrepareExecutorOutcome, TimelineHoverPrepareIncompleteReason,
    TimelineHoverPreparePressure, TimelineHoverPrepareSpanDiagnostics,
};

/// App-owned diagnostics boundary для UI-only частей frame-server workflow.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct AppFrameServerDiagnosticsRecorder {
    /// Нейтральные S29A counters, которые можно показать рядом с player snapshot.
    scrub: ScrubDiagnosticsRecorder,

    /// Timeline leave-grace counters остаются в app-egui, где живёт UX timer.
    hover_leave_grace: TimelineHoverLeaveGraceDiagnosticsRecorder,

    /// Visual-preview counters остаются в app-egui, где живёт overlay/materialization.
    hover_preview: TimelineHoverPreviewDiagnosticsRecorder,
}

/// Read-only snapshot для telemetry panel без доступа к private AppState fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct AppFrameServerDiagnosticsSnapshot {
    /// Нейтральный scrub/hover snapshot, накопленный app-owned hover executor-ом.
    pub(super) scrub: ScrubDiagnosticsSnapshot,

    /// UX lifetime diagnostics для hover leave grace.
    pub(super) hover_leave_grace: TimelineHoverLeaveGraceDiagnosticsSnapshot,

    /// Visual-preview diagnostics, включая disabled-by-config suppression.
    pub(super) hover_preview: TimelineHoverPreviewDiagnosticsSnapshot,

    /// Read-only state network open controller-а без URL/source/target history.
    pub(super) network_open: TimelineHoverNetworkOpenDiagnosticsSnapshot,

    /// Active-backend hover budget diagnostics, если backend дал read-only provider.
    pub(super) hover_budget: Option<FrameServerHoverBudgetDiagnosticsSnapshot>,
}

/// Snapshot UX lifetime grace-а: это retention lifetime, не decode coverage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct TimelineHoverLeaveGraceDiagnosticsSnapshot {
    /// Текущий committed `hover_leave_grace_ms`.
    pub(super) configured_grace: Duration,

    /// Есть ли pending release deadline прямо сейчас.
    pub(super) pending: bool,

    /// Сколько раз leave начал ненулевой grace.
    pub(super) started_count: u64,

    /// Сколько раз re-enter отменил pending release до expiry.
    pub(super) reentered_before_expiry_count: u64,

    /// Сколько immediate releases случилось из-за `hover_leave_grace_ms = 0`.
    pub(super) zero_grace_immediate_release_count: u64,

    /// Сколько releases случилось после expiry.
    pub(super) expired_release_count: u64,

    /// Сколько releases случилось из-за действия вне timeline.
    pub(super) non_timeline_cancel_release_count: u64,

    /// Последняя typed причина release-а, если release уже был.
    pub(super) latest_release_reason: Option<TimelineHoverLeaveGraceReleaseReason>,

    /// Последний outcome cleanup-а с раздельными primary/recent counters.
    pub(super) latest_release_outcome: Option<TimelineHoverPrepareSessionEndReleaseOutcome>,
}

/// Snapshot visual preview-а: только counters/latest state, без истории target-ов.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct TimelineHoverPreviewDiagnosticsSnapshot {
    /// Текущее committed значение `hover_preview_enabled`.
    pub(super) visual_preview_enabled: bool,

    /// Текущее состояние render overlay-а, owned by preview module.
    pub(super) render_state: TimelineHoverPreviewRenderDiagnosticsSnapshot,

    /// Сколько invisible prepare intents пришли, пока visual preview был выключен.
    pub(super) disabled_by_config_count: u64,

    /// Сколько раз preview update пытался borrow/materialize shared prepared frame.
    pub(super) update_count: u64,

    /// Сколько раз preview показывал preview-only loading state.
    pub(super) loading_count: u64,

    /// Сколько раз preview получил готовый materialized borrow.
    pub(super) ready_count: u64,

    /// Сколько раз preview сохранил previous ready frame из-за Busy.
    pub(super) busy_kept_last_ready_count: u64,

    /// Сколько раз preview очистился или не смог materialize frame.
    pub(super) unavailable_count: u64,

    /// Сколько раз visual preview state был очищен без нового borrow outcome.
    pub(super) clear_count: u64,

    /// Последний typed update outcome для compact telemetry mapping.
    pub(super) latest_update_outcome: Option<TimelineHoverPreviewUpdateOutcome>,

    /// Последний preview-only load state; `NetworkOpening` не является inline status.
    pub(super) latest_load_state: TimelineHoverPreviewLoadState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TimelineHoverLeaveGraceDiagnosticsRecorder {
    started_count: u64,
    reentered_before_expiry_count: u64,
    zero_grace_immediate_release_count: u64,
    expired_release_count: u64,
    non_timeline_cancel_release_count: u64,
    latest_release_reason: Option<TimelineHoverLeaveGraceReleaseReason>,
    latest_release_outcome: Option<TimelineHoverPrepareSessionEndReleaseOutcome>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TimelineHoverPreviewDiagnosticsRecorder {
    disabled_by_config_count: u64,
    update_count: u64,
    loading_count: u64,
    ready_count: u64,
    busy_kept_last_ready_count: u64,
    unavailable_count: u64,
    clear_count: u64,
    latest_update_outcome: Option<TimelineHoverPreviewUpdateOutcome>,
    latest_load_state: TimelineHoverPreviewLoadState,
}

impl AppFrameServerDiagnosticsRecorder {
    /// Собирает read-only snapshot из recorder-а и текущих owner snapshots.
    #[must_use]
    pub(super) fn snapshot(
        &self,
        configured_leave_grace: Duration,
        hover_leave_grace_pending: bool,
        visual_preview_enabled: bool,
        preview_render_state: TimelineHoverPreviewRenderDiagnosticsSnapshot,
        network_open: TimelineHoverNetworkOpenDiagnosticsSnapshot,
        hover_budget: Option<FrameServerHoverBudgetDiagnosticsSnapshot>,
    ) -> AppFrameServerDiagnosticsSnapshot {
        AppFrameServerDiagnosticsSnapshot {
            scrub: self.scrub.snapshot(),
            hover_leave_grace: self
                .hover_leave_grace
                .snapshot(configured_leave_grace, hover_leave_grace_pending),
            hover_preview: self
                .hover_preview
                .snapshot(visual_preview_enabled, preview_render_state),
            network_open,
            hover_budget,
        }
    }

    /// Записывает app-owned hover prepare outcome в neutral S29A counters.
    pub(super) fn record_hover_prepare_outcome(
        &mut self,
        transition: TimelineHoverPrepareControllerTransition,
        executor_outcome: TimelineHoverPrepareExecutorOutcome,
        completion_outcome: TimelineHoverPrepareCompletionOutcome,
        preview_load_state: TimelineHoverPreviewLoadState,
    ) {
        self.scrub
            .record_request_accepted(ScrubRequestKind::TimelineHoverPrepareWindow);
        self.record_hover_prepare_transition(transition);
        self.record_hover_prepare_executor_outcome(executor_outcome);
        self.record_hover_prepare_completion_outcome(completion_outcome);
        self.hover_preview.record_load_state(preview_load_state);
    }

    /// Запоминает, что visual preview был выключен, но invisible prepare не отключался.
    pub(super) fn record_hover_preview_disabled_by_config(&mut self) {
        self.hover_preview.record_disabled_by_config();
    }

    /// Записывает outcome visual preview borrow/materialization.
    pub(super) fn record_hover_preview_update(
        &mut self,
        load_state: TimelineHoverPreviewLoadState,
        update_outcome: TimelineHoverPreviewUpdateOutcome,
    ) {
        self.hover_preview.record_update(load_state, update_outcome);
    }

    /// Записывает очистку preview state без смешения с invisible prepare.
    pub(super) fn record_hover_preview_cleared(&mut self) {
        self.hover_preview.record_clear();
    }

    /// Записывает старт ненулевого hover leave grace.
    pub(super) fn record_hover_leave_grace_started(&mut self) {
        self.hover_leave_grace.started_count =
            self.hover_leave_grace.started_count.saturating_add(1);
    }

    /// Записывает re-enter, который сохранил prepared entries до expiry.
    pub(super) fn record_hover_leave_grace_reentered_before_expiry(&mut self) {
        self.hover_leave_grace.reentered_before_expiry_count = self
            .hover_leave_grace
            .reentered_before_expiry_count
            .saturating_add(1);
    }

    /// Записывает release hover-owned entries после завершения hover session.
    pub(super) fn record_hover_leave_grace_release(
        &mut self,
        reason: TimelineHoverLeaveGraceReleaseReason,
        outcome: TimelineHoverPrepareSessionEndReleaseOutcome,
    ) {
        match reason {
            TimelineHoverLeaveGraceReleaseReason::ImmediateTimelineLeave => {
                self.hover_leave_grace.zero_grace_immediate_release_count = self
                    .hover_leave_grace
                    .zero_grace_immediate_release_count
                    .saturating_add(1);
            }
            TimelineHoverLeaveGraceReleaseReason::LeaveGraceExpired => {
                self.hover_leave_grace.expired_release_count = self
                    .hover_leave_grace
                    .expired_release_count
                    .saturating_add(1);
            }
            TimelineHoverLeaveGraceReleaseReason::NonTimelineAction => {
                self.hover_leave_grace.non_timeline_cancel_release_count = self
                    .hover_leave_grace
                    .non_timeline_cancel_release_count
                    .saturating_add(1);
            }
        }

        self.hover_leave_grace.latest_release_reason = Some(reason);
        self.hover_leave_grace.latest_release_outcome = Some(outcome);
    }

    fn record_hover_prepare_transition(
        &mut self,
        transition: TimelineHoverPrepareControllerTransition,
    ) {
        match transition {
            TimelineHoverPrepareControllerTransition::Started => {}
            TimelineHoverPrepareControllerTransition::RetargetedWithinSpan => {
                self.scrub.record_hover_dependency_span_outcome(
                    ScrubHoverDependencySpanOutcome::SameSpanRetarget,
                );
            }
            TimelineHoverPrepareControllerTransition::ExtendedForward => {
                self.scrub.record_hover_dependency_span_outcome(
                    ScrubHoverDependencySpanOutcome::SpanTailExtended,
                );
            }
            TimelineHoverPrepareControllerTransition::Superseded { reason, .. } => {
                let outcome = match reason {
                    TimelineHoverPrepareCancellationReason::EarlierDecodeSafeStartRequired => {
                        ScrubHoverDependencySpanOutcome::RestartedForEarlierDecodeSafeStart
                    }
                    TimelineHoverPrepareCancellationReason::IncompatibleTargetContext => {
                        ScrubHoverDependencySpanOutcome::SupersededByIncompatibleContext
                    }
                    TimelineHoverPrepareCancellationReason::TimelineLeft
                    | TimelineHoverPrepareCancellationReason::SourceSwitched
                    | TimelineHoverPrepareCancellationReason::BackendSwitched
                    | TimelineHoverPrepareCancellationReason::SettingsRebuilt
                    | TimelineHoverPrepareCancellationReason::LiveScrubStarted
                    | TimelineHoverPrepareCancellationReason::ForwardExtensionTooFar => {
                        ScrubHoverDependencySpanOutcome::Incomplete(
                            ScrubHoverDependencySpanIncompleteReason::StaleGeneration,
                        )
                    }
                };
                self.scrub.record_hover_dependency_span_outcome(outcome);
            }
        }
    }

    fn record_hover_prepare_executor_outcome(
        &mut self,
        executor_outcome: TimelineHoverPrepareExecutorOutcome,
    ) {
        match executor_outcome {
            TimelineHoverPrepareExecutorOutcome::PreparedHit { diagnostics, .. } => {
                self.record_hover_span_progress(diagnostics, 1);
            }
            TimelineHoverPrepareExecutorOutcome::WorkingSetHit { .. } => {
                self.scrub.record_working_set_hit();
            }
            TimelineHoverPrepareExecutorOutcome::IncompleteSpan {
                reason,
                diagnostics,
            } => {
                self.record_hover_span_progress(diagnostics, 0);
                self.scrub.record_hover_dependency_span_outcome(
                    ScrubHoverDependencySpanOutcome::Incomplete(
                        hover_incomplete_reason_from_executor_incomplete(reason),
                    ),
                );
            }
            TimelineHoverPrepareExecutorOutcome::NoOp { reason } => {
                self.record_hover_prepare_no_op(reason);
            }
            TimelineHoverPrepareExecutorOutcome::Pressure { pressure } => {
                self.record_hover_prepare_pressure(pressure);
            }
        }
    }

    fn record_hover_prepare_completion_outcome(
        &mut self,
        completion_outcome: TimelineHoverPrepareCompletionOutcome,
    ) {
        match completion_outcome {
            TimelineHoverPrepareCompletionOutcome::AcceptedExactPreparedHit { .. } => {
                self.scrub
                    .record_request_completed(ScrubRequestKind::TimelineHoverPrepareWindow);
                self.scrub.record_hover_dependency_span_outcome(
                    ScrubHoverDependencySpanOutcome::Resolved,
                );
            }
            TimelineHoverPrepareCompletionOutcome::AcceptedWorkingSetHit { .. } => {
                self.scrub
                    .record_request_completed(ScrubRequestKind::TimelineHoverPrepareWindow);
                self.scrub.record_hover_dependency_span_outcome(
                    ScrubHoverDependencySpanOutcome::Resolved,
                );
            }
            TimelineHoverPrepareCompletionOutcome::RejectedStaleSpan { .. }
            | TimelineHoverPrepareCompletionOutcome::RejectedStaleTarget { .. } => {
                self.scrub.record_hover_dependency_span_outcome(
                    ScrubHoverDependencySpanOutcome::Incomplete(
                        ScrubHoverDependencySpanIncompleteReason::StaleGeneration,
                    ),
                );
            }
            TimelineHoverPrepareCompletionOutcome::RejectedApproximate { .. }
            | TimelineHoverPrepareCompletionOutcome::RejectedTiming { .. } => {
                self.scrub.record_hover_dependency_span_outcome(
                    ScrubHoverDependencySpanOutcome::Incomplete(
                        ScrubHoverDependencySpanIncompleteReason::ResolveFailed,
                    ),
                );
            }
            TimelineHoverPrepareCompletionOutcome::NoPreparedHit => {}
        }
    }

    fn record_hover_prepare_no_op(&mut self, reason: TimelineHoverPrepareExecutorNoOpReason) {
        let incomplete_reason = match reason {
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackExecutorUnavailable {
                ..
            } => {
                self.scrub.record_working_set_miss();
                ScrubHoverDependencySpanIncompleteReason::SourceUnavailable
            }
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceMissing { .. } => {
                self.scrub.record_working_set_miss();
                ScrubHoverDependencySpanIncompleteReason::SourceUnavailable
            }
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceUnsupported { .. } => {
                self.scrub.record_working_set_miss();
                ScrubHoverDependencySpanIncompleteReason::SeekUnsupported
            }
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceOpenFailed { .. } => {
                self.scrub.record_working_set_miss();
                ScrubHoverDependencySpanIncompleteReason::SourceUnavailable
            }
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceReadyDecodeNotWired {
                ..
            } => {
                self.scrub.record_working_set_miss();
                ScrubHoverDependencySpanIncompleteReason::DecodeExecutionNotWired
            }
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackNetworkSourceOpening {
                ..
            } => {
                self.scrub.record_working_set_miss();
                self.scrub
                    .record_hover_network_state(ScrubHoverNetworkState::Opening);
                ScrubHoverDependencySpanIncompleteReason::NetworkOpening
            }
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackNetworkSourceThrottled {
                ..
            } => {
                self.scrub.record_working_set_miss();
                self.scrub
                    .record_hover_network_state(ScrubHoverNetworkState::Throttled);
                ScrubHoverDependencySpanIncompleteReason::NetworkThrottled
            }
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackNetworkSourceFailedNoRetry {
                ..
            } => {
                self.scrub.record_working_set_miss();
                self.scrub
                    .record_hover_network_state(ScrubHoverNetworkState::FailedTargetHeld);
                ScrubHoverDependencySpanIncompleteReason::NetworkFailedNoRetry
            }
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackDependencySpanSeekUnsupported => {
                ScrubHoverDependencySpanIncompleteReason::SeekUnsupported
            }
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackDependencySpanSeekUnavailable => {
                ScrubHoverDependencySpanIncompleteReason::SeekUnavailable
            }
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackDependencySpanResolveFailed => {
                ScrubHoverDependencySpanIncompleteReason::ResolveFailed
            }
            TimelineHoverPrepareExecutorNoOpReason::PausedStoppedExecutorNotWired { .. } => {
                self.scrub.record_working_set_miss();
                ScrubHoverDependencySpanIncompleteReason::ResolverNotWired
            }
            TimelineHoverPrepareExecutorNoOpReason::PausedStoppedDependencySpanResolverNotWired => {
                ScrubHoverDependencySpanIncompleteReason::ResolverNotWired
            }
            TimelineHoverPrepareExecutorNoOpReason::ResumePendingNoSpareCapacity => {
                ScrubHoverDependencySpanIncompleteReason::ResourcePressure
            }
            TimelineHoverPrepareExecutorNoOpReason::WorkingSetMiss { .. } => {
                self.scrub.record_working_set_miss();
                ScrubHoverDependencySpanIncompleteReason::DecodeExecutionNotWired
            }
            TimelineHoverPrepareExecutorNoOpReason::WorkingSetTimingRejected => {
                ScrubHoverDependencySpanIncompleteReason::ResolveFailed
            }
            TimelineHoverPrepareExecutorNoOpReason::LiveScrubSuspended => {
                ScrubHoverDependencySpanIncompleteReason::ResourcePressure
            }
        };

        self.scrub.record_hover_dependency_span_outcome(
            ScrubHoverDependencySpanOutcome::Incomplete(incomplete_reason),
        );
    }

    fn record_hover_prepare_pressure(&mut self, _pressure: TimelineHoverPreparePressure) {
        self.scrub.record_hover_dependency_span_outcome(
            ScrubHoverDependencySpanOutcome::Incomplete(
                ScrubHoverDependencySpanIncompleteReason::ResourcePressure,
            ),
        );
    }

    fn record_hover_span_progress(
        &mut self,
        diagnostics: TimelineHoverPrepareSpanDiagnostics,
        prepared_targets_produced: u64,
    ) {
        self.scrub
            .record_hover_dependency_span_progress(ScrubHoverDependencySpanProgress {
                packets_decoded_to_target: u64::from(diagnostics.decoded_packets()),
                frames_decoded_to_target: u64::from(diagnostics.decoded_frames()),
                post_target_reorder_drain_frames: u64::from(
                    diagnostics.post_target_reorder_drain_frames(),
                ),
                prepared_targets_produced,
            });
    }
}

impl TimelineHoverLeaveGraceDiagnosticsRecorder {
    fn snapshot(
        self,
        configured_grace: Duration,
        pending: bool,
    ) -> TimelineHoverLeaveGraceDiagnosticsSnapshot {
        TimelineHoverLeaveGraceDiagnosticsSnapshot {
            configured_grace,
            pending,
            started_count: self.started_count,
            reentered_before_expiry_count: self.reentered_before_expiry_count,
            zero_grace_immediate_release_count: self.zero_grace_immediate_release_count,
            expired_release_count: self.expired_release_count,
            non_timeline_cancel_release_count: self.non_timeline_cancel_release_count,
            latest_release_reason: self.latest_release_reason,
            latest_release_outcome: self.latest_release_outcome,
        }
    }
}

impl TimelineHoverPreviewDiagnosticsRecorder {
    fn snapshot(
        self,
        visual_preview_enabled: bool,
        render_state: TimelineHoverPreviewRenderDiagnosticsSnapshot,
    ) -> TimelineHoverPreviewDiagnosticsSnapshot {
        TimelineHoverPreviewDiagnosticsSnapshot {
            visual_preview_enabled,
            render_state,
            disabled_by_config_count: self.disabled_by_config_count,
            update_count: self.update_count,
            loading_count: self.loading_count,
            ready_count: self.ready_count,
            busy_kept_last_ready_count: self.busy_kept_last_ready_count,
            unavailable_count: self.unavailable_count,
            clear_count: self.clear_count,
            latest_update_outcome: self.latest_update_outcome,
            latest_load_state: self.latest_load_state,
        }
    }

    fn record_disabled_by_config(&mut self) {
        self.disabled_by_config_count = self.disabled_by_config_count.saturating_add(1);
    }

    fn record_load_state(&mut self, load_state: TimelineHoverPreviewLoadState) {
        self.latest_load_state = load_state;
    }

    fn record_clear(&mut self) {
        self.clear_count = self.clear_count.saturating_add(1);
        self.latest_load_state = TimelineHoverPreviewLoadState::Idle;
    }

    fn record_update(
        &mut self,
        load_state: TimelineHoverPreviewLoadState,
        update_outcome: TimelineHoverPreviewUpdateOutcome,
    ) {
        self.update_count = self.update_count.saturating_add(1);
        self.latest_load_state = load_state;
        self.latest_update_outcome = Some(update_outcome);

        match update_outcome {
            TimelineHoverPreviewUpdateOutcome::Loading => {
                self.loading_count = self.loading_count.saturating_add(1);
            }
            TimelineHoverPreviewUpdateOutcome::Ready
            | TimelineHoverPreviewUpdateOutcome::ApproximateReady => {
                self.ready_count = self.ready_count.saturating_add(1);
            }
            TimelineHoverPreviewUpdateOutcome::BusyKeptLastReady => {
                self.busy_kept_last_ready_count = self.busy_kept_last_ready_count.saturating_add(1);
            }
            TimelineHoverPreviewUpdateOutcome::BusyEmpty
            | TimelineHoverPreviewUpdateOutcome::MissingMaterializer
            | TimelineHoverPreviewUpdateOutcome::WorkingSetMiss
            | TimelineHoverPreviewUpdateOutcome::TimingRejected
            | TimelineHoverPreviewUpdateOutcome::Missing
            | TimelineHoverPreviewUpdateOutcome::Unsupported
            | TimelineHoverPreviewUpdateOutcome::Error => {
                self.unavailable_count = self.unavailable_count.saturating_add(1);
            }
        }
    }
}

const fn hover_incomplete_reason_from_executor_incomplete(
    reason: TimelineHoverPrepareIncompleteReason,
) -> ScrubHoverDependencySpanIncompleteReason {
    match reason {
        TimelineHoverPrepareIncompleteReason::DecodeBudgetExhausted
        | TimelineHoverPrepareIncompleteReason::ResourceBudgetExhausted => {
            ScrubHoverDependencySpanIncompleteReason::ResourcePressure
        }
        TimelineHoverPrepareIncompleteReason::EndOfStreamBeforeTarget => {
            ScrubHoverDependencySpanIncompleteReason::EndOfStreamBeforeTarget
        }
        TimelineHoverPrepareIncompleteReason::DecodeExecutionNotWired => {
            ScrubHoverDependencySpanIncompleteReason::DecodeExecutionNotWired
        }
    }
}
