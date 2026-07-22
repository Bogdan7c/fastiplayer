//! Renderer-bound executor одной process-lifetime checkpoint resume attempt.
//!
//! Сам checkpoint остаётся у `PlaylistRuntime`; здесь живут только candidate resources и
//! request-owned receipts, которые обязаны исчезнуть вместе с текущим player binding.

use std::num::NonZeroU64;

use player_core::{
    InstalledMediaRelease, InstalledMediaReleaseOutcome, InstalledMediaReleaseReceipt,
    InstalledMediaStateRestore, InstalledMediaStateRestoreOutcome,
    InstalledMediaStateRestoreReceipt, InstalledPositionRestore, InstalledSubtitleRestore,
    InstalledTrackRestore, MediaInstallCompletion, MediaInstallRequestId, MediaInstanceId,
    PlaybackIntent, PlaybackIntentRevision, PlaybackIntentUpdate, PlaybackIntentUpdateOutcome,
    PlaybackIntentUpdateReceipt,
};

use super::AppState;
use super::strong_media_open::ProductionCandidateOwner;
use crate::media_open::{
    ActiveMediaSource, AuthorizationDispatchResolution, MediaOpenClientKey, MediaOpenInstallIntent,
    MediaOpenPhase, MediaOpenRequestId, MediaOpenSourceRequest, MediaOpenStartMode,
    MediaOpenStartOutcome, MediaOpenTerminalOutcome,
};
use crate::playlist_runtime::{
    PlaylistRuntime, ResumeAttempt, ResumeCheckpointError, ResumePlaybackIntent,
    ResumePositionWarning,
};
use crate::video_pipeline_candidate::{
    WgpuCandidateVideoPipelineResourceDriver, player_selected_video_candidate_boundary,
};
use render_wgpu_shell::Renderer;

const RESUME_MEDIA_CLIENT_KEY: MediaOpenClientKey = MediaOpenClientKey::from_non_zero(
    NonZeroU64::new(2).expect("resume media client key is non-zero"),
);

pub(super) struct SuspendedMediaResume {
    attempt: ResumeAttempt,
    request_id: MediaOpenRequestId,
    phase: ResumePhase,
}

enum ResumePhase {
    Preparing,
    Installing {
        candidate: ProductionCandidateOwner,
    },
    Seeking {
        player_request_id: MediaInstallRequestId,
        media_instance_id: MediaInstanceId,
        receipt: InstalledMediaStateRestoreReceipt,
    },
    RestoringIntent {
        player_request_id: MediaInstallRequestId,
        media_instance_id: MediaInstanceId,
        warning: Option<ResumePositionWarning>,
        receipt: PlaybackIntentUpdateReceipt,
    },
    Releasing {
        failure: ResumeCheckpointError,
        receipt: InstalledMediaReleaseReceipt,
    },
}

