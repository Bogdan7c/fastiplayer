//! Stable manual transport policy поверх canonical queue и exact player boundaries.

#[cfg(test)]
mod tests;

use std::num::NonZeroU64;
use std::time::Duration;

use player_core::{
    ExactMediaTransportAction, ExactMediaTransportOutcome, ExactMediaTransportRequest,
    PlaybackIntent, PlaybackIntentRevision, PlaybackIntentUpdate, PlaybackState,
};
use playlist_core::{
    ManualNavigationDirection, ManualNavigationIntent, ManualNavigationNoItem,
    ManualNavigationOutcome, PlaylistItemId, ReservedQueueMutation,
};

use super::PlaylistController;
pub(super) use super::discovery_navigation::PendingManualTraversal;
pub(crate) use super::discovery_navigation::{
    DiscoveryManualWaitAvailability, ManualNavigationWaitId, SiblingDiscoveryScopeId,
};
use super::install::{
    DeferredControllerIntent, DeferredTransportIntent, LifecycleIntentOutcome,
    PlaylistInstallMutation,
};
use super::manual_navigation::{
    CursorStepOutcome, ManualNavigationCancelOutcome, ManualNavigationInvalidation,
};
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::identity::{
    PendingTargetOrigin, PlaylistItemErrorPhase, TransportActionOrigin,
};
use crate::playlist_runtime::view::PlaylistDirtySignal;

/// Последнее явное устойчивое Play/Pause намерение; transient player states его не заменяют.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StablePlaybackIntent {
    Playing,
    Paused,
}

impl StablePlaybackIntent {
    const fn as_install_intent(self) -> PlaybackIntent {
        match self {
            Self::Playing => PlaybackIntent::StartPlaying,
            Self::Paused => PlaybackIntent::StartPaused,
        }
    }
}

/// App-owned `Stopped` не выводится из player snapshot `Paused`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppTransportDisposition {
    Active,
    Stopped,
}

/// Stable revision update адресует current instance и pending install без fallback command-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControllerStableIntentDispatch {
    pub revision: PlaybackIntentRevision,
    pub intent: PlaybackIntent,
    pub exact_current: Option<ExactMediaTransportRequest>,
    pub pending_update: Option<PlaybackIntentUpdate>,
}

/// Typed threshold не допускает float rounding и сохраняет D17 zero semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviousRestartThreshold(Duration);

impl PreviousRestartThreshold {
    pub(crate) const MAX_MILLISECONDS: u64 = 60_000;

    pub(crate) fn from_milliseconds(milliseconds: u64) -> Option<Self> {
        (milliseconds <= Self::MAX_MILLISECONDS).then(|| Self(Duration::from_millis(milliseconds)))
    }

    fn should_restart(self, current_position: Duration) -> bool {
        !self.0.is_zero() && current_position > self.0
    }
}

/// Полностью domain-owned mutation будущего install-а.
pub(crate) struct PlannedPlaylistInstall {
    pub item_id: PlaylistItemId,
    pub playback_intent: PlaybackIntent,
    pub intent_revision: PlaybackIntentRevision,
    pub pending_origin: PendingTargetOrigin,
    pub expected_queue_revision: playlist_core::QueueRevisionSnapshot,
    pub mutation: PlaylistInstallMutation,
}

/// Play item не смешивает exact restart, coalesce и новый install.
pub(crate) enum ControllerPlayItemOutcome {
    ItemNotCommitted {
        item_id: PlaylistItemId,
    },
    RestartActive {
        request: ExactMediaTransportRequest,
        intent_dispatch: ControllerStableIntentDispatch,
    },
    CoalescePending {
        request_id: MediaOpenRequestId,
        intent_dispatch: ControllerStableIntentDispatch,
    },
    StartInstall {
        install: PlannedPlaylistInstall,
        intent_dispatch: ControllerStableIntentDispatch,
    },
    Guarded {
        guard: TransportGuardOutcome,
        intent_dispatch: ControllerStableIntentDispatch,
    },
    IntentRevisionExhausted,
}

