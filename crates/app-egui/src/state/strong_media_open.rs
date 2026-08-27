//! Единственный single-media adapter поверх reusable strong media-open protocol.
//!
//! Модуль владеет orchestration, но не player/session данными: coordinator хранит
//! request phases, player — atomic install, а `AppState` — renderer pointers.

mod compensation;
mod pending;
pub(super) use pending::PendingStrongMediaOpen;

use std::num::NonZeroU64;
use std::sync::Arc;

use player_core::{
    MediaInstallCancellationCause, MediaInstallCompletion, MediaInstallRequestId,
    MediaInstallVideoBackendConstraint, PlaybackIntent, PlaybackIntentRevision, PreparedMedia,
};
use render_wgpu_shell::Renderer;
use render_wgpu_video::WgpuFrameTextureViewMaterializer;

use super::AppState;
use crate::media_open::{
    ActiveMediaSource, AuthorizationDispatchResolution, MediaOpenClientKey, MediaOpenCommandError,
    MediaOpenCompletionDriveError, MediaOpenInstallIntent, MediaOpenPhase, MediaOpenRequestId,
    MediaOpenSnapshot, MediaOpenSourceRequest, MediaOpenStartError, MediaOpenStartMode,
    MediaOpenStartOutcome, MediaOpenTerminalOutcome, PreparedMediaDescriptor, PreparedMediaOpen,
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
    descriptor: Option<PreparedMediaDescriptor>,
    playlist_target: Option<PreparedPlaylistTarget>,
    startup_position: crate::playlist_runtime::StartupPosition,
}

/// Выбирает, должен ли post-install path отправить новый demux seek или принять уже доказанный.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedPositionRestoreStrategy {
    /// Обычный local/direct/extractor path ищет позицию после install.
    SeekAfterInstall,
    /// Native HLS уже открыл exact target segment и передал player-у authoritative result.
    AdoptPreparedInitialPosition,
}

/// Fail-closed нарушение связи player proof-а с app-owned restore intent-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PreparedPositionRestoreContractError {
    /// Positioned proof допустим только для persisted restore target-а.
    #[error("prepared initial position exists without a restored startup target")]
    MissingRestoreTarget,
    /// Player proof и persisted checkpoint обязаны описывать один exact target.
    #[error(
        "prepared initial target {prepared_target:?} differs from restore target {restore_target:?}"
    )]
    TargetMismatch {
        prepared_target: std::time::Duration,
        restore_target: std::time::Duration,
    },
}

/// Атомарно выводит post-install routing из самого player proof-а и persisted intent-а.
pub(crate) fn prepared_position_restore_strategy(
    prepared_initial_position: player_core::PreparedInitialPosition,
    startup_position: crate::playlist_runtime::StartupPosition,
) -> Result<PreparedPositionRestoreStrategy, PreparedPositionRestoreContractError> {
    match (prepared_initial_position, startup_position) {
        (player_core::PreparedInitialPosition::Beginning, _) => {
            Ok(PreparedPositionRestoreStrategy::SeekAfterInstall)
        }
        (
            player_core::PreparedInitialPosition::PositionedAt {
                target_position, ..
            },
            crate::playlist_runtime::StartupPosition::Restore(restore_target),
        ) if target_position.as_duration() == restore_target => {
            Ok(PreparedPositionRestoreStrategy::AdoptPreparedInitialPosition)
        }
        (
            player_core::PreparedInitialPosition::PositionedAt {
                target_position, ..
            },
            crate::playlist_runtime::StartupPosition::Restore(restore_target),
        ) => Err(PreparedPositionRestoreContractError::TargetMismatch {
            prepared_target: target_position.as_duration(),
            restore_target,
        }),
        (player_core::PreparedInitialPosition::PositionedAt { .. }, _) => {
            Err(PreparedPositionRestoreContractError::MissingRestoreTarget)
        }
    }
}

/// App intent определяет domain reservation, не устройство coordinator-а.
enum PreparedPlaylistTarget {
    QueueReplacement(Box<playlist_core::PlaylistItemDraft>),
    RestoredCurrent(crate::playlist_runtime::StartupRestoreTarget),
    Planned {
        install: crate::playlist_runtime::PlannedPlaylistInstall,
        supersedes: Option<crate::media_open::MediaOpenRequestId>,
    },
}

