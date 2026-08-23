//! Runtime-side execution seams для Session 18A transport adapter-а.
//!
//! Эти методы не открывают media и не отправляют player commands: они сохраняют queue/controller
//! ownership, выдают locator по exact plan и принимают correlated D08/D39 request обратно.

use std::sync::Arc;

use player_core::{MediaInstallRequestId, MediaInstanceId};
use playlist_core::PlaylistLocator;

use super::controller::{
    AutomaticLifecycleOutcome, AutomaticTargetFailureOutcome, ControllerManualNavigationOutcome,
    ControllerStableIntentDispatch, PlannedPlaylistInstall, PlaylistInstallRequest,
    UnstagedPlannedTargetFailureOutcome,
};
use super::controller::{ManualNavigationCancelOutcome, ManualNavigationFailureOutcome};
use super::discovery::PlaylistDiscoveryNavigationStatus;
use super::identity::TransportActionOrigin;
use super::{PlaylistMediaOpenGateError, PlaylistRuntime};
use crate::media_open::MediaOpenRequestId;

/// Cancel различает manual/automatic wait и безопасный no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistTransportCancelOutcome {
    CancelledManual,
    CancelledAutomatic,
    NoPendingWait,
}

/// Relative BeyondEnd либо адресует тот же active media, либо остаётся typed stale no-op.
pub(crate) enum RelativeBeyondEndNavigationOutcome {
    Navigation {
        outcome: ControllerManualNavigationOutcome,
    },
    StaleInstance {
        outcome_media_instance_id: MediaInstanceId,
        current_media_instance_id: Option<MediaInstanceId>,
    },
    Unavailable,
}

/// Exact queue-owned open intent без раскрытия item payload storage app renderer-у.
pub(crate) struct PlaylistMediaOpenIntent {
    locator: PlaylistLocator,
    playback_window: Option<player_core::MediaPlaybackWindow>,
}

impl PlaylistMediaOpenIntent {
    /// Передаёт operational locator app service registry.
    pub(crate) fn locator(&self) -> &PlaylistLocator {
        &self.locator
    }

    /// Возвращает neutral playback window, если item является bounded fragment-ом.
    pub(crate) const fn playback_window(&self) -> Option<player_core::MediaPlaybackWindow> {
        self.playback_window
    }
}

impl PlaylistRuntime {
    /// Выполняет Next только относительно exact media instance, породившего BeyondEnd.
    pub(crate) fn request_relative_beyond_end_navigation(
        &mut self,
        outcome_media_instance_id: MediaInstanceId,
        current_position: std::time::Duration,
    ) -> RelativeBeyondEndNavigationOutcome {
        let current_media_instance_id = self
            .controller
            .as_ref()
            .and_then(|controller| controller.active_media())
            .map(|active| active.media_instance_id());
        if current_media_instance_id != Some(outcome_media_instance_id) {
            return RelativeBeyondEndNavigationOutcome::StaleInstance {
                outcome_media_instance_id,
                current_media_instance_id,
            };
        }
        let Some(outcome) = self.request_playlist_navigation(
            playlist_core::ManualNavigationDirection::Next,
            TransportActionOrigin::Mpris,
            current_position,
        ) else {
            return RelativeBeyondEndNavigationOutcome::Unavailable;
        };
        RelativeBeyondEndNavigationOutcome::Navigation { outcome }
    }

    /// Commit-ит app-level Stopped только после exact player owner success.
    pub(crate) fn apply_neutral_stop_outcome(
        &mut self,
        outcome: &player_core::ExactMediaTransportOutcome,
    ) -> bool {
        self.controller
            .as_mut()
            .is_some_and(|controller| controller.apply_neutral_stop_outcome(outcome))
    }