/// D17/D33/D50 outcomes остаются различимыми для UI/MPRIS adapter-а.
pub(crate) enum ControllerManualNavigationOutcome {
    RestartCurrent {
        request: ExactMediaTransportRequest,
    },
    StartInstall {
        install: PlannedPlaylistInstall,
    },
    SupersedeInstall {
        expected_request_id: MediaOpenRequestId,
        cause: player_core::MediaInstallCancellationCause,
        install: PlannedPlaylistInstall,
    },
    AbortedBeforeDispatch {
        request_id: MediaOpenRequestId,
        cause: player_core::MediaInstallCancellationCause,
        next: Option<PlannedPlaylistInstall>,
        no_item: Option<ManualNavigationNoItem>,
    },
    PreviewInvalidated(ManualNavigationInvalidation),
    Waiting {
        wait_id: ManualNavigationWaitId,
        direction: ManualNavigationDirection,
        scope_id: SiblingDiscoveryScopeId,
    },
    NoItem(ManualNavigationNoItem),
    StaleWait {
        wait_id: ManualNavigationWaitId,
    },
    Guarded(TransportGuardOutcome),
    IntentRevisionExhausted,
}

/// Guard outcome не скрывает exact request/cancellation cause и не создаёт FIFO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportGuardOutcome {
    ExecuteNow {
        intent: DeferredTransportIntent,
        aborted_request_id: Option<MediaOpenRequestId>,
        cancellation_cause: Option<player_core::MediaInstallCancellationCause>,
        mode_dirty: Option<PlaylistDirtySignal>,
    },
    CancelPendingThenExecute {
        request_id: MediaOpenRequestId,
        cause: player_core::MediaInstallCancellationCause,
        intent: DeferredTransportIntent,
    },
    AwaitAuthorizationResolution {
        request_id: MediaOpenRequestId,
    },
    AwaitInstalled {
        request_id: MediaOpenRequestId,
    },
    Fatal(super::install::PlaylistControllerInvariantViolation),
}

/// D58 toggle outcome фиксирует, что выключение никогда не resurrect-ит отменённый wait/request.
pub(crate) enum StopAfterCurrentOutcome {
    AppliedToCurrent {
        enabled: bool,
    },
    ClearedManualWait {
        enabled: bool,
        wait_id: ManualNavigationWaitId,
    },
    ClearedManualWaitAndStoppedEndedCurrent {
        wait_id: ManualNavigationWaitId,
        item_id: PlaylistItemId,
        media_instance_id: player_core::MediaInstanceId,
    },
    Guarded(TransportGuardOutcome),
    NoActiveMedia,
    StoppedEndedCurrent {
        item_id: PlaylistItemId,
        media_instance_id: player_core::MediaInstanceId,
    },
}

/// Контекст повторной оценки ровно одного transport-intent после terminal drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeferredTransportExecutionContext {
    pub current_position: Duration,
    pub previous_restart_threshold: PreviousRestartThreshold,
    pub wait_availability: DiscoveryManualWaitAvailability,
}

/// Результат terminal transport executor-а не смешивает разные player/domain boundaries.
pub(crate) enum DeferredTransportExecutionOutcome {
    PlayItem(ControllerPlayItemOutcome),
    Navigation(ControllerManualNavigationOutcome),
    NeutralStop(Option<Result<ExactMediaTransportRequest, TransportGuardOutcome>>),
    StopAfterCurrent(StopAfterCurrentOutcome),
    CancelManualNavigation(ManualNavigationCancelOutcome),
}

impl PlaylistController {
    pub(crate) const fn stable_playback_intent(&self) -> StablePlaybackIntent {
        self.stable_playback_intent
    }

    pub(crate) const fn transport_disposition(&self) -> AppTransportDisposition {
        self.transport_disposition
    }

    /// Snapshot transient/Ended observation намеренно не переписывает explicit intent.
    pub(crate) fn observe_player_snapshot_state(&mut self, _state: PlaybackState) -> bool {
        false
    }

