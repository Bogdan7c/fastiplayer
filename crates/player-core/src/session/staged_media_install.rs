use std::sync::Arc;
use std::time::{Duration, Instant};

use audio_core::AudioDecoderConfig;
use codec_core::VideoDecodeRequirement;
use media_core::TrackId;
use video_backend_api::{
    DetachedVideoBackendCandidateCancellationCause, DetachedVideoBackendCandidateStatus,
    DetachedVideoBackendConfigurationError, DetachedVideoBackendPortError,
    DetachedVideoBackendRequest, DetachedVideoBackendResourceError,
};
use video_core::VideoStreamDecodeConfig;
use video_frame_contract::VideoFrameContract;

use crate::media_install::MediaInstallProtocol;
use crate::pipeline::VideoTextureReleaseEffect;
use crate::{
    CancelMediaInstall, MediaInstallCancellationCause, MediaInstallControl,
    MediaInstallControlOutcome, MediaInstallFailure, MediaInstallFailureStage,
    MediaInstallPhaseCompletionPort, MediaInstallRequestId, MediaInstallVideoResourcePort,
    MediaInstanceId, MediaSummary, PlaybackState, PlayerError, PlayerErrorKind, PlayerEvent,
    PreparedMedia, StartedVideoBackend, TrackSelectionSnapshot,
};

use super::PlayerSession;
use super::audio_runtime::audio_decoder_init_spec_from_tracks;

/// Единственный request, удерживаемый player owner-ом между preparation и terminal.
struct StagedMediaInstall {
    /// Exact correlation identity transaction-а.
    request_id: MediaInstallRequestId,

    /// Session 00B phase/terminal state machine.
    protocol: MediaInstallProtocol,

    /// Prepared media/player plan, который ещё не стал active.
    prepared_media: Option<PreparedStagedMedia>,

    /// Configured detached backend half либо `None` для media без video.
    started_video_backend: Option<StartedVideoBackend>,

    /// Port остаётся жив до cancel/Installed terminal для exact app half.
    video_resource_port: MediaInstallVideoResourcePort,
}

/// Bounded owner slot: максимум одна staged transaction и один terminal tombstone.
#[derive(Default)]
pub(super) struct StagedMediaInstallRegistry {
    /// Текущий cancellable request до accepted authorization.
    active: Option<StagedMediaInstall>,

    /// Последний terminal request нужен для typed duplicate rejection после cleanup.
    last_terminal_request_id: Option<MediaInstallRequestId>,
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

    /// Backend ID выбранного playable output-а, если capability snapshot доступен.
    pub(super) expected_backend_id: Option<String>,
}

/// Полностью подготовленный player-side media plan до resource request-а.
pub(crate) struct PreparedStagedMedia {
    /// Новый media и его demuxer остаются detached от active pipeline.
    prepared_media: PreparedMedia,

    /// Pure lazy-audio plan либо явное отсутствие audio.
    audio_plan: Option<StagedAudioTrackPlan>,

    /// Pure video plan либо явное отсутствие video.
    video_plan: Option<StagedVideoTrackPlan>,