    /// Выдаёт locator + optional window только для exact revision/item controller plan-а.
    pub(crate) fn media_open_intent_for_planned_install(
        &self,
        install: &PlannedPlaylistInstall,
    ) -> Result<PlaylistMediaOpenIntent, PlaylistMediaOpenGateError> {
        let controller = self
            .controller
            .as_ref()
            .ok_or(PlaylistMediaOpenGateError::LoadDecisionPending)?;
        if controller.queue().revision_snapshot() != install.expected_queue_revision {
            return Err(PlaylistMediaOpenGateError::StalePlannedTarget);
        }
        let item = controller
            .queue()
            .item(install.item_id)
            .ok_or(PlaylistMediaOpenGateError::StalePlannedTarget)?;
        let playback_window = item
            .durable_payload()
            .and_then(playlist_core::PlaylistSingleDurablePayload::playback_span)
            .map(|span| {
                player_core::MediaPlaybackWindow::new(span.start(), span.end_exclusive())
                    .map_err(|_| PlaylistMediaOpenGateError::InvalidPlaybackSpan)
            })
            .transpose()?;
        Ok(PlaylistMediaOpenIntent {
            locator: item.locator().clone(),
            playback_window,
        })
    }

    /// Связывает обычный manual/automatic plan с уже staged player request.
    pub(crate) fn accept_planned_playlist_install(
        &mut self,
        request_id: MediaOpenRequestId,
        player_request_id: MediaInstallRequestId,
        install: PlannedPlaylistInstall,
    ) -> Result<(), PlaylistMediaOpenGateError> {
        let controller = self
            .controller
            .as_mut()
            .ok_or(PlaylistMediaOpenGateError::LoadDecisionPending)?;
        controller
            .accept_install_request(playlist_install_request(
                request_id,
                player_request_id,
                install,
            ))
            .map_err(PlaylistMediaOpenGateError::InstallAdmission)
    }

    /// D53 заменяет только exact AwaitingReady request; FIFO не создаётся.
    pub(crate) fn accept_superseding_playlist_install(
        &mut self,
        expected_request_id: MediaOpenRequestId,
        request_id: MediaOpenRequestId,
        player_request_id: MediaInstallRequestId,
        install: PlannedPlaylistInstall,
    ) -> Result<(), PlaylistMediaOpenGateError> {
        let controller = self
            .controller
            .as_mut()
            .ok_or(PlaylistMediaOpenGateError::LoadDecisionPending)?;
        controller
            .supersede_install_request_before_ready(
                expected_request_id,
                playlist_install_request(request_id, player_request_id, install),
            )
            .map_err(PlaylistMediaOpenGateError::InstallAdmission)
    }

    /// Toggle опирается на controller-owned stable intent, а не на transient player state.
    pub(crate) fn toggle_ui_stable_transport_intent(
        &mut self,
    ) -> Option<ControllerStableIntentDispatch> {
        self.controller
            .as_mut()?
            .toggle_stable_transport_intent(TransportActionOrigin::Ui)
    }

    /// D52 адресует coordinator request, не заставляя AppState угадывать ID mapping.
    pub(crate) fn apply_stable_pending_intent_update(
        &self,
        dispatch: &ControllerStableIntentDispatch,
    ) -> Result<
        Option<(MediaOpenRequestId, player_core::PlaybackIntentUpdateReceipt)>,
        crate::media_open::MediaOpenCommandError,
    > {
        let Some(update) = dispatch.pending_update else {
            return Ok(None);
        };
        let request_id = self
            .controller
            .as_ref()
            .and_then(|controller| controller.install_request_id())
            .ok_or(crate::media_open::MediaOpenCommandError::StaleRequest)?;
        let receipt =
            self.media_open
                .update_playback_intent(request_id, update.revision, update.intent)?;
        Ok(Some((request_id, receipt)))
    }

    /// Preparation/player failure маршрутизируется владельцу exact navigation plan-а.
    pub(crate) fn report_playlist_navigation_failure(
        &mut self,
        request_id: MediaOpenRequestId,
        item_id: playlist_core::PlaylistItemId,
    ) -> Option<PlannedPlaylistInstall> {
        let mut automatic_continuation = None;
        if let Some(controller) = self.controller.as_mut() {
            match controller.report_automatic_target_failure(
                request_id,
                Arc::from("Не удалось подготовить следующий элемент очереди"),
            ) {
                AutomaticTargetFailureOutcome::OpenItem { install } => {
                    automatic_continuation = Some(install);
                }
                AutomaticTargetFailureOutcome::Stopped { .. } => {}
                AutomaticTargetFailureOutcome::StaleRequest { .. } => {
                    let outcome = controller.report_manual_navigation_target_failure(request_id);
                    if matches!(outcome, ManualNavigationFailureOutcome::NotManualNavigation) {
                        controller.report_unstaged_manual_navigation_target_failure(item_id);
                    }
                }
            }
            self.discovery.synchronize_navigation_interest(controller);
        }
        automatic_continuation
    }

