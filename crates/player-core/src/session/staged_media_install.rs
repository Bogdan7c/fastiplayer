use std::sync::Arc;
use std::time::{Duration, Instant};

use audio_core::AudioDecoderConfig;
use codec_core::VideoDecodeRequirement;
use media_core::TrackId;
use video_backend_api::{
    DetachedVideoBackendCandidateCancellationCause, DetachedVideoBackendCandidateStatus,
    DetachedVideoBackendConfigurationError, DetachedVideoBackendPortError,
    DetachedVideoBackendRequest, DetachedVideoBackendResourceError, DetachedVideoBackendSelection,
};
use video_core::VideoStreamDecodeConfig;
use video_frame_contract::VideoFrameContract;

use crate::media_install::AcceptedPlaybackIntent;
use crate::media_install::MediaInstallProtocol;
use crate::pipeline::VideoTextureReleaseEffect;
use crate::{
    CancelMediaInstall, MediaInstallCancellationCause, MediaInstallControl,
    MediaInstallControlOutcome, MediaInstallFailure, MediaInstallFailureStage,
    MediaInstallPhaseCompletionPort, MediaInstallRequestId, MediaInstallVideoResourcePort,
    MediaInstanceId, MediaOpenRequest, MediaSummary, PlaybackIntent, PlaybackIntentRevision,
    PlaybackState, PlayerError, PlayerErrorKind, PlayerEvent, PreparedMedia, StartedVideoBackend,
    TrackSelectionSnapshot,
};

use super::PlayerSession;
use super::audio_runtime::audio_decoder_init_spec_from_tracks;
use super::staged_video_preflight::{
    StagedVideoPlanner, StagedVideoPlanningMode, StagedVideoPlanningOutcome,
};

mod commit;

/// Единственный request, удерживаемый player owner-ом между preparation и terminal.
struct StagedMediaInstall {
    /// Exact correlation identity transaction-а.
    request_id: MediaInstallRequestId,

    /// Session 00B phase/terminal state machine.
    protocol: MediaInstallProtocol,

    /// Resumable preflight либо полностью готовый commit payload.
    preparation: StagedMediaPreparation,

    /// Configured detached backend half либо `None` для media без video.
    started_video_backend: Option<StartedVideoBackend>,

    /// Strong app port либо `None` только у временного compatibility facade.
    video_resource_port: Option<MediaInstallVideoResourcePort>,
}

/// Typed generation request-owned staged preflight-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StagedPreflightGeneration(u64);

/// Fence до появления будущего `MediaInstanceId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StagedPreflightFence {
    /// Exact install request не позволяет продолжить superseded candidate.
    request_id: MediaInstallRequestId,

    /// Owner generation защищает от повторного использования registry slot-а.
    generation: StagedPreflightGeneration,
}

/// Preparation state одного active staged request-а.
enum StagedMediaPreparation {
    /// Preflight временно остановлен readiness event-ом.
    Pending(PendingStagedPreflight),

    /// Все fallible player/backend stages завершены до Ready barrier-а.
    Ready(Option<PreparedStagedMedia>),
}

/// Player-owned continuation staged video preflight-а.
struct PendingStagedPreflight {
    /// Request + staged-generation fence continuation-а.
    fence: StagedPreflightFence,

    /// Detached media остаётся у player owner-а до terminal outcome.
    prepared_media: PreparedMedia,

    /// Audio plan вычисляется один раз и не повторяет fallible работу после retry.
    audio_plan: Option<StagedAudioTrackPlan>,

    /// Exact/compatibility policy исходного ingress-а.
    video_planning_mode: StagedVideoPlanningMode,

    /// Packet reader, budgets и current-track progress сохраняются между wakeup-ами.
    video_planner: StagedVideoPlanner,

    /// Earliest retry текущего temporary readiness event-а.
    retry_deadline: Option<Instant>,

    /// Независимый terminal wall-clock deadline всего preflight-а.
    timeout_deadline: Instant,
}

/// Bounded owner slot: максимум одна staged transaction и один terminal tombstone.
#[derive(Default)]
pub(super) struct StagedMediaInstallRegistry {
    /// Текущий cancellable request до accepted authorization.
    active: Option<StagedMediaInstall>,