impl PreparedSingleMediaOpen {
    /// Группирует ownership до передачи в coordinator; I/O здесь не выполняется.
    pub(crate) fn new(
        prepared_media: PreparedMedia,
        source: ActiveMediaSource,
        safe_label: SafeMediaLabel,
    ) -> Self {
        let source = match prepared_media.playback_window() {
            Some(window) => source.with_playback_window(window),
            None => source,
        };
        Self {
            prepared_media,
            source,
            safe_label,
            descriptor: None,
            playlist_target: None,
            startup_position: crate::playlist_runtime::StartupPosition::KeepStart,
        }
    }

    /// Explicit local target обязан пройти D08 target-only replacement reservation.
    pub(crate) fn target_replacement(
        prepared_media: PreparedMedia,
        source: ActiveMediaSource,
        safe_label: SafeMediaLabel,
        target_draft: playlist_core::PlaylistItemDraft,
    ) -> Self {
        let source = match prepared_media.playback_window() {
            Some(window) => source.with_playback_window(window),
            None => source,
        };
        Self {
            prepared_media,
            source,
            safe_label,
            descriptor: None,
            playlist_target: Some(PreparedPlaylistTarget::QueueReplacement(Box::new(
                target_draft,
            ))),
            startup_position: crate::playlist_runtime::StartupPosition::KeepStart,
        }
    }

    /// Restored row уже имеет stable Item ID; traversal коммитится только после Installed.
    pub(crate) fn restored_current(
        prepared_media: PreparedMedia,
        source: ActiveMediaSource,
        safe_label: SafeMediaLabel,
        target: crate::playlist_runtime::StartupRestoreTarget,
    ) -> Self {
        let startup_position = target.position();
        let source = match prepared_media.playback_window() {
            Some(window) => source.with_playback_window(window),
            None => source,
        };
        Self {
            prepared_media,
            source,
            safe_label,
            descriptor: None,
            playlist_target: Some(PreparedPlaylistTarget::RestoredCurrent(target)),
            startup_position,
        }
    }

    /// Повторно применяет preserved window к freshly reopened settings candidate.
    #[must_use]
    pub(crate) fn with_playback_window(
        mut self,
        playback_window: Option<player_core::MediaPlaybackWindow>,
    ) -> Self {
        if let Some(playback_window) = playback_window {
            self.prepared_media = self
                .prepared_media
                .with_playback_window(playback_window)
                .expect("active static source cannot contain a dynamic live timeline");
            self.source = self.source.with_playback_window(playback_window);
        }
        self
    }

    /// Сохраняет rich descriptor background-prepared source-а до exact Installed.
    #[must_use]
    pub(crate) fn with_descriptor(mut self, descriptor: PreparedMediaDescriptor) -> Self {
        self.descriptor = Some(descriptor);
        self
    }
}

/// Exact successful result, достаточный для settings restore и observable commit-а.
#[derive(Clone)]
pub(crate) struct InstalledSingleMediaOpen {
    pub(crate) player_request_id: MediaInstallRequestId,
    pub(crate) completion: MediaInstallCompletion,
    pub(crate) source: ActiveMediaSource,
    descriptor: Box<PreparedMediaDescriptor>,
    pub(crate) position_warning: Option<crate::playlist_runtime::ResumePositionWarning>,
}

impl InstalledSingleMediaOpen {
    /// Descriptor доступен app lifecycle owner-у только как immutable installed fact.
    pub(crate) fn descriptor(&self) -> &PreparedMediaDescriptor {
        self.descriptor.as_ref()
    }
}

/// Результат одного неблокирующего шага renderer-bound strong install.
pub(crate) enum StrongMediaOpenPoll {
    /// Владелец receipt ещё не опубликовал следующий authoritative outcome.
    Pending,
    /// Транзакция завершилась exactly once и больше не принадлежит `AppState`.
    Installed(Box<InstalledSingleMediaOpen>),
    /// Typed terminal failure сохраняет barrier classification для startup owner-а.
    Failed(Box<StrongMediaOpenError>),
}

