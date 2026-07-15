//! Единственный single-media adapter поверх reusable strong media-open protocol.
//!
//! Модуль владеет orchestration, но не player/session данными: coordinator хранит
//! request phases, player — atomic install, а `AppState` — renderer pointers.

use std::num::NonZeroU64;
use std::sync::Arc;

use player_core::{
    MediaInstallCancellationCause, MediaInstallCompletion, MediaInstallRequestId, PlaybackIntent,
    PlaybackIntentRevision, PreparedMedia,
};
use render_wgpu_shell::Renderer;
use render_wgpu_video::WgpuFrameTextureViewMaterializer;

use super::AppState;
use crate::media_open::{
    ActiveMediaSource, AuthorizationDispatchResolution, MediaOpenClientKey, MediaOpenCommandError,
    MediaOpenCompletionDriveError, MediaOpenInstallIntent, MediaOpenPhase, MediaOpenRequestId,
    MediaOpenStartError, MediaOpenStartOutcome, MediaOpenTerminalOutcome, PreparedMediaOpen,
    SafeMediaLabel,
};
use crate::playlist_runtime::{PlaylistMediaOpenGateError, PlaylistRuntime};
use crate::video_pipeline_candidate::{
    ActiveVideoPipelinePointers, AppVideoPipelineCandidateOwner,
    PostInstalledVideoPipelineInvariantViolation, WgpuCandidateVideoPipelineResourceDriver,
    player_selected_video_candidate_boundary,
};
use render_wgpu_video::WgpuSubmissionQueueBinding;

/// Один policy-neutral client key сериализует временные single-media callers.
const SINGLE_MEDIA_CLIENT_KEY: MediaOpenClientKey = MediaOpenClientKey::from_non_zero(
    NonZeroU64::new(1).expect("single-media client key is non-zero"),
);

/// Prepared input старых background owners без повторного demux open.
pub(crate) struct PreparedSingleMediaOpen {
    prepared_media: PreparedMedia,
    source: ActiveMediaSource,
    safe_label: SafeMediaLabel,
}

impl PreparedSingleMediaOpen {
    /// Группирует ownership до передачи в coordinator; I/O здесь не выполняется.
    pub(crate) fn new(
        prepared_media: PreparedMedia,
        source: ActiveMediaSource,
        safe_label: SafeMediaLabel,
    ) -> Self {
        Self {
            prepared_media,
            source,
            safe_label,
        }
    }
}

/// Exact successful result, достаточный для settings restore и observable commit-а.
pub(crate) struct InstalledSingleMediaOpen {
    pub(crate) player_request_id: MediaInstallRequestId,
    pub(crate) completion: MediaInstallCompletion,
    pub(crate) source: ActiveMediaSource,
}