    /// Последний terminal request нужен для typed duplicate rejection после cleanup.
    last_terminal_request_id: Option<MediaInstallRequestId>,

    /// Монотонная owner generation новых preflight continuation-ов.
    next_preflight_generation: u64,
}

/// Чистый audio plan candidate-а без decoder/output side effects.
pub(crate) struct StagedAudioTrackPlan {
    /// Выбранный audio track будущего media.
    track_id: TrackId,

    /// Deferred decoder config, который станет active только после commit-а.
    decoder_config: AudioDecoderConfig,
}

/// Чистый video plan candidate-а до получения detached backend half-а.
pub(crate) struct StagedVideoTrackPlan {
    /// Выбранный video track будущего media.
    pub(super) track_id: TrackId,

    /// Capability requirement, закреплённый за выбранным output-ом.
    pub(super) requirement: VideoDecodeRequirement,

    /// Exact decoder-to-renderer contract candidate pair-а.
    pub(super) frame_contract: VideoFrameContract,

    /// Полностью построенный decoder stream config для fallible preflight configure.
    pub(super) stream_config: VideoStreamDecodeConfig,

    /// Typed backend plan: exact для strong install или отдельный compatibility state.
    pub(super) backend_plan: StagedVideoBackendPlan,
}

/// Backend selection state staged video plan-а без двусмысленного `Option<String>`.
pub(super) enum StagedVideoBackendPlan {
    /// Capability layer выбрал точный backend до запроса detached resource.
    Exact {
        /// Canonical backend ID выбранного playable output-а.
        backend_id: String,
    },

    /// Временный compatibility ingress не запрашивает detached backend resource.
    CompatibilityDeferred,
}

/// Полностью подготовленный player-side media plan до resource request-а.
pub(crate) struct PreparedStagedMedia {
    /// Новый media и его demuxer остаются detached от active pipeline.
    prepared_media: PreparedMedia,

    /// Pure lazy-audio plan либо явное отсутствие audio.
    audio_plan: Option<StagedAudioTrackPlan>,

    /// Pure video plan либо явное отсутствие video.
    video_plan: Option<StagedVideoTrackPlan>,

    /// Instance identity выделяется до `ReadyToCommit`.
    media_instance_id: MediaInstanceId,
}

impl PreparedStagedMedia {
    /// Возвращает immutable stream config для detached decoder preflight-а.
    pub(crate) fn video_plan(&self) -> Option<&StagedVideoTrackPlan> {
        self.video_plan.as_ref()
    }

    /// Собирает единственный infallible commit payload после configured backend preflight-а.
    pub(crate) fn into_commit(
        self,
        started_video_backend: Option<StartedVideoBackend>,
    ) -> PreparedStagedMediaCommit {
        let defer_video_backend_to_compatibility_adapter =
            self.video_plan.is_some() && started_video_backend.is_none();
        PreparedStagedMediaCommit {
            prepared_media: self.prepared_media,
            audio_plan: self.audio_plan,
            video_plan: self.video_plan,
            media_instance_id: self.media_instance_id,
            started_video_backend,
            defer_video_backend_to_compatibility_adapter,
        }
    }
}

/// Linear payload atomic player ownership switch-а.
pub(crate) struct PreparedStagedMediaCommit {
    /// Detached media ownership, подготовленный source layer-ом.
    prepared_media: PreparedMedia,

    /// Уже проверенный audio plan.
    audio_plan: Option<StagedAudioTrackPlan>,

    /// Уже проверенный и configured video plan.
    video_plan: Option<StagedVideoTrackPlan>,

    /// Заранее выделенная identity будущего active instance.
    media_instance_id: MediaInstanceId,

    /// Configured detached backend либо `None` для media без video.
    started_video_backend: Option<StartedVideoBackend>,

    /// Только compatibility facade откладывает concrete backend до app adapter-а Session 10D.
    defer_video_backend_to_compatibility_adapter: bool,
}

/// Deferred release-действия прежних video frames.
#[derive(Default)]
struct RetiredVideoFrameReleases {
    /// Handles, которые после switch нужно вернуть прежнему decoder-у.
    decoder_handles: Vec<video_core::FrameResourceHandle>,

