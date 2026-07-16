//! Guarded-переход от paused explicit target к первому committed элементу новой очереди.

use player_core::{ExactMediaTransportAction, ExactMediaTransportRequest, PlaybackIntent};
use playlist_core::{PlaylistItemId, ReservedQueueMutation};

use super::{
    ControllerStableIntentDispatch, PlannedPlaylistInstall, PlaylistController,
    PlaylistInstallMutation, StablePlaybackIntent,
};
use crate::playlist_runtime::identity::{
    ActiveMediaIdentity, PendingTargetOrigin, TransportActionOrigin,
};

/// Opaque guard связывает отложенный старт с exact paused lineage и transport revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InitialQueuePlaybackGuard {
    expected_active_media: ActiveMediaIdentity,
    expected_stable_intent_revision: u64,
    target_item_id: PlaylistItemId,
    desired_intent: StablePlaybackIntent,
}

/// Ошибка создания guard-а сразу после explicit target install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InitialQueuePlaybackGuardError {
    /// Exact Installed ещё не опубликовал committed active Item ID.
    MissingCommittedActiveItem,
    /// Другой install всё ещё владеет linearization boundary.
    InstallInProgress,
}

/// Причина, по которой готовый discovery intent больше нельзя применять.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InitialQueuePlaybackPlanError {
    /// Пользовательский transport/lifecycle intent уже заменил captured guard.
    Superseded,
    /// Нарушен обязательный инвариант: committed queue неожиданно пуста.
    EmptyQueue,
    /// Monotonic stable intent revision исчерпан.
    IntentRevisionExhausted,
}

/// Единственное действие, которое UI adapter применяет через существующий strong boundary.
pub(crate) enum ControllerInitialQueuePlaybackAction {
    /// Выбранный target уже является первым: достаточно exact restart/resume без reopen.
    RestartCurrent {
        request: ExactMediaTransportRequest,
        intent_dispatch: ControllerStableIntentDispatch,
    },
    /// Первый Item ID отличается от paused target и требует обычного planned install-а.
    InstallFirst {
        install: PlannedPlaylistInstall,
        intent_dispatch: ControllerStableIntentDispatch,
    },
}

impl PlaylistController {
    /// Захватывает exact guard после установки выбранного файла в paused состоянии.
    pub(crate) fn begin_initial_queue_playback(
        &mut self,
        desired_intent: StablePlaybackIntent,
    ) -> Result<InitialQueuePlaybackGuard, InitialQueuePlaybackGuardError> {
        if self.install_state.is_some() {
            return Err(InitialQueuePlaybackGuardError::InstallInProgress);
        }
        let Some(expected_active_media) = self.active_media else {
            return Err(InitialQueuePlaybackGuardError::MissingCommittedActiveItem);
        };
        let Some(target_item_id) = expected_active_media.item_id() else {
            return Err(InitialQueuePlaybackGuardError::MissingCommittedActiveItem);
        };
        if self.queue.item(target_item_id).is_none() {
            return Err(InitialQueuePlaybackGuardError::MissingCommittedActiveItem);
        }

        // Strong open уже установил exact target с `StartPaused`. Синхронизируем app-owned
        // stable intent без новой пользовательской revision: старая очередь могла играть,
        // но её intent не должен ошибочно отменять guarded start новой directory queue.
        self.stable_playback_intent = StablePlaybackIntent::Paused;

        Ok(InitialQueuePlaybackGuard {
            expected_active_media,
            expected_stable_intent_revision: self.stable_intent_revision,
            target_item_id,
            desired_intent,
        })
    }

    /// После доказанного начала очереди выбирает первый Item ID без обхода controller policy.
    pub(crate) fn finish_initial_queue_playback(
        &mut self,
        guard: InitialQueuePlaybackGuard,
    ) -> Result<ControllerInitialQueuePlaybackAction, InitialQueuePlaybackPlanError> {
        if self.active_media != Some(guard.expected_active_media)
            || self.stable_intent_revision != guard.expected_stable_intent_revision
            || self.stable_playback_intent != StablePlaybackIntent::Paused
            || self.install_state.is_some()
            || self.queue.item(guard.target_item_id).is_none()
        {
            return Err(InitialQueuePlaybackPlanError::Superseded);
        }

        let first_item_id = self
            .queue
            .items()
            .first()
            .map(playlist_core::PlaylistItem::item_id)
            .ok_or(InitialQueuePlaybackPlanError::EmptyQueue)?;
        let playback_intent = playback_intent(guard.desired_intent);
        let Some(mut intent_dispatch) =
            self.record_stable_transport_intent(guard.desired_intent, TransportActionOrigin::Ui)
        else {
            return Err(InitialQueuePlaybackPlanError::IntentRevisionExhausted);
        };

        // Нельзя запускать paused target перед install-ом первого элемента.
        intent_dispatch.exact_current = None;
        self.stop_after_current = None;

        if first_item_id == guard.target_item_id {
            return Ok(ControllerInitialQueuePlaybackAction::RestartCurrent {
                request: ExactMediaTransportRequest {
                    media_instance_id: guard.expected_active_media.media_instance_id(),
                    action: ExactMediaTransportAction::RestartFromBeginning {
                        intent: playback_intent,
                    },
                },
                intent_dispatch,
            });
        }

        Ok(ControllerInitialQueuePlaybackAction::InstallFirst {
            install: PlannedPlaylistInstall {
                item_id: first_item_id,
                playback_intent,
                intent_revision: intent_dispatch.revision,
                pending_origin: PendingTargetOrigin::ExplicitOpen,
                expected_queue_revision: self.queue.revision_snapshot(),
                mutation: PlaylistInstallMutation::Reserved(
                    ReservedQueueMutation::select_committed(first_item_id),
                ),
            },
            intent_dispatch,
        })
    }
}