    /// Explicit Play/Pause повышает revision ровно один раз и строит exact D52 dispatch.
    pub(crate) fn record_stable_transport_intent(
        &mut self,
        intent: StablePlaybackIntent,
        _origin: TransportActionOrigin,
    ) -> Option<ControllerStableIntentDispatch> {
        let next_revision = self.stable_intent_revision.checked_add(1)?;
        self.stable_intent_revision = next_revision;
        self.stable_playback_intent = intent;
        if intent == StablePlaybackIntent::Playing {
            self.transport_disposition = AppTransportDisposition::Active;
        }
        let revision = PlaybackIntentRevision::from_non_zero(NonZeroU64::new(next_revision)?);
        let install_intent = intent.as_install_intent();
        let pending_update = self
            .install_state
            .as_ref()
            .map(|state| PlaybackIntentUpdate {
                request_id: state.player_request_id(),
                revision,
                intent: install_intent,
            });
        // D52 уже применяет matching staged update к exact old current instance.
        // Отдельный exact transport нужен только без pending install, иначе Play/Pause удвоится.
        let exact_current = if pending_update.is_none() {
            self.active_media.map(|active| ExactMediaTransportRequest {
                media_instance_id: active.media_instance_id(),
                action: ExactMediaTransportAction::SetPlaybackIntent {
                    intent: install_intent,
                },
            })
        } else {
            None
        };
        Some(ControllerStableIntentDispatch {
            revision,
            intent: install_intent,
            exact_current,
            pending_update,
        })
    }

    /// Play row выбирает restart/coalesce/reinstall по exact Item ID и runtime state.
    pub(crate) fn play_item(
        &mut self,
        item_id: PlaylistItemId,
        origin: TransportActionOrigin,
    ) -> ControllerPlayItemOutcome {
        if self.queue.item(item_id).is_none() {
            return ControllerPlayItemOutcome::ItemNotCommitted { item_id };
        }
        self.cancel_automatic_continuation_for_manual_intent();
        self.pending_manual_traversal = None;
        if self.install_state.is_none() {
            let _discarded = self.manual_navigation_cursor.discard(
                &self.queue,
                player_core::MediaInstallCancellationCause::Superseded,
                None,
            );
        }
        let Some(intent_dispatch) =
            self.record_stable_transport_intent(StablePlaybackIntent::Playing, origin)
        else {
            return ControllerPlayItemOutcome::IntentRevisionExhausted;
        };

        if self
            .pending_target
            .is_some_and(|pending| pending.item_id() == Some(item_id))
        {
            return ControllerPlayItemOutcome::CoalescePending {
                request_id: self
                    .pending_target
                    .expect("checked pending target")
                    .request_id(),
                intent_dispatch,
            };
        }
        if self.install_state.is_some() {
            let guarded =
                self.request_transport_guard(DeferredTransportIntent::PlayItem { item_id, origin });
            return ControllerPlayItemOutcome::Guarded {
                guard: guarded,
                intent_dispatch,
            };
        }

        let active_matches = self
            .active_media
            .is_some_and(|active| active.item_id() == Some(item_id));
        let runtime_failed = self
            .runtime_errors
            .get(&item_id)
            .is_some_and(|error| error.phase() == PlaylistItemErrorPhase::Playback);
        if active_matches && !runtime_failed {
            let active = self.active_media.expect("checked active identity");
            let mut restart_dispatch = intent_dispatch;
            restart_dispatch.exact_current = None;
            return ControllerPlayItemOutcome::RestartActive {
                request: ExactMediaTransportRequest {
                    media_instance_id: active.media_instance_id(),
                    action: ExactMediaTransportAction::RestartFromBeginning {
                        intent: PlaybackIntent::StartPlaying,
                    },
                },
                intent_dispatch: restart_dispatch,
            };
        }

        self.stop_after_current = None;
        ControllerPlayItemOutcome::StartInstall {
            install: PlannedPlaylistInstall {
                item_id,
                playback_intent: PlaybackIntent::StartPlaying,
                intent_revision: intent_dispatch.revision,
                pending_origin: PendingTargetOrigin::ExplicitRowPlay,
                expected_queue_revision: self.queue.revision_snapshot(),
                mutation: PlaylistInstallMutation::Reserved(
                    ReservedQueueMutation::select_committed(item_id),
                ),
            },
            intent_dispatch,
        }
    }

