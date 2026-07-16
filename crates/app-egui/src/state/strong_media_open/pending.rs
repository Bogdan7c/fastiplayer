//! Renderer-bound пошаговая strong install транзакция для UI/event loop.

use super::*;

/// Renderer-bound ownership незавершённой strong install транзакции.
pub(crate) struct PendingStrongMediaOpen {
    request_id: MediaOpenRequestId,
    candidate_owner: Option<ProductionCandidateOwner>,
    source: ActiveMediaSource,
    intent: PlaybackIntent,
    phase: PendingStrongMediaOpenPhase,
}

/// Player protocol и post-Installed intent acknowledgement остаются разными этапами.
enum PendingStrongMediaOpenPhase {
    Protocol {
        deferred_rejection: Option<PlaylistMediaOpenGateError>,
    },
    PlaybackIntent {
        installed: InstalledSingleMediaOpen,
        media_instance_id: player_core::MediaInstanceId,
        receipt: player_core::PlaybackIntentUpdateReceipt,
    },
}

impl AppState {
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
                    source,
                    intent,
                    phase: PendingStrongMediaOpenPhase::Protocol {
                        deferred_rejection: Some(error),
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
            source,
            intent,
            phase: PendingStrongMediaOpenPhase::Protocol { deferred_rejection },
        });
        Ok(())
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
            },
        );
        let result = match phase {
            PendingStrongMediaOpenPhase::Protocol {
                mut deferred_rejection,
            } => {
                let result = self.poll_strong_media_protocol(
                    playlist_runtime,
                    &mut pending,
                    &mut deferred_rejection,
                );
                if matches!(result, StrongMediaOpenPoll::Pending)
                    && matches!(pending.phase, PendingStrongMediaOpenPhase::Protocol { .. })
                {
                    pending.phase = PendingStrongMediaOpenPhase::Protocol { deferred_rejection };
                }
                result
            }
            PendingStrongMediaOpenPhase::PlaybackIntent {
                mut installed,
                media_instance_id,
                receipt,
            } => {
                let result = self.poll_strong_media_intent(
                    playlist_runtime,
                    pending.request_id,
                    pending.intent,
                    &mut installed,
                    media_instance_id,
                    &receipt,
                );
                if matches!(result, StrongMediaOpenPoll::Pending) {
                    pending.phase = PendingStrongMediaOpenPhase::PlaybackIntent {
                        installed,
                        media_instance_id,
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
            MediaOpenPhase::Prepared => StrongMediaOpenPoll::completed(Err(
                StrongMediaOpenError::Command(MediaOpenCommandError::InvalidPhase {
                    actual: MediaOpenPhase::Prepared,
                }),
            )),
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

    /// Installed забирается exactly once, затем начинается отдельный intent receipt phase.
    fn begin_post_installed_intent(
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
        let installed = match self.finish_media_open_terminal(
            candidate_owner,
            pending.source.clone(),
            terminal,
        ) {
            Ok(installed) => installed,
            Err(error) => return StrongMediaOpenPoll::completed(Err(error)),
        };
        let MediaInstallCompletion::Installed {
            media_instance_id, ..
        } = installed.completion
        else {
            return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::MissingTerminal));
        };
        let exact_revision = PlaybackIntentRevision::from_non_zero(
            NonZeroU64::new(2).expect("post-Installed revision is non-zero"),
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
                    let rejection = match error {
                        player_core::PlayerWorkerSendError::Full => {
                            crate::media_open::PlayerDispatchRejection::Backpressure
                        }
                        player_core::PlayerWorkerSendError::Disconnected => {
                            crate::media_open::PlayerDispatchRejection::Disconnected
                        }
                    };
                    return StrongMediaOpenPoll::completed(Err(
                        StrongMediaOpenError::PlaybackIntentDispatch(rejection),
                    ));
                }
            };
        pending.phase = PendingStrongMediaOpenPhase::PlaybackIntent {
            installed,
            media_instance_id,
            receipt,
        };
        StrongMediaOpenPoll::Pending
    }

    /// Завершает lineage/domain commit только после неблокирующего intent acknowledgement.
    fn poll_strong_media_intent(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        request_id: MediaOpenRequestId,
        intent: PlaybackIntent,
        installed: &mut InstalledSingleMediaOpen,
        media_instance_id: player_core::MediaInstanceId,
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
        if media_instance_id != confirmed_instance {
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
            request_id,
            installed.player_request_id,
            media_instance_id,
            binding,
            installed.source.clone(),
            intent,
        ) {
            Ok(active_media) => active_media,
            Err(error) => {
                return StrongMediaOpenPoll::completed(Err(
                    StrongMediaOpenError::LineageRegistration(error),
                ));
            }
        };
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
        }))
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
