// S18 намеренно добавляет boundary до S24 UI wiring, поэтому часть API пока
// используется только synthetic tests и будущим pointer/focus stream.
#![allow(dead_code)]

use std::cmp::Ordering;
use std::num::NonZeroUsize;

use frame_server_core::{
    BackendRevision, FrameExactnessPolicy, ScrubGenerationToken, ScrubTrackSelection,
    SourceRevision, TimelineHoverFrameBucket, TimelineHoverPrepareCapacityReconfigureOutcome,
    TimelineHoverPrepareFrameKey, TimelineHoverPrepareFrameLookupRequest,
    TimelineHoverPrepareLookupMissReason, TimelineHoverPrepareSessionEndReleaseOutcome,
    TimelineHoverPrepareSessionEndReleaseReason, TimelineHoverRecentSupersededBudget,
    TimelineHoverRecentSupersededReconfigureOutcome,
};
use media_core::{DemuxSeekRequest, MediaDemuxError, TrackTimestamp};
use player_core::{PlayerTimelineHoverPrepareBorrowOutcome, PlayerTimelineHoverPrepareHandoff};
use rustiplayer_config::{NetworkConfig, PlayerDemuxConfig, YoutubeConfig};
use tracing::trace;

use crate::timeline_hover_decode::{
    HoverSpanDecodeDrive, HoverSpanDecodeFailure, HoverSpanDecodeState, TimelineHoverDecodeSession,
    cancel_hover_span_decode, drive_hover_span_decode,
};
use crate::timeline_hover_network::{
    TimelineHoverNetworkOpenController, TimelineHoverNetworkOpenDiagnosticsSnapshot,
    TimelineHoverNetworkOpenOutcome,
};
use crate::timeline_hover_source::{
    TimelineHoverOpenFailedSourceKind, TimelineHoverOpenedSource, TimelineHoverSourceFactory,
    TimelineHoverSourceIdentity, TimelineHoverSourceOpenOutcome,
    TimelineHoverUnsupportedSourceKind,
};

/// Controller type, которым владеет `AppState`.
pub(crate) type AppTimelineHoverPrepareController =
    TimelineHoverPrepareController<AppTimelineHoverPrepareExecutor>;

/// Production executor: проверяет общий working set и владеет app-side hover source.
pub(crate) struct AppTimelineHoverPrepareExecutor {
    handoff: PlayerTimelineHoverPrepareHandoff,
    source_factory: TimelineHoverSourceFactory,
    network_open_controller: TimelineHoverNetworkOpenController,
    active_hover_source: Option<TimelineHoverOpenedSource>,
    decode_session: Option<TimelineHoverDecodeSession>,
    span_decode_state: Option<HoverSpanDecodeState>,
}

/// App-owned controller для будущего unified timeline hover/focus intent-а.
pub(crate) struct TimelineHoverPrepareController<Executor> {
    executor: Executor,
    active_span: Option<InFlightDecodeDependencySpan>,
    pending_unresolved_target: Option<InFlightTimelineHoverPrepareTarget>,
    next_span_id: TimelineHoverPrepareSpanId,
}

/// Hover target из UI/player snapshot; factual dependency span может резолвить executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimelineHoverPrepareTarget {
    context: TimelineHoverPrepareTargetContext,
    target_pts: TrackTimestamp,
    target_bucket: TimelineHoverFrameBucket,
    dependency_span: Option<DecodeDependencySpan>,
    playback_mode: TimelineHoverPreparePlaybackMode,
}

/// Guarded context, который должен совпасть для shared prepared hit-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TimelineHoverPrepareTargetContext {
    source_revision: SourceRevision,
    backend_revision: BackendRevision,
    track_selection: ScrubTrackSelection,
    hover_generation: ScrubGenerationToken,
    exactness_policy: FrameExactnessPolicy,
}

/// Playback режим влияет только на executor admission/degradation, не на commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPreparePlaybackMode {
    PausedOrStopped,
    ActivePlayback,
    ResumePendingAfterSeek { spare_capacity_available: bool },
    LiveScrubActive,
}