    /// Обычный one-step Next/Previous; fast cursor намеренно остаётся Session 11C.
    pub(crate) fn manual_navigation(
        &mut self,
        direction: ManualNavigationDirection,
        origin: TransportActionOrigin,
        current_position: Duration,
        restart_threshold: PreviousRestartThreshold,
        wait_availability: DiscoveryManualWaitAvailability,
    ) -> ControllerManualNavigationOutcome {
        self.cancel_automatic_continuation_for_manual_intent();
        if let Some(wait) = self.pending_manual_traversal {
            if wait.direction == direction {
                return ControllerManualNavigationOutcome::Waiting {
                    wait_id: wait.wait_id,
                    direction: wait.direction,
                    scope_id: wait.scope_id,
                };
            }
            // Противоположное нажатие supersede-ит logical wait, но не bulk discovery scope.
            self.pending_manual_traversal = None;
        }
        if let Some((phase, request_id)) = self.manual_navigation_install_phase() {
            match phase {
                super::install::ControllerInstallPhase::AwaitingReady => {
                    return self.continue_manual_cursor(direction, Some((request_id, false)));
                }
                super::install::ControllerInstallPhase::ReservedAwaitingAuthorization => {
                    if let Err(violation) =
                        self.abort_reserved_manual_navigation_before_dispatch(request_id)
                    {
                        return ControllerManualNavigationOutcome::Guarded(
                            TransportGuardOutcome::Fatal(violation),
                        );
                    }
                    return self.continue_manual_cursor(direction, Some((request_id, true)));
                }
                super::install::ControllerInstallPhase::AuthorizationDispatchPending
                | super::install::ControllerInstallPhase::AuthorizationInFlight => {}
            }
        }
        if self.install_state.is_some() {
            return ControllerManualNavigationOutcome::Guarded(
                self.request_transport_guard(DeferredTransportIntent::Navigate {
                    direction,
                    origin,
                }),
            );
        }

        if self
            .manual_navigation_cursor
            .latest_target_item_id()
            .is_some()
        {
            return self.continue_manual_cursor(direction, None);
        }

        if direction == ManualNavigationDirection::Previous
            && restart_threshold.should_restart(current_position)
            && let Some(active) = self.active_media
        {
            return ControllerManualNavigationOutcome::RestartCurrent {
                request: ExactMediaTransportRequest {
                    media_instance_id: active.media_instance_id(),
                    action: ExactMediaTransportAction::RestartFromBeginning {
                        intent: self.intent_for_navigation(origin),
                    },
                },
            };
        }

        self.begin_manual_step(direction, origin, wait_availability)
    }

    pub(super) fn begin_manual_step(
        &mut self,
        direction: ManualNavigationDirection,
        origin: TransportActionOrigin,
        wait_availability: DiscoveryManualWaitAvailability,
    ) -> ControllerManualNavigationOutcome {
        let domain_intent = match direction {
            ManualNavigationDirection::Next => ManualNavigationIntent::next(self.repeat_mode),
            ManualNavigationDirection::Previous => {
                ManualNavigationIntent::previous(self.repeat_mode)
            }
        };
        match self.queue.begin_manual_navigation(domain_intent) {
            ManualNavigationOutcome::OpenItem { item_id, preview } => {
                self.stop_after_current = None;
                self.manual_navigation_cursor
                    .begin(preview, origin, self.active_media);
                ControllerManualNavigationOutcome::StartInstall {
                    install: self.planned_manual_install(item_id, origin),
                }
            }
            ManualNavigationOutcome::NoItem(reason) => {
                self.manual_no_item(direction, origin, reason, wait_availability)
            }
        }
    }

