//! Player-owner применение exact-instance position/track restore intent-а.

use crossbeam_channel::Sender;

use crate::media_install::{InstalledMediaTargetMatch, PendingInstalledPositionRestore};
use crate::{
    InstalledMediaRelease, InstalledMediaReleaseOutcome, InstalledMediaRestoreFailureStage,
    InstalledMediaStateRestore, InstalledMediaStateRestoreOutcome, InstalledPositionRestore,
    InstalledPositionUnavailableReason, InstalledSubtitleRestore, InstalledTrackRestore,
    InstalledVolumeRestore, PlayerCommand, PlayerError, PlayerErrorKind, PlayerEvent, SeekRequest,
};

use super::PlayerSession;
use super::dynamic_timeline::LiveSameItemPositionRestoreDecision;

/// Результат синхронной части restore до terminal seek commit-а.
enum InstalledPositionRestoreStart {
    /// Position менять не требовалось; restore уже полностью применён.
    CompletedWithoutSeek,
    /// Fresh live snapshot потребовал explicit safe-edge adjustment.
    AdjustedToLiveEdge {
        requested_position: std::time::Duration,
        live_edge: std::time::Duration,
        reason: crate::InstalledLiveEdgeAdjustmentReason,
    },
    /// Seek принят, а authoritative outcome должен дождаться exact generation commit-а.
    AwaitingSeekCommit {
        seek_generation: u64,
        requires_live_anchor_retention: bool,
    },
}

impl PlayerSession {
    /// Сохраняет terminal success, если app ещё не забрал staged adoption outcome.
    pub(super) fn complete_unclaimed_staged_position(&mut self, seek_generation: u64) {
        let Some(installed) = self.installed_staged_position.as_mut() else {
            return;
        };
        if matches!(
            installed.outcome,
            super::staged_media_install::InstalledStagedPositionOutcome::AwaitingSeekCommit {
                seek_generation: expected,
            } if expected == seek_generation
        ) {
            installed.outcome =
                super::staged_media_install::InstalledStagedPositionOutcome::Completed;
        }
    }

    /// Сохраняет terminal failure, если app ещё не забрал staged adoption outcome.
    pub(super) fn fail_unclaimed_staged_position(&mut self, error: PlayerError) {
        let Some(installed) = self.installed_staged_position.as_mut() else {
            return;
        };
        if matches!(
            installed.outcome,
            super::staged_media_install::InstalledStagedPositionOutcome::AwaitingSeekCommit { .. }
        ) {
            installed.outcome =
                super::staged_media_install::InstalledStagedPositionOutcome::Failed(error);
        }
    }

    /// Освобождает current media только при exact request+instance совпадении.
    pub(crate) fn release_installed_media(
        &mut self,
        release: InstalledMediaRelease,
    ) -> InstalledMediaReleaseOutcome {
        match self
            .playback_intent_control
            .match_installed_target(release.request_id, release.media_instance_id)
        {
            InstalledMediaTargetMatch::Matching => {}
            InstalledMediaTargetMatch::NotInstalledYet
            | InstalledMediaTargetMatch::UnknownOrSupersededRequest => {
                return InstalledMediaReleaseOutcome::Absent;
            }
            InstalledMediaTargetMatch::StaleInstance => {
                return InstalledMediaReleaseOutcome::StaleInstance;
            }
        }

        if self.snapshot.media_instance_id != Some(release.media_instance_id) {
            return InstalledMediaReleaseOutcome::StaleInstance;
        }

        match self.stop() {
            Ok(_) => InstalledMediaReleaseOutcome::Applied {
                media_instance_id: release.media_instance_id,
            },
            Err(error) => InstalledMediaReleaseOutcome::Failed { error },
        }
    }