    /// Provider-owned releases уже rendered frames прежнего поколения.
    provider_releases: Vec<(
        video_core::FrameResourceHandle,
        crate::PresentFrameResourceProviderHandle,
    )>,
}

impl PlayerSession {
    /// Обрабатывает sender-registered command только если он всё ещё latest request.
    ///
    /// Более новый enqueue может supersede-нуть command ещё до его owner turn-а; такой stale
    /// payload получает lossless Cancelled terminal и не перезаписывает latest intent slot.
    pub(crate) fn stage_registered_prepared_media_install(
        &mut self,
        request_id: MediaInstallRequestId,
        prepared_media: PreparedMedia,
        initial_intent: PlaybackIntent,
        initial_intent_revision: PlaybackIntentRevision,
        install_port: Arc<dyn MediaInstallPhaseCompletionPort>,
        mut video_resource_port: MediaInstallVideoResourcePort,
    ) {
        if self
            .playback_intent_control
            .staged_request_is_latest(request_id)
        {
            self.stage_prepared_media_install(
                request_id,
                prepared_media,
                initial_intent,
                initial_intent_revision,
                install_port,
                video_resource_port,
            );
            return;
        }

        let mut protocol = MediaInstallProtocol::accept(request_id, install_port);
        let cancellation = CancelMediaInstall {
            request_id,
            cause: MediaInstallCancellationCause::Superseded,
        };
        let outcome = protocol.apply_control(MediaInstallControl::Cancel(cancellation), || {
            unreachable!("superseded pre-owner command не может commit-иться")
        });
        debug_assert_eq!(outcome, MediaInstallControlOutcome::CancellationAccepted);
        if let Err(error) = video_resource_port.publish_candidate_status(
            DetachedVideoBackendCandidateStatus::Cancelled {
                request_id,
                cause: detached_cancellation_cause(MediaInstallCancellationCause::Superseded),
            },
        ) {
            tracing::warn!(
                request_id = request_id.get(),
                %error,
                "superseded pre-owner candidate cancellation status не доставлен"
            );
        }
        self.staged_media_install.last_terminal_request_id = Some(request_id);
    }

    /// Принимает strong staged request с exact detached app resource port-ом.
    pub(crate) fn stage_prepared_media_install(
        &mut self,
        request_id: MediaInstallRequestId,
        prepared_media: PreparedMedia,
        initial_intent: PlaybackIntent,
        initial_intent_revision: PlaybackIntentRevision,
        install_port: Arc<dyn MediaInstallPhaseCompletionPort>,
        video_resource_port: MediaInstallVideoResourcePort,
    ) {
        self.stage_prepared_media_install_with_resource_mode(
            request_id,
            prepared_media,
            initial_intent,
            initial_intent_revision,
            install_port,
            Some(video_resource_port),
        );
    }

    /// Временный app compatibility facade использует тот же strong player install algorithm.
    ///
    /// До Session 10D concrete candidate port ещё не передаётся из startup/settings adapters,
    /// поэтому video backend запрашивается app-ом после atomic media install. Второго
    /// destructive player algorithm здесь нет.
    pub(crate) fn stage_prepared_media_install_compatibility(
        &mut self,
        request_id: MediaInstallRequestId,
        prepared_media: PreparedMedia,
        initial_intent: PlaybackIntent,
        initial_intent_revision: PlaybackIntentRevision,
        install_port: Arc<dyn MediaInstallPhaseCompletionPort>,
    ) {
        self.stage_prepared_media_install_with_resource_mode(
            request_id,
            prepared_media,
            initial_intent,
            initial_intent_revision,
            install_port,
            None,
        );
    }

