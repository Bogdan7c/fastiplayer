//! Post-Installed восстановление позиции до playback intent и lineage registration.

use super::live_same_item_restore::{
    PendingPositionRestore, PositionRestoreOutcomeRoute, PositionRestoreTimeline,
    route_position_restore_outcome, same_lineage_position_restore,
};
use super::{
    AppState, InstalledSingleMediaOpen, MediaInstallCompletion, NonZeroU64,
    PendingStrongLineageCommit, PendingStrongMediaOpen, PendingStrongMediaOpenPhase,
    PlaybackIntentRevision, PlaylistRuntime, StrongMediaOpenError, StrongMediaOpenPoll,
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
        let video_swap_checkpoint = if let PendingStrongLineageCommit::SameLineage {
            video_swap_checkpoint,
            ..
        } = &mut pending.lineage_commit
        {
            let Some(video_swap_checkpoint) = video_swap_checkpoint.take() else {
                return StrongMediaOpenPoll::completed(Err(
                    StrongMediaOpenError::PendingPhaseStateLost,
                ));
            };
            Some(video_swap_checkpoint)
        } else {
            None
        };
        let installed = match Self::installed_media_from_terminal(source, terminal) {
            Ok(installed) => installed,
            Err(error) => return StrongMediaOpenPoll::completed(Err(error)),
        };
        if let Err(error) =
            self.commit_installed_video_candidate(&candidate_owner, installed.player_request_id)
        {
            return self.begin_post_installed_compensation(
                playlist_runtime,
                pending,
                installed,
                error,
            );
        }
        let MediaInstallCompletion::Installed {
            media_instance_id, ..
        } = installed.completion
        else {
            return self.begin_post_installed_compensation(
                playlist_runtime,
                pending,
                installed,
                StrongMediaOpenError::MissingTerminal,
            );
        };
        if let Some(video_swap_checkpoint) = video_swap_checkpoint {
            self.begin_backend_swap_video_freeze(*video_swap_checkpoint);
        }
        let snapshot = self.refresh_player_snapshot();
        if snapshot.media_instance_id != Some(media_instance_id) {
            return self.begin_post_installed_compensation(
                playlist_runtime,
                pending,
                installed,
                StrongMediaOpenError::MissingTerminal,
            );
        }
        let is_live = snapshot.timeline.mode == media_core::TimelineMode::Live;
        let initial_checkpoint_position = if is_live {
            crate::playlist_runtime::InstalledCheckpointPosition::Live
        } else if snapshot.timeline.seekable {
            crate::playlist_runtime::InstalledCheckpointPosition::Seekable(
                snapshot.current_position,
            )
        } else {
            crate::playlist_runtime::InstalledCheckpointPosition::NonSeekable
        };
        let same_lineage_restore = match &pending.lineage_commit {
            PendingStrongLineageCommit::SameLineage { restore, .. } => {
                let Some(restore) = restore.clone() else {
                    return self.begin_post_installed_compensation(
                        playlist_runtime,
                        pending,
                        installed,
                        StrongMediaOpenError::PendingPhaseStateLost,
                    );
                };
                Some(restore)
            }
            PendingStrongLineageCommit::NewLineageOrQueue => None,
        };
        if let Some(restore) = same_lineage_restore {
            let (position, timeline) =
                same_lineage_position_restore(snapshot.timeline.mode, restore.position);
            let restore_request = player_core::InstalledMediaStateRestore {
                request_id: installed.player_request_id,
                media_instance_id,
                video_track: applicable_track_restore(
                    restore.selected_tracks.video_track,
                    media_core::TrackKind::Video,
                    &snapshot.tracks,
                ),
                audio_track: applicable_track_restore(
                    restore.selected_tracks.audio_track,
                    media_core::TrackKind::Audio,
                    &snapshot.tracks,
                ),
                subtitle_track: subtitle_restore_from_selection(
                    restore.selected_tracks.subtitle_track,
                ),
                volume: player_core::InstalledVolumeRestore::Set(restore.volume),
                position,
            };
            return match self
                .player_worker
                .restore_installed_media_state(restore_request)
            {
                Ok(receipt) => {
                    pending.phase = PendingStrongMediaOpenPhase::PositionRestore {
                        installed,
                        media_instance_id,
                        restore: PendingPositionRestore {
                            requested_position: restore.position,
                            timeline,
                            receipt,
                        },
                    };
                    StrongMediaOpenPoll::Pending
                }
                Err(error) => self.begin_post_installed_compensation(
                    playlist_runtime,
                    pending,
                    installed,
                    StrongMediaOpenError::PositionRestoreDispatch(player_dispatch_rejection(error)),
                ),
            };
        }
        match pending.startup_position {
            crate::playlist_runtime::StartupPosition::KeepStart => self.begin_playback_intent(
                playlist_runtime,
                pending,
                installed,
                media_instance_id,
                initial_checkpoint_position,
                None,
            ),
            crate::playlist_runtime::StartupPosition::Restore(_) if is_live => self
                .begin_playback_intent(
                    playlist_runtime,
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
                    playlist_runtime,
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
                    volume: player_core::InstalledVolumeRestore::KeepCurrent,
                    position: player_core::InstalledPositionRestore::SeekTo(requested_position),
                };
                match self.player_worker.restore_installed_media_state(restore) {
                    Ok(receipt) => {
                        pending.phase = PendingStrongMediaOpenPhase::PositionRestore {
                            installed,
                            media_instance_id,
                            restore: PendingPositionRestore {
                                requested_position,
                                timeline: PositionRestoreTimeline::Static,
                                receipt,
                            },
                        };
                        StrongMediaOpenPoll::Pending
                    }
                    Err(error) => self.begin_post_installed_compensation(
                        playlist_runtime,
                        pending,
                        installed,
                        StrongMediaOpenError::PositionRestoreDispatch(player_dispatch_rejection(
                            error,
                        )),
                    ),
                }
            }
        }
    }

    pub(super) fn poll_strong_media_position_restore(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        pending: &mut PendingStrongMediaOpen,
        installed: &mut InstalledSingleMediaOpen,
        media_instance_id: player_core::MediaInstanceId,
        restore: &PendingPositionRestore,
    ) -> StrongMediaOpenPoll {
        let outcome = match restore.receipt.try_take_outcome() {
            Ok(Some(outcome)) => outcome,
            Ok(None) => return StrongMediaOpenPoll::Pending,
            Err(_) => {
                return self.begin_post_installed_compensation(
                    playlist_runtime,
                    pending,
                    installed.clone(),
                    StrongMediaOpenError::PositionRestoreReceipt,
                );
            }
        };
        match route_position_restore_outcome(
            outcome,
            media_instance_id,
            restore.requested_position,
            self.refresh_player_snapshot().current_position,
            restore.timeline,
        ) {
            PositionRestoreOutcomeRoute::Resume {
                checkpoint_position,
                warning,
            } => self.begin_playback_intent(
                playlist_runtime,
                pending,
                installed.clone(),
                media_instance_id,
                checkpoint_position,
                warning,
            ),
            PositionRestoreOutcomeRoute::Fail(outcome) => self.begin_post_installed_compensation(
                playlist_runtime,
                pending,
                installed.clone(),
                StrongMediaOpenError::PositionRestore(outcome),
            ),
        }
    }

    fn begin_playback_intent(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        pending: &mut PendingStrongMediaOpen,
        installed: InstalledSingleMediaOpen,
        media_instance_id: player_core::MediaInstanceId,
        checkpoint_position: crate::playlist_runtime::InstalledCheckpointPosition,
        warning: Option<crate::playlist_runtime::ResumePositionWarning>,
    ) -> StrongMediaOpenPoll {
        let Some(next_revision) = pending.intent_revision.get().checked_add(1) else {
            return self.begin_post_installed_compensation(
                playlist_runtime,
                pending,
                installed,
                StrongMediaOpenError::MissingTerminal,
            );
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
                    return self.begin_post_installed_compensation(
                        playlist_runtime,
                        pending,
                        installed,
                        StrongMediaOpenError::PlaybackIntentDispatch(player_dispatch_rejection(
                            error,
                        )),
                    );
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
        pending: &mut PendingStrongMediaOpen,
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
                return self.begin_post_installed_compensation(
                    playlist_runtime,
                    pending,
                    installed.clone(),
                    StrongMediaOpenError::PlaybackIntent(outcome),
                );
            }
        };
        if resume_commit.media_instance_id != confirmed_instance {
            return self.begin_post_installed_compensation(
                playlist_runtime,
                pending,
                installed.clone(),
                StrongMediaOpenError::PlaybackIntent(
                    player_core::PlaybackIntentUpdateOutcome::StaleInstance,
                ),
            );
        }
        let binding = match self.playlist_runtime_binding() {
            Some(binding) => binding,
            None => {
                return self.begin_post_installed_compensation(
                    playlist_runtime,
                    pending,
                    installed.clone(),
                    StrongMediaOpenError::LineageRegistration(
                        crate::playlist_runtime::ResumeCheckpointError::StalePlayerBinding,
                    ),
                );
            }
        };
        let active_media_result = match &pending.lineage_commit {
            PendingStrongLineageCommit::NewLineageOrQueue => playlist_runtime
                .register_successful_strong_install(
                    pending.request_id,
                    installed.player_request_id,
                    resume_commit.media_instance_id,
                    binding,
                    installed.source.clone(),
                    pending.intent,
                ),
            PendingStrongLineageCommit::SameLineage {
                expected_active, ..
            } => playlist_runtime.complete_same_item_media_switch(
                *expected_active,
                resume_commit.media_instance_id,
                binding,
                installed.source.clone(),
            ),
        };
        let active_media = match active_media_result {
            Ok(active_media) => active_media,
            Err(error) => {
                return self.begin_post_installed_compensation(
                    playlist_runtime,
                    pending,
                    installed.clone(),
                    StrongMediaOpenError::LineageRegistration(error),
                );
            }
        };
        if matches!(
            pending.lineage_commit,
            PendingStrongLineageCommit::SameLineage { .. }
        ) {
            self.record_installed_media(installed);
        }
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