/// Явно переводит app-owned stable intent в player install intent.
const fn playback_intent(intent: StablePlaybackIntent) -> PlaybackIntent {
    match intent {
        StablePlaybackIntent::Playing => PlaybackIntent::StartPlaying,
        StablePlaybackIntent::Paused => PlaybackIntent::StartPaused,
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::path::PathBuf;

    use player_core::MediaInstanceId;
    use playlist_core::{
        CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind,
    };

    use super::*;
    use crate::playlist_runtime::PlaylistBindingGeneration;
    use crate::playlist_runtime::controller::ControllerAppendOutcome;
    use crate::playlist_runtime::identity::ActiveMediaLineageId;

    /// Строит non-zero ID без повторения unwrap-логики в каждом тесте.
    fn non_zero(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("test identity must be non-zero")
    }

    /// Создаёт минимальную local строку новой директории.
    fn local_draft(path: &str) -> PlaylistItemDraft {
        PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from(path)),
            None,
            CachedPlaylistMetadata::new(path, PlaylistMediaKind::Video),
        )
    }

    /// Создаёт controller, в котором selected target уже установлен paused.
    fn installed_target_controller(
        target_index: usize,
    ) -> (PlaylistController, Vec<PlaylistItemId>) {
        let mut controller = PlaylistController::new();
        let outcome = controller
            .append(vec![
                local_draft("/media/new/01.mkv"),
                local_draft("/media/new/02.mkv"),
                local_draft("/media/new/03.mkv"),
            ])
            .expect("test append must succeed");
        let ControllerAppendOutcome::Added { item_ids, .. } = outcome else {
            panic!("test append must add rows");
        };
        let target_item_id = item_ids[target_index];
        controller
            .queue
            .set_traversal_current(target_item_id)
            .expect("target must be committed");
        controller.active_media = Some(ActiveMediaIdentity::installed(
            Some(target_item_id),
            ActiveMediaLineageId::from_non_zero(non_zero(1)),
            MediaInstanceId::from_non_zero(non_zero(2)),
            PlaylistBindingGeneration(3),
        ));
        (controller, item_ids)
    }

    #[test]
    fn middle_target_plans_first_committed_item_without_starting_target() {
        let (mut controller, item_ids) = installed_target_controller(1);
        let guard = controller
            .begin_initial_queue_playback(StablePlaybackIntent::Playing)
            .expect("paused target must create guard");

        let action = controller
            .finish_initial_queue_playback(guard)
            .expect("unchanged guard must finish");

        let ControllerInitialQueuePlaybackAction::InstallFirst {
            install,
            intent_dispatch,
        } = action
        else {
            panic!("middle target must install the first row");
        };
        assert_eq!(install.item_id, item_ids[0]);
        assert_eq!(install.playback_intent, PlaybackIntent::StartPlaying);
        assert!(intent_dispatch.exact_current.is_none());
    }

    #[test]
    fn first_target_uses_exact_restart_and_preserves_paused_autoplay_policy() {
        let (mut controller, _) = installed_target_controller(0);
        let guard = controller
            .begin_initial_queue_playback(StablePlaybackIntent::Paused)
            .expect("paused target must create guard");

        let action = controller
            .finish_initial_queue_playback(guard)
            .expect("unchanged guard must finish");

        let ControllerInitialQueuePlaybackAction::RestartCurrent { request, .. } = action else {
            panic!("first target must not be reopened");
        };
        assert_eq!(
            request.action,
            ExactMediaTransportAction::RestartFromBeginning {
                intent: PlaybackIntent::StartPaused,
            }
        );
    }

    #[test]
    fn newer_manual_transport_intent_supersedes_delayed_directory_start() {
        let (mut controller, _) = installed_target_controller(1);
        let guard = controller
            .begin_initial_queue_playback(StablePlaybackIntent::Playing)
            .expect("paused target must create guard");
        let _manual_dispatch = controller
            .record_stable_transport_intent(
                StablePlaybackIntent::Playing,
                TransportActionOrigin::Ui,
            )
            .expect("manual intent revision must advance");

        assert_eq!(
            controller.finish_initial_queue_playback(guard).err(),
            Some(InitialQueuePlaybackPlanError::Superseded)
        );
    }

    #[test]
    fn previous_playing_queue_cannot_cancel_new_paused_target_guard() {
        let (mut controller, item_ids) = installed_target_controller(1);
        let _old_queue_dispatch = controller
            .record_stable_transport_intent(
                StablePlaybackIntent::Playing,
                TransportActionOrigin::Ui,
            )
            .expect("old queue intent revision must advance");

        let guard = controller
            .begin_initial_queue_playback(StablePlaybackIntent::Playing)
            .expect("installed paused target must replace stale old-queue intent");
        let action = controller
            .finish_initial_queue_playback(guard)
            .expect("synchronized guard must remain valid");

        let ControllerInitialQueuePlaybackAction::InstallFirst { install, .. } = action else {
            panic!("middle target must still select the first row");
        };
        assert_eq!(install.item_id, item_ids[0]);
    }
}