    /// Общий preparation path для strong и временного compatibility resource mode.
    fn stage_prepared_media_install_with_resource_mode(
        &mut self,
        request_id: MediaInstallRequestId,
        prepared_media: PreparedMedia,
        initial_intent: PlaybackIntent,
        initial_intent_revision: PlaybackIntentRevision,
        install_port: Arc<dyn MediaInstallPhaseCompletionPort>,
        video_resource_port: Option<MediaInstallVideoResourcePort>,
    ) {
        self.cancel_active_staged_media_install(MediaInstallCancellationCause::Superseded);
        self.playback_intent_control.register_staged_request(
            request_id,
            AcceptedPlaybackIntent {
                revision: initial_intent_revision,
                intent: initial_intent,
            },
        );

        let protocol = MediaInstallProtocol::accept(request_id, install_port);
        let video_planning_mode = if video_resource_port.is_some() {
            StagedVideoPlanningMode::ExactBackendRequired
        } else {
            StagedVideoPlanningMode::CompatibilityDeferredAllowed
        };
        let audio_plan = match plan_staged_audio_track(prepared_media.tracks()) {
            Ok(audio_plan) => audio_plan,
            Err(error) => {
                let mut protocol = protocol;
                protocol.complete_failed(MediaInstallFailure::new(
                    MediaInstallFailureStage::AudioTrackPlanning,
                    error,
                ));
                self.playback_intent_control
                    .forget_staged_request(request_id);
                self.staged_media_install.last_terminal_request_id = Some(request_id);
                return;
            }
        };
        if self.is_shutdown_requested() {
            let mut protocol = protocol;
            protocol.complete_failed(MediaInstallFailure::new(
                MediaInstallFailureStage::OpenTransition,
                PlayerError::new(
                    PlayerErrorKind::InvalidCommand,
                    "Player session уже завершает shutdown",
                ),
            ));
            self.playback_intent_control
                .forget_staged_request(request_id);
            self.staged_media_install.last_terminal_request_id = Some(request_id);
            return;
        }

        self.staged_media_install.next_preflight_generation = self
            .staged_media_install
            .next_preflight_generation
            .saturating_add(1);
        let generation =
            StagedPreflightGeneration(self.staged_media_install.next_preflight_generation);
        let started_at = Instant::now();
        let timeout_deadline = started_at
            .checked_add(self.staged_video_preflight_timeout)
            .unwrap_or(started_at);
        let video_planner = StagedVideoPlanner::new(&prepared_media);

        self.staged_media_install.active = Some(StagedMediaInstall {
            request_id,
            protocol,
            preparation: StagedMediaPreparation::Pending(PendingStagedPreflight {
                fence: StagedPreflightFence {
                    request_id,
                    generation,
                },
                prepared_media,
                audio_plan,
                video_planning_mode,
                video_planner,
                retry_deadline: None,
                timeout_deadline,
            }),
            started_video_backend: None,
            video_resource_port,
        });
        self.service_pending_staged_preflight(started_at);
    }

