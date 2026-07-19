//! Post-Installed восстановление позиции до playback intent и lineage registration.

use super::{
    AppState, InstalledSingleMediaOpen, MediaInstallCompletion, NonZeroU64, PendingStrongMediaOpen,
    PendingStrongMediaOpenPhase, PlaybackIntentRevision, PlaylistRuntime, StrongMediaOpenError,
    StrongMediaOpenPoll,
};

/// Post-Installed resume facts передаются вместе и не расползаются по positional arguments.
#[derive(Clone, Copy)]
pub(super) struct InstalledResumeCommit {
    pub(super) media_instance_id: player_core::MediaInstanceId,
    pub(super) checkpoint_position: crate::playlist_runtime::InstalledCheckpointPosition,
    pub(super) warning: Option<crate::playlist_runtime::ResumePositionWarning>,
}

impl AppState {
    /// Installed забирается exactly once, затем restore position предшествует intent/lineage.
    pub(super) fn begin_post_installed_intent(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        pending: &mut PendingStrongMediaOpen,
    ) -> StrongMediaOpenPoll {
        let terminal = match playlist_runtime.take_media_open_terminal(pending.request_id) {
            Ok(Some(terminal)) => terminal,
            Ok(None) => {
                return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::MissingTerminal));
            }
            Err(error) => {
                return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::Command(error)));
            }
        };
        let Some(candidate_owner) = pending.candidate_owner.take() else {
            return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::MissingTerminal));
        };
        let Some(source) = pending.source.clone() else {
            return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::MissingTerminal));
        };
        let installed = match self.finish_media_open_terminal(candidate_owner, source, terminal) {
            Ok(installed) => installed,
            Err(error) => return StrongMediaOpenPoll::completed(Err(error)),
        };
        let MediaInstallCompletion::Installed {
            media_instance_id, ..
        } = installed.completion
        else {
            return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::MissingTerminal));
        };
        let snapshot = self.refresh_player_snapshot();
        if snapshot.media_instance_id != Some(media_instance_id) {
            return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::MissingTerminal));
        }
        let initial_checkpoint_position = if snapshot.timeline.seekable {
            crate::playlist_runtime::InstalledCheckpointPosition::Seekable(
                snapshot.current_position,
            )
        } else {
            crate::playlist_runtime::InstalledCheckpointPosition::NonSeekable
        };
        match pending.startup_position {
            crate::playlist_runtime::StartupPosition::KeepStart => self.begin_playback_intent(
                pending,
                installed,
                media_instance_id,
                initial_checkpoint_position,
                None,
            ),
            crate::playlist_runtime::StartupPosition::Restore(requested_position)
                if snapshot.timeline.seekable
                    && snapshot
                        .duration
                        .is_some_and(|duration| requested_position > duration) =>
            {
                self.begin_playback_intent(
                    pending,
                    installed,
                    media_instance_id,
                    initial_checkpoint_position,
                    Some(crate::playlist_runtime::ResumePositionWarning {
                        requested_position,
                        available_position: snapshot.current_position,
                    }),
                )
            }
            crate::playlist_runtime::StartupPosition::Restore(requested_position) => {
                let restore = player_core::InstalledMediaStateRestore {
                    request_id: installed.player_request_id,
                    media_instance_id,
                    video_track: player_core::InstalledTrackRestore::KeepDefault,
                    audio_track: player_core::InstalledTrackRestore::KeepDefault,
                    subtitle_track: player_core::InstalledSubtitleRestore::KeepDefault,
                    position: player_core::InstalledPositionRestore::SeekTo(requested_position),
                };
                match self.player_worker.restore_installed_media_state(restore) {
                    Ok(receipt) => {
                        pending.phase = PendingStrongMediaOpenPhase::PositionRestore {
                            installed,
                            media_instance_id,
                            requested_position,
                            receipt,
                        };
                        StrongMediaOpenPoll::Pending
                    }
                    Err(error) => StrongMediaOpenPoll::completed(Err(
                        StrongMediaOpenError::PositionRestoreDispatch(player_dispatch_rejection(
                            error,
                        )),
                    )),
                }
            }
        }
    }

    pub(super) fn poll_strong_media_position_restore(
        &mut self,
        _playlist_runtime: &mut PlaylistRuntime,
        pending: &mut PendingStrongMediaOpen,
        installed: &mut InstalledSingleMediaOpen,
        media_instance_id: player_core::MediaInstanceId,
        requested_position: std::time::Duration,
        receipt: &player_core::InstalledMediaStateRestoreReceipt,
    ) -> StrongMediaOpenPoll {
        let outcome = match receipt.try_take_outcome() {
            Ok(Some(outcome)) => outcome,
            Ok(None) => return StrongMediaOpenPoll::Pending,
            Err(_) => {
                return StrongMediaOpenPoll::completed(Err(
                    StrongMediaOpenError::PositionRestoreReceipt,
                ));
            }
        };
        match outcome {
            player_core::InstalledMediaStateRestoreOutcome::Applied {
                media_instance_id: applied,
            } if applied == media_instance_id => self.begin_playback_intent(
                pending,
                installed.clone(),
                media_instance_id,
                crate::playlist_runtime::InstalledCheckpointPosition::Seekable(requested_position),
                None,
            ),
            player_core::InstalledMediaStateRestoreOutcome::PositionUnavailable {
                media_instance_id: applied,
                requested_position,
                available_position,
                ..
            } if applied == media_instance_id => self.begin_playback_intent(
                pending,
                installed.clone(),
                media_instance_id,
                crate::playlist_runtime::InstalledCheckpointPosition::NonSeekable,
                Some(crate::playlist_runtime::ResumePositionWarning {
                    requested_position,
                    available_position,
                }),
            ),
            outcome => {
                StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::PositionRestore(outcome)))
            }
        }
    }

    fn begin_playback_intent(
        &mut self,
        pending: &mut PendingStrongMediaOpen,
        installed: InstalledSingleMediaOpen,
        media_instance_id: player_core::MediaInstanceId,
        checkpoint_position: crate::playlist_runtime::InstalledCheckpointPosition,
        warning: Option<crate::playlist_runtime::ResumePositionWarning>,
    ) -> StrongMediaOpenPoll {
        let Some(next_revision) = pending.intent_revision.get().checked_add(1) else {
            return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::MissingTerminal));
        };
        let exact_revision = PlaybackIntentRevision::from_non_zero(
            NonZeroU64::new(next_revision).expect("checked increment remains non-zero"),
        );
        let receipt =
            match self
                .player_worker
                .update_playback_intent(player_core::PlaybackIntentUpdate {
                    request_id: installed.player_request_id,
                    revision: exact_revision,
                    intent: pending.intent,
                }) {
                Ok(receipt) => receipt,
                Err(error) => {
                    return StrongMediaOpenPoll::completed(Err(
                        StrongMediaOpenError::PlaybackIntentDispatch(player_dispatch_rejection(
                            error,
                        )),
                    ));
                }
            };
        pending.phase = PendingStrongMediaOpenPhase::PlaybackIntent {
            installed,
            resume_commit: InstalledResumeCommit {
                media_instance_id,
                checkpoint_position,
                warning,
            },
            receipt,
        };
        StrongMediaOpenPoll::Pending
    }

    /// Завершает lineage/domain commit только после неблокирующего intent acknowledgement.
    pub(super) fn poll_strong_media_intent(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        pending: &PendingStrongMediaOpen,
        installed: &mut InstalledSingleMediaOpen,
        resume_commit: InstalledResumeCommit,
        receipt: &player_core::PlaybackIntentUpdateReceipt,
    ) -> StrongMediaOpenPoll {
        let Some(outcome) = receipt.try_outcome() else {
            return StrongMediaOpenPoll::Pending;
        };
        let confirmed_instance = match outcome {
            player_core::PlaybackIntentUpdateOutcome::AppliedToInstalled { media_instance_id } => {
                media_instance_id
            }
            outcome => {
                return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::PlaybackIntent(
                    outcome,
                )));
            }
        };
        if resume_commit.media_instance_id != confirmed_instance {
            return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::PlaybackIntent(
                player_core::PlaybackIntentUpdateOutcome::StaleInstance,
            )));
        }
        let binding = match self.playlist_runtime_binding() {
            Some(binding) => binding,
            None => {
                return StrongMediaOpenPoll::completed(Err(
                    StrongMediaOpenError::LineageRegistration(
                        crate::playlist_runtime::ResumeCheckpointError::StalePlayerBinding,
                    ),
                ));
            }
        };
        let active_media = match playlist_runtime.register_successful_strong_install(
            pending.request_id,
            installed.player_request_id,
            resume_commit.media_instance_id,
            binding,
            installed.source.clone(),
            pending.intent,
        ) {
            Ok(active_media) => active_media,
            Err(error) => {
                return StrongMediaOpenPoll::completed(Err(
                    StrongMediaOpenError::LineageRegistration(error),
                ));
            }
        };
        playlist_runtime.record_installed_resume_checkpoint(
            binding.binding_generation(),
            resume_commit.media_instance_id,
            resume_commit.checkpoint_position,
        );
        if let Some(item_id) = active_media.item_id() {
            let cache_outcome = playlist_runtime
                .record_successful_item_open_metadata(item_id, &installed.descriptor);
            tracing::debug!(
                ?cache_outcome,
                "Exact Installed обновил last-known playlist metadata cache"
            );
        }
        StrongMediaOpenPoll::completed(Ok(InstalledSingleMediaOpen {
            player_request_id: installed.player_request_id,
            completion: installed.completion.clone(),
            source: installed.source.clone(),
            descriptor: installed.descriptor.clone(),
            position_warning: resume_commit.warning,
        }))
    }
}

fn player_dispatch_rejection(
    error: player_core::PlayerWorkerSendError,
) -> crate::media_open::PlayerDispatchRejection {
    match error {
        player_core::PlayerWorkerSendError::Full => {
            crate::media_open::PlayerDispatchRejection::Backpressure
        }
        player_core::PlayerWorkerSendError::Disconnected => {
            crate::media_open::PlayerDispatchRejection::Disconnected
        }
    }
}