    /// Синхронная source-boundary ошибка возникает ещё до появления media-open request ID.
    pub(crate) fn report_unstaged_playlist_navigation_failure(
        &mut self,
        item_id: playlist_core::PlaylistItemId,
    ) {
        if let Some(controller) = self.controller.as_mut() {
            controller.report_unstaged_manual_navigation_target_failure(item_id);
            self.discovery.synchronize_navigation_interest(controller);
        }
    }

    /// Pre-staging failure маршрутизируется по origin/mutation самого exact plan-а.
    pub(crate) fn report_unstaged_planned_playlist_navigation_failure(
        &mut self,
        install: PlannedPlaylistInstall,
    ) -> UnstagedPlannedTargetFailureOutcome {
        let Some(controller) = self.controller.as_mut() else {
            return UnstagedPlannedTargetFailureOutcome::RuntimeUnavailable;
        };
        let outcome = controller.report_unstaged_planned_target_failure(
            install,
            Arc::from("Не удалось подготовить следующий элемент очереди"),
        );
        self.discovery.synchronize_navigation_interest(controller);
        outcome
    }

    /// D58-like explicit Cancel убирает только navigation interest, bulk scan продолжает жить.
    pub(crate) fn cancel_global_playlist_navigation_wait(
        &mut self,
    ) -> PlaylistTransportCancelOutcome {
        let status = self.playlist_discovery_navigation_status();
        let Some(controller) = self.controller.as_mut() else {
            return PlaylistTransportCancelOutcome::NoPendingWait;
        };
        let outcome = match status {
            PlaylistDiscoveryNavigationStatus::WaitingManual {
                wait_id, scope_id, ..
            } if controller.cancel_manual_navigation_wait(wait_id, scope_id) => {
                PlaylistTransportCancelOutcome::CancelledManual
            }
            PlaylistDiscoveryNavigationStatus::WaitingAutomatic { .. } => {
                let automatic = controller.cancel_deferred_automatic_advance();
                if matches!(automatic, AutomaticLifecycleOutcome::NoAction) {
                    PlaylistTransportCancelOutcome::NoPendingWait
                } else {
                    PlaylistTransportCancelOutcome::CancelledAutomatic
                }
            }
            _ => PlaylistTransportCancelOutcome::NoPendingWait,
        };
        self.discovery.synchronize_navigation_interest(controller);
        outcome
    }

    /// Один UI intent маршрутизируется либо в D55 cursor Cancel, либо в D50 wait Cancel.
    pub(crate) fn cancel_playlist_navigation_from_ui(&mut self) -> bool {
        if self.controller.as_ref().is_some_and(|controller| {
            controller
                .view_snapshot()
                .awaiting_user_after_navigation_failure()
        }) {
            let Some(controller) = self.controller.as_mut() else {
                return false;
            };
            let outcome = controller.cancel_manual_navigation();
            self.discovery.synchronize_navigation_interest(controller);
            return match outcome {
                ManualNavigationCancelOutcome::NoManualNavigation => false,
                ManualNavigationCancelOutcome::Fatal(_) => {
                    self.set_playlist_safe_feedback("Не удалось отменить переход");
                    true
                }
                ManualNavigationCancelOutcome::Discarded(_)
                | ManualNavigationCancelOutcome::CancelPending { .. }
                | ManualNavigationCancelOutcome::AwaitAuthorizationResolution { .. }
                | ManualNavigationCancelOutcome::AwaitInstalled { .. } => true,
            };
        }
        !matches!(
            self.cancel_global_playlist_navigation_wait(),
            PlaylistTransportCancelOutcome::NoPendingWait
        )
    }
}