impl StrongMediaOpenPoll {
    /// Нормализует внутренний `Result`, не раздувая pending enum большим payload-ом.
    fn completed(result: Result<InstalledSingleMediaOpen, StrongMediaOpenError>) -> Self {
        match result {
            Ok(installed) => Self::Installed(Box::new(installed)),
            Err(error) => Self::Failed(Box::new(error)),
        }
    }
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
    #[error("prepared initial-position contract rejected: {0}")]
    PreparedPositionContract(#[from] PreparedPositionRestoreContractError),
    #[error("пошаговая strong media-open транзакция потеряла текущую фазу между poll-вызовами")]
    PendingPhaseStateLost,
    #[error("exact post-Installed playback intent restore was rejected: {0:?}")]
    PlaybackIntent(player_core::PlaybackIntentUpdateOutcome),
    #[error("exact post-Installed playback intent dispatch failed: {0:?}")]
    PlaybackIntentDispatch(crate::media_open::PlayerDispatchRejection),
    #[error("exact post-Installed position restore dispatch failed: {0:?}")]
    PositionRestoreDispatch(crate::media_open::PlayerDispatchRejection),
    #[error("exact post-Installed position restore owner outcome was lost")]
    PositionRestoreReceipt,
    #[error("exact post-Installed position restore failed: {0:?}")]
    PositionRestore(player_core::InstalledMediaStateRestoreOutcome),
    #[error("media-open authorization was accepted without enqueue barrier")]
    MissingAuthorizationBarrier,
    #[error("same-lineage media switch lost its expected active instance before barrier")]
    SameLineageStale,
    #[error("installed video candidate cannot replace missing active renderer pointers")]
    MissingActiveVideoPointers,
    #[error("post-Installed app video pointer commit violated correlation: {0:?}")]
    PostInstalledVideo(PostInstalledVideoPipelineInvariantViolation),
    #[error("successful strong install could not be registered in app lineage owner: {0:?}")]
    LineageRegistration(crate::playlist_runtime::ResumeCheckpointError),
    #[error("post-Installed failure was compensated after exact release: {failure}")]
    PostInstalledCompensated {
        request_id: MediaOpenRequestId,
        failure: Box<StrongMediaOpenError>,
    },
    #[error("post-Installed compensation failed ({cleanup}); original failure: {failure}")]
    PostInstalledCompensationFailed {
        request_id: MediaOpenRequestId,
        failure: Box<StrongMediaOpenError>,
        cleanup: PostInstalledCompensationFailure,
    },
}

/// Cleanup failure не маскируется исходной ошибкой post-Installed шага.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PostInstalledCompensationFailure {
    #[error("exact release dispatch failed: {0:?}")]
    ReleaseDispatch(crate::media_open::PlayerDispatchRejection),
    #[error("exact release receipt was lost")]
    ReleaseReceipt,
    #[error("exact release returned a non-applied outcome: {0:?}")]
    ReleaseOutcome(player_core::InstalledMediaReleaseOutcome),
    #[error("controller reconciliation failed: {0:?}")]
    Controller(crate::playlist_runtime::ResumeCheckpointError),
}

impl StrongMediaOpenError {
    /// Terminal request correlation нужна D22 только для proven pre-barrier failure.
    pub(crate) const fn terminal_request_id(&self) -> Option<MediaOpenRequestId> {
        match self {
            Self::Terminal(terminal) => Some(match terminal {
                MediaOpenTerminalOutcome::Installed { request_id, .. }
                | MediaOpenTerminalOutcome::Cancelled { request_id, .. }
                | MediaOpenTerminalOutcome::PreparationFailed { request_id, .. }
                | MediaOpenTerminalOutcome::PlayerRejected { request_id, .. }
                | MediaOpenTerminalOutcome::PlayerFailed { request_id, .. }
                | MediaOpenTerminalOutcome::FatalInvariant { request_id, .. } => *request_id,
            }),
            Self::PostInstalledCompensated { request_id, .. }
            | Self::PostInstalledCompensationFailed { request_id, .. } => Some(*request_id),
            _ => None,
        }
    }

    /// Показывает, что player ownership уже мог переключиться и settings обязан rollback-нуть route.
    pub(crate) const fn may_have_crossed_install_barrier(&self) -> bool {
        matches!(
            self,
            Self::Completion(_)
                | Self::MissingTerminal
                | Self::PendingPhaseStateLost
                | Self::PlaybackIntent(_)
                | Self::PlaybackIntentDispatch(_)
                | Self::PositionRestoreDispatch(_)
                | Self::PositionRestoreReceipt
                | Self::PositionRestore(_)
                | Self::MissingActiveVideoPointers
                | Self::PostInstalledVideo(_)
                | Self::PostInstalledCompensated { .. }
                | Self::PostInstalledCompensationFailed { .. }
        )
    }