    /// Начинает restore и удерживает receipt до terminal seek outcome-а.
    pub(crate) fn begin_installed_media_state_restore(
        &mut self,
        restore: InstalledMediaStateRestore,
        outcome_tx: Sender<InstalledMediaStateRestoreOutcome>,
    ) {
        let media_instance_id = restore.media_instance_id;
        match self.start_installed_media_state_restore(restore) {
            Ok(InstalledPositionRestoreStart::CompletedWithoutSeek) => {
                Self::publish_installed_restore_outcome(
                    outcome_tx,
                    InstalledMediaStateRestoreOutcome::Applied { media_instance_id },
                );
            }
            Ok(InstalledPositionRestoreStart::AdjustedToLiveEdge {
                requested_position,
                live_edge,
                reason,
            }) => {
                Self::publish_installed_restore_outcome(
                    outcome_tx,
                    InstalledMediaStateRestoreOutcome::AdjustedToLiveEdge {
                        media_instance_id,
                        requested_position,
                        live_edge,
                        reason,
                    },
                );
            }
            Ok(InstalledPositionRestoreStart::AwaitingSeekCommit {
                seek_generation,
                requires_live_anchor_retention,
            }) => {
                self.pending_installed_position_restore = Some(PendingInstalledPositionRestore {
                    request_id: restore.request_id,
                    media_instance_id,
                    seek_generation,
                    requires_live_anchor_retention,
                    outcome_tx,
                });
            }
            Err(outcome) => Self::publish_installed_restore_outcome(outcome_tx, outcome),
        }
    }