/// Ошибка synthetic target-а до запуска executor work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPrepareTargetError {
    DecodeSafeStartTrackMismatch {
        target: media_core::TrackId,
        decode_safe_start: media_core::TrackId,
    },
    DrainUntilTrackMismatch {
        target: media_core::TrackId,
        drain_until: media_core::TrackId,
    },
    DecodeSafeStartAfterTarget {
        decode_safe_start_pts: TrackTimestamp,
        target_pts: TrackTimestamp,
    },
    DrainUntilBeforeTarget {
        drain_until_pts: TrackTimestamp,
        target_pts: TrackTimestamp,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecodeDependencySpan {
    context: TimelineHoverPrepareTargetContext,
    decode_safe_start_pts: TrackTimestamp,
    drain_until_pts: TrackTimestamp,
    post_target_reorder_drain_frames: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimelineHoverPrepareSpanId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimelineHoverPrepareExecutorRequest {
    pub(crate) span_id: TimelineHoverPrepareSpanId,
    pub(crate) transition: TimelineHoverPrepareExecutorTransition,
    pub(crate) target: TimelineHoverPrepareTarget,
    pub(crate) span: DecodeDependencySpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPrepareExecutorTransition {
    Start,
    RetargetWithinSpan,
    ExtendForward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimelineHoverPrepareCancelRequest {
    span_id: TimelineHoverPrepareSpanId,
    latest_target: TimelineHoverPrepareTarget,
    reason: TimelineHoverPrepareCancellationReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPrepareCancellationReason {
    TimelineLeft,
    SourceSwitched,
    BackendSwitched,
    SettingsRebuilt,
    LiveScrubStarted,
    EarlierDecodeSafeStartRequired,
    IncompatibleTargetContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimelineHoverPrepareControllerOutcome {
    span_id: TimelineHoverPrepareSpanId,
    transition: TimelineHoverPrepareControllerTransition,
    executor_outcome: TimelineHoverPrepareExecutorOutcome,
    completion_outcome: TimelineHoverPrepareCompletionOutcome,
}

impl TimelineHoverPrepareControllerOutcome {
    #[must_use]
    pub(crate) const fn transition(self) -> TimelineHoverPrepareControllerTransition {
        self.transition
    }

    #[must_use]
    pub(crate) const fn executor_outcome(self) -> TimelineHoverPrepareExecutorOutcome {
        self.executor_outcome
    }

    #[must_use]
    pub(crate) const fn completion_outcome(self) -> TimelineHoverPrepareCompletionOutcome {
        self.completion_outcome
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPrepareControllerTransition {
    Started,
    RetargetedWithinSpan,
    ExtendedForward,
    Superseded {
        cancelled_span_id: TimelineHoverPrepareSpanId,
        reason: TimelineHoverPrepareCancellationReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPrepareExecutorOutcome {
    PreparedHit {
        actual_pts: TrackTimestamp,
        exactness: TimelineHoverPreparePreparedHitExactness,
        diagnostics: TimelineHoverPrepareSpanDiagnostics,
    },
    WorkingSetHit {
        actual_pts: TrackTimestamp,
    },
    IncompleteSpan {
        reason: TimelineHoverPrepareIncompleteReason,
        diagnostics: TimelineHoverPrepareSpanDiagnostics,
    },
    NoOp {
        reason: TimelineHoverPrepareExecutorNoOpReason,
    },
    Pressure {
        pressure: TimelineHoverPreparePressure,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPreparePreparedHitExactness {
    ExactTargetOrAfter,
    ApproximateKeyframe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimelineHoverPrepareSpanDiagnostics {
    decoded_packets: u32,
    decoded_frames: u32,
    post_target_reorder_drain_frames: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPrepareIncompleteReason {
    DecodeBudgetExhausted,
    ResourceBudgetExhausted,
    EndOfStreamBeforeTarget,
    DecodeExecutionNotWired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPrepareExecutorNoOpReason {
    ActivePlaybackExecutorUnavailable {
        lookup_miss: TimelineHoverPrepareLookupMissReason,
    },
    ActivePlaybackSourceMissing {
        lookup_miss: TimelineHoverPrepareLookupMissReason,
    },
    ActivePlaybackSourceUnsupported {
        lookup_miss: TimelineHoverPrepareLookupMissReason,
        source_kind: TimelineHoverUnsupportedSourceKind,
    },
    ActivePlaybackSourceOpenFailed {
        lookup_miss: TimelineHoverPrepareLookupMissReason,
        source_kind: TimelineHoverOpenFailedSourceKind,
    },
    ActivePlaybackSourceReadyDecodeNotWired {
        lookup_miss: TimelineHoverPrepareLookupMissReason,
    },
    ActivePlaybackNetworkSourceOpening {
        lookup_miss: TimelineHoverPrepareLookupMissReason,
    },
    ActivePlaybackNetworkSourceThrottled {
        lookup_miss: TimelineHoverPrepareLookupMissReason,
    },
    ActivePlaybackNetworkSourceFailedNoRetry {
        lookup_miss: TimelineHoverPrepareLookupMissReason,
    },
    ActivePlaybackDependencySpanSeekUnsupported,
    ActivePlaybackDependencySpanSeekUnavailable,
    ActivePlaybackDependencySpanResolveFailed,
    PausedStoppedExecutorNotWired {
        lookup_miss: TimelineHoverPrepareLookupMissReason,
    },
    PausedStoppedDependencySpanResolverNotWired,
    ResumePendingNoSpareCapacity,
    WorkingSetMiss {
        lookup_miss: TimelineHoverPrepareLookupMissReason,
    },
    WorkingSetTimingRejected,
    LiveScrubSuspended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPreparePressure {
    ProviderBudgetExhausted,
    DecoderBackpressure,
    HostUploadBackpressure,
    ResourceBusy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverDependencySpanResolutionOutcome {
    Resolved(DecodeDependencySpan),
    NoOp {
        reason: TimelineHoverPrepareExecutorNoOpReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPrepareCompletionOutcome {
    AcceptedExactPreparedHit {
        span_id: TimelineHoverPrepareSpanId,
        actual_pts: TrackTimestamp,
        diagnostics: TimelineHoverPrepareSpanDiagnostics,
    },
    AcceptedWorkingSetHit {
        span_id: TimelineHoverPrepareSpanId,
        actual_pts: TrackTimestamp,
    },
    RejectedStaleSpan {
        completion_span_id: TimelineHoverPrepareSpanId,
        active_span_id: Option<TimelineHoverPrepareSpanId>,
    },
    RejectedStaleTarget {
        completion_span_id: TimelineHoverPrepareSpanId,
    },
    RejectedApproximate {
        completion_span_id: TimelineHoverPrepareSpanId,
        actual_pts: TrackTimestamp,
    },
    RejectedTiming {
        completion_span_id: TimelineHoverPrepareSpanId,
        actual_pts: TrackTimestamp,
        reason: TimelineHoverPreparePreparedHitTimingRejection,
    },
    NoPreparedHit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPreparePreparedHitTimingRejection {
    ActualPtsTrackMismatch {
        target: media_core::TrackId,
        actual: media_core::TrackId,
    },
    ActualPtsBeforeTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InFlightDecodeDependencySpan {
    span_id: TimelineHoverPrepareSpanId,
    span: DecodeDependencySpan,
    latest_target: TimelineHoverPrepareTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InFlightTimelineHoverPrepareTarget {
    span_id: TimelineHoverPrepareSpanId,
    latest_target: TimelineHoverPrepareTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeDependencySpanChange {
    Compatible,
    ForwardExtension,
    RequiresEarlierDecodeSafeStart,
    IncompatibleContext,
}

pub(crate) trait TimelineHoverPrepareExecutor {
    fn resolve_dependency_span(
        &mut self,
        target: TimelineHoverPrepareTarget,
    ) -> TimelineHoverDependencySpanResolutionOutcome {
        match target.dependency_span() {
            Some(span) => TimelineHoverDependencySpanResolutionOutcome::Resolved(span),
            None => {
                let reason =
                    TimelineHoverPrepareExecutorNoOpReason::PausedStoppedDependencySpanResolverNotWired;
                TimelineHoverDependencySpanResolutionOutcome::NoOp { reason }
            }
        }
    }

    fn prepare_exact_dependency_span(
        &mut self,
        request: TimelineHoverPrepareExecutorRequest,
    ) -> TimelineHoverPrepareExecutorOutcome;

    fn cancel_dependency_span(&mut self, request: TimelineHoverPrepareCancelRequest);
}

impl AppTimelineHoverPrepareExecutor {
    pub(crate) fn new(handoff: PlayerTimelineHoverPrepareHandoff) -> Self {
        Self::with_demux_config(handoff, PlayerDemuxConfig::default())
    }

    pub(crate) fn with_demux_config(
        handoff: PlayerTimelineHoverPrepareHandoff,
        demux_config: PlayerDemuxConfig,
    ) -> Self {
        Self::with_open_config(
            handoff,
            NetworkConfig::default(),
            YoutubeConfig::default(),
            demux_config,
            std::time::Duration::from_millis(300),
        )
    }

    pub(crate) fn with_open_config(
        handoff: PlayerTimelineHoverPrepareHandoff,
        network_config: NetworkConfig,
        youtube_config: YoutubeConfig,
        demux_config: PlayerDemuxConfig,
        network_hover_prepare_throttle: std::time::Duration,
    ) -> Self {
        Self {
            handoff,
            source_factory: TimelineHoverSourceFactory::with_open_config(
                network_config,
                youtube_config,
                demux_config,
            ),
            network_open_controller: TimelineHoverNetworkOpenController::new(
                network_hover_prepare_throttle,
            ),
            active_hover_source: None,
            decode_session: None,
            span_decode_state: None,
        }
    }

    /// Устанавливает/заменяет active-playback hover decode session после backend build.
    ///
    /// Замена session делает hover-owned leases старого provider-а невалидными,
    /// поэтому они освобождаются через существующий session-end boundary.
    pub(crate) fn set_decode_session(&mut self, session: Option<TimelineHoverDecodeSession>) {
        self.span_decode_state = None;
        if self.decode_session.take().is_some() {
            let release_outcome = self.handoff.release_hover_owned_entries_for_session_end(
                TimelineHoverPrepareSessionEndReleaseReason::SourceOrBackendSwitched,
            );
            trace!(
                total_entries_released = release_outcome.total_entries_released(),
                "Hover decode session replaced; hover-owned entries released"
            );
        }
        self.decode_session = session;
    }

    /// Provider активной decode session-и для выбора matching preview materializer-а.
    #[must_use]
    pub(crate) fn decode_session_resource_provider(
        &self,
    ) -> Option<&video_backend_api::PresentFrameResourceProviderHandle> {
        self.decode_session
            .as_ref()
            .map(TimelineHoverDecodeSession::resource_provider)
    }

    /// Остался ли незавершённый decode continuation у активного span-а.
    #[must_use]
    pub(crate) fn has_pending_span_decode_work(&self) -> bool {
        self.span_decode_state.is_some()
            && self
                .decode_session
                .as_ref()
                .is_some_and(|session| !session.is_failed())
    }

    fn update_open_config(
        &mut self,
        network_config: NetworkConfig,
        youtube_config: YoutubeConfig,
        demux_config: PlayerDemuxConfig,
        network_hover_prepare_throttle: std::time::Duration,
    ) {
        // Config влияет на новые opens, поэтому старый hover source и pending
        // network job нельзя считать актуальными после config commit-а.
        self.reset_span_decode_for_source_loss();
        self.active_hover_source = None;
        self.network_open_controller.invalidate_source_context();
        self.network_open_controller
            .update_inter_start_throttle(network_hover_prepare_throttle);
        self.source_factory
            .update_open_config(network_config, youtube_config, demux_config);
    }

    /// Decode continuation привязан к demux-позиции активного hover source-а,
    /// поэтому потеря source-а обязана сбрасывать span decode state.
    fn reset_span_decode_for_source_loss(&mut self) {
        if let Some(session) = self.decode_session.as_mut() {
            cancel_hover_span_decode(session, &mut self.span_decode_state);
        } else {
            self.span_decode_state = None;
        }
    }

    fn update_network_hover_prepare_throttle(
        &mut self,
        network_hover_prepare_throttle: std::time::Duration,
    ) {
        self.network_open_controller
            .update_inter_start_throttle(network_hover_prepare_throttle);
    }

    fn set_hover_source(&mut self, source: TimelineHoverSourceIdentity) {
        self.reset_span_decode_for_source_loss();
        self.active_hover_source = None;
        self.network_open_controller.invalidate_source_context();
        self.source_factory.set_active_source(source);
    }

    fn invalidate_hover_source(&mut self) {
        self.reset_span_decode_for_source_loss();
        self.active_hover_source = None;
        self.network_open_controller.invalidate_source_context();
        self.source_factory.invalidate_active_source();
    }

    /// Borrow shared prepared entry для HoverPreview без promotion ownership transfer.
    #[must_use]
    pub(crate) fn borrow_prepared_frame(
        &self,
        request: TimelineHoverPrepareFrameLookupRequest,
    ) -> PlayerTimelineHoverPrepareBorrowOutcome {
        self.handoff.borrow_prepared_frame(request)
    }

    pub(crate) fn release_hover_owned_entries_for_session_end(
        &self,
        reason: TimelineHoverPrepareSessionEndReleaseReason,
    ) -> TimelineHoverPrepareSessionEndReleaseOutcome {
        self.handoff
            .release_hover_owned_entries_for_session_end(reason)
    }

    pub(crate) fn reconfigure_hover_prepare_primary_capacity(
        &self,
        new_capacity: NonZeroUsize,
        protected_key: Option<TimelineHoverPrepareFrameKey>,
    ) -> TimelineHoverPrepareCapacityReconfigureOutcome {
        self.handoff
            .reconfigure_hover_prepare_primary_capacity_protecting(new_capacity, protected_key)
    }

    pub(crate) fn reconfigure_recent_superseded_budget(
        &self,
        new_budget: TimelineHoverRecentSupersededBudget,
    ) -> TimelineHoverRecentSupersededReconfigureOutcome {
        self.handoff
            .reconfigure_recent_superseded_budget(new_budget)
    }

    /// Read-only network open state для telemetry без раскрытия controller storage.
    #[must_use]
    pub(crate) fn network_open_diagnostics_snapshot(
        &self,
    ) -> TimelineHoverNetworkOpenDiagnosticsSnapshot {
        self.network_open_controller.diagnostics_snapshot()
    }
}

impl TimelineHoverPrepareExecutor for AppTimelineHoverPrepareExecutor {
    fn resolve_dependency_span(
        &mut self,
        target: TimelineHoverPrepareTarget,
    ) -> TimelineHoverDependencySpanResolutionOutcome {
        if let Some(span) = target.dependency_span() {
            return TimelineHoverDependencySpanResolutionOutcome::Resolved(span);
        }

        match target.playback_mode() {
            TimelineHoverPreparePlaybackMode::ActivePlayback => {
                self.resolve_active_playback_dependency_span(target)
            }
            TimelineHoverPreparePlaybackMode::LiveScrubActive => {
                TimelineHoverDependencySpanResolutionOutcome::NoOp {
                    reason: TimelineHoverPrepareExecutorNoOpReason::LiveScrubSuspended,
                }
            }
            TimelineHoverPreparePlaybackMode::ResumePendingAfterSeek {
                spare_capacity_available: false,
            } => TimelineHoverDependencySpanResolutionOutcome::NoOp {
                reason: TimelineHoverPrepareExecutorNoOpReason::ResumePendingNoSpareCapacity,
            },
            TimelineHoverPreparePlaybackMode::PausedOrStopped
            | TimelineHoverPreparePlaybackMode::ResumePendingAfterSeek {
                spare_capacity_available: true,
            } => TimelineHoverDependencySpanResolutionOutcome::NoOp {
                reason:
                    TimelineHoverPrepareExecutorNoOpReason::PausedStoppedDependencySpanResolverNotWired,
            },
        }
    }

    fn prepare_exact_dependency_span(
        &mut self,
        request: TimelineHoverPrepareExecutorRequest,
    ) -> TimelineHoverPrepareExecutorOutcome {
        if matches!(
            request.target.playback_mode(),
            TimelineHoverPreparePlaybackMode::LiveScrubActive
        ) {
            return TimelineHoverPrepareExecutorOutcome::NoOp {
                reason: TimelineHoverPrepareExecutorNoOpReason::LiveScrubSuspended,
            };
        }

        if matches!(
            request.target.playback_mode(),
            TimelineHoverPreparePlaybackMode::ResumePendingAfterSeek {
                spare_capacity_available: false
            }
        ) {
            return TimelineHoverPrepareExecutorOutcome::NoOp {
                reason: TimelineHoverPrepareExecutorNoOpReason::ResumePendingNoSpareCapacity,
            };
        }

        match self
            .handoff
            .borrow_prepared_frame(request.target.lookup_request())
        {
            PlayerTimelineHoverPrepareBorrowOutcome::Borrowed(prepared_frame) => {
                TimelineHoverPrepareExecutorOutcome::WorkingSetHit {
                    actual_pts: prepared_frame.timing().actual_pts(),
                }
            }
            PlayerTimelineHoverPrepareBorrowOutcome::Miss(lookup_miss) => {
                if matches!(
                    request.target.playback_mode(),
                    TimelineHoverPreparePlaybackMode::ActivePlayback
                ) {
                    if self.active_hover_source.is_some() {
                        return self.execute_active_playback_span_decode(request, lookup_miss);
                    }

                    return TimelineHoverPrepareExecutorOutcome::NoOp {
                        reason: self
                            .active_playback_source_no_op_reason(request.target, lookup_miss),
                    };
                }

                TimelineHoverPrepareExecutorOutcome::NoOp {
                    reason: no_op_reason_for_missing_executor(
                        request.target.playback_mode(),
                        lookup_miss,
                    ),
                }
            }
            PlayerTimelineHoverPrepareBorrowOutcome::TimingRejected(_rejection) => {
                TimelineHoverPrepareExecutorOutcome::NoOp {
                    reason: TimelineHoverPrepareExecutorNoOpReason::WorkingSetTimingRejected,
                }
            }
        }
    }

    fn cancel_dependency_span(&mut self, _request: TimelineHoverPrepareCancelRequest) {
        self.network_open_controller.cancel_pending_target();
        if let Some(session) = self.decode_session.as_mut() {
            cancel_hover_span_decode(session, &mut self.span_decode_state);
        } else {
            self.span_decode_state = None;
        }
    }
}

impl AppTimelineHoverPrepareExecutor {
    /// Исполняет resolved dependency span через hover decode session.
    fn execute_active_playback_span_decode(
        &mut self,
        request: TimelineHoverPrepareExecutorRequest,
        lookup_miss: TimelineHoverPrepareLookupMissReason,
    ) -> TimelineHoverPrepareExecutorOutcome {
        let Some(session) = self.decode_session.as_mut() else {
            return TimelineHoverPrepareExecutorOutcome::NoOp {
                reason: TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackExecutorUnavailable {
                    lookup_miss,
                },
            };
        };
        if session.is_failed() {
            return TimelineHoverPrepareExecutorOutcome::NoOp {
                reason: TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackExecutorUnavailable {
                    lookup_miss,
                },
            };
        }
        let Some(hover_source) = self.active_hover_source.as_mut() else {
            return TimelineHoverPrepareExecutorOutcome::NoOp {
                reason: TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceMissing {
                    lookup_miss,
                },
            };
        };
        let Some(stream_context) = self.handoff.hover_stream_decode_context() else {
            return TimelineHoverPrepareExecutorOutcome::NoOp {
                reason: TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackExecutorUnavailable {
                    lookup_miss,
                },
            };
        };

        let drive = drive_hover_span_decode(
            session,
            &mut self.span_decode_state,
            hover_source,
            &self.handoff,
            &stream_context,
            &request,
        );

        match drive {
            HoverSpanDecodeDrive::PreparedHit {
                actual_pts,
                diagnostics,
            } => {
                self.span_decode_state = None;
                TimelineHoverPrepareExecutorOutcome::PreparedHit {
                    actual_pts,
                    exactness: TimelineHoverPreparePreparedHitExactness::ExactTargetOrAfter,
                    diagnostics,
                }
            }
            HoverSpanDecodeDrive::Incomplete {
                reason,
                diagnostics,
            } => TimelineHoverPrepareExecutorOutcome::IncompleteSpan {
                reason,
                diagnostics,
            },
            HoverSpanDecodeDrive::Pressure { pressure } => {
                TimelineHoverPrepareExecutorOutcome::Pressure { pressure }
            }
            HoverSpanDecodeDrive::Failed { failure } => {
                self.span_decode_state = None;
                let reason = match failure {
                    HoverSpanDecodeFailure::StreamConfigRejected
                    | HoverSpanDecodeFailure::DecoderFatal => {
                        TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackExecutorUnavailable {
                            lookup_miss,
                        }
                    }
                    HoverSpanDecodeFailure::DemuxReadFailed
                    | HoverSpanDecodeFailure::TracksChanged => {
                        // Старый hover source больше не может честно исполнять span.
                        self.invalidate_hover_source();
                        TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackDependencySpanResolveFailed
                    }
                };
                TimelineHoverPrepareExecutorOutcome::NoOp { reason }
            }
        }
    }

    fn resolve_active_playback_dependency_span(
        &mut self,
        target: TimelineHoverPrepareTarget,
    ) -> TimelineHoverDependencySpanResolutionOutcome {
        if self.source_factory.active_source_is_network() {
            let lookup_miss = TimelineHoverPrepareLookupMissReason::NoEntryForKey;
            let reason = if self.active_hover_source.is_some() {
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceReadyDecodeNotWired {
                    lookup_miss,
                }
            } else {
                self.active_playback_network_source_no_op_reason(target, lookup_miss)
            };
            return TimelineHoverDependencySpanResolutionOutcome::NoOp { reason };
        }

        let hover_source = match self.ensure_active_playback_hover_source(target) {
            Ok(hover_source) => hover_source,
            Err(reason) => {
                return TimelineHoverDependencySpanResolutionOutcome::NoOp { reason };
            }
        };

        let target_position = target.target_pts.to_media_time().as_duration();
        let seek_result = match hover_source
            .demuxer_mut()
            .seek_with_request(DemuxSeekRequest::decode_point_before(target_position))
        {
            Ok(seek_result) => seek_result,
            Err(error) => {
                return TimelineHoverDependencySpanResolutionOutcome::NoOp {
                    reason: dependency_span_seek_error_no_op_reason(&error),
                };
            }
        };

        let decode_safe_start_pts = seek_result
            .actual_track_timestamp
            .unwrap_or_else(|| target.pts_from_media_time(seek_result.actual_position));
        let resolved_target = match target.with_dependency_span(
            decode_safe_start_pts,
            target.target_pts,
            0,
        ) {
            Ok(resolved_target) => resolved_target,
            Err(_error) => {
                let reason =
                    TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackDependencySpanResolveFailed;
                return TimelineHoverDependencySpanResolutionOutcome::NoOp { reason };
            }
        };

        TimelineHoverDependencySpanResolutionOutcome::Resolved(
            resolved_target
                .dependency_span()
                .expect("resolved target must contain dependency span"),
        )
    }

    fn ensure_active_playback_hover_source(
        &mut self,
        target: TimelineHoverPrepareTarget,
    ) -> Result<&mut TimelineHoverOpenedSource, TimelineHoverPrepareExecutorNoOpReason> {
        if self.active_hover_source.is_none() {
            let lookup_miss = TimelineHoverPrepareLookupMissReason::NoEntryForKey;
            let reason = self.active_playback_source_no_op_reason(target, lookup_miss);
            if !matches!(
                reason,
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceReadyDecodeNotWired { .. }
            ) {
                return Err(reason);
            }
        }

        self.active_hover_source.as_mut().ok_or(
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackDependencySpanResolveFailed,
        )
    }

    fn active_playback_source_no_op_reason(
        &mut self,
        target: TimelineHoverPrepareTarget,
        lookup_miss: TimelineHoverPrepareLookupMissReason,
    ) -> TimelineHoverPrepareExecutorNoOpReason {
        if self.active_hover_source.is_some() {
            return TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceReadyDecodeNotWired {
                lookup_miss,
            };
        }

        if self.source_factory.active_source_is_network() {
            return self.active_playback_network_source_no_op_reason(target, lookup_miss);
        }

        match self.source_factory.open_active_source() {
            TimelineHoverSourceOpenOutcome::Opened(source) => {
                self.active_hover_source = Some(source);
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceReadyDecodeNotWired {
                    lookup_miss,
                }
            }
            TimelineHoverSourceOpenOutcome::MissingActiveSource => {
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceMissing { lookup_miss }
            }
            TimelineHoverSourceOpenOutcome::Unsupported { source_kind } => {
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceUnsupported {
                    lookup_miss,
                    source_kind,
                }
            }
            TimelineHoverSourceOpenOutcome::OpenFailed { source_kind } => {
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceOpenFailed {
                    lookup_miss,
                    source_kind,
                }
            }
        }
    }

    fn active_playback_network_source_no_op_reason(
        &mut self,
        target: TimelineHoverPrepareTarget,
        lookup_miss: TimelineHoverPrepareLookupMissReason,
    ) -> TimelineHoverPrepareExecutorNoOpReason {
        match self
            .network_open_controller
            .prepare_network_source(&self.source_factory, target)
        {
            TimelineHoverNetworkOpenOutcome::NonNetworkSource => {
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackExecutorUnavailable {
                    lookup_miss,
                }
            }
            TimelineHoverNetworkOpenOutcome::Opened(source) => {
                self.active_hover_source = Some(source);
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceReadyDecodeNotWired {
                    lookup_miss,
                }
            }
            TimelineHoverNetworkOpenOutcome::Opening => {
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackNetworkSourceOpening {
                    lookup_miss,
                }
            }
            TimelineHoverNetworkOpenOutcome::Throttled => {
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackNetworkSourceThrottled {
                    lookup_miss,
                }
            }
            TimelineHoverNetworkOpenOutcome::MissingActiveSource => {
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceMissing { lookup_miss }
            }
            TimelineHoverNetworkOpenOutcome::Unsupported { source_kind } => {
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceUnsupported {
                    lookup_miss,
                    source_kind,
                }
            }
            TimelineHoverNetworkOpenOutcome::OpenFailed { source_kind } => {
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceOpenFailed {
                    lookup_miss,
                    source_kind,
                }
            }
            TimelineHoverNetworkOpenOutcome::Disconnected => {
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackExecutorUnavailable {
                    lookup_miss,
                }
            }
            TimelineHoverNetworkOpenOutcome::FailedTargetHeld => {
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackNetworkSourceFailedNoRetry {
                    lookup_miss,
                }
            }
        }
    }
}

impl<Executor> TimelineHoverPrepareController<Executor> {
    pub(crate) fn new(executor: Executor) -> Self {
        Self {
            executor,
            active_span: None,
            pending_unresolved_target: None,
            next_span_id: TimelineHoverPrepareSpanId(1),
        }
    }

    pub(crate) fn executor(&self) -> &Executor {
        &self.executor
    }

    pub(crate) fn executor_mut(&mut self) -> &mut Executor {
        &mut self.executor
    }
}

impl TimelineHoverPrepareController<AppTimelineHoverPrepareExecutor> {
    pub(crate) fn update_hover_open_config(
        &mut self,
        network_config: NetworkConfig,
        youtube_config: YoutubeConfig,
        demux_config: PlayerDemuxConfig,
        network_hover_prepare_throttle: std::time::Duration,
    ) {
        self.executor.update_open_config(
            network_config,
            youtube_config,
            demux_config,
            network_hover_prepare_throttle,
        );
    }

    pub(crate) fn update_network_hover_prepare_throttle(
        &mut self,
        network_hover_prepare_throttle: std::time::Duration,
    ) {
        self.executor
            .update_network_hover_prepare_throttle(network_hover_prepare_throttle);
    }

    pub(crate) fn set_hover_source(&mut self, source: TimelineHoverSourceIdentity) {
        self.executor.set_hover_source(source);
    }

    pub(crate) fn invalidate_hover_source(&mut self) {
        self.executor.invalidate_hover_source();
    }

    pub(crate) fn release_hover_owned_entries_for_session_end(
        &self,
        reason: TimelineHoverPrepareSessionEndReleaseReason,
    ) -> TimelineHoverPrepareSessionEndReleaseOutcome {
        self.executor
            .release_hover_owned_entries_for_session_end(reason)
    }

    pub(crate) fn reconfigure_hover_prepare_primary_capacity(
        &self,
        new_capacity: NonZeroUsize,
        protected_key: Option<TimelineHoverPrepareFrameKey>,
    ) -> TimelineHoverPrepareCapacityReconfigureOutcome {
        self.executor
            .reconfigure_hover_prepare_primary_capacity(new_capacity, protected_key)
    }

    pub(crate) fn reconfigure_recent_superseded_budget(
        &self,
        new_budget: TimelineHoverRecentSupersededBudget,
    ) -> TimelineHoverRecentSupersededReconfigureOutcome {
        self.executor
            .reconfigure_recent_superseded_budget(new_budget)
    }
}

impl<Executor> TimelineHoverPrepareController<Executor>
where
    Executor: TimelineHoverPrepareExecutor,
{
    /// Latest target активного span-а для decode continuation pump-а.
    #[must_use]
    pub(crate) fn active_span_latest_target(&self) -> Option<TimelineHoverPrepareTarget> {
        self.active_span.map(|span| span.latest_target)
    }

    pub(crate) fn prepare_hover_target(
        &mut self,
        target: TimelineHoverPrepareTarget,
    ) -> TimelineHoverPrepareControllerOutcome {
        let requested_span = match target
            .dependency_span()
            .is_none()
            .then(|| self.active_span_extension_for_target(target))
            .flatten()
        {
            Some(span) => span,
            None => match self.executor.resolve_dependency_span(target) {
                TimelineHoverDependencySpanResolutionOutcome::Resolved(span) => span,
                TimelineHoverDependencySpanResolutionOutcome::NoOp { reason } => {
                    return self.unresolved_target_no_op_outcome(target, reason);
                }
            },
        };
        self.pending_unresolved_target = None;
        let (span_id, controller_transition, executor_transition) =
            self.apply_target_to_active_span(target, requested_span);
        let active_span = self
            .active_span
            .expect("prepare_hover_target must install an active span before executor call");
        let request = TimelineHoverPrepareExecutorRequest {
            span_id,
            transition: executor_transition,
            target,
            span: active_span.span,
        };
        let executor_outcome = self.executor.prepare_exact_dependency_span(request);
        let completion_outcome =
            self.completion_outcome_for_executor_result(span_id, target, executor_outcome);

        TimelineHoverPrepareControllerOutcome {
            span_id,
            transition: controller_transition,
            executor_outcome,
            completion_outcome,
        }
    }

    pub(crate) fn cancel_active_span(
        &mut self,
        reason: TimelineHoverPrepareCancellationReason,
    ) -> Option<TimelineHoverPrepareCancelRequest> {
        let request = if let Some(active_span) = self.active_span.take() {
            self.pending_unresolved_target = None;
            TimelineHoverPrepareCancelRequest {
                span_id: active_span.span_id,
                latest_target: active_span.latest_target,
                reason,
            }
        } else {
            let pending_target = self.pending_unresolved_target.take()?;
            TimelineHoverPrepareCancelRequest {
                span_id: pending_target.span_id,
                latest_target: pending_target.latest_target,
                reason,
            }
        };
        self.executor.cancel_dependency_span(request);
        Some(request)
    }

    fn unresolved_target_no_op_outcome(
        &mut self,
        target: TimelineHoverPrepareTarget,
        reason: TimelineHoverPrepareExecutorNoOpReason,
    ) -> TimelineHoverPrepareControllerOutcome {
        let (span_id, transition) = if unresolved_reason_keeps_pending_work(reason) {
            self.install_pending_unresolved_target(target)
        } else {
            self.pending_unresolved_target = None;
            (
                self.allocate_span_id(),
                TimelineHoverPrepareControllerTransition::Started,
            )
        };

        TimelineHoverPrepareControllerOutcome {
            span_id,
            transition,
            executor_outcome: TimelineHoverPrepareExecutorOutcome::NoOp { reason },
            completion_outcome: TimelineHoverPrepareCompletionOutcome::NoPreparedHit,
        }
    }

    fn install_pending_unresolved_target(
        &mut self,
        target: TimelineHoverPrepareTarget,
    ) -> (
        TimelineHoverPrepareSpanId,
        TimelineHoverPrepareControllerTransition,
    ) {
        if let Some(pending_target) = self.pending_unresolved_target {
            if pending_target.latest_target == target {
                return (
                    pending_target.span_id,
                    TimelineHoverPrepareControllerTransition::RetargetedWithinSpan,
                );
            }

            let cancellation = TimelineHoverPrepareCancelRequest {
                span_id: pending_target.span_id,
                latest_target: pending_target.latest_target,
                reason: TimelineHoverPrepareCancellationReason::IncompatibleTargetContext,
            };
            self.executor.cancel_dependency_span(cancellation);
            let span_id = self.allocate_span_id();
            self.pending_unresolved_target = Some(InFlightTimelineHoverPrepareTarget {
                span_id,
                latest_target: target,
            });
            return (
                span_id,
                TimelineHoverPrepareControllerTransition::Superseded {
                    cancelled_span_id: pending_target.span_id,
                    reason: TimelineHoverPrepareCancellationReason::IncompatibleTargetContext,
                },
            );
        }

        let span_id = self.allocate_span_id();
        self.pending_unresolved_target = Some(InFlightTimelineHoverPrepareTarget {
            span_id,
            latest_target: target,
        });
        (span_id, TimelineHoverPrepareControllerTransition::Started)
    }

    fn active_span_extension_for_target(
        &self,
        target: TimelineHoverPrepareTarget,
    ) -> Option<DecodeDependencySpan> {
        let active_span = self.active_span?;
        if active_span.span.context != target.context {
            return None;
        }
        if target
            .target_pts
            .cmp_timeline_position(active_span.span.decode_safe_start_pts)
            == Ordering::Less
        {
            return None;
        }

        let mut requested_span = active_span.span;
        if target
            .target_pts
            .cmp_timeline_position(requested_span.drain_until_pts)
            == Ordering::Greater
        {
            requested_span.drain_until_pts = target.target_pts;
        }
        Some(requested_span)
    }

    fn apply_target_to_active_span(
        &mut self,
        target: TimelineHoverPrepareTarget,
        requested_span: DecodeDependencySpan,
    ) -> (
        TimelineHoverPrepareSpanId,
        TimelineHoverPrepareControllerTransition,
        TimelineHoverPrepareExecutorTransition,
    ) {
        let Some(mut active_span) = self.active_span else {
            let span_id = self.allocate_span_id();
            self.active_span = Some(InFlightDecodeDependencySpan {
                span_id,
                span: requested_span,
                latest_target: target,
            });
            return (
                span_id,
                TimelineHoverPrepareControllerTransition::Started,
                TimelineHoverPrepareExecutorTransition::Start,
            );
        };

        match active_span.span.compare_requested_span(requested_span) {
            DecodeDependencySpanChange::Compatible => {
                active_span.latest_target = target;
                let span_id = active_span.span_id;
                self.active_span = Some(active_span);
                (
                    span_id,
                    TimelineHoverPrepareControllerTransition::RetargetedWithinSpan,
                    TimelineHoverPrepareExecutorTransition::RetargetWithinSpan,
                )
            }
            DecodeDependencySpanChange::ForwardExtension => {
                active_span.span = requested_span;
                active_span.latest_target = target;
                let span_id = active_span.span_id;
                self.active_span = Some(active_span);
                (
                    span_id,
                    TimelineHoverPrepareControllerTransition::ExtendedForward,
                    TimelineHoverPrepareExecutorTransition::ExtendForward,
                )
            }
            DecodeDependencySpanChange::RequiresEarlierDecodeSafeStart => self
                .supersede_active_span(
                    active_span,
                    target,
                    requested_span,
                    TimelineHoverPrepareCancellationReason::EarlierDecodeSafeStartRequired,
                ),
            DecodeDependencySpanChange::IncompatibleContext => self.supersede_active_span(
                active_span,
                target,
                requested_span,
                TimelineHoverPrepareCancellationReason::IncompatibleTargetContext,
            ),
        }
    }

    fn supersede_active_span(
        &mut self,
        active_span: InFlightDecodeDependencySpan,
        target: TimelineHoverPrepareTarget,
        requested_span: DecodeDependencySpan,
        reason: TimelineHoverPrepareCancellationReason,
    ) -> (
        TimelineHoverPrepareSpanId,
        TimelineHoverPrepareControllerTransition,
        TimelineHoverPrepareExecutorTransition,
    ) {
        let cancel_request = TimelineHoverPrepareCancelRequest {
            span_id: active_span.span_id,
            latest_target: active_span.latest_target,
            reason,
        };
        self.executor.cancel_dependency_span(cancel_request);

        let new_span_id = self.allocate_span_id();
        self.active_span = Some(InFlightDecodeDependencySpan {
            span_id: new_span_id,
            span: requested_span,
            latest_target: target,
        });
        (
            new_span_id,
            TimelineHoverPrepareControllerTransition::Superseded {
                cancelled_span_id: active_span.span_id,
                reason,
            },
            TimelineHoverPrepareExecutorTransition::Start,
        )
    }

    fn completion_outcome_for_executor_result(
        &self,
        span_id: TimelineHoverPrepareSpanId,
        target: TimelineHoverPrepareTarget,
        executor_outcome: TimelineHoverPrepareExecutorOutcome,
    ) -> TimelineHoverPrepareCompletionOutcome {
        match executor_outcome {
            TimelineHoverPrepareExecutorOutcome::PreparedHit {
                actual_pts,
                exactness,
                diagnostics,
            } => self.accept_prepared_hit(span_id, target, actual_pts, exactness, diagnostics),
            TimelineHoverPrepareExecutorOutcome::WorkingSetHit { actual_pts } => {
                if self.active_span_matches(span_id, target) {
                    TimelineHoverPrepareCompletionOutcome::AcceptedWorkingSetHit {
                        span_id,
                        actual_pts,
                    }
                } else {
                    self.stale_completion_outcome(span_id, target)
                }
            }
            TimelineHoverPrepareExecutorOutcome::IncompleteSpan { .. }
            | TimelineHoverPrepareExecutorOutcome::NoOp { .. }
            | TimelineHoverPrepareExecutorOutcome::Pressure { .. } => {
                TimelineHoverPrepareCompletionOutcome::NoPreparedHit
            }
        }
    }

    fn accept_prepared_hit(
        &self,
        span_id: TimelineHoverPrepareSpanId,
        target: TimelineHoverPrepareTarget,
        actual_pts: TrackTimestamp,
        exactness: TimelineHoverPreparePreparedHitExactness,
        diagnostics: TimelineHoverPrepareSpanDiagnostics,
    ) -> TimelineHoverPrepareCompletionOutcome {
        if !self.active_span_matches(span_id, target) {
            return self.stale_completion_outcome(span_id, target);
        }

        if exactness != TimelineHoverPreparePreparedHitExactness::ExactTargetOrAfter {
            return TimelineHoverPrepareCompletionOutcome::RejectedApproximate {
                completion_span_id: span_id,
                actual_pts,
            };
        }

        if let Err(reason) = target.validate_exact_actual_pts(actual_pts) {
            return TimelineHoverPrepareCompletionOutcome::RejectedTiming {
                completion_span_id: span_id,
                actual_pts,
                reason,
            };
        }

        TimelineHoverPrepareCompletionOutcome::AcceptedExactPreparedHit {
            span_id,
            actual_pts,
            diagnostics,
        }
    }

    fn active_span_matches(
        &self,
        span_id: TimelineHoverPrepareSpanId,
        target: TimelineHoverPrepareTarget,
    ) -> bool {
        self.active_span
            .map(|active_span| {
                active_span.span_id == span_id && active_span.latest_target == target
            })
            .unwrap_or(false)
    }

    fn stale_completion_outcome(
        &self,
        span_id: TimelineHoverPrepareSpanId,
        target: TimelineHoverPrepareTarget,
    ) -> TimelineHoverPrepareCompletionOutcome {
        match self.active_span {
            Some(active_span) if active_span.span_id == span_id => {
                let _stale_target = target;
                TimelineHoverPrepareCompletionOutcome::RejectedStaleTarget {
                    completion_span_id: span_id,
                }
            }
            active_span => TimelineHoverPrepareCompletionOutcome::RejectedStaleSpan {
                completion_span_id: span_id,
                active_span_id: active_span.map(|span| span.span_id),
            },
        }
    }

    fn allocate_span_id(&mut self) -> TimelineHoverPrepareSpanId {
        let span_id = self.next_span_id;
        self.next_span_id = TimelineHoverPrepareSpanId(span_id.0.saturating_add(1));
        span_id
    }
}

impl TimelineHoverPrepareTarget {
    pub(crate) const fn unresolved(
        context: TimelineHoverPrepareTargetContext,
        target_pts: TrackTimestamp,
        target_bucket: TimelineHoverFrameBucket,
        playback_mode: TimelineHoverPreparePlaybackMode,
    ) -> Self {
        Self {
            context,
            target_pts,
            target_bucket,
            dependency_span: None,
            playback_mode,
        }
    }

    pub(crate) fn new(
        context: TimelineHoverPrepareTargetContext,
        target_pts: TrackTimestamp,
        target_bucket: TimelineHoverFrameBucket,
        decode_safe_start_pts: TrackTimestamp,
        drain_until_pts: TrackTimestamp,
        post_target_reorder_drain_frames: u16,
        playback_mode: TimelineHoverPreparePlaybackMode,
    ) -> Result<Self, TimelineHoverPrepareTargetError> {
        Self::unresolved(context, target_pts, target_bucket, playback_mode).with_dependency_span(
            decode_safe_start_pts,
            drain_until_pts,
            post_target_reorder_drain_frames,
        )
    }

    pub(crate) fn with_dependency_span(
        self,
        decode_safe_start_pts: TrackTimestamp,
        drain_until_pts: TrackTimestamp,
        post_target_reorder_drain_frames: u16,
    ) -> Result<Self, TimelineHoverPrepareTargetError> {
        validate_dependency_span_points(self.target_pts, decode_safe_start_pts, drain_until_pts)?;

        Ok(Self {
            dependency_span: Some(DecodeDependencySpan {
                context: self.context,
                decode_safe_start_pts,
                drain_until_pts,
                post_target_reorder_drain_frames,
            }),
            ..self
        })
    }

    pub(crate) fn lookup_request(self) -> TimelineHoverPrepareFrameLookupRequest {
        TimelineHoverPrepareFrameLookupRequest::new(self.prepared_key(), self.target_pts)
    }

    pub(crate) fn prepared_key(self) -> TimelineHoverPrepareFrameKey {
        TimelineHoverPrepareFrameKey::new(
            self.context.source_revision,
            self.context.track_selection,
            self.context.backend_revision,
            self.context.hover_generation,
            self.context.exactness_policy,
            self.target_bucket,
        )
    }

    pub(crate) const fn playback_mode(self) -> TimelineHoverPreparePlaybackMode {
        self.playback_mode
    }

    fn validate_exact_actual_pts(
        self,
        actual_pts: TrackTimestamp,
    ) -> Result<(), TimelineHoverPreparePreparedHitTimingRejection> {
        if actual_pts.track_id != self.target_pts.track_id {
            return Err(
                TimelineHoverPreparePreparedHitTimingRejection::ActualPtsTrackMismatch {
                    target: self.target_pts.track_id,
                    actual: actual_pts.track_id,
                },
            );
        }

        if actual_pts.cmp_timeline_position(self.target_pts) == Ordering::Less {
            return Err(TimelineHoverPreparePreparedHitTimingRejection::ActualPtsBeforeTarget);
        }

        Ok(())
    }

    fn dependency_span(self) -> Option<DecodeDependencySpan> {
        self.dependency_span
    }

    /// Возвращает `true`, если actual PTS декодированного кадра ещё до target-а.
    ///
    /// Используется decode driver-ом для pre-target release; track id совпадает
    /// по построению (driver конвертирует media time через этот же target).
    #[must_use]
    pub(crate) fn actual_pts_is_before_target(self, actual_pts: TrackTimestamp) -> bool {
        actual_pts.cmp_timeline_position(self.target_pts) == Ordering::Less
    }

    pub(crate) fn pts_from_media_time(self, position: media_core::MediaTime) -> TrackTimestamp {
        TrackTimestamp {
            track_id: self.target_pts.track_id,
            units: self
                .target_pts
                .time_base
                .media_time_to_track_units_saturating(position),
            time_base: self.target_pts.time_base,
        }
    }
}

impl TimelineHoverPrepareTargetContext {
    pub(crate) const fn new(
        source_revision: SourceRevision,
        backend_revision: BackendRevision,
        track_selection: ScrubTrackSelection,
        hover_generation: ScrubGenerationToken,
        exactness_policy: FrameExactnessPolicy,
    ) -> Self {
        Self {
            source_revision,
            backend_revision,
            track_selection,
            hover_generation,
            exactness_policy,
        }
    }
}

impl DecodeDependencySpan {
    /// Сколько post-target кадров decoder может отдать из reorder/DPB буфера.
    #[must_use]
    pub(crate) const fn post_target_reorder_drain_frames(self) -> u16 {
        self.post_target_reorder_drain_frames
    }

    fn compare_requested_span(self, requested: Self) -> DecodeDependencySpanChange {
        if self.context != requested.context {
            return DecodeDependencySpanChange::IncompatibleContext;
        }

        if requested
            .decode_safe_start_pts
            .cmp_timeline_position(self.decode_safe_start_pts)
            == Ordering::Less
        {
            return DecodeDependencySpanChange::RequiresEarlierDecodeSafeStart;
        }

        if requested
            .drain_until_pts
            .cmp_timeline_position(self.drain_until_pts)
            == Ordering::Greater
            || requested.post_target_reorder_drain_frames > self.post_target_reorder_drain_frames
        {
            return DecodeDependencySpanChange::ForwardExtension;
        }

        DecodeDependencySpanChange::Compatible
    }
}

impl TimelineHoverPrepareSpanDiagnostics {
    pub(crate) const fn new(
        decoded_packets: u32,
        decoded_frames: u32,
        post_target_reorder_drain_frames: u16,
    ) -> Self {
        Self {
            decoded_packets,
            decoded_frames,
            post_target_reorder_drain_frames,
        }
    }

    pub(crate) const fn decoded_packets(self) -> u32 {
        self.decoded_packets
    }

    pub(crate) const fn decoded_frames(self) -> u32 {
        self.decoded_frames
    }

    pub(crate) const fn post_target_reorder_drain_frames(self) -> u16 {
        self.post_target_reorder_drain_frames
    }
}

fn validate_dependency_span_points(
    target_pts: TrackTimestamp,
    decode_safe_start_pts: TrackTimestamp,
    drain_until_pts: TrackTimestamp,
) -> Result<(), TimelineHoverPrepareTargetError> {
    if decode_safe_start_pts.track_id != target_pts.track_id {
        return Err(
            TimelineHoverPrepareTargetError::DecodeSafeStartTrackMismatch {
                target: target_pts.track_id,
                decode_safe_start: decode_safe_start_pts.track_id,
            },
        );
    }

    if drain_until_pts.track_id != target_pts.track_id {
        return Err(TimelineHoverPrepareTargetError::DrainUntilTrackMismatch {
            target: target_pts.track_id,
            drain_until: drain_until_pts.track_id,
        });
    }

    if decode_safe_start_pts.cmp_timeline_position(target_pts) == Ordering::Greater {
        return Err(
            TimelineHoverPrepareTargetError::DecodeSafeStartAfterTarget {
                decode_safe_start_pts,
                target_pts,
            },
        );
    }

    if drain_until_pts.cmp_timeline_position(target_pts) == Ordering::Less {
        return Err(TimelineHoverPrepareTargetError::DrainUntilBeforeTarget {
            drain_until_pts,
            target_pts,
        });
    }

    Ok(())
}

fn no_op_reason_for_missing_executor(
    playback_mode: TimelineHoverPreparePlaybackMode,
    lookup_miss: TimelineHoverPrepareLookupMissReason,
) -> TimelineHoverPrepareExecutorNoOpReason {
    match playback_mode {
        TimelineHoverPreparePlaybackMode::ActivePlayback => {
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackExecutorUnavailable {
                lookup_miss,
            }
        }
        TimelineHoverPreparePlaybackMode::PausedOrStopped => {
            TimelineHoverPrepareExecutorNoOpReason::PausedStoppedExecutorNotWired { lookup_miss }
        }
        TimelineHoverPreparePlaybackMode::ResumePendingAfterSeek {
            spare_capacity_available: true,
        } => TimelineHoverPrepareExecutorNoOpReason::WorkingSetMiss { lookup_miss },
        TimelineHoverPreparePlaybackMode::ResumePendingAfterSeek {
            spare_capacity_available: false,
        } => TimelineHoverPrepareExecutorNoOpReason::ResumePendingNoSpareCapacity,
        TimelineHoverPreparePlaybackMode::LiveScrubActive => {
            TimelineHoverPrepareExecutorNoOpReason::LiveScrubSuspended
        }
    }
}

fn dependency_span_seek_error_no_op_reason(
    error: &anyhow::Error,
) -> TimelineHoverPrepareExecutorNoOpReason {
    match error.downcast_ref::<MediaDemuxError>() {
        Some(MediaDemuxError::UnsupportedSeekMode { .. }) => {
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackDependencySpanSeekUnsupported
        }
        Some(MediaDemuxError::SeekUnavailable { .. }) => {
            TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackDependencySpanSeekUnavailable
        }
        None => TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackDependencySpanResolveFailed,
    }
}

fn unresolved_reason_keeps_pending_work(reason: TimelineHoverPrepareExecutorNoOpReason) -> bool {
    matches!(
        reason,
        TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackNetworkSourceOpening { .. }
    )
}

#[cfg(test)]
mod tests;
