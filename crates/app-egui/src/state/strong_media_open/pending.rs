//! Renderer-bound пошаговая strong install транзакция для UI/event loop.

use super::*;

mod resume;
use resume::InstalledResumeCommit;

/// Renderer-bound ownership незавершённой strong install транзакции.
pub(crate) struct PendingStrongMediaOpen {
    request_id: MediaOpenRequestId,
    candidate_owner: Option<ProductionCandidateOwner>,
    source: Option<ActiveMediaSource>,
    intent: PlaybackIntent,
    intent_revision: PlaybackIntentRevision,
    startup_position: crate::playlist_runtime::StartupPosition,
    phase: PendingStrongMediaOpenPhase,
}

struct PendingStrongMediaStaging {
    video_resource_port: player_core::MediaInstallVideoResourcePort,
    playlist_target: PreparedPlaylistTarget,
}

/// Player protocol и post-Installed intent acknowledgement остаются разными этапами.
enum PendingStrongMediaOpenPhase {
    Protocol {
        deferred_rejection: Option<PlaylistMediaOpenGateError>,
        pending_staging: Option<PendingStrongMediaStaging>,
    },
    PositionRestore {
        installed: InstalledSingleMediaOpen,
        media_instance_id: player_core::MediaInstanceId,
        requested_position: std::time::Duration,
        receipt: player_core::InstalledMediaStateRestoreReceipt,
    },
    PlaybackIntent {
        installed: InstalledSingleMediaOpen,
        resume_commit: InstalledResumeCommit,
        receipt: player_core::PlaybackIntentUpdateReceipt,
    },
}

impl AppState {
    /// Синхронизирует renderer-bound post-Installed phase с accepted D52 revision.
    pub(crate) fn update_pending_strong_playlist_intent(
        &mut self,
        request_id: MediaOpenRequestId,
        revision: PlaybackIntentRevision,
        intent: PlaybackIntent,
    ) {
        let Some(pending) = self.pending_strong_media_open.as_mut() else {
            return;
        };
        if pending.request_id == request_id {
            pending.intent = intent;
            pending.intent_revision = revision;
        }
    }

    /// Начинает renderer-bound strong install и возвращает управление до любого receipt wait.
    pub(crate) fn begin_prepared_media_strong(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
        prepared_input: PreparedSingleMediaOpen,
        intent: PlaybackIntent,
    ) -> Result<(), StrongMediaOpenError> {
        if self.pending_strong_media_open.is_some() {
            return Err(StrongMediaOpenError::Start(MediaOpenStartError::Busy));
        }
        self.cancel_suspended_media_resume_for_explicit_open(playlist_runtime)
            .map_err(StrongMediaOpenError::LineageRegistration)?;
        let source = prepared_input.source.clone();
        let playlist_target = prepared_input.playlist_target;
        let startup_position = prepared_input.startup_position;
        let prepared_open = PreparedMediaOpen::from_caller_prepared(
            prepared_input.prepared_media,
            prepared_input.source,
            prepared_input.safe_label.clone(),
        );
        let request_id = match playlist_runtime.start_prepared_media_open(
            SINGLE_MEDIA_CLIENT_KEY,
            prepared_open,
            prepared_input.safe_label,
        )? {
            MediaOpenStartOutcome::Accepted { request_id } => request_id,
            MediaOpenStartOutcome::Coalesced { .. } => {
                return Err(StrongMediaOpenError::Start(MediaOpenStartError::Busy));
            }
        };

        let driver = WgpuCandidateVideoPipelineResourceDriver::new(
            renderer.instance(),
            renderer.adapter(),
            renderer.device(),
            renderer.queue(),
        );
        let (candidate_owner, video_resource_port) = player_selected_video_candidate_boundary(
            self.renderer_generation,
            self.player_worker.decoder_thread_config(),
            driver,
        );
        let initial_revision = PlaybackIntentRevision::from_non_zero(
            NonZeroU64::new(1).expect("revision is non-zero"),
        );
        let player_request_id = match playlist_runtime.stage_media_open_at_player(
            request_id,
            MediaOpenInstallIntent {
                intent,
                revision: initial_revision,
            },
            video_resource_port,
        ) {
            Ok(player_request_id) => player_request_id,
            Err(error) => {
                self.pending_strong_media_open = Some(PendingStrongMediaOpen {
                    request_id,
                    candidate_owner: Some(candidate_owner),
                    source: Some(source),
                    intent,
                    intent_revision: initial_revision,
                    startup_position,
                    phase: PendingStrongMediaOpenPhase::Protocol {
                        deferred_rejection: Some(error),
                        pending_staging: None,
                    },
                });
                return Ok(());
            }
        };

        let deferred_rejection = playlist_target.and_then(|playlist_target| {
            let admission = match playlist_target {
                PreparedPlaylistTarget::QueueReplacement(target_draft) => playlist_runtime
                    .accept_explicit_target_install(
                        request_id,
                        player_request_id,
                        target_draft,
                        initial_revision,
                    ),
                PreparedPlaylistTarget::RestoredCurrent(target) => playlist_runtime
                    .accept_startup_restore_install(request_id, player_request_id, target),
                PreparedPlaylistTarget::Planned {
                    install,
                    supersedes,
                } => match supersedes {
                    Some(expected_request_id) => playlist_runtime
                        .accept_superseding_playlist_install(
                            expected_request_id,
                            request_id,
                            player_request_id,
                            install,
                        ),
                    None => playlist_runtime.accept_planned_playlist_install(
                        request_id,
                        player_request_id,
                        install,
                    ),
                },
            };
            admission.err()
        });
        if deferred_rejection.is_some() {
            playlist_runtime
                .cancel_media_open_lossless(
                    request_id,
                    MediaInstallCancellationCause::StructuralInvalidation,
                )
                .map_err(StrongMediaOpenError::Command)?;
        }
        self.pending_strong_media_open = Some(PendingStrongMediaOpen {
            request_id,
            candidate_owner: Some(candidate_owner),
            source: Some(source),
            intent,
            intent_revision: initial_revision,
            startup_position,
            phase: PendingStrongMediaOpenPhase::Protocol {
                deferred_rejection,
                pending_staging: None,
            },
        });
        Ok(())
    }