    /// Продолжает один exact staged preflight owner-step без блокировки worker loop-а.
    pub(crate) fn service_pending_staged_preflight(&mut self, now: Instant) {
        let Some(mut staged_install) = self.staged_media_install.active.take() else {
            return;
        };
        let preparation = std::mem::replace(
            &mut staged_install.preparation,
            StagedMediaPreparation::Ready(None),
        );
        let StagedMediaPreparation::Pending(mut pending) = preparation else {
            staged_install.preparation = preparation;
            self.staged_media_install.active = Some(staged_install);
            return;
        };
        let current_generation =
            StagedPreflightGeneration(self.staged_media_install.next_preflight_generation);
        if pending.fence.request_id != staged_install.request_id
            || pending.fence.generation != current_generation
        {
            self.finish_staged_preflight_failure(
                staged_install,
                MediaInstallFailure::new(
                    MediaInstallFailureStage::VideoStreamConfiguration,
                    PlayerError::new(
                        PlayerErrorKind::RuntimeError,
                        "Staged preflight потерял exact request fence",
                    ),
                ),
            );
            return;
        }
        if now >= pending.timeout_deadline {
            self.finish_staged_preflight_failure(
                staged_install,
                MediaInstallFailure::new(
                    MediaInstallFailureStage::VideoPreflightTimeout,
                    PlayerError::new(
                        PlayerErrorKind::DemuxError,
                        "Staged video preflight превысил bounded wall-clock deadline",
                    ),
                ),
            );
            return;
        }
        if pending
            .retry_deadline
            .is_some_and(|retry_deadline| now < retry_deadline)
        {
            staged_install.preparation = StagedMediaPreparation::Pending(pending);
            self.staged_media_install.active = Some(staged_install);
            return;
        }

        pending.retry_deadline = None;
        let video_plan = match self.resume_staged_video_track_plan(
            &mut pending.prepared_media,
            pending.video_planning_mode,
            &mut pending.video_planner,
        ) {
            Ok(StagedVideoPlanningOutcome::Ready(video_plan)) => video_plan,
            Ok(StagedVideoPlanningOutcome::Pending(hint)) => {
                pending.retry_deadline = Some(now.checked_add(hint.retry_after()).unwrap_or(now));
                staged_install.preparation = StagedMediaPreparation::Pending(pending);
                self.staged_media_install.active = Some(staged_install);
                return;
            }
            Err(failure) => {
                self.finish_staged_preflight_failure(
                    staged_install,
                    MediaInstallFailure::new(
                        MediaInstallFailureStage::VideoStreamConfiguration,
                        failure,
                    ),
                );
                return;
            }
        };
        if let Err(error) = pending.prepared_media.prepare_playback_window() {
            self.finish_staged_preflight_failure(
                staged_install,
                MediaInstallFailure::new(
                    MediaInstallFailureStage::PlaybackWindowPreparation,
                    PlayerError::new(
                        PlayerErrorKind::SeekUnavailable,
                        format!("Не удалось подготовить playback window: {error}"),
                    ),
                ),
            );
            return;
        }
        let prepared_media = PreparedStagedMedia {
            prepared_media: pending.prepared_media,
            audio_plan: pending.audio_plan,
            video_plan,
            media_instance_id: MediaInstanceId::new_unique(),
        };
        let started_video_backend = match staged_install.video_resource_port.as_mut() {
            Some(video_resource_port) => match prepare_detached_video_backend(
                staged_install.request_id,
                &prepared_media,
                video_resource_port.as_mut(),
            ) {
                Ok(started_video_backend) => started_video_backend,
                Err(failure) => {
                    self.finish_staged_preflight_failure(staged_install, failure);
                    return;
                }
            },
            None => None,
        };

        staged_install.protocol.mark_ready_to_commit();
        staged_install.preparation = StagedMediaPreparation::Ready(Some(prepared_media));
        staged_install.started_video_backend = started_video_backend;
        self.staged_media_install.active = Some(staged_install);
    }

    /// Terminal-resolve-ит pre-Ready failure ровно один раз.
    fn finish_staged_preflight_failure(
        &mut self,
        mut staged_install: StagedMediaInstall,
        failure: MediaInstallFailure,
    ) {
        staged_install.protocol.complete_failed(failure);
        self.playback_intent_control
            .forget_staged_request(staged_install.request_id);
        self.staged_media_install.last_terminal_request_id = Some(staged_install.request_id);
    }

    /// Возвращает ближайший retry/timeout deadline active staged continuation-а.
    #[must_use]
    pub(crate) fn staged_preflight_wakeup_delay(&self, now: Instant) -> Option<Duration> {
        let staged_install = self.staged_media_install.active.as_ref()?;
        let StagedMediaPreparation::Pending(pending) = &staged_install.preparation else {
            return None;
        };
        let retry_deadline = pending.retry_deadline.unwrap_or(now);
        let nearest_deadline = retry_deadline.min(pending.timeout_deadline);
        Some(nearest_deadline.saturating_duration_since(now))
    }