pub(super) fn playlist_install_request(
    request_id: MediaOpenRequestId,
    player_request_id: MediaInstallRequestId,
    install: PlannedPlaylistInstall,
) -> PlaylistInstallRequest {
    PlaylistInstallRequest {
        request_id,
        player_request_id,
        target_item_id: Some(install.item_id),
        origin: install.pending_origin,
        intent_revision: install.intent_revision,
        expected_queue_revision: install.expected_queue_revision,
        mutation: install.mutation,
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64};
    use std::path::PathBuf;
    use std::time::Duration;

    use media_core::MediaTime;
    use player_core::{MediaInstanceId, PlaybackState};
    use playlist_core::{
        CachedPlaylistMetadata, DurableReopenLocator, LocalLocator, PlaylistImportAvailability,
        PlaylistImportProvenance, PlaylistImportSourceKind, PlaylistItemDraft, PlaylistMediaKind,
        PlaylistPlaybackSpan, PlaylistSingleDurablePayload,
    };

    use super::*;
    use crate::app_wake::{AppWakeOwner, AppWakePort};
    use crate::media_open::AuthorizationDispatchResolution;
    use crate::playlist_runtime::PlaylistBindingGeneration;
    use crate::playlist_runtime::controller::{
        AutomaticDeferredAvailability, AutomaticLifecycleOutcome, ControllerAppendOutcome,
        ControllerManualNavigationOutcome, ControllerPlayItemOutcome,
        DiscoveryManualWaitAvailability, EndedSnapshotKind, InstallReadyOutcome,
        PlaylistController, PlaylistErrorBehavior, PreviousRestartThreshold,
    };
    use crate::playlist_runtime::identity::PendingTargetOrigin;

    fn non_zero(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("test identity is non-zero")
    }

    #[test]
    fn planned_cue_item_maps_durable_span_to_neutral_open_intent() {
        let physical_locator = LocalLocator::Native(PathBuf::from("/music/disc/album.flac"));
        let span =
            PlaylistPlaybackSpan::new(MediaTime::from_secs(60), Some(MediaTime::from_secs(120)))
                .expect("valid CUE window");
        let payload = PlaylistSingleDurablePayload::new(
            DurableReopenLocator::local(physical_locator.clone()),
            Some(span),
            Vec::new(),
            PlaylistImportProvenance::new(
                DurableReopenLocator::local(LocalLocator::Native(PathBuf::from(
                    "/music/disc/source.cue",
                ))),
                PlaylistImportSourceKind::Cue,
                NonZeroU32::new(2),
            ),
            PlaylistImportAvailability::Available,
        )
        .expect("durable CUE payload");
        let mut controller = PlaylistController::new();
        let item_id = match controller
            .append(vec![
                PlaylistItemDraft::local(
                    physical_locator.clone(),
                    None,
                    CachedPlaylistMetadata::new("CUE track 02", PlaylistMediaKind::Audio),
                )
                .with_durable_payload(payload),
            ])
            .expect("append CUE item")
        {
            crate::playlist_runtime::controller::ControllerAppendOutcome::Added {
                item_ids,
                ..
            } => item_ids[0],
            _ => panic!("focused append is non-empty"),
        };
        let ControllerPlayItemOutcome::StartInstall { install, .. } =
            controller.play_item(item_id, TransportActionOrigin::Ui)
        else {
            panic!("first Play starts exact install");
        };
        let mut runtime =
            PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
        runtime.controller.install(controller);
        let intent = runtime
            .media_open_intent_for_planned_install(&install)
            .expect("exact planned target");

        assert_eq!(intent.locator(), &PlaylistLocator::Local(physical_locator));
        let window = intent.playback_window().expect("CUE item keeps window");
        assert_eq!(window.start(), MediaTime::from_secs(60));
        assert_eq!(window.end_exclusive(), Some(MediaTime::from_secs(120)));
        assert_eq!(install.item_id, item_id);
    }

    #[test]
    fn first_pre_barrier_failure_routes_to_failed_anchor_and_next_opens_second_item() {
        let mut controller = PlaylistController::new();
        let item_ids = match controller
            .append(
                (0..3)
                    .map(|index| {
                        let label = format!("failed-first-{index}.webm");
                        PlaylistItemDraft::local(
                            LocalLocator::Native(PathBuf::from(&label)),
                            None,
                            CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
                        )
                    })
                    .collect(),
            )
            .expect("append first-failure fixture")
        {
            ControllerAppendOutcome::Added { item_ids, .. } => item_ids,
            ControllerAppendOutcome::NoItemsProvided => panic!("fixture is non-empty"),
        };
        let mut runtime =
            PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
        runtime.controller.install(controller);

        assert!(
            runtime
                .report_playlist_navigation_failure(
                    MediaOpenRequestId::from_non_zero(non_zero(390)),
                    item_ids[0],
                )
                .is_none(),
            "explicit first-item failure must not start automatic skipping"
        );
        let controller = runtime
            .controller
            .as_mut()
            .expect("controller remains installed");
        assert!(
            controller
                .view_snapshot()
                .awaiting_user_after_navigation_failure()
        );
        assert!(controller.queue.traversal_current().is_none());

        let ControllerManualNavigationOutcome::StartInstall { install } = controller
            .manual_navigation(
                playlist_core::ManualNavigationDirection::Next,
                TransportActionOrigin::Mpris,
                Duration::ZERO,
                PreviousRestartThreshold::from_milliseconds(0).expect("zero threshold"),
                DiscoveryManualWaitAvailability::Exhausted,
            )
        else {
            panic!("explicit Next must escape the failed first item")
        };
        assert_eq!(install.item_id, item_ids[1]);
        assert_eq!(
            install.pending_origin,
            PendingTargetOrigin::ManualNavigation {
                origin: TransportActionOrigin::Mpris
            }
        );
        assert!(controller.queue.traversal_current().is_none());
    }

    #[test]
    fn automatic_install_failure_returns_fixed_plan_continuation_instead_of_manual_fallback() {
        let mut controller = PlaylistController::new();
        let item_ids = match controller
            .append(
                (0..3)
                    .map(|index| {
                        let label = format!("transition-{index}.webm");
                        PlaylistItemDraft::local(
                            LocalLocator::Native(PathBuf::from(&label)),
                            None,
                            CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
                        )
                    })
                    .collect(),
            )
            .expect("append transition fixture")
        {
            ControllerAppendOutcome::Added { item_ids, .. } => item_ids,
            ControllerAppendOutcome::NoItemsProvided => panic!("transition fixture is non-empty"),
        };

        let ControllerPlayItemOutcome::StartInstall { install, .. } =
            controller.play_item(item_ids[0], TransportActionOrigin::Ui)
        else {
            panic!("first item starts strong install");
        };
        let first_request_id = MediaOpenRequestId::from_non_zero(non_zero(401));
        let first_player_request_id = MediaInstallRequestId::from_non_zero(non_zero(501));
        controller
            .accept_install_request(playlist_install_request(
                first_request_id,
                first_player_request_id,
                install,
            ))
            .expect("first install admission");
        assert!(matches!(
            controller.on_ready_to_commit(first_request_id),
            InstallReadyOutcome::RequestAuthorization { .. }
        ));
        controller
            .begin_authorization_dispatch(first_request_id)
            .expect("first authorization dispatch");
        controller
            .resolve_authorization_dispatch(
                first_request_id,
                AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
            )
            .expect("first enqueue barrier");
        controller
            .on_installed(
                first_request_id,
                first_player_request_id,
                MediaInstanceId::from_non_zero(non_zero(601)),
                PlaylistBindingGeneration(701),
            )
            .expect("first exact Installed");

        controller.set_error_behavior(PlaylistErrorBehavior::Skip);
        let active = controller.active_media().expect("first item is active");
        let AutomaticLifecycleOutcome::OpenItem { install } = controller
            .observe_automatic_snapshot(
                active.player_binding_generation(),
                Some(active.media_instance_id()),
                PlaybackState::Ended,
                EndedSnapshotKind::Clean,
                AutomaticDeferredAvailability::Unavailable,
            )
        else {
            panic!("clean EOF starts automatic transition");
        };
        assert_eq!(install.item_id, item_ids[1]);

        let failed_request_id = MediaOpenRequestId::from_non_zero(non_zero(402));
        let failed_player_request_id = MediaInstallRequestId::from_non_zero(non_zero(502));
        let mut runtime =
            PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
        runtime.controller.install(controller);
        runtime
            .accept_planned_playlist_install(failed_request_id, failed_player_request_id, install)
            .expect("automatic target admission");

        let continuation = runtime
            .report_playlist_navigation_failure(failed_request_id, item_ids[1])
            .expect("skip policy continues the fixed automatic plan");
        assert_eq!(continuation.item_id, item_ids[2]);
        assert!(
            !runtime
                .controller
                .as_ref()
                .expect("controller remains installed")
                .view_snapshot()
                .awaiting_user_after_navigation_failure(),
            "automatic failure must not enter the manual D55 cursor"
        );
    }
}
