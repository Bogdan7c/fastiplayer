//! D41/D50 marker routing, отделённый от record-bearing batch commit orchestration.

#[cfg(test)]
mod tests;

use std::{sync::Arc, time::Duration};

use player_core::{PlaybackState, PlayerSnapshot};
use playlist_core::{ManualNavigationDirection, PlaylistItemId};
use playlist_discovery::{
    AdmissionAdvanced, AdmissionDirection, DiscoveryCancellationCause, DiscoveryEvent,
    DiscoveryFinalOutcome, FrontierReady, ManifestCandidateKey, ReprioritizeHint,
};

use super::{ActiveDiscoveryScope, PlaylistDiscoveryCoordinator};
use crate::playlist_runtime::controller::{
    AutomaticDeferredAvailability, AutomaticDiscoveryReadiness, AutomaticLifecycleOutcome,
    ControllerManualNavigationOutcome, DiscoveryManualWaitAvailability,
    DiscoveryNavigationInterest, EndedSnapshotKind,
};
use crate::playlist_runtime::identity::TransportActionOrigin;
use crate::playlist_runtime::{PlaylistController, PlaylistRuntime, PlaylistRuntimeBinding};

const FAILED_PLAYBACK_WITHOUT_DETAILS: &str =
    "Воспроизведение завершилось с ошибкой без дополнительных сведений";

/// Event-driven action slot: readiness выбирает intent, а не коммитит queue/media.
#[allow(
    dead_code,
    reason = "typed action is consumed by the later UI/MPRIS adapter"
)]
pub(crate) enum PlaylistDiscoveryNavigationAction {
    Manual(ControllerManualNavigationOutcome),
    Automatic(AutomaticLifecycleOutcome),
    ScopeCancelled {
        scope_id: super::super::controller::SiblingDiscoveryScopeId,
        cause: DiscoveryCancellationCause,
    },
    ScopeFatal {
        scope_id: super::super::controller::SiblingDiscoveryScopeId,
    },
}

/// Read-only модель подходит и global status, и будущему sidebar без UI ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistDiscoveryNavigationStatus {
    Idle,
    WaitingManual {
        scope_id: super::super::controller::SiblingDiscoveryScopeId,
        wait_id: super::super::controller::ManualNavigationWaitId,
        direction: ManualNavigationDirection,
    },
    WaitingAutomatic {
        scope_id: super::super::controller::SiblingDiscoveryScopeId,
    },
    TargetReady {
        scope_id: super::super::controller::SiblingDiscoveryScopeId,
        item_id: PlaylistItemId,
    },
    Exhausted {
        scope_id: super::super::controller::SiblingDiscoveryScopeId,
    },
    Cancelled {
        scope_id: super::super::controller::SiblingDiscoveryScopeId,
        cause: DiscoveryCancellationCause,
    },
    Fatal {
        scope_id: super::super::controller::SiblingDiscoveryScopeId,
    },
}

#[allow(
    dead_code,
    reason = "Session 15A installs action boundaries before playlist UI wiring"
)]
impl PlaylistRuntime {
    /// UI/MPRIS command сериализуется на app owner-е до последующих frontier events.
    pub(crate) fn request_playlist_navigation(
        &mut self,
        direction: ManualNavigationDirection,
        origin: TransportActionOrigin,
        current_position: Duration,
    ) -> Option<ControllerManualNavigationOutcome> {
        let restart_threshold = self.previous_restart_threshold();
        let controller = self.controller.as_mut()?;
        let wait_availability = self.discovery.manual_wait_availability();
        let outcome = controller.manual_navigation(
            direction,
            origin,
            current_position,
            restart_threshold,
            wait_availability,
        );
        self.discovery.synchronize_navigation_interest(controller);
        Some(outcome)
    }