    /// Применяет exact authorization/cancel в ordered owner stream.
    ///
    /// D52 update и commit сериализуются общим playback-intent mutex. Update, принятый до
    /// входа owner-а в commit closure, попадает в commit; update после выхода видит только
    /// exact just-installed request/instance mapping.
    pub(crate) fn apply_staged_media_install_control(
        &mut self,
        control: MediaInstallControl,
    ) -> MediaInstallControlOutcome {
        let control_request_id = media_install_control_request_id(control);
        let Some(mut staged_install) = self.staged_media_install.active.take() else {
            return if self.staged_media_install.last_terminal_request_id == Some(control_request_id)
            {
                MediaInstallControlOutcome::AlreadyTerminal
            } else {
                MediaInstallControlOutcome::StaleRequest
            };
        };

        if staged_install.request_id != control_request_id {
            self.staged_media_install.active = Some(staged_install);
            return MediaInstallControlOutcome::StaleRequest;
        }

        if matches!(control, MediaInstallControl::Authorize(_))
            && !staged_install.protocol.is_ready_to_commit()
        {
            self.staged_media_install.active = Some(staged_install);
            return MediaInstallControlOutcome::NotReady;
        }

        let (outcome, cancellation_cause) = match control {
            MediaInstallControl::Authorize(authorization) => {
                let StagedMediaPreparation::Ready(prepared_media) = &mut staged_install.preparation
                else {
                    self.staged_media_install.active = Some(staged_install);
                    return MediaInstallControlOutcome::NotReady;
                };
                let Some(prepared_media) = prepared_media.take() else {
                    self.staged_media_install.last_terminal_request_id =
                        Some(staged_install.request_id);
                    return MediaInstallControlOutcome::AlreadyTerminal;
                };
                let prepared_commit =
                    prepared_media.into_commit(staged_install.started_video_backend.take());
                let media_instance_id = prepared_commit.media_instance_id;
                let playback_intent_control = Arc::clone(&self.playback_intent_control);
                let outcome = staged_install.protocol.apply_control(
                    MediaInstallControl::Authorize(authorization),
                    || {
                        let applied_intent = playback_intent_control.commit_staged_request(
                            staged_install.request_id,
                            media_instance_id,
                            |accepted_intent| {
                                self.commit_staged_media(prepared_commit, accepted_intent);
                            },
                        );
                        (media_instance_id, applied_intent)
                    },
                );
                (outcome, None)
            }
            MediaInstallControl::Cancel(cancellation) => {
                let outcome = staged_install
                    .protocol
                    .apply_control(MediaInstallControl::Cancel(cancellation), || {
                        unreachable!("cancel control не вызывает media commit closure")
                    });
                self.playback_intent_control
                    .forget_staged_request(staged_install.request_id);
                (outcome, Some(cancellation.cause))
            }
        };

        match outcome {
            MediaInstallControlOutcome::AuthorizationAccepted => {
                self.staged_media_install.last_terminal_request_id =
                    Some(staged_install.request_id);
            }
            MediaInstallControlOutcome::CancellationAccepted => {
                if let Some(cause) = cancellation_cause
                    && let Some(video_resource_port) = staged_install.video_resource_port.as_mut()
                    && let Err(error) = video_resource_port.publish_candidate_status(
                        DetachedVideoBackendCandidateStatus::Cancelled {
                            request_id: staged_install.request_id,
                            cause: detached_cancellation_cause(cause),
                        },
                    )
                {
                    tracing::warn!(
                        request_id = staged_install.request_id.get(),
                        %error,
                        "player terminal опубликован, но app candidate cancellation status не доставлен"
                    );
                }
                self.staged_media_install.last_terminal_request_id =
                    Some(staged_install.request_id);
            }
            MediaInstallControlOutcome::StaleRequest | MediaInstallControlOutcome::NotReady => {
                self.staged_media_install.active = Some(staged_install);
            }
            MediaInstallControlOutcome::AlreadyTerminal => {
                self.staged_media_install.last_terminal_request_id =
                    Some(staged_install.request_id);
            }
        }
        outcome
    }

    /// Lifecycle cancellation гарантирует terminal record до teardown session resources.
    pub(crate) fn cancel_active_staged_media_install(
        &mut self,
        cause: MediaInstallCancellationCause,
    ) -> Option<MediaInstallControlOutcome> {
        let request_id = self
            .staged_media_install
            .active
            .as_ref()
            .map(|staged_install| staged_install.request_id)?;
        Some(
            self.apply_staged_media_install_control(MediaInstallControl::Cancel(
                CancelMediaInstall { request_id, cause },
            )),
        )
    }

    /// Read-only invariant hook для focused bounded-ownership tests.
    pub(crate) fn has_staged_media_install(&self) -> bool {
        self.staged_media_install.active.is_some()
    }
}