    fn continue_manual_cursor(
        &mut self,
        direction: ManualNavigationDirection,
        superseded: Option<(MediaOpenRequestId, bool)>,
    ) -> ControllerManualNavigationOutcome {
        let origin = self
            .manual_navigation_cursor
            .origin()
            .expect("manual install phase always has cursor context");
        match self.manual_navigation_cursor.continue_in_direction(
            &self.queue,
            direction,
            self.repeat_mode,
        ) {
            CursorStepOutcome::OpenItem { item_id } => {
                self.stop_after_current = None;
                let install = self.planned_manual_install(item_id, origin);
                match superseded {
                    Some((expected_request_id, false)) => {
                        ControllerManualNavigationOutcome::SupersedeInstall {
                            expected_request_id,
                            cause: player_core::MediaInstallCancellationCause::Superseded,
                            install,
                        }
                    }
                    Some((request_id, true)) => {
                        ControllerManualNavigationOutcome::AbortedBeforeDispatch {
                            request_id,
                            cause: player_core::MediaInstallCancellationCause::Superseded,
                            next: Some(install),
                            no_item: None,
                        }
                    }
                    None => ControllerManualNavigationOutcome::StartInstall { install },
                }
            }
            CursorStepOutcome::NoItem(reason) => {
                let returned_to_origin = matches!(
                    reason,
                    ManualNavigationNoItem::ReturnedToCommittedOrigin { .. }
                );
                if returned_to_origin && let Some((request_id, was_reserved)) = superseded {
                    if !was_reserved
                        && let Err(violation) =
                            self.retire_awaiting_manual_navigation_request(request_id)
                    {
                        return ControllerManualNavigationOutcome::Guarded(
                            TransportGuardOutcome::Fatal(violation),
                        );
                    }
                    return ControllerManualNavigationOutcome::AbortedBeforeDispatch {
                        request_id,
                        cause: player_core::MediaInstallCancellationCause::Superseded,
                        next: None,
                        no_item: Some(reason),
                    };
                }
                ControllerManualNavigationOutcome::NoItem(reason)
            }
            CursorStepOutcome::Invalidated {
                error: _error,
                terminal_action,
            } => {
                let request_id = superseded.map(|(request_id, _)| request_id);
                if let Some((request_id, false)) = superseded
                    && let Err(violation) =
                        self.retire_awaiting_manual_navigation_request(request_id)
                {
                    return ControllerManualNavigationOutcome::Guarded(
                        TransportGuardOutcome::Fatal(violation),
                    );
                }
                ControllerManualNavigationOutcome::PreviewInvalidated(
                    ManualNavigationInvalidation {
                        cause: player_core::MediaInstallCancellationCause::StructuralInvalidation,
                        request_id,
                        terminal_action,
                    },
                )
            }
        }
    }

    fn manual_no_item(
        &mut self,
        direction: ManualNavigationDirection,
        origin: TransportActionOrigin,
        reason: ManualNavigationNoItem,
        wait_availability: DiscoveryManualWaitAvailability,
    ) -> ControllerManualNavigationOutcome {
        let may_wait =
            !self.queue.shuffle_enabled() || direction == ManualNavigationDirection::Next;
        let DiscoveryManualWaitAvailability::MayProduceCandidate { scope_id } = wait_availability
        else {
            return ControllerManualNavigationOutcome::NoItem(reason);
        };
        let Some(active_media) = self.active_media.filter(|_| may_wait) else {
            return ControllerManualNavigationOutcome::NoItem(reason);
        };
        let Some(raw_wait_id) = NonZeroU64::new(self.next_manual_wait_identity) else {
            return ControllerManualNavigationOutcome::IntentRevisionExhausted;
        };
        let Some(next_identity) = self.next_manual_wait_identity.checked_add(1) else {
            return ControllerManualNavigationOutcome::IntentRevisionExhausted;
        };
        self.next_manual_wait_identity = next_identity;
        let wait_id = ManualNavigationWaitId(raw_wait_id);
        self.pending_manual_traversal = Some(PendingManualTraversal {
            wait_id,
            direction,
            origin,
            active_media,
            scope_id,
        });
        ControllerManualNavigationOutcome::Waiting {
            wait_id,
            direction,
            scope_id,
        }
    }