    /// Player snapshot edge использует active discovery scope только как readiness possibility.
    pub(crate) fn observe_playlist_automatic_snapshot(
        &mut self,
        binding: PlaylistRuntimeBinding,
        player_snapshot: &PlayerSnapshot,
    ) -> Option<AutomaticLifecycleOutcome> {
        if self.validate_binding(binding).is_err() {
            return Some(AutomaticLifecycleOutcome::StaleObservation);
        }

        let ended_kind = automatic_snapshot_kind(player_snapshot);
        let controller = self.controller.as_mut()?;
        let deferred = self.discovery.automatic_deferred_availability();
        let outcome = controller.observe_automatic_snapshot(
            binding.binding_generation(),
            player_snapshot.media_instance_id,
            player_snapshot.playback_state,
            ended_kind,
            deferred,
        );
        self.discovery.synchronize_navigation_interest(controller);
        Some(outcome)
    }

    /// Event-generated target забирается exactly once; marker сам queue не меняет.
    pub(crate) fn take_playlist_discovery_navigation_action(
        &mut self,
    ) -> Option<PlaylistDiscoveryNavigationAction> {
        let action = self.discovery.navigation_action.take();
        if action.is_some()
            && let Some(controller) = self.controller.as_ref()
        {
            self.discovery.synchronize_navigation_interest(controller);
        }
        action
    }

    pub(crate) const fn playlist_discovery_navigation_status(
        &self,
    ) -> PlaylistDiscoveryNavigationStatus {
        self.discovery.navigation_status
    }
}

/// Определяет terminal semantics только по текущему player state.
///
/// Старый `last_error` может пережить успешный replay и поэтому не превращает чистый EOF
/// в runtime failure. Для `Failed` наружу передаётся только безопасная категория:
/// произвольный текст нижнего слоя может содержать locator или другие чувствительные детали.
fn automatic_snapshot_kind(player_snapshot: &PlayerSnapshot) -> EndedSnapshotKind {
    if player_snapshot.playback_state != PlaybackState::Failed {
        return EndedSnapshotKind::Clean;
    }

    let safe_summary = player_snapshot.last_error.as_ref().map_or_else(
        || Arc::<str>::from(FAILED_PLAYBACK_WITHOUT_DETAILS),
        |error| Arc::<str>::from(format!("Ошибка воспроизведения ({:?})", error.kind)),
    );
    EndedSnapshotKind::ErrorAssociated { safe_summary }
}

impl PlaylistDiscoveryCoordinator {
    #[allow(dead_code, reason = "called by the Session 15A app action boundary")]
    pub(crate) fn manual_wait_availability(&self) -> DiscoveryManualWaitAvailability {
        match self.active_scope.as_ref() {
            Some(active) => DiscoveryManualWaitAvailability::MayProduceCandidate {
                scope_id: active.scope_id,
            },
            None => DiscoveryManualWaitAvailability::Exhausted,
        }
    }

    #[allow(dead_code, reason = "called by the Session 15A snapshot boundary")]
    pub(super) fn automatic_deferred_availability(&self) -> AutomaticDeferredAvailability {
        match self.active_scope.as_ref() {
            Some(active) => AutomaticDeferredAvailability::MayProduceCandidate {
                scope_id: active.scope_id,
            },
            None => AutomaticDeferredAvailability::Unavailable,
        }
    }