/// Получает exact detached half, проверяет backend match и fallibly configure-ит stream.
fn prepare_detached_video_backend(
    request_id: MediaInstallRequestId,
    prepared_media: &PreparedStagedMedia,
    video_resource_port: &mut (
             dyn video_backend_api::DetachedVideoBackendResourcePort<
        RequestId = MediaInstallRequestId,
    > + Send
         ),
) -> Result<Option<StartedVideoBackend>, MediaInstallFailure> {
    let Some(video_plan) = prepared_media.video_plan() else {
        return Ok(None);
    };

    let expected_backend_id = match &video_plan.backend_plan {
        StagedVideoBackendPlan::Exact { backend_id } => backend_id.as_str(),
        StagedVideoBackendPlan::CompatibilityDeferred => {
            return Err(MediaInstallFailure::new(
                MediaInstallFailureStage::CandidateVideoBackendMatching,
                PlayerError::new(
                    PlayerErrorKind::HardwareDecoderUnavailable,
                    "Strong media install не получил exact video backend plan",
                ),
            ));
        }
    };
    let selection =
        DetachedVideoBackendSelection::selected(expected_backend_id, video_plan.frame_contract);
    let reply = video_resource_port
        .request_detached_backend(DetachedVideoBackendRequest::new(request_id, selection))
        .map_err(|error| media_install_port_failure(error, "detached backend request"))?;
    let (reply_request_id, detached_backend_result) = reply.into_parts();
    if reply_request_id != request_id {
        publish_candidate_cancelled_for_matching_failure(
            video_resource_port,
            request_id,
            DetachedVideoBackendCandidateCancellationCause::Requested,
        )?;
        return Err(MediaInstallFailure::new(
            MediaInstallFailureStage::CandidateVideoBackendMatching,
            PlayerError::new(
                PlayerErrorKind::InvalidCommand,
                "Detached backend reply принадлежит другому media install request",
            ),
        ));
    }
    let detached_backend = detached_backend_result.map_err(media_install_resource_failure)?;

    if detached_backend.backend_id() != expected_backend_id {
        publish_candidate_cancelled_for_matching_failure(
            video_resource_port,
            request_id,
            DetachedVideoBackendCandidateCancellationCause::Requested,
        )?;
        return Err(MediaInstallFailure::new(
            MediaInstallFailureStage::CandidateVideoBackendMatching,
            PlayerError::new(
                PlayerErrorKind::UnsupportedRenderFormat,
                format!(
                    "Candidate backend `{}` не совпал с planned backend `{expected_backend_id}`",
                    detached_backend.backend_id()
                ),
            ),
        ));
    }

    let configured_backend =
        match detached_backend.configure_stream(video_plan.stream_config.clone()) {
            Ok(configured_backend) => configured_backend,
            Err(error) => {
                video_resource_port
                    .publish_candidate_status(
                        DetachedVideoBackendCandidateStatus::ConfigurationFailed {
                            request_id,
                            error: error.clone(),
                        },
                    )
                    .map_err(|port_error| {
                        media_install_status_failure(port_error, Some(&error.to_string()))
                    })?;
                return Err(MediaInstallFailure::new(
                    MediaInstallFailureStage::CandidateVideoBackendConfiguration,
                    player_error_from_detached_configuration(error),
                ));
            }
        };
    let backend_id = configured_backend.backend_id().to_owned();
    video_resource_port
        .publish_candidate_status(DetachedVideoBackendCandidateStatus::StreamConfigured {
            request_id,
            backend_id,
        })
        .map_err(|error| media_install_status_failure(error, None))?;

    Ok(Some(configured_backend.into_started_backend()))
}

/// Публикует cancellation exact app half-а при matching invariant failure.
fn publish_candidate_cancelled_for_matching_failure(
    video_resource_port: &mut (
             dyn video_backend_api::DetachedVideoBackendResourcePort<
        RequestId = MediaInstallRequestId,
    > + Send
         ),
    request_id: MediaInstallRequestId,
    cause: DetachedVideoBackendCandidateCancellationCause,
) -> Result<(), MediaInstallFailure> {
    video_resource_port
        .publish_candidate_status(DetachedVideoBackendCandidateStatus::Cancelled {
            request_id,
            cause,
        })
        .map_err(|error| media_install_status_failure(error, None))
}