    /// Выполняет synchronous restore actions и классифицирует position lifecycle.
    fn start_installed_media_state_restore(
        &mut self,
        restore: InstalledMediaStateRestore,
    ) -> Result<InstalledPositionRestoreStart, InstalledMediaStateRestoreOutcome> {
        match self
            .playback_intent_control
            .match_installed_target(restore.request_id, restore.media_instance_id)
        {
            InstalledMediaTargetMatch::Matching => {}
            InstalledMediaTargetMatch::NotInstalledYet => {
                return Err(InstalledMediaStateRestoreOutcome::NotInstalledYet);
            }
            InstalledMediaTargetMatch::UnknownOrSupersededRequest => {
                return Err(InstalledMediaStateRestoreOutcome::UnknownOrSupersededRequest);
            }
            InstalledMediaTargetMatch::StaleInstance => {
                return Err(InstalledMediaStateRestoreOutcome::StaleInstance);
            }
        }

        if self.snapshot.media_instance_id != Some(restore.media_instance_id) {
            return Err(InstalledMediaStateRestoreOutcome::StaleInstance);
        }

        self.fail_pending_installed_position_restore(PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            "installed position restore заменён новым matching restore",
        ));
        self.restore_video_track(restore.video_track)?;
        self.restore_audio_track(restore.audio_track)?;
        self.restore_subtitle_track(restore.subtitle_track)?;
        self.restore_volume(restore.volume)?;
        self.start_media_position_restore(
            restore.request_id,
            restore.media_instance_id,
            restore.position,
        )
    }

    /// Применяет volume только после exact request/instance validation выше.
    fn restore_volume(
        &mut self,
        restore: InstalledVolumeRestore,
    ) -> Result<(), InstalledMediaStateRestoreOutcome> {
        let InstalledVolumeRestore::Set(volume) = restore else {
            return Ok(());
        };
        self.set_volume(volume)
            .map_err(|error| InstalledMediaStateRestoreOutcome::Failed {
                stage: InstalledMediaRestoreFailureStage::Volume,
                error,
            })
    }

    fn restore_video_track(
        &mut self,
        restore: InstalledTrackRestore,
    ) -> Result<(), InstalledMediaStateRestoreOutcome> {
        let InstalledTrackRestore::Select(track_id) = restore else {
            return Ok(());
        };
        self.dispatch_command(PlayerCommand::SelectVideoTrack(track_id))
            .map(|_| ())
            .map_err(|error| InstalledMediaStateRestoreOutcome::Failed {
                stage: InstalledMediaRestoreFailureStage::VideoTrack,
                error,
            })
    }

    fn restore_audio_track(
        &mut self,
        restore: InstalledTrackRestore,
    ) -> Result<(), InstalledMediaStateRestoreOutcome> {
        let InstalledTrackRestore::Select(track_id) = restore else {
            return Ok(());
        };
        self.dispatch_command(PlayerCommand::SelectAudioTrack(track_id))
            .map(|_| ())
            .map_err(|error| InstalledMediaStateRestoreOutcome::Failed {
                stage: InstalledMediaRestoreFailureStage::AudioTrack,
                error,
            })
    }

    fn restore_subtitle_track(
        &mut self,
        restore: InstalledSubtitleRestore,
    ) -> Result<(), InstalledMediaStateRestoreOutcome> {
        let track_id = match restore {
            InstalledSubtitleRestore::KeepDefault => return Ok(()),
            InstalledSubtitleRestore::Disabled => None,
            InstalledSubtitleRestore::Select(track_id) => Some(track_id),
        };
        self.dispatch_command(PlayerCommand::SelectSubtitleTrack(track_id))
            .map(|_| ())
            .map_err(|error| InstalledMediaStateRestoreOutcome::Failed {
                stage: InstalledMediaRestoreFailureStage::SubtitleTrack,
                error,
            })
    }

    fn start_media_position_restore(
        &mut self,
        request_id: crate::MediaInstallRequestId,
        media_instance_id: crate::MediaInstanceId,
        restore: InstalledPositionRestore,
    ) -> Result<InstalledPositionRestoreStart, InstalledMediaStateRestoreOutcome> {
        let position = match restore {
            InstalledPositionRestore::KeepStart => {
                return Ok(InstalledPositionRestoreStart::CompletedWithoutSeek);
            }
            InstalledPositionRestore::SeekTo(position) => position,
            InstalledPositionRestore::RestoreLiveSameItemPosition {
                previous_absolute_position,
            } => match self.decide_live_same_item_position_restore(
                media_instance_id,
                previous_absolute_position,
            )? {
                LiveSameItemPositionRestoreDecision::RestoreRetainedPosition(position) => position,
                LiveSameItemPositionRestoreDecision::AdjustedToLiveEdge {
                    requested_position,
                    live_edge,
                    reason,
                } => {
                    return Ok(InstalledPositionRestoreStart::AdjustedToLiveEdge {
                        requested_position,
                        live_edge,
                        reason,
                    });
                }
            },
            InstalledPositionRestore::AdoptPreparedSameLineagePosition => {
                let Some(installed) = self.installed_staged_position.take() else {
                    return Err(InstalledMediaStateRestoreOutcome::Failed {
                        stage: InstalledMediaRestoreFailureStage::Position,
                        error: PlayerError::new(
                            PlayerErrorKind::SeekUnavailable,
                            "installed same-lineage position result is missing",
                        ),
                    });
                };
                if installed.request_id != request_id
                    || installed.media_instance_id != media_instance_id
                {
                    return Err(InstalledMediaStateRestoreOutcome::StaleInstance);
                }
                return match installed.outcome {
                    super::staged_media_install::InstalledStagedPositionOutcome::Completed => {
                        Ok(InstalledPositionRestoreStart::CompletedWithoutSeek)
                    }
                    super::staged_media_install::InstalledStagedPositionOutcome::AwaitingSeekCommit {
                        seek_generation,
                    } => Ok(InstalledPositionRestoreStart::AwaitingSeekCommit {
                        seek_generation,
                        requires_live_anchor_retention: true,
                    }),
                    super::staged_media_install::InstalledStagedPositionOutcome::AdjustedToLiveEdge {
                        requested_position,
                        live_edge,
                        reason,
                    } => Ok(InstalledPositionRestoreStart::AdjustedToLiveEdge {
                        requested_position,
                        live_edge,
                        reason,
                    }),
                    super::staged_media_install::InstalledStagedPositionOutcome::Failed(error) => {
                        Err(InstalledMediaStateRestoreOutcome::Failed {
                            stage: InstalledMediaRestoreFailureStage::Position,
                            error,
                        })
                    }
                };
            }
        };
        if !self.snapshot.timeline.seekable
            && self.snapshot.timeline.not_seekable_reason
                == Some(media_core::TimelineNotSeekableReason::SourceNotSeekable)
        {
            return Err(InstalledMediaStateRestoreOutcome::PositionUnavailable {
                media_instance_id,
                requested_position: position,
                available_position: self.snapshot.current_position,
                reason: InstalledPositionUnavailableReason::SourceNotSeekable,
            });
        }

        let first_seek_event = self.pending_events.len();
        if let Err(error) =
            self.dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(position.into())))
        {
            return Err(InstalledMediaStateRestoreOutcome::Failed {
                stage: InstalledMediaRestoreFailureStage::Position,
                error,
            });
        }
        if !self.seek_runtime.seek_landing_active() && !self.snapshot.timeline.seeking {
            let error = self
                .seek_start_error_since(first_seek_event)
                .unwrap_or_else(|| {
                    PlayerError::new(
                        PlayerErrorKind::SeekUnavailable,
                        "position restore не был принят seek transaction",
                    )
                });
            return Err(InstalledMediaStateRestoreOutcome::Failed {
                stage: InstalledMediaRestoreFailureStage::Position,
                error,
            });
        }

        Ok(InstalledPositionRestoreStart::AwaitingSeekCommit {
            seek_generation: self.pipeline.seek_generation(),
            requires_live_anchor_retention: false,
        })
    }

    /// Возвращает exact ошибку, опубликованную синхронной частью seek-а.
    fn seek_start_error_since(&self, first_event: usize) -> Option<PlayerError> {
        self.pending_events[first_event..]
            .iter()
            .rev()
            .find_map(|correlated_event| match &correlated_event.event {
                PlayerEvent::RecoverableError(error) | PlayerEvent::FatalError(error) => {
                    Some(error.clone())
                }
                _ => None,
            })
    }

    /// Публикует successful terminal только от matching generation и instance.
    pub(super) fn finish_installed_position_restore(&mut self, seek_generation: u64) {
        let Some(pending) = self.pending_installed_position_restore.take() else {
            return;
        };
        let target_is_current = self.snapshot.media_instance_id == Some(pending.media_instance_id)
            && self
                .playback_intent_control
                .match_installed_target(pending.request_id, pending.media_instance_id)
                == InstalledMediaTargetMatch::Matching;
        let outcome = if !target_is_current {
            InstalledMediaStateRestoreOutcome::StaleInstance
        } else if pending.seek_generation != seek_generation {
            InstalledMediaStateRestoreOutcome::Failed {
                stage: InstalledMediaRestoreFailureStage::Position,
                error: PlayerError::new(
                    PlayerErrorKind::SeekUnavailable,
                    "position restore получил commit от другого seek generation",
                ),
            }
        } else {
            InstalledMediaStateRestoreOutcome::Applied {
                media_instance_id: pending.media_instance_id,
            }
        };
        Self::publish_installed_restore_outcome(pending.outcome_tx, outcome);
    }

    /// Закрывает pending restore typed position failure-ом без false `Applied`.
    pub(super) fn fail_pending_installed_position_restore(&mut self, error: PlayerError) {
        let Some(pending) = self.pending_installed_position_restore.take() else {
            return;
        };
        Self::publish_installed_restore_outcome(
            pending.outcome_tx,
            InstalledMediaStateRestoreOutcome::Failed {
                stage: InstalledMediaRestoreFailureStage::Position,
                error,
            },
        );
    }

    /// Не позволяет прежнему instance дождаться commit-а уже нового media.
    pub(crate) fn reconcile_installed_position_restore_identity(&mut self) {
        let is_stale = self
            .pending_installed_position_restore
            .as_ref()
            .is_some_and(|pending| {
                self.snapshot.media_instance_id != Some(pending.media_instance_id)
                    || self
                        .playback_intent_control
                        .match_installed_target(pending.request_id, pending.media_instance_id)
                        != InstalledMediaTargetMatch::Matching
            });
        if !is_stale {
            return;
        }
        let pending = self
            .pending_installed_position_restore
            .take()
            .expect("stale pending installed restore was just observed");
        Self::publish_installed_restore_outcome(
            pending.outcome_tx,
            InstalledMediaStateRestoreOutcome::StaleInstance,
        );
    }

    /// Dropped receipt не меняет playback, но обязан остаться видимым в diagnostics.
    fn publish_installed_restore_outcome(
        outcome_tx: Sender<InstalledMediaStateRestoreOutcome>,
        outcome: InstalledMediaStateRestoreOutcome,
    ) {
        if outcome_tx.send(outcome).is_err() {
            tracing::warn!("Installed media restore outcome receiver was dropped");
        }
    }
}