    pub(super) fn planned_manual_install(
        &self,
        item_id: PlaylistItemId,
        origin: TransportActionOrigin,
    ) -> PlannedPlaylistInstall {
        PlannedPlaylistInstall {
            item_id,
            playback_intent: self.intent_for_navigation(origin),
            intent_revision: PlaybackIntentRevision::from_non_zero(
                NonZeroU64::new(self.stable_intent_revision)
                    .expect("controller stable revision is always non-zero"),
            ),
            pending_origin: PendingTargetOrigin::ManualNavigation { origin },
            expected_queue_revision: self.queue.revision_snapshot(),
            mutation: PlaylistInstallMutation::ManualNavigation,
        }
    }

    fn intent_for_navigation(&self, origin: TransportActionOrigin) -> PlaybackIntent {
        if origin == TransportActionOrigin::Mpris
            && self.transport_disposition == AppTransportDisposition::Stopped
        {
            PlaybackIntent::StartPaused
        } else {
            self.stable_playback_intent.as_install_intent()
        }
    }

    /// Neutral Stop не применяет destructive `PlayerCommand::Stop`.
    pub(crate) fn neutral_stop(
        &mut self,
        origin: TransportActionOrigin,
    ) -> Option<Result<ExactMediaTransportRequest, TransportGuardOutcome>> {
        self.pending_manual_traversal = None;
        self.stable_playback_intent = StablePlaybackIntent::Paused;
        if self.install_state.is_some() {
            return Some(Err(
                self.request_transport_guard(DeferredTransportIntent::Stop { origin })
            ));
        }
        let _discarded = self.manual_navigation_cursor.discard(
            &self.queue,
            player_core::MediaInstallCancellationCause::TransportStop,
            None,
        );
        self.active_media.map(|active| {
            Ok(ExactMediaTransportRequest {
                media_instance_id: active.media_instance_id(),
                action: ExactMediaTransportAction::NeutralStop,
            })
        })
    }

    /// App публикует Stopped только после matching full player success.
    pub(crate) fn apply_neutral_stop_outcome(
        &mut self,
        outcome: &ExactMediaTransportOutcome,
    ) -> bool {
        let ExactMediaTransportOutcome::Applied { media_instance_id } = outcome else {
            return false;
        };
        if self
            .active_media
            .is_some_and(|active| active.media_instance_id() == *media_instance_id)
        {
            self.transport_disposition = AppTransportDisposition::Stopped;
            return true;
        }
        false
    }

    /// D58 cancel/defer использует тот же guard winner и один latest intent slot.
    pub(crate) fn toggle_stop_after_current(
        &mut self,
        enabled: bool,
        origin: TransportActionOrigin,
    ) -> StopAfterCurrentOutcome {
        if let Some(wait) = self.pending_manual_traversal.take() {
            self.set_stop_after_current(enabled);
            if enabled
                && let super::automatic_lifecycle::AutomaticLifecycleOutcome::Stop {
                    item_id: Some(item_id),
                    media_instance_id,
                    ..
                } = self.stop_held_ended_without_reevaluation(
                    super::automatic_lifecycle::AutomaticStopCause::StopAfterCurrent,
                )
            {
                self.stop_after_current = None;
                return StopAfterCurrentOutcome::ClearedManualWaitAndStoppedEndedCurrent {
                    wait_id: wait.wait_id,
                    item_id,
                    media_instance_id,
                };
            }
            return StopAfterCurrentOutcome::ClearedManualWait {
                enabled,
                wait_id: wait.wait_id,
            };
        }
        if self.install_state.is_some() {
            return StopAfterCurrentOutcome::Guarded(self.request_transport_guard(
                DeferredTransportIntent::StopAfterCurrent { enabled, origin },
            ));
        }
        let _discarded = self.manual_navigation_cursor.discard(
            &self.queue,
            player_core::MediaInstallCancellationCause::StopAfterCurrent,
            None,
        );
        if self.active_media.is_none() {
            return StopAfterCurrentOutcome::NoActiveMedia;
        }
        self.set_stop_after_current(enabled);
        if enabled
            && let super::automatic_lifecycle::AutomaticLifecycleOutcome::Stop {
                item_id: Some(item_id),
                media_instance_id,
                ..
            } = self.stop_held_ended_without_reevaluation(
                super::automatic_lifecycle::AutomaticStopCause::StopAfterCurrent,
            )
        {
            self.stop_after_current = None;
            return StopAfterCurrentOutcome::StoppedEndedCurrent {
                item_id,
                media_instance_id,
            };
        }
        StopAfterCurrentOutcome::AppliedToCurrent { enabled }
    }