    #[allow(dead_code, reason = "called by the Session 15A app action boundary")]
    pub(crate) fn synchronize_navigation_interest(&mut self, controller: &PlaylistController) {
        let Some(active) = self.active_scope.as_ref() else {
            self.navigation_status = PlaylistDiscoveryNavigationStatus::Idle;
            return;
        };
        match controller.discovery_navigation_interest() {
            DiscoveryNavigationInterest::None => {
                self.navigation_status = PlaylistDiscoveryNavigationStatus::Idle;
                let hint = super::mapping::manifest_priority_hint(
                    &active.manifest,
                    active.target_key,
                    controller,
                );
                let _reprioritized = active
                    .job
                    .reprioritize(ReprioritizeHint::new(hint.into_boxed_slice()));
            }
            DiscoveryNavigationInterest::Manual {
                wait_id,
                scope_id,
                direction,
                shuffle,
            } if scope_id == active.scope_id => {
                self.navigation_status = PlaylistDiscoveryNavigationStatus::WaitingManual {
                    scope_id,
                    wait_id,
                    direction,
                };
                let hint = directional_priority_hint(active, direction, shuffle);
                let _reprioritized = active
                    .job
                    .reprioritize(ReprioritizeHint::new(hint.into_boxed_slice()));
            }
            DiscoveryNavigationInterest::Automatic { scope_id, shuffle }
                if scope_id == active.scope_id =>
            {
                self.navigation_status =
                    PlaylistDiscoveryNavigationStatus::WaitingAutomatic { scope_id };
                let hint =
                    directional_priority_hint(active, ManualNavigationDirection::Next, shuffle);
                let _reprioritized = active
                    .job
                    .reprioritize(ReprioritizeHint::new(hint.into_boxed_slice()));
            }
            _ => {
                self.navigation_status = PlaylistDiscoveryNavigationStatus::Idle;
            }
        }
    }

    pub(super) fn route_navigation_event(
        &mut self,
        controller: &mut PlaylistController,
        active: &mut ActiveDiscoveryScope,
        event: DiscoveryEvent,
    ) {
        match event {
            DiscoveryEvent::AdmissionAdvanced(marker) => {
                self.route_admission_advanced(controller, active, marker);
            }
            DiscoveryEvent::FrontierReady(ready) => {
                self.route_frontier_ready(controller, active, ready);
            }
            DiscoveryEvent::AdmittedBatch(_) => {}
        }
        self.synchronize_navigation_interest_for_active(controller, active);
    }

    pub(super) fn finish_navigation_scope(
        &mut self,
        controller: &mut PlaylistController,
        active: &ActiveDiscoveryScope,
        outcome: DiscoveryFinalOutcome,
    ) {
        let cancelled = matches!(outcome, DiscoveryFinalOutcome::Cancelled(_));
        let fatal = outcome == DiscoveryFinalOutcome::ExecutorDisconnected;
        let mut resolved_wait = false;
        match controller.discovery_navigation_interest() {
            DiscoveryNavigationInterest::Manual {
                wait_id, scope_id, ..
            } if scope_id == active.scope_id => {
                resolved_wait = true;
                if cancelled || fatal {
                    let _cancelled_wait =
                        controller.cancel_manual_navigation_wait(wait_id, scope_id);
                } else {
                    let result = controller.resume_manual_navigation_wait(wait_id, scope_id, true);
                    self.navigation_action =
                        Some(PlaylistDiscoveryNavigationAction::Manual(result));
                }
            }
            DiscoveryNavigationInterest::Automatic { scope_id, .. }
                if scope_id == active.scope_id =>
            {
                resolved_wait = true;
                if cancelled || fatal {
                    let _cancelled_latch = controller.cancel_deferred_automatic_advance();
                } else {
                    let result = controller.resume_deferred_automatic_advance(
                        scope_id,
                        AutomaticDiscoveryReadiness::Exhausted,
                    );
                    self.navigation_action =
                        Some(PlaylistDiscoveryNavigationAction::Automatic(result));
                }
            }
            _ => {}
        }
        if !resolved_wait {
            return;
        }
        match outcome {
            DiscoveryFinalOutcome::Completed | DiscoveryFinalOutcome::LimitReached => {
                self.navigation_status = PlaylistDiscoveryNavigationStatus::Exhausted {
                    scope_id: active.scope_id,
                };
            }
            DiscoveryFinalOutcome::Cancelled(cause) => {
                self.navigation_action = Some(PlaylistDiscoveryNavigationAction::ScopeCancelled {
                    scope_id: active.scope_id,
                    cause,
                });
                self.navigation_status = PlaylistDiscoveryNavigationStatus::Cancelled {
                    scope_id: active.scope_id,
                    cause,
                };
            }
            DiscoveryFinalOutcome::ExecutorDisconnected => {
                self.navigation_action = Some(PlaylistDiscoveryNavigationAction::ScopeFatal {
                    scope_id: active.scope_id,
                });
                self.navigation_status = PlaylistDiscoveryNavigationStatus::Fatal {
                    scope_id: active.scope_id,
                };
            }
        }
    }