    /// Только доказанный pre-barrier terminal разрешает startup fallback/skip.
    pub(crate) const fn is_proven_pre_barrier_failure(&self) -> bool {
        matches!(
            self,
            Self::Terminal(
                MediaOpenTerminalOutcome::Cancelled { .. }
                    | MediaOpenTerminalOutcome::PreparationFailed { .. }
                    | MediaOpenTerminalOutcome::PlayerRejected { .. }
                    | MediaOpenTerminalOutcome::PlayerFailed { .. }
            ) | Self::SameLineageStale
        )
    }

    /// VOD expiry может повторить только source preparation failure до install barrier-а.
    /// Cancel/stale/player failures отражают внешний lifecycle и не должны воскрешать media.
    pub(crate) const fn allows_vod_endpoint_recovery_retry(&self) -> bool {
        matches!(
            self,
            Self::Terminal(MediaOpenTerminalOutcome::PreparationFailed { .. })
        )
    }

    /// Навигация может продолжиться только до barrier-а либо после подтверждённой компенсации.
    pub(crate) const fn allows_navigation_failure_recovery(&self) -> bool {
        self.is_proven_pre_barrier_failure()
            || matches!(self, Self::PostInstalledCompensated { .. })
    }
}

impl From<PostInstalledVideoPipelineInvariantViolation> for StrongMediaOpenError {
    fn from(violation: PostInstalledVideoPipelineInvariantViolation) -> Self {
        Self::PostInstalledVideo(violation)
    }
}