/// Сохраняет resource exhaustion/startup/backpressure distinction в stage/message.
fn media_install_resource_failure(error: DetachedVideoBackendResourceError) -> MediaInstallFailure {
    MediaInstallFailure::new(
        MediaInstallFailureStage::CandidateVideoResourceAcquisition,
        PlayerError::new(
            PlayerErrorKind::HardwareDecoderUnavailable,
            error.to_string(),
        ),
    )
}

/// Мапит disconnect resource port-а до commit barrier в ordinary candidate failure.
fn media_install_port_failure(
    error: DetachedVideoBackendPortError,
    operation: &'static str,
) -> MediaInstallFailure {
    MediaInstallFailure::new(
        MediaInstallFailureStage::CandidateVideoResourceAcquisition,
        PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!("{operation} не доставлен: {error}"),
        ),
    )
}

/// Отделяет status-delivery failure от decoder configuration failure.
fn media_install_status_failure(
    error: DetachedVideoBackendPortError,
    configuration_error: Option<&str>,
) -> MediaInstallFailure {
    let context = configuration_error.map_or_else(
        || "candidate status не доставлен".to_owned(),
        |configuration_error| {
            format!(
                "candidate status не доставлен после configuration failure `{configuration_error}`"
            )
        },
    );
    MediaInstallFailure::new(
        MediaInstallFailureStage::CandidateVideoStatusPublication,
        PlayerError::new(PlayerErrorKind::RuntimeError, format!("{context}: {error}")),
    )
}

/// Переводит neutral detached configure outcome в существующую player taxonomy.
fn player_error_from_detached_configuration(
    error: DetachedVideoBackendConfigurationError,
) -> PlayerError {
    let kind = match error {
        DetachedVideoBackendConfigurationError::AbsentDecoder => {
            PlayerErrorKind::HardwareDecoderUnavailable
        }
        DetachedVideoBackendConfigurationError::Unsupported(_) => {
            PlayerErrorKind::UnsupportedVideoCodec
        }
        DetachedVideoBackendConfigurationError::UnexpectedClear
        | DetachedVideoBackendConfigurationError::Backpressure(_)
        | DetachedVideoBackendConfigurationError::Fatal(_) => PlayerErrorKind::RuntimeError,
    };
    PlayerError::new(kind, error.to_string())
}

/// Возвращает request ID без знания конкретного control variant в caller-е.
fn media_install_control_request_id(control: MediaInstallControl) -> MediaInstallRequestId {
    match control {
        MediaInstallControl::Authorize(authorization) => authorization.request_id,
        MediaInstallControl::Cancel(cancellation) => cancellation.request_id,
    }
}

/// Мапит полный player cancellation cause в более узкий Session 00C resource vocabulary.
fn detached_cancellation_cause(
    cause: MediaInstallCancellationCause,
) -> DetachedVideoBackendCandidateCancellationCause {
    match cause {
        MediaInstallCancellationCause::Superseded => {
            DetachedVideoBackendCandidateCancellationCause::Superseded
        }
        MediaInstallCancellationCause::LifecycleSuspended => {
            DetachedVideoBackendCandidateCancellationCause::RendererSuspended
        }
        MediaInstallCancellationCause::LifecycleShutdown => {
            DetachedVideoBackendCandidateCancellationCause::Disconnected
        }
        MediaInstallCancellationCause::UserCancelled
        | MediaInstallCancellationCause::TransportStop
        | MediaInstallCancellationCause::StructuralInvalidation => {
            DetachedVideoBackendCandidateCancellationCause::Requested
        }
    }
}

/// Строит deferred audio config, не создавая decoder/output и не меняя session.
fn plan_staged_audio_track(
    tracks: &[media_core::TrackInfo],
) -> Result<Option<StagedAudioTrackPlan>, PlayerError> {
    let Some(init_spec) = audio_decoder_init_spec_from_tracks(tracks)? else {
        return Ok(None);
    };
    let decoder_config = AudioDecoderConfig::from_track_metadata(
        init_spec.track_id.get(),
        init_spec.codec_id,
        init_spec.initial_sample_rate,
        init_spec.initial_channels,
    )
    .with_codec_private(init_spec.codec_private);

    Ok(Some(StagedAudioTrackPlan {
        track_id: init_spec.track_id,
        decoder_config,
    }))
}
