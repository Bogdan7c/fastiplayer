//! Edge-triggered Ended, D42 hold, D26 latch and bounded automatic error policy.

#[cfg(test)]
mod tests;

use std::num::NonZeroU64;
use std::sync::Arc;

use player_core::{
    ExactMediaTransportAction, ExactMediaTransportRequest, MediaInstanceId, PlaybackIntent,
    PlaybackIntentRevision, PlaybackState,
};
use playlist_core::{
    AutomaticEndedIntent, AutomaticStopReason, AutomaticTraversalAdvance, AutomaticTraversalPlan,
    AutomaticTraversalStart, PlaylistItemId, RepeatMode,
};

use super::PlaylistController;
use super::install::PlaylistInstallMutation;
use super::manual_navigation::ManualNavigationTerminalAction;
use super::transport::PlannedPlaylistInstall;
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::PlaylistBindingGeneration;
use crate::playlist_runtime::identity::{
    ActiveMediaIdentity, PendingTargetOrigin, PlaylistItemErrorCategory, PlaylistItemErrorPhase,
};

/// Runtime setting shell; config wiring остаётся Session 13.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistErrorBehavior {
    Stop,
    Skip,
}

/// Ended snapshot не выводит clean/error semantics из старого `last_error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EndedSnapshotKind {
    Clean,
    ErrorAssociated { safe_summary: Arc<str> },
}

/// D26 interface принимает только readiness possibility, не discovery executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutomaticDeferredAvailability {
    Unavailable,
    MayProduceCandidate {
        scope_id: super::transport::SiblingDiscoveryScopeId,
    },
}

/// Typed terminal reason не смешивает domain boundary, user Stop и all-failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutomaticStopCause {
    Domain(AutomaticStopReason),
    ErrorPolicy,
    RepeatOneError,
    AllCandidatesFailed { attempted_count: usize },
    ManualTraversalCancelled,
    StructuralInvalidation,
    DeferredCancelled,
}

/// Controller action для одного exact edge/reevaluation.
pub(crate) enum AutomaticLifecycleOutcome {
    NoAction,
    StaleObservation,
    HeldForExplicitIntent {
        active: ActiveMediaIdentity,
    },
    ReplayCurrent {
        request: ExactMediaTransportRequest,
    },
    OpenItem {
        install: PlannedPlaylistInstall,
    },
    Deferred {
        item_id: PlaylistItemId,
        scope_id: super::transport::SiblingDiscoveryScopeId,
    },
    Stop {
        item_id: Option<PlaylistItemId>,
        media_instance_id: MediaInstanceId,
        cause: AutomaticStopCause,
    },
}

