//! Intent-level boundary между discovery markers и controller-owned traversal.

#[cfg(test)]
mod tests;

use std::num::NonZeroU64;

use playlist_core::{
    ManualNavigationDirection, ManualNavigationIntent, ManualNavigationOutcome, PlaylistItemId,
};

use super::PlaylistController;
use super::automatic_lifecycle::{
    AutomaticLifecycleOutcome, AutomaticStopCause, DeferredAdvanceLatch, EndedDisposition,
    ObservedEndedEdge,
};
use super::transport::ControllerManualNavigationOutcome;
use crate::playlist_runtime::identity::{ActiveMediaIdentity, TransportActionOrigin};
use playlist_core::{AutomaticEndedIntent, AutomaticStopReason, AutomaticTraversalStart};

/// Opaque discovery scope identity; controller не знает filesystem/probe internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SiblingDiscoveryScopeId(NonZeroU64);

impl SiblingDiscoveryScopeId {
    pub(crate) const fn from_non_zero(identity: NonZeroU64) -> Self {
        Self(identity)
    }

    /// Возвращает process-local число только для корреляции app-owned ports и request revisions.
    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }
}

/// D50 получает только readiness fact, не discovery wiring или candidate path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiscoveryManualWaitAvailability {
    Exhausted,
    MayProduceCandidate { scope_id: SiblingDiscoveryScopeId },
}

/// Монотонная identity одного latest-only D50 wait-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ManualNavigationWaitId(pub(super) NonZeroU64);

impl ManualNavigationWaitId {
    /// Число используется только для process-local correlation и read-only diagnostics.
    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Runtime-only one-slot wait; queue/traversal остаются неизменными.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PendingManualTraversal {
    pub(super) wait_id: ManualNavigationWaitId,
    pub(super) direction: ManualNavigationDirection,
    pub(super) origin: TransportActionOrigin,
    pub(super) active_media: ActiveMediaIdentity,
    pub(super) scope_id: SiblingDiscoveryScopeId,
}

/// Discovery интерес controller-а остаётся policy-level и не содержит filesystem key/path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiscoveryNavigationInterest {
    None,
    Manual {
        wait_id: ManualNavigationWaitId,
        scope_id: SiblingDiscoveryScopeId,
        direction: ManualNavigationDirection,
        shuffle: bool,
    },
    Automatic {
        scope_id: SiblingDiscoveryScopeId,
        shuffle: bool,
    },
}

/// Readiness kind не позволяет discovery самостоятельно назначить shuffle target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutomaticDiscoveryReadiness {
    ExactNaturalNext { item_id: PlaylistItemId },
    CommittedAdmissionAdvanced,
    Exhausted,
}

impl PlaylistController {
    /// Readiness event переоценивает queue и dispatch-ит ровно один transition.
    pub(crate) fn resume_manual_navigation_wait(
        &mut self,
        wait_id: ManualNavigationWaitId,
        scope_id: SiblingDiscoveryScopeId,
        exhausted: bool,
    ) -> ControllerManualNavigationOutcome {
        let Some(wait) = self.pending_manual_traversal else {
            return ControllerManualNavigationOutcome::StaleWait { wait_id };
        };
        if wait.wait_id != wait_id
            || wait.scope_id != scope_id
            || self.active_media != Some(wait.active_media)
        {
            return ControllerManualNavigationOutcome::StaleWait { wait_id };
        }
        if exhausted {
            self.pending_manual_traversal = None;
            return self.begin_manual_step(
                wait.direction,
                wait.origin,
                DiscoveryManualWaitAvailability::Exhausted,
            );
        }
        // Marker без нового domain target не должен создавать новый wait identity или polling spin.
        let outcome = self.queue.begin_manual_navigation(match wait.direction {
            ManualNavigationDirection::Next => ManualNavigationIntent::next(self.repeat_mode),
            ManualNavigationDirection::Previous => {
                ManualNavigationIntent::previous(self.repeat_mode)
            }
        });
        match outcome {
            ManualNavigationOutcome::OpenItem { item_id, preview } => {
                self.pending_manual_traversal = None;
                self.manual_navigation_cursor
                    .begin(preview, wait.origin, self.active_media);
                ControllerManualNavigationOutcome::StartInstall {
                    install: self.planned_manual_install(item_id, wait.origin),
                }
            }
            ManualNavigationOutcome::NoItem(_) => ControllerManualNavigationOutcome::Waiting {
                wait_id,
                direction: wait.direction,
                scope_id,
            },
        }
    }

    /// Возвращает единственный актуальный wait/latch для app-owned priority/readiness router-а.
    pub(crate) fn discovery_navigation_interest(&self) -> DiscoveryNavigationInterest {
        if let Some(wait) = self.pending_manual_traversal {
            return DiscoveryNavigationInterest::Manual {
                wait_id: wait.wait_id,
                scope_id: wait.scope_id,
                direction: wait.direction,
                shuffle: self.queue.shuffle_enabled(),
            };
        }
        if let Some(latch) = self.automatic_lifecycle.deferred_advance {
            return DiscoveryNavigationInterest::Automatic {
                scope_id: latch.scope_id,
                shuffle: self.queue.shuffle_enabled(),
            };
        }
        DiscoveryNavigationInterest::None
    }