    fn route_admission_advanced(
        &mut self,
        controller: &mut PlaylistController,
        active: &mut ActiveDiscoveryScope,
        marker: AdmissionAdvanced,
    ) {
        let Some(side_index) = correlated_direction_index(
            active,
            marker.job_id(),
            marker.request_revision(),
            marker.policy_revision(),
            marker.direction(),
        ) else {
            return;
        };
        if !accept_monotonic_revision(
            &mut active.admission_revisions[side_index],
            marker.revision(),
        ) {
            return;
        }
        match controller.discovery_navigation_interest() {
            DiscoveryNavigationInterest::Manual {
                wait_id,
                scope_id,
                direction: ManualNavigationDirection::Next,
                shuffle: true,
            } if scope_id == active.scope_id => {
                let result = controller.resume_manual_navigation_wait(wait_id, scope_id, false);
                self.record_manual_action(scope_id, result);
            }
            DiscoveryNavigationInterest::Automatic {
                scope_id,
                shuffle: true,
            } if scope_id == active.scope_id => {
                let result = controller.resume_deferred_automatic_advance(
                    scope_id,
                    AutomaticDiscoveryReadiness::CommittedAdmissionAdvanced,
                );
                self.record_automatic_action(scope_id, result);
            }
            _ => {}
        }
    }

    fn route_frontier_ready(
        &mut self,
        controller: &mut PlaylistController,
        active: &mut ActiveDiscoveryScope,
        ready: FrontierReady,
    ) {
        let Some(side_index) = correlated_direction_index(
            active,
            ready.job_id(),
            ready.request_revision(),
            ready.policy_revision(),
            ready.direction(),
        ) else {
            return;
        };
        if !accept_monotonic_revision(
            &mut active.readiness_revisions[side_index],
            ready.revision(),
        ) {
            return;
        }
        let Some(item_id) = active
            .committed_ids_by_key
            .get(&ready.candidate_key())
            .copied()
        else {
            return;
        };
        match controller.discovery_navigation_interest() {
            DiscoveryNavigationInterest::Manual {
                wait_id,
                scope_id,
                direction,
                shuffle: false,
            } if scope_id == active.scope_id
                && admission_direction_matches(direction, ready.direction()) =>
            {
                let result = controller.resume_manual_navigation_exact(wait_id, scope_id, item_id);
                self.record_manual_action(scope_id, result);
            }
            DiscoveryNavigationInterest::Automatic {
                scope_id,
                shuffle: false,
            } if scope_id == active.scope_id => {
                let result = controller.resume_deferred_automatic_advance(
                    scope_id,
                    AutomaticDiscoveryReadiness::ExactNaturalNext { item_id },
                );
                self.record_automatic_action(scope_id, result);
            }
            _ => {}
        }
    }

    fn record_manual_action(
        &mut self,
        scope_id: super::super::controller::SiblingDiscoveryScopeId,
        result: ControllerManualNavigationOutcome,
    ) {
        if let ControllerManualNavigationOutcome::StartInstall { install } = &result {
            self.navigation_status = PlaylistDiscoveryNavigationStatus::TargetReady {
                scope_id,
                item_id: install.item_id,
            };
        }
        if !matches!(
            result,
            ControllerManualNavigationOutcome::Waiting { .. }
                | ControllerManualNavigationOutcome::StaleWait { .. }
        ) {
            self.navigation_action = Some(PlaylistDiscoveryNavigationAction::Manual(result));
        }
    }