impl AppState {
    /// Suspend terminal-cancel-ит renderer-bound resume request до drop AppState.
    pub(crate) fn cancel_suspended_media_resume_for_suspend(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> Result<(), ResumeCheckpointError> {
        self.cancel_suspended_media_resume(
            playlist_runtime,
            player_core::MediaInstallCancellationCause::LifecycleSuspended,
            true,
        )
    }

    /// Новый explicit open terminal-resolve-ит старую resume attempt до supersede checkpoint-а.
    pub(crate) fn cancel_suspended_media_resume_for_explicit_open(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> Result<(), ResumeCheckpointError> {
        self.cancel_suspended_media_resume(
            playlist_runtime,
            player_core::MediaInstallCancellationCause::Superseded,
            false,
        )
    }

    fn cancel_suspended_media_resume(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        cancellation_cause: player_core::MediaInstallCancellationCause,
        preserve_checkpoint: bool,
    ) -> Result<(), ResumeCheckpointError> {
        let Some(resume) = self.suspended_media_resume.take() else {
            if !preserve_checkpoint {
                playlist_runtime.supersede_suspended_media_checkpoint();
            }
            return Ok(());
        };
        match resume.phase {
            ResumePhase::Seeking {
                player_request_id,
                media_instance_id,
                ..
            }
            | ResumePhase::RestoringIntent {
                player_request_id,
                media_instance_id,
                ..
            } => self.release_resume_candidate_blocking(player_request_id, media_instance_id)?,
            ResumePhase::Releasing { receipt, .. } => match receipt.wait_for_outcome() {
                Ok(InstalledMediaReleaseOutcome::Applied { .. }) => {}
                _ => return Err(ResumeCheckpointError::CandidateReleaseFailed),
            },
            ResumePhase::Preparing | ResumePhase::Installing { .. } => {
                let _ = playlist_runtime.cancel_media_open(resume.request_id, cancellation_cause);
                loop {
                    let snapshot = playlist_runtime
                        .media_open_snapshot()
                        .ok_or(ResumeCheckpointError::MissingAuthorizationResolution)?;
                    match snapshot.phase {
                        MediaOpenPhase::Accepted
                        | MediaOpenPhase::Preparing
                        | MediaOpenPhase::PlayerStaging
                        | MediaOpenPhase::AuthorizationDispatchPending
                        | MediaOpenPhase::EnqueuedAtPlayerOwner => {
                            let _phase = playlist_runtime
                                .wait_for_media_open_progress(resume.request_id)
                                .map_err(|_| {
                                    ResumeCheckpointError::MissingAuthorizationResolution
                                })?;
                        }
                        MediaOpenPhase::Installed => {
                            let terminal = playlist_runtime
                                .take_media_open_terminal(resume.request_id)
                                .map_err(|_| ResumeCheckpointError::MissingInstalledTerminal)?
                                .ok_or(ResumeCheckpointError::MissingInstalledTerminal)?;
                            let MediaOpenTerminalOutcome::Installed {
                                player_request_id,
                                completion:
                                    MediaInstallCompletion::Installed {
                                        media_instance_id, ..
                                    },
                                ..
                            } = terminal
                            else {
                                return Err(ResumeCheckpointError::MissingInstalledTerminal);
                            };
                            self.release_resume_candidate_blocking(
                                player_request_id,
                                media_instance_id,
                            )?;
                            break;
                        }
                        MediaOpenPhase::Failed => {
                            let terminal = playlist_runtime
                                .take_media_open_terminal(resume.request_id)
                                .map_err(|_| ResumeCheckpointError::MissingAuthorizationResolution)?
                                .ok_or(ResumeCheckpointError::MissingAuthorizationResolution)?;
                            if !matches!(terminal, MediaOpenTerminalOutcome::Cancelled { .. }) {
                                return Err(ResumeCheckpointError::InstallFailed);
                            }
                            break;
                        }
                        MediaOpenPhase::Prepared | MediaOpenPhase::ReadyToCommit => {
                            playlist_runtime
                                .cancel_media_open(resume.request_id, cancellation_cause)
                                .map_err(|_| {
                                    ResumeCheckpointError::MissingAuthorizationResolution
                                })?;
                        }
                    }
                }
            }
        }
        if preserve_checkpoint {
            playlist_runtime.pause_suspended_media_resume_for_suspend();
        } else {
            playlist_runtime.supersede_suspended_media_checkpoint();
        }
        Ok(())
    }

    fn release_resume_candidate_blocking(
        &self,
        player_request_id: MediaInstallRequestId,
        media_instance_id: MediaInstanceId,
    ) -> Result<(), ResumeCheckpointError> {
        let receipt = self
            .player_worker
            .release_installed_media(InstalledMediaRelease {
                request_id: player_request_id,
                media_instance_id,
            })
            .map_err(|_| ResumeCheckpointError::CandidateReleaseFailed)?;
        match receipt.wait_for_outcome() {
            Ok(InstalledMediaReleaseOutcome::Applied { .. }) => Ok(()),
            _ => Err(ResumeCheckpointError::CandidateReleaseFailed),
        }
    }

    /// Defensive polling остаётся fallback; primary progress приходит через AppWake.
    pub(crate) fn has_pending_suspended_media_resume(&self) -> bool {
        self.suspended_media_resume.is_some()
    }

    /// Запускает automatic resume только для checkpoint, которому не нужен explicit Retry.
    pub(crate) fn start_suspended_media_resume(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> bool {
        self.start_suspended_media_resume_with_retry(playlist_runtime, false)
    }

    /// Explicit Retry повторяет terminal/recoverable checkpoint без hidden queue navigation.
    #[allow(
        dead_code,
        reason = "typed Retry boundary предшествует отдельному playlist lifecycle UI action"
    )]
    pub(crate) fn retry_suspended_media_resume(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> bool {
        self.start_suspended_media_resume_with_retry(playlist_runtime, true)
    }

    fn start_suspended_media_resume_with_retry(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        explicit_retry: bool,
    ) -> bool {
        if self.suspended_media_resume.is_some() {
            return false;
        }
        let Some(attempt) = playlist_runtime.begin_suspended_media_resume(explicit_retry) else {
            return false;
        };
        let source_request = match self.resume_source_request(&attempt.source) {
            Ok(source_request) => source_request,
            Err(error) => {
                playlist_runtime.fail_suspended_media_resume(error);
                return true;
            }
        };
        let request_id = match playlist_runtime.start_media_open(
            RESUME_MEDIA_CLIENT_KEY,
            source_request,
            MediaOpenStartMode::RequireIdle,
        ) {
            Ok(MediaOpenStartOutcome::Accepted { request_id }) => request_id,
            Ok(MediaOpenStartOutcome::Coalesced { .. }) | Err(_) => {
                playlist_runtime
                    .fail_suspended_media_resume(ResumeCheckpointError::PreparationFailed);
                return true;
            }
        };
        self.suspended_media_resume = Some(SuspendedMediaResume {
            attempt,
            request_id,
            phase: ResumePhase::Preparing,
        });
        true
    }

    fn resume_source_request(
        &self,
        source: &ActiveMediaSource,
    ) -> Result<MediaOpenSourceRequest, ResumeCheckpointError> {
        let config = self.committed_app_config();
        let physical_request = match source.physical_source() {
            ActiveMediaSource::LocalFile(path) => MediaOpenSourceRequest::Local {
                path: path.clone(),
                expected_fingerprint: None,
                demux_config: config.player.demux,
            },
            ActiveMediaSource::DirectMediaUrl(locator) => MediaOpenSourceRequest::Direct {
                locator: locator.clone(),
                network_config: config.network,
                demux_config: config.player.demux,
            },
            ActiveMediaSource::YtDlpUrl {
                source_locator,
                candidate_selection,
            } => {
                let capabilities = self
                    .system_capabilities_snapshot
                    .clone()
                    .ok_or(ResumeCheckpointError::PreparationFailed)?;
                MediaOpenSourceRequest::YtDlp {
                    locator: source_locator.clone(),
                    selection_intent: crate::web_media_open::YtDlpCandidateOpenIntent::Exact(
                        candidate_selection.clone(),
                    ),
                    network_config: config.network,
                    yt_dlp_config: config.yt_dlp,
                    demux_config: config.player.demux,
                    preferred_video_codec_order: config.player.preferred_video_codec_order,
                    system_capabilities: capabilities,
                    audio_capabilities: self.audio_decode_capability_snapshot(),
                }
            }
            ActiveMediaSource::PlaybackWindow { .. } => {
                unreachable!("physical_source removes playback-window wrappers")
            }
        };
        Ok(source.wrap_reopen_request(physical_request))
    }

    /// Неблокирующе продвигает ровно одну resume attempt по owner receipts.
    pub(crate) fn drive_suspended_media_resume(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) -> bool {
        let Some(mut resume) = self.suspended_media_resume.take() else {
            return false;
        };
        let keep = match &mut resume.phase {
            ResumePhase::Preparing => {
                self.drive_resume_preparing(playlist_runtime, renderer, &mut resume)
            }
            ResumePhase::Installing { candidate } => self.drive_resume_installing(
                playlist_runtime,
                &resume.attempt,
                resume.request_id,
                candidate,
            ),
            ResumePhase::Seeking {
                player_request_id,
                media_instance_id,
                receipt,
            } => self.drive_resume_seek(
                playlist_runtime,
                &resume.attempt,
                *player_request_id,
                *media_instance_id,
                receipt,
            ),
            ResumePhase::RestoringIntent {
                player_request_id,
                media_instance_id,
                warning,
                receipt,
            } => self.drive_resume_intent(
                playlist_runtime,
                &resume.attempt,
                *player_request_id,
                *media_instance_id,
                *warning,
                receipt,
            ),
            ResumePhase::Releasing { failure, receipt } => match receipt.try_take_outcome() {
                Ok(Some(InstalledMediaReleaseOutcome::Applied { .. })) => {
                    playlist_runtime.fail_suspended_media_resume(*failure);
                    ResumeDrive::Finished
                }
                Ok(Some(_)) | Err(_) => {
                    playlist_runtime
                        .fail_suspended_media_resume(ResumeCheckpointError::CandidateReleaseFailed);
                    ResumeDrive::Finished
                }
                Ok(None) => ResumeDrive::Pending,
            },
        };
        match keep {
            ResumeDrive::Pending => {
                self.suspended_media_resume = Some(resume);
                false
            }
            ResumeDrive::Replace(phase) => {
                resume.phase = phase;
                self.suspended_media_resume = Some(resume);
                true
            }
            ResumeDrive::Finished => true,
        }
    }

    fn drive_resume_preparing(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
        resume: &mut SuspendedMediaResume,
    ) -> ResumeDrive {
        let Some(snapshot) = playlist_runtime.media_open_snapshot() else {
            playlist_runtime.fail_suspended_media_resume(ResumeCheckpointError::PreparationFailed);
            return ResumeDrive::Finished;
        };
        if snapshot.request_id != resume.request_id {
            playlist_runtime.fail_suspended_media_resume(ResumeCheckpointError::PreparationFailed);
            return ResumeDrive::Finished;
        }
        match snapshot.phase {
            MediaOpenPhase::Accepted | MediaOpenPhase::Preparing => ResumeDrive::Pending,
            MediaOpenPhase::Prepared => {
                let driver = WgpuCandidateVideoPipelineResourceDriver::new(
                    renderer.instance(),
                    renderer.adapter(),
                    renderer.device(),
                    renderer.queue(),
                );
                let (candidate, video_resource_port) = player_selected_video_candidate_boundary(
                    self.renderer_generation,
                    self.player_worker.decoder_thread_config(),
                    driver,
                );
                let revision = PlaybackIntentRevision::from_non_zero(
                    NonZeroU64::new(1).expect("resume initial revision is non-zero"),
                );
                if playlist_runtime
                    .stage_media_open_at_player(
                        resume.request_id,
                        MediaOpenInstallIntent {
                            intent: PlaybackIntent::StartPaused,
                            revision,
                        },
                        video_resource_port,
                    )
                    .is_err()
                {
                    return Self::finish_failed_resume_media_open(
                        playlist_runtime,
                        resume.request_id,
                        ResumeCheckpointError::InstallFailed,
                    );
                }
                ResumeDrive::Replace(ResumePhase::Installing { candidate })
            }
            MediaOpenPhase::Failed => Self::finish_failed_resume_media_open(
                playlist_runtime,
                resume.request_id,
                ResumeCheckpointError::PreparationFailed,
            ),
            _ => {
                playlist_runtime.fail_suspended_media_resume(ResumeCheckpointError::InstallFailed);
                ResumeDrive::Finished
            }
        }
    }

    fn drive_resume_installing(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        attempt: &ResumeAttempt,
        request_id: MediaOpenRequestId,
        candidate: &ProductionCandidateOwner,
    ) -> ResumeDrive {
        let Some(snapshot) = playlist_runtime.media_open_snapshot() else {
            playlist_runtime.fail_suspended_media_resume(ResumeCheckpointError::InstallFailed);
            return ResumeDrive::Finished;
        };
        match snapshot.phase {
            MediaOpenPhase::PlayerStaging
            | MediaOpenPhase::AuthorizationDispatchPending
            | MediaOpenPhase::EnqueuedAtPlayerOwner => ResumeDrive::Pending,
            MediaOpenPhase::ReadyToCommit => {
                match playlist_runtime.authorize_ready_media_open(request_id) {
                    Ok(AuthorizationDispatchResolution::EnqueuedAtPlayerOwner) => {
                        ResumeDrive::Pending
                    }
                    _ => Self::finish_failed_resume_media_open(
                        playlist_runtime,
                        request_id,
                        ResumeCheckpointError::InstallFailed,
                    ),
                }
            }
            MediaOpenPhase::Installed => {
                let terminal = match playlist_runtime.take_media_open_terminal(request_id) {
                    Ok(Some(terminal)) => terminal,
                    _ => {
                        playlist_runtime.fail_suspended_media_resume(
                            ResumeCheckpointError::MissingInstalledTerminal,
                        );
                        return ResumeDrive::Finished;
                    }
                };
                let MediaOpenTerminalOutcome::Installed {
                    player_request_id,
                    completion:
                        MediaInstallCompletion::Installed {
                            media_instance_id, ..
                        },
                    ..
                } = terminal
                else {
                    playlist_runtime
                        .fail_suspended_media_resume(ResumeCheckpointError::InstallFailed);
                    return ResumeDrive::Finished;
                };
                if self
                    .commit_installed_video_candidate(candidate, player_request_id)
                    .is_err()
                {
                    return self.release_failed_resume_candidate(
                        playlist_runtime,
                        player_request_id,
                        media_instance_id,
                        ResumeCheckpointError::InstallFailed,
                    );
                }
                let restore = InstalledMediaStateRestore {
                    request_id: player_request_id,
                    media_instance_id,
                    video_track: InstalledTrackRestore::KeepDefault,
                    audio_track: InstalledTrackRestore::KeepDefault,
                    subtitle_track: InstalledSubtitleRestore::KeepDefault,
                    position: InstalledPositionRestore::SeekTo(attempt.position),
                };
                match self.player_worker.restore_installed_media_state(restore) {
                    Ok(receipt) => ResumeDrive::Replace(ResumePhase::Seeking {
                        player_request_id,
                        media_instance_id,
                        receipt,
                    }),
                    Err(_) => self.release_failed_resume_candidate(
                        playlist_runtime,
                        player_request_id,
                        media_instance_id,
                        ResumeCheckpointError::SeekFailed,
                    ),
                }
            }
            MediaOpenPhase::Failed => Self::finish_failed_resume_media_open(
                playlist_runtime,
                request_id,
                ResumeCheckpointError::InstallFailed,
            ),
            _ => ResumeDrive::Pending,
        }
    }

    fn drive_resume_seek(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        attempt: &ResumeAttempt,
        player_request_id: MediaInstallRequestId,
        media_instance_id: MediaInstanceId,
        receipt: &InstalledMediaStateRestoreReceipt,
    ) -> ResumeDrive {
        let outcome = match receipt.try_take_outcome() {
            Ok(Some(outcome)) => outcome,
            Ok(None) => return ResumeDrive::Pending,
            Err(_) => {
                return self.release_failed_resume_candidate(
                    playlist_runtime,
                    player_request_id,
                    media_instance_id,
                    ResumeCheckpointError::SeekFailed,
                );
            }
        };
        let warning = match outcome {
            InstalledMediaStateRestoreOutcome::Applied {
                media_instance_id: applied,
            } if applied == media_instance_id => None,
            InstalledMediaStateRestoreOutcome::PositionUnavailable {
                media_instance_id: applied,
                requested_position,
                available_position,
                ..
            } if applied == media_instance_id => Some(ResumePositionWarning {
                requested_position,
                available_position,
            }),
            _ => {
                return self.release_failed_resume_candidate(
                    playlist_runtime,
                    player_request_id,
                    media_instance_id,
                    ResumeCheckpointError::SeekFailed,
                );
            }
        };
        let revision = PlaybackIntentRevision::from_non_zero(
            NonZeroU64::new(2).expect("resume post-seek revision is non-zero"),
        );
        let intent = match attempt.intent {
            ResumePlaybackIntent::Playing => PlaybackIntent::StartPlaying,
            ResumePlaybackIntent::Paused => PlaybackIntent::StartPaused,
        };
        match self
            .player_worker
            .update_playback_intent(PlaybackIntentUpdate {
                request_id: player_request_id,
                revision,
                intent,
            }) {
            Ok(receipt) => ResumeDrive::Replace(ResumePhase::RestoringIntent {
                player_request_id,
                media_instance_id,
                warning,
                receipt,
            }),
            Err(_) => self.release_failed_resume_candidate(
                playlist_runtime,
                player_request_id,
                media_instance_id,
                ResumeCheckpointError::IntentRestoreFailed,
            ),
        }
    }

    fn drive_resume_intent(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        attempt: &ResumeAttempt,
        player_request_id: MediaInstallRequestId,
        media_instance_id: MediaInstanceId,
        warning: Option<ResumePositionWarning>,
        receipt: &PlaybackIntentUpdateReceipt,
    ) -> ResumeDrive {
        let Some(outcome) = receipt.try_outcome() else {
            return ResumeDrive::Pending;
        };
        if !matches!(
            outcome,
            PlaybackIntentUpdateOutcome::AppliedToInstalled {
                media_instance_id: applied
            } if applied == media_instance_id
        ) {
            return self.release_failed_resume_candidate(
                playlist_runtime,
                player_request_id,
                media_instance_id,
                ResumeCheckpointError::IntentRestoreFailed,
            );
        }
        let Some(binding) = self.playlist_runtime_binding() else {
            return self.release_failed_resume_candidate(
                playlist_runtime,
                player_request_id,
                media_instance_id,
                ResumeCheckpointError::StalePlayerBinding,
            );
        };
        match playlist_runtime.complete_suspended_media_resume(
            attempt.expected_active,
            media_instance_id,
            binding.binding_generation(),
            warning,
        ) {
            Ok(_) => {
                self.record_installed_media_source(attempt.source.clone());
                ResumeDrive::Finished
            }
            Err(error) => self.release_failed_resume_candidate(
                playlist_runtime,
                player_request_id,
                media_instance_id,
                error,
            ),
        }
    }

    fn release_failed_resume_candidate(
        &self,
        playlist_runtime: &mut PlaylistRuntime,
        player_request_id: MediaInstallRequestId,
        media_instance_id: MediaInstanceId,
        failure: ResumeCheckpointError,
    ) -> ResumeDrive {
        match self
            .player_worker
            .release_installed_media(InstalledMediaRelease {
                request_id: player_request_id,
                media_instance_id,
            }) {
            Ok(receipt) => ResumeDrive::Replace(ResumePhase::Releasing { failure, receipt }),
            Err(_) => {
                playlist_runtime
                    .fail_suspended_media_resume(ResumeCheckpointError::CandidateReleaseFailed);
                ResumeDrive::Finished
            }
        }
    }

    /// Освобождает coordinator slot после pre-Installed failure, чтобы explicit Retry не видел Busy.
    fn finish_failed_resume_media_open(
        playlist_runtime: &mut PlaylistRuntime,
        request_id: MediaOpenRequestId,
        failure: ResumeCheckpointError,
    ) -> ResumeDrive {
        let terminal_ready = match playlist_runtime.media_open_snapshot() {
            Some(snapshot) if snapshot.request_id == request_id => {
                if snapshot.phase != MediaOpenPhase::Failed
                    && playlist_runtime
                        .cancel_media_open(
                            request_id,
                            player_core::MediaInstallCancellationCause::LifecycleSuspended,
                        )
                        .is_err()
                {
                    false
                } else {
                    matches!(
                        playlist_runtime.take_media_open_terminal(request_id),
                        Ok(Some(_))
                    )
                }
            }
            _ => false,
        };
        playlist_runtime.fail_suspended_media_resume(if terminal_ready {
            failure
        } else {
            ResumeCheckpointError::MissingAuthorizationResolution
        });
        ResumeDrive::Finished
    }
}

enum ResumeDrive {
    Pending,
    Replace(ResumePhase),
    Finished,
}