/// Восстанавливает только exact track ID, который существует у нового source того же kind-а.
fn applicable_track_restore(
    selected_track: Option<media_core::TrackId>,
    expected_kind: media_core::TrackKind,
    installed_tracks: &[player_core::TrackSummarySnapshot],
) -> player_core::InstalledTrackRestore {
    selected_track
        .filter(|selected_track| {
            installed_tracks
                .iter()
                .any(|track| track.id == *selected_track && track.kind == expected_kind)
        })
        .map_or(
            player_core::InstalledTrackRestore::KeepDefault,
            player_core::InstalledTrackRestore::Select,
        )
}

/// Subtitle inventory пока не входит в A/V track summaries, поэтому сохраняем его exact selection.
fn subtitle_restore_from_selection(
    selected_track: Option<media_core::TrackId>,
) -> player_core::InstalledSubtitleRestore {
    selected_track.map_or(
        player_core::InstalledSubtitleRestore::Disabled,
        player_core::InstalledSubtitleRestore::Select,
    )
}

pub(super) fn player_dispatch_rejection(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u32, kind: media_core::TrackKind) -> player_core::TrackSummarySnapshot {
        player_core::TrackSummarySnapshot {
            id: media_core::TrackId::new(id),
            kind,
            codec_id: "fixture".to_owned(),
            sample_rate: None,
            channels: None,
            duration: None,
            video: None,
            video_color_summary: None,
        }
    }

    #[test]
    fn applicable_track_restore_selects_only_existing_track_of_same_kind() {
        let installed_tracks = [
            track(1, media_core::TrackKind::Video),
            track(2, media_core::TrackKind::Audio),
        ];

        assert_eq!(
            applicable_track_restore(
                Some(media_core::TrackId::new(2)),
                media_core::TrackKind::Audio,
                &installed_tracks,
            ),
            player_core::InstalledTrackRestore::Select(media_core::TrackId::new(2))
        );
        assert_eq!(
            applicable_track_restore(
                Some(media_core::TrackId::new(2)),
                media_core::TrackKind::Video,
                &installed_tracks,
            ),
            player_core::InstalledTrackRestore::KeepDefault
        );
        assert_eq!(
            applicable_track_restore(
                Some(media_core::TrackId::new(3)),
                media_core::TrackKind::Audio,
                &installed_tracks,
            ),
            player_core::InstalledTrackRestore::KeepDefault
        );
    }

    #[test]
    fn subtitle_restore_preserves_explicit_off_and_exact_selection() {
        assert_eq!(
            subtitle_restore_from_selection(None),
            player_core::InstalledSubtitleRestore::Disabled
        );
        assert_eq!(
            subtitle_restore_from_selection(Some(media_core::TrackId::new(7))),
            player_core::InstalledSubtitleRestore::Select(media_core::TrackId::new(7))
        );
    }
}