pub(super) type ProductionCandidateOwner = AppVideoPipelineCandidateOwner<
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
        video_backend_constraint: MediaInstallVideoBackendConstraint,
    ) -> Result<InstalledSingleMediaOpen, StrongMediaOpenError> {
        self.cancel_suspended_media_resume_for_explicit_open(playlist_runtime)
            .map_err(StrongMediaOpenError::LineageRegistration)?;
        let source = prepared_input.source.clone();
        let playlist_target = prepared_input.playlist_target;
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
            video_backend_constraint,
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
        };
        if let Some(playlist_target) = playlist_target {
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
            if let Err(error) = admission {
                self.cancel_rejected_media_open(playlist_runtime, request_id)?;
                return Err(StrongMediaOpenError::PlaylistGate(error));
            }
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
                    let authorization = if playlist_runtime.playlist_install_matches(request_id) {
                        playlist_runtime.authorize_ready_target_install(request_id)
                    } else {
                        playlist_runtime.authorize_ready_media_open(request_id)
                    };
                    match authorization {
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
                    let installed = Self::installed_media_from_terminal(source, terminal)?;
                    if let Err(failure) = self.commit_installed_video_candidate(
                        &candidate_owner,
                        installed.player_request_id,
                    ) {
                        return Err(self.compensate_post_installed_failure_blocking(
                            playlist_runtime,
                            request_id,
                            &installed,
                            failure,
                        ));
                    }
                    let MediaInstallCompletion::Installed {
                        media_instance_id, ..
                    } = installed.completion
                    else {
                        return Err(self.compensate_post_installed_failure_blocking(
                            playlist_runtime,
                            request_id,
                            &installed,
                            StrongMediaOpenError::MissingTerminal,
                        ));
                    };
                    let exact_revision = PlaybackIntentRevision::from_non_zero(
                        NonZeroU64::new(2).expect("post-Installed revision is non-zero"),
                    );
                    let intent_receipt = match self.player_worker.update_playback_intent(
                        player_core::PlaybackIntentUpdate {
                            request_id: installed.player_request_id,
                            revision: exact_revision,
                            intent,
                        },
                    ) {
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
                            return Err(self.compensate_post_installed_failure_blocking(
                                playlist_runtime,
                                request_id,
                                &installed,
                                StrongMediaOpenError::PlaybackIntentDispatch(rejection),
                            ));
                        }
                    };
                    let confirmed_instance = match intent_receipt.wait_for_outcome() {
                        player_core::PlaybackIntentUpdateOutcome::AppliedToInstalled {
                            media_instance_id,
                        } => media_instance_id,
                        outcome => {
                            return Err(self.compensate_post_installed_failure_blocking(
                                playlist_runtime,
                                request_id,
                                &installed,
                                StrongMediaOpenError::PlaybackIntent(outcome),
                            ));
                        }
                    };
                    if media_instance_id != confirmed_instance {
                        return Err(self.compensate_post_installed_failure_blocking(
                            playlist_runtime,
                            request_id,
                            &installed,
                            StrongMediaOpenError::PlaybackIntent(
                                player_core::PlaybackIntentUpdateOutcome::StaleInstance,
                            ),
                        ));
                    }
                    let Some(binding) = self.playlist_runtime_binding() else {
                        return Err(self.compensate_post_installed_failure_blocking(
                            playlist_runtime,
                            request_id,
                            &installed,
                            StrongMediaOpenError::LineageRegistration(
                                crate::playlist_runtime::ResumeCheckpointError::StalePlayerBinding,
                            ),
                        ));
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
                            return Err(self.compensate_post_installed_failure_blocking(
                                playlist_runtime,
                                request_id,
                                &installed,
                                StrongMediaOpenError::LineageRegistration(error),
                            ));
                        }
                    };
                    let installed_snapshot = self.refresh_player_snapshot();
                    if installed_snapshot.media_instance_id == Some(media_instance_id) {
                        let checkpoint_position =
                            if installed_snapshot.timeline.mode == media_core::TimelineMode::Live {
                                crate::playlist_runtime::InstalledCheckpointPosition::Live
                            } else if installed_snapshot.timeline.seekable {
                                crate::playlist_runtime::InstalledCheckpointPosition::Seekable(
                                    installed_snapshot.current_position,
                                )
                            } else {
                                crate::playlist_runtime::InstalledCheckpointPosition::NonSeekable
                            };
                        playlist_runtime.record_installed_resume_checkpoint(
                            binding.binding_generation(),
                            media_instance_id,
                            checkpoint_position,
                        );
                    }
                    if let Some(item_id) = active_media.item_id() {
                        let cache_outcome = playlist_runtime
                            .record_successful_item_open_metadata(item_id, &installed.descriptor);
                        tracing::debug!(
                            ?cache_outcome,
                            "Exact Installed обновил last-known playlist metadata cache"
                        );
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

    /// Извлекает exact Installed facts до любого fallible app-side commit-а.
    fn installed_media_from_terminal(
        source: ActiveMediaSource,
        terminal: MediaOpenTerminalOutcome,
    ) -> Result<InstalledSingleMediaOpen, StrongMediaOpenError> {
        let MediaOpenTerminalOutcome::Installed {
            request_id: _,
            player_request_id,
            descriptor,
            completion,
        } = terminal
        else {
            return Err(StrongMediaOpenError::Terminal(terminal));
        };
        Ok(InstalledSingleMediaOpen {
            player_request_id,
            completion,
            source,
            descriptor,
            position_warning: None,
        })
    }

    /// Переключает renderer pointers после player Installed, сохраняя old aggregate при mismatch.
    pub(super) fn commit_installed_video_candidate(
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

#[cfg(test)]
mod startup_poll_tests {
    use super::*;

    fn positioned_at(target: std::time::Duration) -> player_core::PreparedInitialPosition {
        let target = media_core::MediaTime::from_duration(target);
        player_core::PreparedInitialPosition::PositionedAt {
            target_position: target,
            landing_policy: player_core::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
            result: media_core::DemuxSeekResult {
                requested_position: target,
                actual_position: target.saturating_sub(media_core::MediaDuration::from_secs(5)),
                actual_track_timestamp: None,
            },
        }
    }

    #[test]
    fn prepared_position_strategy_is_derived_from_exact_restore_contract() {
        let target = std::time::Duration::from_secs(355);
        assert_eq!(
            prepared_position_restore_strategy(
                positioned_at(target),
                crate::playlist_runtime::StartupPosition::Restore(target),
            ),
            Ok(PreparedPositionRestoreStrategy::AdoptPreparedInitialPosition)
        );
        assert_eq!(
            prepared_position_restore_strategy(
                player_core::PreparedInitialPosition::Beginning,
                crate::playlist_runtime::StartupPosition::Restore(target),
            ),
            Ok(PreparedPositionRestoreStrategy::SeekAfterInstall)
        );
    }

    #[test]
    fn prepared_position_strategy_rejects_missing_or_different_restore_target() {
        let prepared_target = std::time::Duration::from_secs(355);
        assert_eq!(
            prepared_position_restore_strategy(
                positioned_at(prepared_target),
                crate::playlist_runtime::StartupPosition::KeepStart,
            ),
            Err(PreparedPositionRestoreContractError::MissingRestoreTarget)
        );
        assert_eq!(
            prepared_position_restore_strategy(
                positioned_at(prepared_target),
                crate::playlist_runtime::StartupPosition::Restore(std::time::Duration::from_secs(
                    180
                ),),
            ),
            Err(PreparedPositionRestoreContractError::TargetMismatch {
                prepared_target,
                restore_target: std::time::Duration::from_secs(180),
            })
        );
    }

    /// Startup orchestration не должна снова вызвать blocking compatibility wrapper.
    #[test]
    fn startup_orchestration_uses_only_stepwise_strong_install_boundary() {
        let orchestration_source = include_str!("../startup_media/orchestration.rs");
        let pending_install_source = include_str!("../startup_media/pending_install.rs");

        assert!(orchestration_source.contains("begin_prepared_media_strong("));
        assert!(pending_install_source.contains("poll_prepared_media_strong("));
        assert!(!orchestration_source.contains("install_prepared_media_strong("));
        assert!(!pending_install_source.contains("install_prepared_media_strong("));
        assert!(!orchestration_source.contains("wait_for_media_open_progress("));
        assert!(!pending_install_source.contains("wait_for_media_open_progress("));
        assert!(!orchestration_source.contains("wait_for_outcome("));
        assert!(!pending_install_source.contains("wait_for_outcome("));
    }

    /// Visual checkpoint принадлежит pending-транзакции и не переснимается после Installed.
    #[test]
    fn same_lineage_visual_checkpoint_crosses_install_barrier_in_order() {
        let pending_source = include_str!("strong_media_open/pending.rs");
        let resume_source = include_str!("strong_media_open/pending/resume.rs");

        let last_capture = pending_source
            .rfind("capture_same_lineage_restore_before_barrier(")
            .expect("same-lineage path must capture the pre-barrier checkpoint");
        let authorization = pending_source
            .find("authorize_ready_same_lineage_media_open(")
            .expect("same-lineage path must cross the explicit authorization barrier");
        assert!(last_capture < authorization);

        assert!(!resume_source.contains("capture_backend_swap_video_checkpoint("));
        let terminal_finish = resume_source
            .find("installed_media_from_terminal(source, terminal)")
            .expect("Installed terminal facts must be captured before visual activation");
        let video_commit = resume_source
            .find("commit_installed_video_candidate(")
            .expect("Installed video candidate must commit before visual activation");
        let freeze_activation = resume_source
            .find("begin_backend_swap_video_freeze(")
            .expect("successful same-lineage install must activate its checkpoint");
        assert!(terminal_finish < video_commit && video_commit < freeze_activation);
        let intent_outcome = resume_source
            .find("receipt.try_outcome()")
            .expect("playback intent receipt remains the final fallible pre-commit barrier");
        let same_lineage_rebind = resume_source
            .find("complete_same_item_media_switch(")
            .expect("same-lineage identity must rebind after successful intent");
        assert!(intent_outcome < same_lineage_rebind);
    }

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
        let compensated = StrongMediaOpenError::PostInstalledCompensated {
            request_id,
            failure: Box::new(StrongMediaOpenError::PositionRestoreReceipt),
        };
        let cleanup_failed = StrongMediaOpenError::PostInstalledCompensationFailed {
            request_id,
            failure: Box::new(StrongMediaOpenError::PositionRestoreReceipt),
            cleanup: PostInstalledCompensationFailure::ReleaseReceipt,
        };
        assert!(compensated.allows_navigation_failure_recovery());
        assert_eq!(compensated.terminal_request_id(), Some(request_id));
        assert!(!cleanup_failed.allows_navigation_failure_recovery());
        assert!(cleanup_failed.may_have_crossed_install_barrier());
    }
}