    /// Запускает locator-based preparation в том же strong coordinator/protocol-е.
    pub(crate) fn begin_playlist_source_media_strong(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
        source_request: MediaOpenSourceRequest,
        install: crate::playlist_runtime::PlannedPlaylistInstall,
        supersedes: Option<MediaOpenRequestId>,
    ) -> Result<MediaOpenRequestId, StrongMediaOpenError> {
        if self.pending_strong_media_open.is_some() {
            return Err(StrongMediaOpenError::Start(MediaOpenStartError::Busy));
        }
        self.cancel_suspended_media_resume_for_explicit_open(playlist_runtime)
            .map_err(StrongMediaOpenError::LineageRegistration)?;
        let intent = install.playback_intent;
        let intent_revision = install.intent_revision;
        let request_id = match playlist_runtime.start_media_open(
            SINGLE_MEDIA_CLIENT_KEY,
            source_request,
            MediaOpenStartMode::RequireIdle,
        )? {
            MediaOpenStartOutcome::Accepted { request_id } => request_id,
            MediaOpenStartOutcome::Coalesced { .. } => {
                return Err(StrongMediaOpenError::Start(MediaOpenStartError::Busy));
            }
        };
        let driver = WgpuCandidateVideoPipelineResourceDriver::new(
            renderer.instance(),
            renderer.adapter(),
            renderer.device(),
            renderer.queue(),
        );
        let (candidate_owner, video_resource_port) = player_selected_video_candidate_boundary(
            self.renderer_generation,
            self.player_worker.decoder_thread_config(),
            driver,
        );
        self.pending_strong_media_open = Some(PendingStrongMediaOpen {
            request_id,
            candidate_owner: Some(candidate_owner),
            source: None,
            intent,
            intent_revision,
            startup_position: crate::playlist_runtime::StartupPosition::KeepStart,
            phase: PendingStrongMediaOpenPhase::Protocol {
                deferred_rejection: None,
                pending_staging: Some(PendingStrongMediaStaging {
                    video_resource_port,
                    playlist_target: PreparedPlaylistTarget::Planned {
                        install,
                        supersedes,
                    },
                }),
            },
        });
        Ok(request_id)
    }