    /// Terminal drain вызывает этот boundary только после commit/abort и применения modes.
    pub(crate) fn execute_deferred_transport_intent(
        &mut self,
        intent: DeferredTransportIntent,
        context: DeferredTransportExecutionContext,
    ) -> DeferredTransportExecutionOutcome {
        match intent {
            DeferredTransportIntent::PlayItem { item_id, origin } => {
                DeferredTransportExecutionOutcome::PlayItem(self.play_item(item_id, origin))
            }
            DeferredTransportIntent::Navigate { direction, origin } => {
                DeferredTransportExecutionOutcome::Navigation(self.manual_navigation(
                    direction,
                    origin,
                    context.current_position,
                    context.previous_restart_threshold,
                    context.wait_availability,
                ))
            }
            DeferredTransportIntent::Stop { origin } => {
                DeferredTransportExecutionOutcome::NeutralStop(self.neutral_stop(origin))
            }
            DeferredTransportIntent::StopAfterCurrent { enabled, origin } => {
                DeferredTransportExecutionOutcome::StopAfterCurrent(
                    self.toggle_stop_after_current(enabled, origin),
                )
            }
            DeferredTransportIntent::CancelManualNavigation => {
                DeferredTransportExecutionOutcome::CancelManualNavigation(
                    self.cancel_manual_navigation(),
                )
            }
        }
    }

    pub(super) fn request_transport_guard(
        &mut self,
        intent: DeferredTransportIntent,
    ) -> TransportGuardOutcome {
        let outcome =
            match self.request_deferred_intent(DeferredControllerIntent::Transport(intent)) {
                Ok(outcome) => outcome,
                Err(violation) => return TransportGuardOutcome::Fatal(violation),
            };
        match outcome {
            LifecycleIntentOutcome::Immediate {
                intent: DeferredControllerIntent::Transport(intent),
                aborted_request_id,
                cancellation_cause,
                mode_dirty,
            } => TransportGuardOutcome::ExecuteNow {
                intent,
                aborted_request_id,
                cancellation_cause,
                mode_dirty,
            },
            LifecycleIntentOutcome::CancelPendingRequest {
                request_id,
                cause,
                intent: DeferredControllerIntent::Transport(intent),
            } => TransportGuardOutcome::CancelPendingThenExecute {
                request_id,
                cause,
                intent,
            },
            LifecycleIntentOutcome::AwaitAuthorizationResolution { request_id } => {
                TransportGuardOutcome::AwaitAuthorizationResolution { request_id }
            }
            LifecycleIntentOutcome::AwaitInstalled { request_id } => {
                TransportGuardOutcome::AwaitInstalled { request_id }
            }
            LifecycleIntentOutcome::NoPendingInstall => TransportGuardOutcome::ExecuteNow {
                intent,
                aborted_request_id: None,
                cancellation_cause: None,
                mode_dirty: None,
            },
            LifecycleIntentOutcome::Fatal(violation) => TransportGuardOutcome::Fatal(violation),
            LifecycleIntentOutcome::Immediate { .. }
            | LifecycleIntentOutcome::CancelPendingRequest { .. } => {
                unreachable!("transport guard must return the same typed transport intent")
            }
        }
    }
}