    fn record_automatic_action(
        &mut self,
        scope_id: super::super::controller::SiblingDiscoveryScopeId,
        result: AutomaticLifecycleOutcome,
    ) {
        if let AutomaticLifecycleOutcome::OpenItem { install } = &result {
            self.navigation_status = PlaylistDiscoveryNavigationStatus::TargetReady {
                scope_id,
                item_id: install.item_id,
            };
        }
        if !matches!(
            result,
            AutomaticLifecycleOutcome::NoAction | AutomaticLifecycleOutcome::StaleObservation
        ) {
            self.navigation_action = Some(PlaylistDiscoveryNavigationAction::Automatic(result));
        }
    }

    fn synchronize_navigation_interest_for_active(
        &mut self,
        controller: &PlaylistController,
        active: &ActiveDiscoveryScope,
    ) {
        match controller.discovery_navigation_interest() {
            DiscoveryNavigationInterest::Manual {
                wait_id,
                scope_id,
                direction,
                ..
            } if scope_id == active.scope_id => {
                self.navigation_status = PlaylistDiscoveryNavigationStatus::WaitingManual {
                    scope_id,
                    wait_id,
                    direction,
                };
            }
            DiscoveryNavigationInterest::Automatic { scope_id, .. }
                if scope_id == active.scope_id =>
            {
                self.navigation_status =
                    PlaylistDiscoveryNavigationStatus::WaitingAutomatic { scope_id };
            }
            _ if self.navigation_action.is_none() => {
                self.navigation_status = PlaylistDiscoveryNavigationStatus::Idle;
            }
            _ => {}
        }
    }
}

fn correlated_direction_index(
    active: &ActiveDiscoveryScope,
    job_id: playlist_discovery::DiscoveryJobId,
    request_revision: playlist_discovery::DiscoveryRequestRevision,
    policy_revision: Option<playlist_discovery::SiblingPolicyRevision>,
    direction: AdmissionDirection,
) -> Option<usize> {
    if active.job.id() != job_id
        || active.request_revision != request_revision
        || Some(active.policy_revision) != policy_revision
    {
        return None;
    }
    match direction {
        AdmissionDirection::Before => Some(0),
        AdmissionDirection::After => Some(1),
        AdmissionDirection::NonDirectional => None,
    }
}

fn admission_direction_matches(
    direction: ManualNavigationDirection,
    admission_direction: AdmissionDirection,
) -> bool {
    matches!(
        (direction, admission_direction),
        (
            ManualNavigationDirection::Previous,
            AdmissionDirection::Before
        ) | (ManualNavigationDirection::Next, AdmissionDirection::After)
    )
}

/// Latest-only frontier revision отвергает duplicate/out-of-order completion без FIFO.
fn accept_monotonic_revision(last_accepted: &mut u64, candidate: u64) -> bool {
    if candidate <= *last_accepted {
        return false;
    }
    *last_accepted = candidate;
    true
}

#[allow(dead_code, reason = "called by the Session 15A app action boundary")]
fn directional_priority_hint(
    active: &ActiveDiscoveryScope,
    direction: ManualNavigationDirection,
    shuffle: bool,
) -> Vec<ManifestCandidateKey> {
    let mut before = active
        .manifest
        .records()
        .iter()
        .map(|record| record.candidate_key())
        .filter(|key| *key < active.target_key)
        .collect::<Vec<_>>();
    let mut after = active
        .manifest
        .records()
        .iter()
        .map(|record| record.candidate_key())
        .filter(|key| *key > active.target_key)
        .collect::<Vec<_>>();
    before.reverse();
    if shuffle {
        // Responsive shuffle приоритизирует admission, но target всё равно выбирает domain.
        after.extend(before);
        return after;
    }
    match direction {
        ManualNavigationDirection::Previous => before,
        ManualNavigationDirection::Next => after,
    }
}