    /// Продвигает не более одного логического этапа и никогда не ждёт worker receipt.
    pub(crate) fn poll_prepared_media_strong(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> StrongMediaOpenPoll {
        let Some(mut pending) = self.pending_strong_media_open.take() else {
            return StrongMediaOpenPoll::Pending;
        };
        let phase = std::mem::replace(
            &mut pending.phase,
            PendingStrongMediaOpenPhase::Protocol {
                deferred_rejection: None,
                pending_staging: None,
            },
        );
        let result = match phase {
            PendingStrongMediaOpenPhase::Protocol {
                mut deferred_rejection,
                mut pending_staging,
            } => {
                let result = self.poll_strong_media_protocol(
                    playlist_runtime,
                    &mut pending,
                    &mut deferred_rejection,
                    &mut pending_staging,
                );
                if matches!(result, StrongMediaOpenPoll::Pending)
                    && matches!(pending.phase, PendingStrongMediaOpenPhase::Protocol { .. })
                {
                    pending.phase = PendingStrongMediaOpenPhase::Protocol {
                        deferred_rejection,
                        pending_staging,
                    };
                }
                result
            }
            PendingStrongMediaOpenPhase::PlaybackIntent {
                mut installed,
                resume_commit,
                receipt,
            } => {
                let result = self.poll_strong_media_intent(
                    playlist_runtime,
                    &pending,
                    &mut installed,
                    resume_commit,
                    &receipt,
                );
                if matches!(result, StrongMediaOpenPoll::Pending) {
                    pending.phase = PendingStrongMediaOpenPhase::PlaybackIntent {
                        installed,
                        resume_commit,
                        receipt,
                    };
                }
                result
            }
            PendingStrongMediaOpenPhase::PositionRestore {
                mut installed,
                media_instance_id,
                requested_position,
                receipt,
            } => {
                let result = self.poll_strong_media_position_restore(
                    playlist_runtime,
                    &mut pending,
                    &mut installed,
                    media_instance_id,
                    requested_position,
                    &receipt,
                );
                if matches!(result, StrongMediaOpenPoll::Pending)
                    && matches!(
                        pending.phase,
                        PendingStrongMediaOpenPhase::PositionRestore { .. }
                    )
                {
                    pending.phase = PendingStrongMediaOpenPhase::PositionRestore {
                        installed,
                        media_instance_id,
                        requested_position,
                        receipt,
                    };
                }
                result
            }
        };
        match result {
            StrongMediaOpenPoll::Pending => {
                self.pending_strong_media_open = Some(pending);
                StrongMediaOpenPoll::Pending
            }
            completed => completed,
        }
    }

    /// `true`, пока scheduler обязан продолжать доставлять wake/poll.
    #[must_use]
    pub(crate) const fn has_pending_prepared_media_strong(&self) -> bool {
        self.pending_strong_media_open.is_some()
    }

    /// Запрашивает lossless cancel startup transaction; enqueue-win всё равно будет drained.
    pub(crate) fn supersede_pending_prepared_media_strong(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> Result<(), StrongMediaOpenError> {
        let Some(pending) = self.pending_strong_media_open.as_ref() else {
            return Ok(());
        };
        if !matches!(pending.phase, PendingStrongMediaOpenPhase::Protocol { .. }) {
            return Ok(());
        }
        playlist_runtime
            .cancel_media_open_lossless(
                pending.request_id,
                MediaInstallCancellationCause::Superseded,
            )
            .map(|_| ())
            .map_err(StrongMediaOpenError::Command)
    }

    /// Coordinator phase machine уже сама drain-ит только готовые slots.
    fn poll_strong_media_protocol(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        pending: &mut PendingStrongMediaOpen,
        deferred_rejection: &mut Option<PlaylistMediaOpenGateError>,
        pending_staging: &mut Option<PendingStrongMediaStaging>,
    ) -> StrongMediaOpenPoll {
        let Some(snapshot) = playlist_runtime.media_open_snapshot() else {
            return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::MissingTerminal));
        };
        if snapshot.request_id != pending.request_id {
            return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::Command(
                MediaOpenCommandError::StaleRequest,
            )));
        }
        match snapshot.phase {
            MediaOpenPhase::Accepted
            | MediaOpenPhase::Preparing
            | MediaOpenPhase::PlayerStaging
            | MediaOpenPhase::AuthorizationDispatchPending
            | MediaOpenPhase::EnqueuedAtPlayerOwner => StrongMediaOpenPoll::Pending,
            MediaOpenPhase::Prepared => self.stage_prepared_playlist_source(
                playlist_runtime,
                pending,
                deferred_rejection,
                pending_staging,
                snapshot,
            ),
            MediaOpenPhase::ReadyToCommit => {
                if deferred_rejection.is_some() {
                    return StrongMediaOpenPoll::Pending;
                }
                let authorization = if playlist_runtime.playlist_install_matches(pending.request_id)
                {
                    playlist_runtime.authorize_ready_target_install(pending.request_id)
                } else {
                    playlist_runtime.authorize_ready_media_open(pending.request_id)
                };
                match authorization {
                    Ok(AuthorizationDispatchResolution::EnqueuedAtPlayerOwner) => {
                        StrongMediaOpenPoll::Pending
                    }
                    Ok(_) => StrongMediaOpenPoll::completed(Err(
                        StrongMediaOpenError::MissingAuthorizationBarrier,
                    )),
                    Err(error) => {
                        *deferred_rejection = Some(error);
                        match playlist_runtime.cancel_media_open_lossless(
                            pending.request_id,
                            MediaInstallCancellationCause::StructuralInvalidation,
                        ) {
                            Ok(_) => StrongMediaOpenPoll::Pending,
                            Err(error) => StrongMediaOpenPoll::completed(Err(
                                StrongMediaOpenError::Command(error),
                            )),
                        }
                    }
                }
            }
            MediaOpenPhase::Installed => {
                self.begin_post_installed_intent(playlist_runtime, pending)
            }
            MediaOpenPhase::Failed => {
                let terminal = match playlist_runtime.take_media_open_terminal(pending.request_id) {
                    Ok(Some(terminal)) => terminal,
                    Ok(None) => {
                        return StrongMediaOpenPoll::completed(Err(
                            StrongMediaOpenError::MissingTerminal,
                        ));
                    }
                    Err(error) => {
                        return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::Command(
                            error,
                        )));
                    }
                };
                StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::Terminal(terminal)))
            }
        }
    }

    /// Prepared source получает player resources и controller admission ровно один раз.
    fn stage_prepared_playlist_source(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        pending: &mut PendingStrongMediaOpen,
        deferred_rejection: &mut Option<PlaylistMediaOpenGateError>,
        pending_staging: &mut Option<PendingStrongMediaStaging>,
        snapshot: MediaOpenSnapshot,
    ) -> StrongMediaOpenPoll {
        let Some(staging) = pending_staging.take() else {
            return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::Command(
                MediaOpenCommandError::InvalidPhase {
                    actual: MediaOpenPhase::Prepared,
                },
            )));
        };
        let Some(descriptor) = snapshot.descriptor else {
            return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::MissingTerminal));
        };
        pending.source = Some(descriptor.active_source());
        let player_request_id = match playlist_runtime.stage_media_open_at_player(
            pending.request_id,
            MediaOpenInstallIntent {
                intent: pending.intent,
                revision: pending.intent_revision,
            },
            staging.video_resource_port,
        ) {
            Ok(player_request_id) => player_request_id,
            Err(error) => {
                *deferred_rejection = Some(error);
                return StrongMediaOpenPoll::Pending;
            }
        };
        let admission = match staging.playlist_target {
            PreparedPlaylistTarget::QueueReplacement(target_draft) => playlist_runtime
                .accept_explicit_target_install(
                    pending.request_id,
                    player_request_id,
                    target_draft,
                    pending.intent_revision,
                ),
            PreparedPlaylistTarget::RestoredCurrent(target) => playlist_runtime
                .accept_startup_restore_install(pending.request_id, player_request_id, target),
            PreparedPlaylistTarget::Planned {
                install,
                supersedes,
            } => match supersedes {
                Some(expected_request_id) => playlist_runtime.accept_superseding_playlist_install(
                    expected_request_id,
                    pending.request_id,
                    player_request_id,
                    install,
                ),
                None => playlist_runtime.accept_planned_playlist_install(
                    pending.request_id,
                    player_request_id,
                    install,
                ),
            },
        };
        if let Err(error) = admission {
            *deferred_rejection = Some(error);
            if let Err(error) = playlist_runtime.cancel_media_open_lossless(
                pending.request_id,
                MediaInstallCancellationCause::StructuralInvalidation,
            ) {
                return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::Command(error)));
            }
        }
        StrongMediaOpenPoll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cancel-win разрешает fallback, а missing/fatal terminal остаётся sticky fatal.
    #[test]
    fn fallback_classification_accepts_only_proven_pre_barrier_terminal() {
        let request_id = MediaOpenRequestId::from_non_zero(
            NonZeroU64::new(17).expect("fixture request id is non-zero"),
        );
        let cancelled = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::Cancelled {
            request_id,
            cause: MediaInstallCancellationCause::Superseded,
        });
        let fatal = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::FatalInvariant {
            request_id,
            violation:
                crate::media_open::MediaOpenInvariantViolation::MissingPlayerControlResolution,
        });

        assert!(cancelled.is_proven_pre_barrier_failure());
        assert_eq!(cancelled.terminal_request_id(), Some(request_id));
        assert!(!fatal.is_proven_pre_barrier_failure());
        assert_eq!(fatal.terminal_request_id(), Some(request_id));
        assert!(!StrongMediaOpenError::MissingTerminal.is_proven_pre_barrier_failure());
    }
}