    pub(crate) fn resume_manual_navigation_exact(
        &mut self,
        wait_id: ManualNavigationWaitId,
        scope_id: SiblingDiscoveryScopeId,
        exact_item_id: PlaylistItemId,
    ) -> ControllerManualNavigationOutcome {
        let Some(wait) = self.pending_manual_traversal else {
            return ControllerManualNavigationOutcome::StaleWait { wait_id };
        };
        if wait.wait_id != wait_id
            || wait.scope_id != scope_id
            || self.active_media != Some(wait.active_media)
            || self.queue.shuffle_enabled()
        {
            return ControllerManualNavigationOutcome::StaleWait { wait_id };
        }
        let domain_target = self.queue.begin_manual_navigation(match wait.direction {
            ManualNavigationDirection::Next => ManualNavigationIntent::next(self.repeat_mode),
            ManualNavigationDirection::Previous => {
                ManualNavigationIntent::previous(self.repeat_mode)
            }
        });
        if !matches!(
            domain_target,
            ManualNavigationOutcome::OpenItem { item_id, .. } if item_id == exact_item_id
        ) {
            return ControllerManualNavigationOutcome::StaleWait { wait_id };
        }
        self.resume_manual_navigation_wait(wait_id, scope_id, false)
    }

    /// Scan terminal отменяет только matching wait и не запускает новый domain target.
    pub(crate) fn cancel_manual_navigation_wait(
        &mut self,
        wait_id: ManualNavigationWaitId,
        scope_id: SiblingDiscoveryScopeId,
    ) -> bool {
        let Some(wait) = self.pending_manual_traversal else {
            return false;
        };
        if wait.wait_id != wait_id
            || wait.scope_id != scope_id
            || self.active_media != Some(wait.active_media)
        {
            return false;
        }
        self.pending_manual_traversal = None;
        let _stopped =
            self.stop_held_ended_without_reevaluation(AutomaticStopCause::DeferredCancelled);
        true
    }

    /// D41 one-shot resume: exact key для canonical order, domain re-query для shuffle.
    pub(crate) fn resume_deferred_automatic_advance(
        &mut self,
        scope_id: SiblingDiscoveryScopeId,
        readiness: AutomaticDiscoveryReadiness,
    ) -> AutomaticLifecycleOutcome {
        let Some(latch) = self.automatic_lifecycle.deferred_advance else {
            return AutomaticLifecycleOutcome::NoAction;
        };
        if latch.scope_id != scope_id || self.active_media != Some(latch.active) {
            return AutomaticLifecycleOutcome::StaleObservation;
        }
        let shuffle = self.queue.shuffle_enabled();
        if matches!(
            readiness,
            AutomaticDiscoveryReadiness::CommittedAdmissionAdvanced
        ) && !shuffle
        {
            return AutomaticLifecycleOutcome::NoAction;
        }
        if matches!(
            readiness,
            AutomaticDiscoveryReadiness::ExactNaturalNext { .. }
        ) && shuffle
        {
            return AutomaticLifecycleOutcome::NoAction;
        }
        if matches!(readiness, AutomaticDiscoveryReadiness::Exhausted) {
            self.consume_deferred_latch_as_handled(latch);
            return self.stop_for_active(
                latch.active,
                AutomaticStopCause::Domain(AutomaticStopReason::EndOfQueue {
                    current_item_id: latch
                        .active
                        .item_id()
                        .expect("deferred automatic latch always has a committed item"),
                }),
            );
        }
        match self
            .queue
            .begin_automatic_traversal(AutomaticEndedIntent::new(self.repeat_mode))
        {
            AutomaticTraversalStart::OpenItem { item_id, plan } => {
                if let AutomaticDiscoveryReadiness::ExactNaturalNext {
                    item_id: exact_item_id,
                } = readiness
                    && item_id != exact_item_id
                {
                    return AutomaticLifecycleOutcome::NoAction;
                }
                self.consume_deferred_latch_as_handled(latch);
                self.mark_current_edge_automatic_pending(latch.active);
                AutomaticLifecycleOutcome::OpenItem {
                    install: self.planned_automatic_install(item_id, plan),
                }
            }
            AutomaticTraversalStart::ReplayCurrent { .. } => {
                // Repeat-one выполняется до arm latch; этот outcome означает stale policy event.
                AutomaticLifecycleOutcome::NoAction
            }
            AutomaticTraversalStart::Stop(_) => {
                // Admission marker без committed upcoming сохраняет latch armed и не poll-ит.
                AutomaticLifecycleOutcome::NoAction
            }
        }
    }

    fn consume_deferred_latch_as_handled(&mut self, latch: DeferredAdvanceLatch) {
        self.automatic_lifecycle.deferred_advance = None;
        self.automatic_lifecycle.observed_ended = Some(ObservedEndedEdge {
            active: latch.active,
            disposition: EndedDisposition::Handled,
        });
    }
}