    /// Playback intent, применяемый только внутри atomic commit-а.
    autoplay: bool,

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
        PreparedStagedMediaCommit {
            prepared_media: self.prepared_media,
            audio_plan: self.audio_plan,
            video_plan: self.video_plan,
            autoplay: self.autoplay,
            media_instance_id: self.media_instance_id,
            started_video_backend,
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

    /// Latest intent этого bounded Session 00C1 request-а.
    autoplay: bool,

    /// Заранее выделенная identity будущего active instance.
    media_instance_id: MediaInstanceId,

    /// Configured detached backend либо `None` для media без video.
    started_video_backend: Option<StartedVideoBackend>,
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
    /// Принимает новый staged request, supersede-ит прежний и публикует ready/failure.
    pub(crate) fn stage_prepared_media_install(
        &mut self,
        request_id: MediaInstallRequestId,
        prepared_media: PreparedMedia,
        autoplay: bool,
        install_port: Arc<dyn MediaInstallPhaseCompletionPort>,
        mut video_resource_port: MediaInstallVideoResourcePort,
    ) {
        self.cancel_active_staged_media_install(MediaInstallCancellationCause::Superseded);

        let mut protocol = MediaInstallProtocol::accept(request_id, install_port);
        let prepared_media = match self.prepare_staged_media(prepared_media, autoplay) {
            Ok(prepared_media) => prepared_media,
            Err(failure) => {
                protocol.complete_failed(failure);
                self.staged_media_install.last_terminal_request_id = Some(request_id);
                return;
            }
        };

        let started_video_backend = match prepare_detached_video_backend(
            request_id,
            &prepared_media,
            video_resource_port.as_mut(),
        ) {
            Ok(started_video_backend) => started_video_backend,
            Err(failure) => {
                protocol.complete_failed(failure);
                self.staged_media_install.last_terminal_request_id = Some(request_id);
                return;
            }
        };

        protocol.mark_ready_to_commit();
        self.staged_media_install.active = Some(StagedMediaInstall {
            request_id,
            protocol,
            prepared_media: Some(prepared_media),
            started_video_backend,
            video_resource_port,
        });
    }

    /// Применяет exact authorization/cancel в том же ordered worker command stream.
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
                let Some(prepared_media) = staged_install.prepared_media.take() else {
                    self.staged_media_install.last_terminal_request_id =
                        Some(staged_install.request_id);
                    return MediaInstallControlOutcome::AlreadyTerminal;
                };
                let prepared_commit =
                    prepared_media.into_commit(staged_install.started_video_backend.take());
                let outcome = staged_install
                    .protocol
                    .apply_control(MediaInstallControl::Authorize(authorization), || {
                        self.commit_staged_media(prepared_commit)
                    });
                (outcome, None)
            }
            MediaInstallControl::Cancel(cancellation) => {
                let outcome = staged_install.protocol.apply_control(
                    MediaInstallControl::Cancel(cancellation),
                    MediaInstanceId::new_unique,
                );
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
                    && let Err(error) = staged_install.video_resource_port.publish_candidate_status(
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
    #[cfg(test)]
    pub(crate) fn has_staged_media_install(&self) -> bool {
        self.staged_media_install.active.is_some()
    }

    /// Выполняет все pure/fallible player preflight stages без active-state mutation.
    pub(crate) fn prepare_staged_media(
        &self,
        prepared_media: PreparedMedia,
        autoplay: bool,
    ) -> Result<PreparedStagedMedia, MediaInstallFailure> {
        if self.is_shutdown_requested() {
            return Err(MediaInstallFailure::new(
                MediaInstallFailureStage::OpenTransition,
                PlayerError::new(
                    PlayerErrorKind::InvalidCommand,
                    "Player session уже завершает shutdown",
                ),
            ));
        }

        let audio_plan = plan_staged_audio_track(prepared_media.tracks()).map_err(|error| {
            MediaInstallFailure::new(MediaInstallFailureStage::AudioTrackPlanning, error)
        })?;
        let video_plan = self
            .plan_staged_video_track(
                prepared_media.tracks(),
                prepared_media.missing_video_track_message(),
            )
            .map_err(|error| {
                MediaInstallFailure::new(MediaInstallFailureStage::VideoStreamConfiguration, error)
            })?;

        Ok(PreparedStagedMedia {
            prepared_media,
            audio_plan,
            video_plan,
            autoplay,
            media_instance_id: MediaInstanceId::new_unique(),
        })
    }

    /// В одном owner turn меняет media/decoder/generation/clocks/pipeline state.
    ///
    /// Все ordinary fallible work завершено до создания payload-а. Метод намеренно
    /// не возвращает `Result`: после входа rollback к старому media запрещён.
    pub(crate) fn commit_staged_media(
        &mut self,
        prepared_commit: PreparedStagedMediaCommit,
    ) -> MediaInstanceId {
        let PreparedStagedMediaCommit {
            prepared_media,
            audio_plan,
            video_plan,
            autoplay,
            media_instance_id,
            started_video_backend,
        } = prepared_commit;

        let media_summary = MediaSummary {
            title: prepared_media.media_title(),
            source_label: prepared_media.source_label(),
            duration: prepared_media.duration(),
        };
        let seekability = prepared_media.seekability();
        let retired_render_generation = self.pipeline.render_generation();
        let frame_releases = self.prepare_retired_video_frame_releases();
        let retired_resources = self.pipeline.retire_media_resource_owners();
        let backend_id = started_video_backend
            .as_ref()
            .map(|backend| backend.backend_id().to_owned());
        let decoder_thread = started_video_backend.map(StartedVideoBackend::into_decoder_thread);

        self.pipeline.reset_media_slots();
        self.pipeline.install_staged_video_decoder(decoder_thread);
        self.active_video_backend_id = backend_id;
        self.pipeline.advance_render_generation();

        let crate::media_opening::PreparedMediaSlots {
            demuxer,
            file_path,
            source_label,
            tracks,
            source_info,
        } = prepared_media.into_pipeline_slots();
        self.pipeline
            .install_opened_media(demuxer, file_path, source_label, tracks);
        self.pipeline.update_media_source_info(source_info);

        if let Some(audio_plan) = audio_plan {
            self.pipeline
                .install_deferred_audio_decoder_config(audio_plan.decoder_config);
            self.pipeline.select_audio_track(audio_plan.track_id);
        }
        if let Some(video_plan) = video_plan {
            self.pipeline.select_video_track_with_frame_contract(
                video_plan.track_id,
                video_plan.requirement,
                video_plan.frame_contract,
            );
        }

        self.reset_session_state_for_staged_media_commit();
        self.snapshot.media_instance_id = Some(media_instance_id);
        self.snapshot.media_title = media_summary.title.clone();
        self.snapshot.source_label = Some(media_summary.source_label.clone());
        self.set_snapshot_duration(media_summary.duration);
        self.apply_demux_seekability(seekability);
        self.reset_playback_rate_for_media_load();
        self.snapshot.selected_tracks.audio_track = self.pipeline.selected_audio_track_id();
        self.snapshot.selected_tracks.video_track = self.pipeline.selected_video_track_id();
        let committed_playback_state = if autoplay {
            PlaybackState::Buffering
        } else {
            PlaybackState::Paused
        };
        self.set_playback_state(committed_playback_state);
        self.pipeline
            .reset_audio_clock_sample(Duration::ZERO, Instant::now());
        self.clear_error();
        self.push_player_event(PlayerEvent::MediaOpened(media_summary));

        self.release_retired_video_frames(frame_releases, &retired_resources);
        let retired_decoder = retired_resources.release_non_video_owners_and_take_decoder();
        self.pipeline
            .retain_retired_video_decoder_for_outstanding_leases(
                retired_render_generation,
                retired_decoder,
            );
        media_instance_id
    }

    /// Извлекает старые frames и фиксирует их release paths до смены generation.
    fn prepare_retired_video_frame_releases(&mut self) -> RetiredVideoFrameReleases {
        let mut resource_handles = self.pipeline.clear_video_queues();
        if let Some(frame) = self.pipeline.clear_seek_preroll_fallback_video_frame() {
            resource_handles.push(frame.resource_handle);
        }
        if let Some(frame) = self.pipeline.take_present_video_frame() {
            resource_handles.push(frame.resource_handle);
        }

        let mut releases = RetiredVideoFrameReleases::default();
        for resource_handle in resource_handles {
            match self.pipeline.request_video_texture_release(resource_handle) {
                VideoTextureReleaseEffect::DeferredUntilRenderLeaseDrop => {}
                VideoTextureReleaseEffect::ReleaseViaRenderProvider(resource_provider) => {
                    releases
                        .provider_releases
                        .push((resource_handle, resource_provider));
                }
                VideoTextureReleaseEffect::ReleaseNow => {
                    releases.decoder_handles.push(resource_handle);
                }
            }
        }
        releases
    }

    /// Освобождает frame resources только после установки нового ownership state.
    fn release_retired_video_frames(
        &self,
        releases: RetiredVideoFrameReleases,
        retired_resources: &crate::pipeline::RetiredMediaResourceOwners,
    ) {
        for resource_handle in releases.decoder_handles {
            retired_resources.release_video_frame(resource_handle);
        }
        for (resource_handle, resource_provider) in releases.provider_releases {
            resource_provider.release_frame(resource_handle);
        }
    }

    /// Очищает session-owned media state без fallible decoder/audio lifecycle calls.
    fn reset_session_state_for_staged_media_commit(&mut self) {
        self.reset_diagnostics_for_media();
        self.clear_pending_video_backend_reselection();
        self.media_lifecycle.clear_pending_autoplay();
        self.seek_runtime.clear_active_commit();
        self.prepared_seek_landing.clear_promoted_seek_ownership();
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_simple_scrub();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.snapshot.clear_timeline();
        self.snapshot.selected_tracks = TrackSelectionSnapshot::default();
        self.snapshot.tracks.clear();
        self.last_audio_starvation_warn_at = None;
        self.last_seen_audio_underrun_callbacks = 0;
        self.last_tick_observed_at = None;
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

    let reply = video_resource_port
        .request_detached_backend(DetachedVideoBackendRequest::new(request_id))
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

    if let Some(expected_backend_id) = video_plan.expected_backend_id.as_deref()
        && detached_backend.backend_id() != expected_backend_id
    {
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
        | MediaInstallCancellationCause::StopAfterCurrent
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