pub(crate) enum AutomaticTargetFailureOutcome {
    StaleRequest { request_id: MediaOpenRequestId },
    Stopped { cause: AutomaticStopCause },
    OpenItem { install: PlannedPlaylistInstall },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EndedDisposition {
    Held,
    Handled,
    Deferred,
    AutomaticPending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ObservedEndedEdge {
    pub(super) active: ActiveMediaIdentity,
    pub(super) disposition: EndedDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DeferredAdvanceLatch {
    pub(super) active: ActiveMediaIdentity,
    pub(super) scope_id: super::transport::SiblingDiscoveryScopeId,
}

#[derive(Default)]
pub(super) struct AutomaticLifecycle {
    pub(super) observed_ended: Option<ObservedEndedEdge>,
    pub(super) deferred_advance: Option<DeferredAdvanceLatch>,
    released_plan: Option<(MediaOpenRequestId, PlaylistItemId, AutomaticTraversalPlan)>,
}

impl PlaylistController {
    /// Suspend consumes an already observed terminal edge without navigation/error policy.
    pub(crate) fn consume_terminal_edge_for_suspend(
        &mut self,
        expected_active: ActiveMediaIdentity,
        playback_state: PlaybackState,
    ) -> bool {
        if self.active_media != Some(expected_active)
            || !matches!(playback_state, PlaybackState::Ended | PlaybackState::Failed)
        {
            return false;
        }
        self.automatic_lifecycle.observed_ended = Some(ObservedEndedEdge {
            active: expected_active,
            disposition: EndedDisposition::Handled,
        });
        self.automatic_lifecycle.deferred_advance = None;
        self.publish_view(false);
        true
    }

    /// Ended checkpoint переносит consumed edge на same-lineage rebound instance.
    pub(crate) fn carry_consumed_eof_edge_after_rebind(
        &mut self,
        previous_active: ActiveMediaIdentity,
        rebound_active: ActiveMediaIdentity,
    ) -> bool {
        if self.active_media != Some(rebound_active)
            || !self.automatic_lifecycle.observed_ended.is_some_and(|edge| {
                edge.active == previous_active && edge.disposition == EndedDisposition::Handled
            })
        {
            return false;
        }
        self.automatic_lifecycle.observed_ended = Some(ObservedEndedEdge {
            active: rebound_active,
            disposition: EndedDisposition::Handled,
        });
        true
    }

    pub(crate) const fn error_behavior(&self) -> PlaylistErrorBehavior {
        self.error_behavior
    }

    pub(crate) fn set_error_behavior(&mut self, behavior: PlaylistErrorBehavior) -> bool {
        if self.error_behavior == behavior {
            return false;
        }
        self.error_behavior = behavior;
        true
    }

    /// Exact snapshot observation создаёт action только на входе matching instance в Ended.
    pub(crate) fn observe_automatic_snapshot(
        &mut self,
        binding_generation: PlaylistBindingGeneration,
        media_instance_id: Option<MediaInstanceId>,
        playback_state: PlaybackState,
        ended_kind: EndedSnapshotKind,
        deferred_availability: AutomaticDeferredAvailability,
    ) -> AutomaticLifecycleOutcome {
        let Some(active) = self.active_media else {
            return AutomaticLifecycleOutcome::StaleObservation;
        };
        if active.player_binding_generation() != binding_generation
            || Some(active.media_instance_id()) != media_instance_id
        {
            return AutomaticLifecycleOutcome::StaleObservation;
        }
        let terminal_observation =
            matches!(playback_state, PlaybackState::Ended | PlaybackState::Failed);
        if !terminal_observation {
            if self
                .automatic_lifecycle
                .observed_ended
                .is_some_and(|edge| edge.active == active)
            {
                self.automatic_lifecycle.observed_ended = None;
                self.automatic_lifecycle.deferred_advance = None;
            }
            return AutomaticLifecycleOutcome::NoAction;
        }
        if self
            .automatic_lifecycle
            .observed_ended
            .is_some_and(|edge| edge.active == active)
        {
            return AutomaticLifecycleOutcome::NoAction;
        }

        let _origin_marked = self.mark_manual_navigation_origin_ended(active);
        if self.install_state.is_some()
            || self.pending_manual_traversal.is_some()
            || self.manual_navigation_cursor.has_state()
        {
            self.automatic_lifecycle.observed_ended = Some(ObservedEndedEdge {
                active,
                disposition: EndedDisposition::Held,
            });
            return AutomaticLifecycleOutcome::HeldForExplicitIntent { active };
        }

        self.automatic_lifecycle.observed_ended = Some(ObservedEndedEdge {
            active,
            disposition: EndedDisposition::Handled,
        });
        match ended_kind {
            EndedSnapshotKind::Clean => {
                if playback_state == PlaybackState::Failed {
                    return self.stop_for_active(active, AutomaticStopCause::ErrorPolicy);
                }
                self.evaluate_clean_ended(active, deferred_availability)
            }
            EndedSnapshotKind::ErrorAssociated { safe_summary } => {
                let Some(item_id) = active.item_id() else {
                    return self.stop_for_active(active, AutomaticStopCause::ErrorPolicy);
                };
                let _recorded =
                    self.record_playback_error(item_id, active.media_instance_id(), safe_summary);
                self.evaluate_runtime_error(active)
            }
        }
    }

    /// Pre-concrete cancel/exhaustion может ровно один раз освободить D42 hold.
    pub(crate) fn reevaluate_held_ended(
        &mut self,
        deferred_availability: AutomaticDeferredAvailability,
    ) -> AutomaticLifecycleOutcome {
        let Some(edge) = self.automatic_lifecycle.observed_ended else {
            return AutomaticLifecycleOutcome::NoAction;
        };
        if edge.disposition != EndedDisposition::Held || self.active_media != Some(edge.active) {
            return AutomaticLifecycleOutcome::NoAction;
        }
        if self.install_state.is_some()
            || self.pending_manual_traversal.is_some()
            || self.manual_navigation_cursor.has_state()
        {
            return AutomaticLifecycleOutcome::HeldForExplicitIntent {
                active: edge.active,
            };
        }
        self.automatic_lifecycle.observed_ended = Some(ObservedEndedEdge {
            active: edge.active,
            disposition: EndedDisposition::Handled,
        });
        self.evaluate_clean_ended(edge.active, deferred_availability)
    }

    /// D56/D57 consume matching held edge без automatic reevaluation.
    pub(super) fn stop_held_ended_without_reevaluation(
        &mut self,
        cause: AutomaticStopCause,
    ) -> AutomaticLifecycleOutcome {
        let Some(edge) = self.automatic_lifecycle.observed_ended else {
            return AutomaticLifecycleOutcome::NoAction;
        };
        if !matches!(
            edge.disposition,
            EndedDisposition::Held
                | EndedDisposition::Deferred
                | EndedDisposition::AutomaticPending
        ) || self.active_media != Some(edge.active)
        {
            return AutomaticLifecycleOutcome::NoAction;
        }
        self.automatic_lifecycle.deferred_advance = None;
        self.automatic_lifecycle.released_plan = None;
        self.automatic_lifecycle.observed_ended = Some(ObservedEndedEdge {
            active: edge.active,
            disposition: EndedDisposition::Handled,
        });
        self.stop_for_active(edge.active, cause)
    }

    pub(super) fn consume_manual_terminal_action(
        &mut self,
        action: ManualNavigationTerminalAction,
        cause: AutomaticStopCause,
    ) {
        if action == ManualNavigationTerminalAction::StopEndedOrigin {
            let _stopped = self.stop_held_ended_without_reevaluation(cause);
        }
    }

    pub(crate) fn cancel_deferred_automatic_advance(&mut self) -> AutomaticLifecycleOutcome {
        let Some(latch) = self.automatic_lifecycle.deferred_advance.take() else {
            return AutomaticLifecycleOutcome::NoAction;
        };
        if self.active_media != Some(latch.active) {
            return AutomaticLifecycleOutcome::StaleObservation;
        }
        self.automatic_lifecycle.observed_ended = Some(ObservedEndedEdge {
            active: latch.active,
            disposition: EndedDisposition::Handled,
        });
        self.stop_for_active(latch.active, AutomaticStopCause::DeferredCancelled)
    }

    /// Explicit Play/Next/Previous отменяет automatic/deferred continuation, но не D42 hold
    /// продолжающегося manual cursor-а.
    pub(super) fn cancel_automatic_continuation_for_manual_intent(&mut self) {
        self.automatic_lifecycle.deferred_advance = None;
        self.automatic_lifecycle.released_plan = None;
        if let Some(edge) = self.automatic_lifecycle.observed_ended.as_mut()
            && matches!(
                edge.disposition,
                EndedDisposition::Deferred | EndedDisposition::AutomaticPending
            )
        {
            edge.disposition = EndedDisposition::Handled;
        }
    }

    /// Automatic open/install failure сохраняет D49 badge и продолжает только fixed plan.
    pub(crate) fn report_automatic_target_failure(
        &mut self,
        request_id: MediaOpenRequestId,
        safe_summary: Arc<str>,
    ) -> AutomaticTargetFailureOutcome {
        let request = self
            .take_awaiting_automatic_failure(request_id)
            .or_else(|| self.take_released_automatic_plan(request_id));
        let Some((item_id, plan)) = request else {
            return AutomaticTargetFailureOutcome::StaleRequest { request_id };
        };
        self.upsert_runtime_error(
            item_id,
            PlaylistItemErrorPhase::Preparation,
            PlaylistItemErrorCategory::Unavailable,
            safe_summary,
            Some(request_id),
            None,
        );
        if self.error_behavior == PlaylistErrorBehavior::Stop {
            self.mark_current_edge_handled();
            return AutomaticTargetFailureOutcome::Stopped {
                cause: AutomaticStopCause::ErrorPolicy,
            };
        }
        match self.queue.advance_automatic_traversal_after_failure(plan) {
            AutomaticTraversalAdvance::OpenItem { item_id, plan } => {
                AutomaticTargetFailureOutcome::OpenItem {
                    install: self.planned_automatic_install(item_id, plan),
                }
            }
            AutomaticTraversalAdvance::AllFailed { attempted_count } => {
                self.mark_current_edge_handled();
                AutomaticTargetFailureOutcome::Stopped {
                    cause: AutomaticStopCause::AllCandidatesFailed { attempted_count },
                }
            }
        }
    }

    fn evaluate_clean_ended(
        &mut self,
        active: ActiveMediaIdentity,
        deferred_availability: AutomaticDeferredAvailability,
    ) -> AutomaticLifecycleOutcome {
        if active.item_id().is_none() {
            return self.evaluate_detached_clean_ended(active);
        }
        let Some(item_id) = active.item_id() else {
            return self.stop_for_active(
                active,
                AutomaticStopCause::Domain(AutomaticStopReason::CurrentItemAbsent),
            );
        };
        match self
            .queue
            .begin_automatic_traversal(AutomaticEndedIntent::new(self.repeat_mode))
        {
            AutomaticTraversalStart::OpenItem { item_id, plan } => {
                self.mark_current_edge_automatic_pending(active);
                AutomaticLifecycleOutcome::OpenItem {
                    install: self.planned_automatic_install(item_id, plan),
                }
            }
            AutomaticTraversalStart::ReplayCurrent {
                item_id: replay_item_id,
            } => {
                if replay_item_id != item_id {
                    return self.stop_for_active(
                        active,
                        AutomaticStopCause::Domain(AutomaticStopReason::CurrentItemAbsent),
                    );
                }
                AutomaticLifecycleOutcome::ReplayCurrent {
                    request: ExactMediaTransportRequest {
                        media_instance_id: active.media_instance_id(),
                        action: ExactMediaTransportAction::RestartFromBeginning {
                            intent: PlaybackIntent::StartPlaying,
                        },
                    },
                }
            }
            AutomaticTraversalStart::Stop(reason) => {
                if let AutomaticDeferredAvailability::MayProduceCandidate { scope_id } =
                    deferred_availability
                    && matches!(reason, AutomaticStopReason::EndOfQueue { .. })
                {
                    self.automatic_lifecycle.observed_ended = Some(ObservedEndedEdge {
                        active,
                        disposition: EndedDisposition::Deferred,
                    });
                    self.automatic_lifecycle.deferred_advance =
                        Some(DeferredAdvanceLatch { active, scope_id });
                    return AutomaticLifecycleOutcome::Deferred { item_id, scope_id };
                }
                self.stop_for_active(active, AutomaticStopCause::Domain(reason))
            }
        }
    }

    /// Tombstone использует pre-removal traversal context, но повторно валидирует target.
    fn evaluate_detached_clean_ended(
        &mut self,
        active: ActiveMediaIdentity,
    ) -> AutomaticLifecycleOutcome {
        let Some(tombstone) = self.detached_active_tombstone.as_mut() else {
            return self.stop_for_active(
                active,
                AutomaticStopCause::Domain(AutomaticStopReason::CurrentItemAbsent),
            );
        };
        if tombstone.active_lineage_id() != active.lineage_id() {
            return self.stop_for_active(
                active,
                AutomaticStopCause::Domain(AutomaticStopReason::CurrentItemAbsent),
            );
        }
        let Some(plan) = tombstone.take_continuation() else {
            return self.stop_for_active(
                active,
                AutomaticStopCause::Domain(AutomaticStopReason::CurrentItemAbsent),
            );
        };
        match self.queue.revalidate_automatic_traversal(*plan) {
            playlist_core::AutomaticTraversalAdvance::OpenItem { item_id, plan } => {
                self.mark_current_edge_automatic_pending(active);
                AutomaticLifecycleOutcome::OpenItem {
                    install: self.planned_automatic_install(item_id, plan),
                }
            }
            playlist_core::AutomaticTraversalAdvance::AllFailed { .. } => self.stop_for_active(
                active,
                AutomaticStopCause::Domain(AutomaticStopReason::CurrentItemAbsent),
            ),
        }
    }

    fn evaluate_runtime_error(&mut self, active: ActiveMediaIdentity) -> AutomaticLifecycleOutcome {
        if self.repeat_mode == RepeatMode::RepeatOne {
            return self.stop_for_active(active, AutomaticStopCause::RepeatOneError);
        }
        if self.error_behavior == PlaylistErrorBehavior::Stop {
            return self.stop_for_active(active, AutomaticStopCause::ErrorPolicy);
        }
        match self
            .queue
            .begin_automatic_error_traversal(AutomaticEndedIntent::new(self.repeat_mode))
        {
            AutomaticTraversalStart::OpenItem { item_id, plan } => {
                self.mark_current_edge_automatic_pending(active);
                AutomaticLifecycleOutcome::OpenItem {
                    install: self.planned_automatic_install(item_id, plan),
                }
            }
            AutomaticTraversalStart::ReplayCurrent { .. } => {
                self.stop_for_active(active, AutomaticStopCause::RepeatOneError)
            }
            AutomaticTraversalStart::Stop(reason) => {
                self.stop_for_active(active, AutomaticStopCause::Domain(reason))
            }
        }
    }

    pub(super) fn planned_automatic_install(
        &self,
        item_id: PlaylistItemId,
        plan: Box<AutomaticTraversalPlan>,
    ) -> PlannedPlaylistInstall {
        PlannedPlaylistInstall {
            item_id,
            playback_intent: PlaybackIntent::StartPlaying,
            intent_revision: PlaybackIntentRevision::from_non_zero(
                NonZeroU64::new(self.stable_intent_revision)
                    .expect("controller stable intent revision remains non-zero"),
            ),
            pending_origin: PendingTargetOrigin::AutomaticAdvance,
            expected_queue_revision: self.queue.revision_snapshot(),
            mutation: PlaylistInstallMutation::AutomaticTraversal(plan),
        }
    }

    pub(super) fn retain_released_automatic_plan(
        &mut self,
        request_id: MediaOpenRequestId,
        item_id: PlaylistItemId,
        plan: AutomaticTraversalPlan,
    ) {
        self.automatic_lifecycle.released_plan = Some((request_id, item_id, plan));
    }

    fn take_released_automatic_plan(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> Option<(PlaylistItemId, AutomaticTraversalPlan)> {
        let (stored_request_id, item_id, plan) = self.automatic_lifecycle.released_plan.take()?;
        if stored_request_id == request_id {
            Some((item_id, plan))
        } else {
            self.automatic_lifecycle.released_plan = Some((stored_request_id, item_id, plan));
            None
        }
    }

    pub(super) fn automatic_install_committed(&mut self, active: ActiveMediaIdentity) {
        self.automatic_lifecycle.released_plan = None;
        if self
            .automatic_lifecycle
            .observed_ended
            .is_some_and(|edge| edge.active != active)
        {
            self.automatic_lifecycle.observed_ended = None;
            self.automatic_lifecycle.deferred_advance = None;
        }
    }

    pub(super) fn stop_for_active(
        &self,
        active: ActiveMediaIdentity,
        cause: AutomaticStopCause,
    ) -> AutomaticLifecycleOutcome {
        AutomaticLifecycleOutcome::Stop {
            item_id: active.item_id().or_else(|| {
                self.queue
                    .traversal_current()
                    .map(|current| current.item_id())
            }),
            media_instance_id: active.media_instance_id(),
            cause,
        }
    }

    pub(super) fn mark_current_edge_automatic_pending(&mut self, active: ActiveMediaIdentity) {
        self.automatic_lifecycle.observed_ended = Some(ObservedEndedEdge {
            active,
            disposition: EndedDisposition::AutomaticPending,
        });
    }

    fn mark_current_edge_handled(&mut self) {
        if let Some(edge) = self.automatic_lifecycle.observed_ended.as_mut() {
            edge.disposition = EndedDisposition::Handled;
        }
    }
}