/// Typed failure не смешивает transport acceptance с terminal install outcome.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StrongMediaOpenError {
    #[error("media-open coordinator rejected request: {0}")]
    Start(#[from] MediaOpenStartError),
    #[error("media-open protocol command failed: {0}")]
    Command(#[from] MediaOpenCommandError),
    #[error("playlist allocator gate rejected media-open install: {0:?}")]
    PlaylistGate(PlaylistMediaOpenGateError),
    #[error("media-open completion state was lost: {0}")]
    Completion(#[from] MediaOpenCompletionDriveError),
    #[error("media-open returned an unexpected terminal outcome: {0:?}")]
    Terminal(MediaOpenTerminalOutcome),
    #[error("media-open did not publish a terminal outcome")]
    MissingTerminal,
    #[error("exact post-Installed playback intent restore was rejected: {0:?}")]
    PlaybackIntent(player_core::PlaybackIntentUpdateOutcome),
    #[error("exact post-Installed playback intent dispatch failed: {0:?}")]
    PlaybackIntentDispatch(crate::media_open::PlayerDispatchRejection),
    #[error("media-open authorization was accepted without enqueue barrier")]
    MissingAuthorizationBarrier,
    #[error("installed video candidate cannot replace missing active renderer pointers")]
    MissingActiveVideoPointers,
    #[error("post-Installed app video pointer commit violated correlation: {0:?}")]
    PostInstalledVideo(PostInstalledVideoPipelineInvariantViolation),
}

impl StrongMediaOpenError {
    /// Показывает, что player ownership уже мог переключиться и settings обязан rollback-нуть route.
    pub(crate) const fn may_have_crossed_install_barrier(&self) -> bool {
        matches!(
            self,
            Self::Completion(_)
                | Self::MissingTerminal
                | Self::PlaybackIntent(_)
                | Self::PlaybackIntentDispatch(_)
                | Self::MissingActiveVideoPointers
                | Self::PostInstalledVideo(_)
        )
    }
}

impl From<PostInstalledVideoPipelineInvariantViolation> for StrongMediaOpenError {
    fn from(violation: PostInstalledVideoPipelineInvariantViolation) -> Self {
        Self::PostInstalledVideo(violation)
    }
}

type ProductionCandidateOwner = AppVideoPipelineCandidateOwner<
    Arc<dyn WgpuFrameTextureViewMaterializer>,
    WgpuSubmissionQueueBinding,
>;

impl AppState {
    /// Проводит уже prepared media через exact Ready/authorize/barrier/Installed ordering.
    pub(crate) fn install_prepared_media_strong(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
        prepared_input: PreparedSingleMediaOpen,
        intent: PlaybackIntent,
    ) -> Result<InstalledSingleMediaOpen, StrongMediaOpenError> {
        let source = prepared_input.source.clone();
        let prepared_open = PreparedMediaOpen::from_caller_prepared(
            prepared_input.prepared_media,
            prepared_input.source,
            prepared_input.safe_label.clone(),
        );
        let start_outcome = playlist_runtime.start_prepared_media_open(
            SINGLE_MEDIA_CLIENT_KEY,
            prepared_open,
            prepared_input.safe_label,
        )?;
        let request_id = match start_outcome {
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
        if let Err(error) = playlist_runtime.stage_media_open_at_player(
            request_id,
            MediaOpenInstallIntent {
                intent,
                revision: initial_revision,
            },
            video_resource_port,
        ) {
            if playlist_runtime
                .take_media_open_terminal(request_id)?
                .is_none()
            {
                playlist_runtime.cancel_media_open(
                    request_id,
                    MediaInstallCancellationCause::StructuralInvalidation,
                )?;
                let _terminal = playlist_runtime
                    .take_media_open_terminal(request_id)?
                    .ok_or(StrongMediaOpenError::MissingTerminal)?;
            }
            return Err(StrongMediaOpenError::PlaylistGate(error));
        }

        self.drive_media_open_to_terminal(
            playlist_runtime,
            request_id,
            candidate_owner,
            source,
            intent,
        )
    }

    /// Ready запускает explicit authorization; только Installed завершает adapter success.
    fn drive_media_open_to_terminal(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        request_id: MediaOpenRequestId,
        candidate_owner: ProductionCandidateOwner,
        source: ActiveMediaSource,
        intent: PlaybackIntent,
    ) -> Result<InstalledSingleMediaOpen, StrongMediaOpenError> {
        loop {
            let snapshot = playlist_runtime
                .media_open_snapshot()
                .ok_or(StrongMediaOpenError::MissingTerminal)?;
            if snapshot.request_id != request_id {
                return Err(StrongMediaOpenError::Command(
                    MediaOpenCommandError::StaleRequest,
                ));
            }

            match snapshot.phase {
                MediaOpenPhase::Accepted
                | MediaOpenPhase::Preparing
                | MediaOpenPhase::PlayerStaging
                | MediaOpenPhase::AuthorizationDispatchPending
                | MediaOpenPhase::EnqueuedAtPlayerOwner => {
                    playlist_runtime.wait_for_media_open_progress(request_id)?;
                }
                MediaOpenPhase::Prepared => {
                    return Err(StrongMediaOpenError::Command(
                        MediaOpenCommandError::InvalidPhase {
                            actual: MediaOpenPhase::Prepared,
                        },
                    ));
                }
                MediaOpenPhase::ReadyToCommit => {
                    match playlist_runtime.authorize_ready_media_open(request_id) {
                        Ok(AuthorizationDispatchResolution::EnqueuedAtPlayerOwner) => {}
                        Ok(_) => return Err(StrongMediaOpenError::MissingAuthorizationBarrier),
                        Err(error) => {
                            self.cancel_rejected_media_open(playlist_runtime, request_id)?;
                            return Err(StrongMediaOpenError::PlaylistGate(error));
                        }
                    }
                }
                MediaOpenPhase::Installed => {
                    let terminal = playlist_runtime
                        .take_media_open_terminal(request_id)?
                        .ok_or(StrongMediaOpenError::MissingTerminal)?;
                    let installed =
                        self.finish_media_open_terminal(candidate_owner, source, terminal)?;
                    let MediaInstallCompletion::Installed {
                        media_instance_id, ..
                    } = installed.completion
                    else {
                        return Err(StrongMediaOpenError::MissingTerminal);
                    };
                    let exact_revision = PlaybackIntentRevision::from_non_zero(
                        NonZeroU64::new(2).expect("post-Installed revision is non-zero"),
                    );
                    let intent_receipt = self
                        .player_worker
                        .update_playback_intent(player_core::PlaybackIntentUpdate {
                            request_id: installed.player_request_id,
                            revision: exact_revision,
                            intent,
                        })
                        .map_err(|error| {
                            let rejection = match error {
                                player_core::PlayerWorkerSendError::Full => {
                                    crate::media_open::PlayerDispatchRejection::Backpressure
                                }
                                player_core::PlayerWorkerSendError::Disconnected => {
                                    crate::media_open::PlayerDispatchRejection::Disconnected
                                }
                            };
                            StrongMediaOpenError::PlaybackIntentDispatch(rejection)
                        })?;
                    let confirmed_instance = match intent_receipt.wait_for_outcome() {
                        player_core::PlaybackIntentUpdateOutcome::AppliedToInstalled {
                            media_instance_id,
                        } => media_instance_id,
                        outcome => return Err(StrongMediaOpenError::PlaybackIntent(outcome)),
                    };
                    if media_instance_id != confirmed_instance {
                        return Err(StrongMediaOpenError::PlaybackIntent(
                            player_core::PlaybackIntentUpdateOutcome::StaleInstance,
                        ));
                    }
                    return Ok(installed);
                }
                MediaOpenPhase::Failed => {
                    let terminal = playlist_runtime
                        .take_media_open_terminal(request_id)?
                        .ok_or(StrongMediaOpenError::MissingTerminal)?;
                    return Err(StrongMediaOpenError::Terminal(terminal));
                }
            }
        }
    }

    /// Downstream rejection остаётся pre-barrier: exact cancel освобождает candidate до return.
    fn cancel_rejected_media_open(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        request_id: MediaOpenRequestId,
    ) -> Result<(), StrongMediaOpenError> {
        if let Err(error) = playlist_runtime.cancel_media_open_lossless(
            request_id,
            MediaInstallCancellationCause::StructuralInvalidation,
        ) {
            let _fatal_terminal = playlist_runtime.take_media_open_terminal(request_id)?;
            return Err(StrongMediaOpenError::Command(error));
        }
        loop {
            let phase = playlist_runtime.wait_for_media_open_progress(request_id)?;
            if phase == MediaOpenPhase::Failed {
                let _terminal = playlist_runtime
                    .take_media_open_terminal(request_id)?
                    .ok_or(StrongMediaOpenError::MissingTerminal)?;
                return Ok(());
            }
        }
    }

    /// Коммитит app half только для matching Installed и сохраняет exact IDs.
    fn finish_media_open_terminal(
        &mut self,
        candidate_owner: ProductionCandidateOwner,
        source: ActiveMediaSource,
        terminal: MediaOpenTerminalOutcome,
    ) -> Result<InstalledSingleMediaOpen, StrongMediaOpenError> {
        let MediaOpenTerminalOutcome::Installed {
            request_id: _,
            player_request_id,
            completion,
            ..
        } = terminal
        else {
            return Err(StrongMediaOpenError::Terminal(terminal));
        };

        self.commit_installed_video_candidate(&candidate_owner, player_request_id)?;
        Ok(InstalledSingleMediaOpen {
            player_request_id,
            completion,
            source,
        })
    }

    /// Переключает renderer pointers после player Installed, сохраняя old aggregate при mismatch.
    fn commit_installed_video_candidate(
        &mut self,
        candidate_owner: &ProductionCandidateOwner,
        player_request_id: MediaInstallRequestId,
    ) -> Result<(), StrongMediaOpenError> {
        if !candidate_owner.has_candidate() {
            return Ok(());
        }
        let Some(backend_kind) = self.current_video_backend_kind.take() else {
            return Err(StrongMediaOpenError::MissingActiveVideoPointers);
        };
        let Some(materializer) = self.wgpu_frame_materializer.take() else {
            self.current_video_backend_kind = Some(backend_kind);
            return Err(StrongMediaOpenError::MissingActiveVideoPointers);
        };
        let Some(submission_binding) = self.wgpu_submission_queue_binding.take() else {
            self.current_video_backend_kind = Some(backend_kind);
            self.wgpu_frame_materializer = Some(materializer);
            return Err(StrongMediaOpenError::MissingActiveVideoPointers);
        };
        let mut active =
            ActiveVideoPipelinePointers::new(backend_kind, materializer, submission_binding);
        let commit_result = candidate_owner.commit_installed(
            player_request_id,
            self.renderer_generation,
            &mut active,
        );
        let (backend_kind, materializer, submission_binding) = active.into_parts();
        self.current_video_backend_kind = Some(backend_kind);
        self.wgpu_frame_materializer = Some(materializer);
        self.wgpu_submission_queue_binding = Some(submission_binding);
        commit_result.map_err(StrongMediaOpenError::from)
    }

    /// Controlled recreation invalidates every candidate tied to the previous renderer lifetime.
    pub(crate) fn advance_renderer_generation(&mut self) {
        self.renderer_generation =
            crate::video_pipeline_candidate::RendererGeneration::new_unique();
    }
}
