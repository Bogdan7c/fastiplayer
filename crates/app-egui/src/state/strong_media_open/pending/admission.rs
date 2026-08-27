//! Admission и resource staging strong media open до phase-machine polling.

use super::*;

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
        let startup_position = prepared_input.startup_position;
        let position_restore_strategy = super::prepared_position_restore_strategy(
            prepared_input.prepared_media.prepared_initial_position(),
            startup_position,
        )?;
        self.cancel_suspended_media_resume_for_explicit_open(playlist_runtime)
            .map_err(StrongMediaOpenError::LineageRegistration)?;
        let source = prepared_input.source.clone();
        let playlist_target = prepared_input.playlist_target;
        let prepared_open = match prepared_input.descriptor {
            Some(descriptor) => PreparedMediaOpen::from_caller_prepared_with_descriptor(
                prepared_input.prepared_media,
                descriptor,
            ),
            None => PreparedMediaOpen::from_caller_prepared(
                prepared_input.prepared_media,
                prepared_input.source,
                prepared_input.safe_label.clone(),
            ),
        };
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
            crate::video_backend_constraint::media_install_video_backend_constraint(
                self.video_backend_preference(),
            ),
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
                    position_restore_strategy,
                    lineage_commit: PendingStrongLineageCommit::NewLineageOrQueue,
                    pre_barrier_failure: None,
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
                        *target_draft,
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
            position_restore_strategy,
            lineage_commit: PendingStrongLineageCommit::NewLineageOrQueue,
            pre_barrier_failure: None,
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
    ) -> Result<MediaOpenRequestId, UnstagedPlaylistMediaOpenError> {
        if self.pending_strong_media_open.is_some() {
            return Err(UnstagedPlaylistMediaOpenError {
                error: StrongMediaOpenError::Start(MediaOpenStartError::Busy),
                install: Box::new(install),
            });
        }
        if let Err(error) = self.cancel_suspended_media_resume_for_explicit_open(playlist_runtime) {
            return Err(UnstagedPlaylistMediaOpenError {
                error: StrongMediaOpenError::LineageRegistration(error),
                install: Box::new(install),
            });
        }
        let request_id = match playlist_runtime.start_media_open(
            SINGLE_MEDIA_CLIENT_KEY,
            source_request,
            MediaOpenStartMode::RequireIdle,
        ) {
            Ok(MediaOpenStartOutcome::Accepted { request_id }) => request_id,
            Ok(MediaOpenStartOutcome::Coalesced { .. }) => {
                return Err(UnstagedPlaylistMediaOpenError {
                    error: StrongMediaOpenError::Start(MediaOpenStartError::Busy),
                    install: Box::new(install),
                });
            }
            Err(error) => {
                return Err(UnstagedPlaylistMediaOpenError {
                    error: StrongMediaOpenError::Start(error),
                    install: Box::new(install),
                });
            }
        };
        Ok(
            self.begin_playlist_media_staging_after_start(
                renderer, request_id, install, supersedes,
            ),
        )
    }

    /// Продолжает обычный strong protocol уже подготовленным source/demux envelope-ом.
    pub(crate) fn begin_preloaded_playlist_media_strong(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
        prepared_open: PreparedMediaOpen,
        safe_label: SafeMediaLabel,
        install: crate::playlist_runtime::PlannedPlaylistInstall,
        supersedes: Option<MediaOpenRequestId>,
    ) -> Result<MediaOpenRequestId, UnstagedPlaylistMediaOpenError> {
        if self.pending_strong_media_open.is_some() {
            return Err(UnstagedPlaylistMediaOpenError {
                error: StrongMediaOpenError::Start(MediaOpenStartError::Busy),
                install: Box::new(install),
            });
        }
        if let Err(error) = self.cancel_suspended_media_resume_for_explicit_open(playlist_runtime) {
            return Err(UnstagedPlaylistMediaOpenError {
                error: StrongMediaOpenError::LineageRegistration(error),
                install: Box::new(install),
            });
        }
        let request_id = match playlist_runtime.start_prepared_media_open(
            SINGLE_MEDIA_CLIENT_KEY,
            prepared_open,
            safe_label,
        ) {
            Ok(MediaOpenStartOutcome::Accepted { request_id }) => request_id,
            Ok(MediaOpenStartOutcome::Coalesced { .. }) => {
                return Err(UnstagedPlaylistMediaOpenError {
                    error: StrongMediaOpenError::Start(MediaOpenStartError::Busy),
                    install: Box::new(install),
                });
            }
            Err(error) => {
                return Err(UnstagedPlaylistMediaOpenError {
                    error: StrongMediaOpenError::Start(error),
                    install: Box::new(install),
                });
            }
        };
        Ok(
            self.begin_playlist_media_staging_after_start(
                renderer, request_id, install, supersedes,
            ),
        )
    }

    /// Единственная queue staging boundary после locator-based или prepared source ingress.
    fn begin_playlist_media_staging_after_start(
        &mut self,
        renderer: &Renderer,
        request_id: MediaOpenRequestId,
        install: crate::playlist_runtime::PlannedPlaylistInstall,
        supersedes: Option<MediaOpenRequestId>,
    ) -> MediaOpenRequestId {
        let intent = install.playback_intent;
        let intent_revision = install.intent_revision;
        let driver = WgpuCandidateVideoPipelineResourceDriver::new(
            renderer.instance(),
            renderer.adapter(),
            renderer.device(),
            renderer.queue(),
        );
        let (candidate_owner, video_resource_port) = player_selected_video_candidate_boundary(
            self.renderer_generation,
            self.player_worker.decoder_thread_config(),
            crate::video_backend_constraint::media_install_video_backend_constraint(
                self.video_backend_preference(),
            ),
            driver,
        );
        self.pending_strong_media_open = Some(PendingStrongMediaOpen {
            request_id,
            candidate_owner: Some(candidate_owner),
            source: None,
            intent,
            intent_revision,
            startup_position: crate::playlist_runtime::StartupPosition::KeepStart,
            position_restore_strategy: super::PreparedPositionRestoreStrategy::SeekAfterInstall,
            lineage_commit: PendingStrongLineageCommit::NewLineageOrQueue,
            pre_barrier_failure: None,
            phase: PendingStrongMediaOpenPhase::Protocol {
                deferred_rejection: None,
                pending_staging: Some(PendingStrongMediaStaging {
                    video_resource_port,
                    admission: PendingStrongMediaAdmission::Playlist(
                        PreparedPlaylistTarget::Planned {
                            install,
                            supersedes,
                        },
                    ),
                }),
            },
        });
        request_id
    }

    /// Запускает background source prepare без queue reservation и новой lineage.
    pub(crate) fn begin_same_lineage_source_media_strong(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
        source_request: MediaOpenSourceRequest,
        expected_active: crate::playlist_runtime::ActiveMediaIdentity,
        intent: PlaybackIntent,
    ) -> Result<MediaOpenRequestId, StrongMediaOpenError> {
        self.begin_same_lineage_source_media_strong_with_position(
            playlist_runtime,
            renderer,
            source_request,
            expected_active,
            intent,
            SameLineageRestorePosition::FreshCurrent,
        )
    }

    /// Запускает expiry recovery с exact late-seek target и fresh остальными controls.
    pub(crate) fn begin_vod_endpoint_recovery_strong(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
        source_request: MediaOpenSourceRequest,
        expected_active: crate::playlist_runtime::ActiveMediaIdentity,
        intent: PlaybackIntent,
        restore_position: std::time::Duration,
    ) -> Result<MediaOpenRequestId, StrongMediaOpenError> {
        self.begin_same_lineage_source_media_strong_with_position(
            playlist_runtime,
            renderer,
            source_request,
            expected_active,
            intent,
            SameLineageRestorePosition::Exact(restore_position),
        )
    }

    /// Общий owner создаёт same-lineage transaction; policy позиции остаётся typed.
    fn begin_same_lineage_source_media_strong_with_position(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
        source_request: MediaOpenSourceRequest,
        expected_active: crate::playlist_runtime::ActiveMediaIdentity,
        intent: PlaybackIntent,
        restore_position: SameLineageRestorePosition,
    ) -> Result<MediaOpenRequestId, StrongMediaOpenError> {
        if self.pending_strong_media_open.is_some() {
            return Err(StrongMediaOpenError::Start(MediaOpenStartError::Busy));
        }
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
            crate::video_backend_constraint::media_install_video_backend_constraint(
                self.video_backend_preference(),
            ),
            driver,
        );
        let initial_revision = PlaybackIntentRevision::from_non_zero(
            NonZeroU64::new(1).expect("revision is non-zero"),
        );
        self.pending_strong_media_open = Some(PendingStrongMediaOpen {
            request_id,
            candidate_owner: Some(candidate_owner),
            source: None,
            intent,
            intent_revision: initial_revision,
            startup_position: crate::playlist_runtime::StartupPosition::KeepStart,
            position_restore_strategy: super::PreparedPositionRestoreStrategy::SeekAfterInstall,
            lineage_commit: PendingStrongLineageCommit::SameLineage {
                expected_active,
                restore_position,
                restore: None,
                video_swap_checkpoint: None,
            },
            pre_barrier_failure: None,
            phase: PendingStrongMediaOpenPhase::Protocol {
                deferred_rejection: None,
                pending_staging: Some(PendingStrongMediaStaging {
                    video_resource_port,
                    admission: PendingStrongMediaAdmission::SameLineage,
                }),
            },
        });
        Ok(request_id)
    }

    /// Prepared source получает player resources и controller admission ровно один раз.
    pub(super) fn stage_prepared_playlist_source(
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
        let install_intent = MediaOpenInstallIntent {
            intent: pending.intent,
            revision: pending.intent_revision,
        };
        let stage_result = match (&pending.lineage_commit, &staging.admission) {
            (
                PendingStrongLineageCommit::SameLineage {
                    expected_active, ..
                },
                PendingStrongMediaAdmission::SameLineage,
            ) => playlist_runtime.stage_same_lineage_media_open_at_player(
                pending.request_id,
                install_intent,
                staging.video_resource_port,
                expected_active.media_instance_id(),
            ),
            _ => playlist_runtime.stage_media_open_at_player(
                pending.request_id,
                install_intent,
                staging.video_resource_port,
            ),
        };
        let player_request_id = match stage_result {
            Ok(player_request_id) => player_request_id,
            Err(error) => {
                *deferred_rejection = Some(error);
                return StrongMediaOpenPoll::Pending;
            }
        };
        let playlist_target = match staging.admission {
            PendingStrongMediaAdmission::Playlist(playlist_target) => playlist_target,
            PendingStrongMediaAdmission::SameLineage => return StrongMediaOpenPoll::Pending,
        };
        let admission = match playlist_target {
            PreparedPlaylistTarget::QueueReplacement(target_draft) => playlist_runtime
                .accept_explicit_target_install(
                    pending.request_id,
                    player_request_id,
                    *target_draft,
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
