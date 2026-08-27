use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use capability_core::SystemCapabilities;
#[cfg(test)]
use codec_core::VideoCodec;
use codec_core::VideoDecodeRequirement;
use frame_server_core::{LiveScrubDiagnostics, ScrubEvent, ValidatedFrameServerConfig};
#[cfg(test)]
use media_core::MediaTime;
use tracing::{debug, info};
#[cfg(test)]
use video_core::VideoDecoderThreadHandle;

#[cfg(test)]
use crate::SeekRequest;
use crate::audio_boundary::{
    missing_audio_decoder_factory, missing_audio_output_factory,
    missing_audio_tempo_processor_factory,
};
#[cfg(test)]
use crate::decoder_boundary::PresentFrameResourceProviderHandle;
use crate::media_install::{PendingInstalledPositionRestore, PlaybackIntentControl};
use crate::playback_window::PlaybackWindowEndState;
#[cfg(test)]
use crate::seek_state::PlaybackResumeIntent;
use crate::seek_state::SeekRuntimeState;
use crate::{
    AudioDecoderFactory, AudioOutputFactory, AudioTempoProcessorFactory, CorrelatedPlayerEvent,
    FrameCounters, MediaPlaybackWindow, PlaybackDiagnostics, PlaybackPipeline, PlaybackState,
    PlayerCommand, PlayerCommandOutcome, PlayerError, PlayerErrorKind, PlayerEvent, PlayerResult,
    PlayerRuntimeApplyError, PlayerRuntimeBoundaryActivity, PlayerSnapshot,
    PlayerVideoBackendInstallIntent, StartedVideoBackend, TrackId,
};

mod audio_packet_window;
mod audio_playback_bounds;
mod audio_runtime;
mod audio_starvation;
mod audio_tempo_rate_change;
mod audio_tempo_runtime;
mod capability_selection;
mod demux_retry;
mod diagnostics_sink;
mod dynamic_timeline;
mod eof_drain;
mod exact_media_transport;
mod installed_media_restore;
mod media_lifecycle;
mod playback_rate;
mod position_clock;
mod prepared_demux_seek;
mod prepared_seek;
mod render_leases;
mod runtime_control;
mod scrub_driver;
mod scrub_orchestration;
mod seek_admission;
mod seek_commit_gates;
mod seek_completion;
mod seek_diagnostics;
mod seek_receipts;
mod seek_start;
mod seek_transaction;
mod snapshot_builder;
mod staged_media_install;
mod staged_video_preflight;
mod tick;
mod timeline_seek;
mod video_packet_framing;
mod video_requirement_error;

#[cfg(test)]
use self::audio_runtime::{
    AudioAutoplayReadiness, AudioDecoderInitSpec, audio_decoder_init_spec_from_tracks,
    classify_autoplay_audio_readiness, classify_seek_audio_gate,
};
use self::demux_retry::DemuxRetryRuntime;
use self::dynamic_timeline::DynamicTimelineRuntime;
pub(crate) use self::dynamic_timeline::DynamicTimelineWaitSource;
use self::eof_drain::EofDrainRuntime;
use self::media_lifecycle::MediaLifecycleState;
use self::prepared_demux_seek::PreparedDemuxSeekRuntime;
use self::prepared_seek::PreparedSeekLandingRuntime;
pub(crate) use self::render_leases::{LeasedPresentFrame, PresentFrameIdentity};
use self::staged_media_install::{InstalledStagedPosition, StagedMediaInstallRegistry};
pub use self::tick::{
    PlayerPipelinePause, PlayerTickConfig, PlayerTickContext, PlayerTickPacket, PlayerTickResult,
    PlayerVideoDropReason, PlayerVideoFrameDrop,
};
pub(crate) use self::tick::{
    PlayerWorkerWakeupPlan, SchedulerTimingDiagnosticsSnapshot, scheduler_timing_diagnostics,
};
#[cfg(test)]
use crate::pipeline::AudioSeekRuntimeState;
#[cfg(test)]
use media_core::TrackInfo;

/// Центральная session плеера: high-level state machine и владение playback pipeline.
pub struct PlayerSession {
    /// Последний базовый read-only snapshot без runtime diagnostics, зависящих от shell.
    snapshot: PlayerSnapshot,

    /// Абсолютная clock position текущего source-а; наружу публикуется relative position.
    current_source_position: Duration,

    /// Физическая duration source-а до применения optional playback window.
    source_duration: Option<Duration>,

    /// Активное player-owned playback window текущего installed media.
    playback_window: Option<MediaPlaybackWindow>,

    /// Active dynamic live port, observed revision и disconnect fence.
    dynamic_timeline: DynamicTimelineRuntime,

    /// Progress выбранных tracks к synthetic EOF bounded window.
    playback_window_end_state: PlaybackWindowEndState,

    /// Media pipeline, закрытый от sibling modules за session-owned boundary methods.
    pipeline: PlaybackPipeline,

    /// Factory, через которую session лениво создаёт audio decoder по первому selected packet-у.
    audio_decoder_factory: Arc<dyn AudioDecoderFactory>,

    /// Factory, через которую session лениво создаёт audio output после decoded spec.
    audio_output_factory: Arc<dyn AudioOutputFactory>,

    /// Factory, через которую session создаёт tempo processor для non-1x decoded PCM.
    audio_tempo_processor_factory: Arc<dyn AudioTempoProcessorFactory>,

    /// Последняя громкость, которая реально могла быть слышимой.
    ///
    /// Нужна, чтобы mute/unmute был session-owned поведением, а UI не угадывал,
    /// какую громкость надо восстановить после `SetVolume(0.0)`.
    last_nonzero_volume: Option<f32>,

    /// Codec/render-neutral diagnostics aggregator для текущего media pipeline.
    diagnostics: PlaybackDiagnostics,

    /// События, накопленные после последнего drain.
    pending_events: Vec<CorrelatedPlayerEvent>,

    /// Scrub/SeekLanding события для отдельного S16 worker boundary.
    pending_scrub_events: Vec<ScrubEvent>,

    /// State, принадлежащий media lifecycle boundary.
    media_lifecycle: MediaLifecycleState,

    /// Единственная bounded strong media transaction либо её last terminal tombstone.
    staged_media_install: StagedMediaInstallRegistry,

    /// Wall-clock policy resumable staged video preflight-а.
    staged_video_preflight_timeout: Duration,

    /// Retry state временно не готового demuxer-а для exact installed generation.
    demux_retry: DemuxRetryRuntime,

    /// Shared D52 linearization state между sender boundary и player owner turn-ом.
    playback_intent_control: Arc<PlaybackIntentControl>,

    /// Был ли принят shutdown-запрос.
    shutdown_requested: bool,

    /// Runtime EOF-drain boundary: наружу выходит только через intent methods.
    eof_drain: EofDrainRuntime,

    /// Последний системный capability report, полученный от shell/backend layer.
    capabilities: Option<SystemCapabilities>,

    /// Canonical backend id текущего started video backend-а.
    ///
    /// Нужен, чтобы stream selection не брала `VideoFrameContract` от другого
    /// playable backend-а из общего system capability report-а.
    active_video_backend_id: Option<String>,

    /// Validated frame-server policy snapshot, с которым создан этот worker/session.
    frame_server_config: ValidatedFrameServerConfig,

    /// Runtime state seek transaction/scrub/trace markers, которым владеет session.
    seek_runtime: SeekRuntimeState,

    /// Exact prepared-media demux seek port и pending receipt fence.
    prepared_demux_seek: PreparedDemuxSeekRuntime,

    /// Request-owned completion активного external exact seek-а.
    pending_exact_timeline_seek:
        Option<crate::media_install::timeline_seek::PendingExactTimelineSeek>,

    /// Request-owned completion position restore-а до exact seek commit-а.
    pending_installed_position_restore: Option<PendingInstalledPositionRestore>,
    installed_staged_position: Option<InstalledStagedPosition>,
    /// S17B bridge: neutral prepared working set плюс seek-owned promoted lease.
    prepared_seek_landing: PreparedSeekLandingRuntime,

    /// Отложенный выбор video-трека: активный backend не может декодировать стрим,
    /// и session ждёт, пока shell установит совместимый backend.
    ///
    /// Хранит requirement и track id, чтобы после `set_video_backend` активировать
    /// трек уже на новом backend-е без переоткрытия media.
    pending_video_backend_reselection: Option<PendingVideoBackendReselection>,

    /// Rate limit для warn о полностью осушенном audio output buffer.
    ///
    /// Осушение при Playing — это слышимый пропуск звука; лог обязан показать
    /// глубины очередей, чтобы отличить video-starved demux от audio-путей.
    last_audio_starvation_warn_at: Option<Instant>,

    /// Последний увиденный счётчик CPAL underrun callbacks.
    ///
    /// Проверка уровня буфера в конце tick-а слепа к голоданию, которое тот же
    /// tick уже успел залатать; дельта device-side underruns ловит каждый
    /// реальный разрыв независимо от момента refill.
    last_seen_audio_underrun_callbacks: u64,

    /// Момент предыдущего tick-а для диагностики стопора worker-треда.
    last_tick_observed_at: Option<Instant>,
}

/// Отложенный выбор video-трека до установки совместимого decode backend-а.
#[derive(Debug, Clone)]
struct PendingVideoBackendReselection {
    /// Decode requirement, под который нужен совместимый backend.
    requirement: VideoDecodeRequirement,

    /// Track id, который нужно активировать после смены backend-а.
    track_id: TrackId,

    /// Способ продолжения demux после установки decoder-а.
    resume_strategy: BackendReselectionResumeStrategy,
}

/// Продолжение playback после установки backend-а под отложенный video track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendReselectionResumeStrategy {
    /// Decoder отсутствовал до появления track-а: продолжаем текущий demux и ждём
    /// ближайший keyframe, потому что rewind может быть ещё не доказан seek index-ом.
    ContinueForwardToKeyframe,

    /// Старый decoder обслуживал playback: возвращаем demux к текущей позиции,
    /// чтобы новый backend не создавал видимый скачок вперёд.
    ReseekCurrentPosition,
}

impl PlayerSession {
    /// Создаёт пустую player session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Создаёт session с audio output factory, переданной composition layer-ом.
    #[must_use]
    pub fn with_audio_output_factory(audio_output_factory: Arc<dyn AudioOutputFactory>) -> Self {
        Self {
            audio_output_factory,
            ..Self::default()
        }
    }

    /// Применяет worker-owned wall-clock policy staged preflight-а.
    pub(crate) fn with_staged_video_preflight_timeout(mut self, timeout: Duration) -> Self {
        self.staged_video_preflight_timeout = timeout;
        self
    }

    /// Создаёт session с audio decoder factory, переданной composition layer-ом.
    #[must_use]
    pub fn with_audio_decoder_factory(audio_decoder_factory: Arc<dyn AudioDecoderFactory>) -> Self {
        Self {
            audio_decoder_factory,
            ..Self::default()
        }
    }

    /// Создаёт session с обеими production audio factories без concrete deps в core.
    #[must_use]
    pub fn with_audio_factories(
        audio_decoder_factory: Arc<dyn AudioDecoderFactory>,
        audio_output_factory: Arc<dyn AudioOutputFactory>,
    ) -> Self {
        Self {
            audio_decoder_factory,
            audio_output_factory,
            ..Self::default()
        }
    }

    /// Подключает worker-shared D52 control state без передачи session ownership наружу.
    #[must_use]
    pub(crate) fn with_playback_intent_control(
        mut self,
        playback_intent_control: Arc<PlaybackIntentControl>,
    ) -> Self {
        self.playback_intent_control = playback_intent_control;
        self
    }

    /// Подставляет tempo processor factory, которой владеет composition layer.
    #[must_use]
    pub fn with_audio_tempo_processor_factory(
        mut self,
        audio_tempo_processor_factory: Arc<dyn AudioTempoProcessorFactory>,
    ) -> Self {
        self.audio_tempo_processor_factory = audio_tempo_processor_factory;
        self
    }

    /// Возвращает последний базовый immutable snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &PlayerSnapshot {
        &self.snapshot
    }

    /// Собирает актуальный snapshot для UI, renderer и desktop integration.
    #[must_use]
    pub fn snapshot_with_frame_counters(&self, frame_counters: FrameCounters) -> PlayerSnapshot {
        snapshot_builder::build_snapshot(self, frame_counters)
    }

    /// Сообщает, что session уже получила shutdown-запрос.
    #[must_use]
    pub const fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested
    }

    /// Возвращает effective playback state с учётом EOF-drain режима.
    #[must_use]
    pub const fn playback_state(&self) -> PlaybackState {
        if self.is_eof_draining() {
            PlaybackState::Draining
        } else {
            self.snapshot.playback_state
        }
    }

    /// Возвращает `true`, если demux loop должен читать новые packets.
    #[must_use]
    pub fn is_demuxing_active(&self) -> bool {
        matches!(
            self.snapshot.playback_state,
            PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Seeking
        ) || self.seek_landing_decode_active()
    }

    /// Возвращает `true`, если scheduler может менять present frame.
    #[must_use]
    pub fn can_present_video(&self) -> bool {
        matches!(
            self.snapshot.playback_state,
            PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Seeking
        ) || self.is_eof_draining()
            || self.seek_landing_decode_active()
    }

    /// S17A exception: public `Scrubbing` остаётся не-playback state, но active
    /// SeekLanding должен демаксить/декодить через уже существующий playback decoder.
    #[must_use]
    pub(crate) fn seek_landing_decode_active(&self) -> bool {
        self.snapshot.playback_state == PlaybackState::Scrubbing
            && self.seek_runtime.seek_landing_decode_active()
    }

    /// Возвращает `true`, если текущая session владеет открытым demuxer-ом.
    #[must_use]
    pub fn has_loaded_media_pipeline(&self) -> bool {
        self.pipeline.has_demuxer()
    }

    /// Сообщает, какая операция сейчас эксклюзивно владеет pipeline reconfigure boundary.
    ///
    /// Staged media install остаётся lifecycle-owner-ом и до `ReadyToCommit`, и после него:
    /// settings rebuild не должен обогнать его atomic authorization и затем быть затёрт
    /// заранее подготовленным backend candidate-ом.
    #[must_use]
    pub(crate) fn runtime_reconfigure_boundary_activity(
        &self,
    ) -> Option<PlayerRuntimeBoundaryActivity> {
        if self.seek_runtime.simple_scrub_active()
            || self.seek_runtime.active_seek_landing_is_live_scrub()
        {
            return Some(PlayerRuntimeBoundaryActivity::Scrub);
        }
        if self.seek_runtime.has_active_commit() || self.seek_runtime.seek_landing_active() {
            return Some(PlayerRuntimeBoundaryActivity::Seek);
        }
        if matches!(self.playback_state(), PlaybackState::Opening)
            || self.has_staged_media_install()
            || self.has_pending_video_backend_reselection()
        {
            return Some(PlayerRuntimeBoundaryActivity::PipelineLifecycle);
        }

        None
    }

    /// Возвращает neutral decoder activity status без раскрытия `PlaybackPipeline` worker-у.
    #[must_use]
    pub(crate) fn video_decoder_activity_status(
        &self,
    ) -> crate::pipeline::VideoDecoderActivityStatus {
        self.pipeline.video_decoder_activity_status()
    }

    /// Настраивает минимальный active Accurate preroll state для worker-level tests.
    #[cfg(test)]
    pub(crate) fn install_active_accurate_preroll_decoder_for_tests(
        &mut self,
        decoder_thread: impl VideoDecoderThreadHandle<
            ResourceProvider = PresentFrameResourceProviderHandle,
        > + 'static,
        target_position: Duration,
    ) -> u64 {
        self.pipeline.set_video_decoder_thread(decoder_thread);
        self.pipeline.select_video_track(
            TrackId::new(1),
            VideoDecodeRequirement::new(VideoCodec::Vp9),
        );
        self.set_playback_state(PlaybackState::Seeking);

        let generation = self.pipeline.begin_seek_generation();
        self.begin_seek_trace_for_tests(generation);
        self.set_seek_commit_for_tests(Some(crate::seek_state::SeekCommitState {
            generation,
            seek_mode: crate::SeekMode::Accurate,
            target_position: MediaTime::from_duration(target_position),
            actual_position: MediaTime::from_duration(target_position),
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
            started_at: Instant::now(),
            public_accepted_at: Instant::now(),
            resume_intent: PlaybackResumeIntent::Pause,
            target_retention: crate::seek_state::SeekTargetRetention::ExactPublicRange,
        }));
        self.mark_decoder_output_floor_applied_for_tests(generation, target_position);

        generation
    }

    /// Возвращает путь текущего локального файла, если media было открыто с диска.
    #[must_use]
    pub fn current_file_path(&self) -> Option<&Path> {
        self.pipeline.source_file_path()
    }

    /// Применяет команду к state machine.
    pub fn dispatch_command(
        &mut self,
        command: PlayerCommand,
    ) -> PlayerResult<PlayerCommandOutcome> {
        debug!(
            command = ?command,
            playback_state = ?self.playback_state(),
            draining_after_eof = self.is_eof_draining(),
            current_position_ms = self.snapshot.current_position.as_secs_f64() * 1000.0,
            duration_ms = ?self
                .snapshot
                .duration
                .map(|duration| duration.as_secs_f64() * 1000.0),
            "Player command received"
        );

        let command_result = match command {
            PlayerCommand::OpenMedia(request) => self.open_media(request),
            PlayerCommand::Play => self.play(),
            PlayerCommand::Pause => self.pause(),
            PlayerCommand::TogglePlayback => self.toggle_playback(),
            PlayerCommand::Seek(request) => self.seek(request),
            PlayerCommand::BeginScrub { live_scrub } => self.begin_scrub(live_scrub),
            PlayerCommand::UpdateScrub(request) => self.update_scrub(request),
            PlayerCommand::PreviewScrub {
                request,
                live_scrub,
            } => self.preview_scrub(request, live_scrub),
            PlayerCommand::EndScrub { policy, live_scrub } => {
                return self
                    .end_scrub(policy, live_scrub)
                    .map(PlayerCommandOutcome::ScrubCommit);
            }
            PlayerCommand::Stop => self.stop(),
            PlayerCommand::SetPlaybackRate(playback_rate) => {
                return Ok(self.set_playback_rate(playback_rate));
            }
            PlayerCommand::SetVolume(volume) => self.set_volume(volume),
            PlayerCommand::ToggleMute { fallback_volume } => self.toggle_mute(fallback_volume),
            PlayerCommand::SelectVideoTrack(track_id) => self.select_video_track(track_id),
            PlayerCommand::SelectAudioTrack(track_id) => self.select_audio_track(track_id),
            PlayerCommand::SelectSubtitleTrack(track_id) => self.select_subtitle_track(track_id),
            PlayerCommand::SelectQuality(selection) => self.select_quality(selection),
            PlayerCommand::ReloadConfig => self.reload_config(),
            PlayerCommand::Shutdown => self.shutdown(),
        };

        command_result.map(|()| PlayerCommandOutcome::Applied)
    }

    /// Отмечает fatal error от media pipeline.
    pub fn mark_fatal_error(&mut self, error: PlayerError) {
        self.fail_pending_seek_receipts(error.clone());
        self.snapshot.last_error = Some(error.clone());
        self.set_playback_state(PlaybackState::Failed);
        self.push_player_event(PlayerEvent::FatalError(error));
    }

    /// Забирает накопленные события без correlation envelope для direct-session compatibility.
    ///
    /// Worker boundary использует `take_correlated_events`; этот метод сохраняет прежний
    /// single-session test/API contract.
    #[must_use]
    pub fn take_events(&mut self) -> Vec<PlayerEvent> {
        self.take_correlated_events()
            .into_iter()
            .map(|correlated_event| correlated_event.event)
            .collect()
    }

    /// Забирает события вместе с immutable media-instance correlation.
    #[must_use]
    pub(crate) fn take_correlated_events(&mut self) -> Vec<CorrelatedPlayerEvent> {
        std::mem::take(&mut self.pending_events)
    }

    /// Фиксирует current instance identity в момент создания player event-а.
    pub(super) fn push_player_event(&mut self, event: PlayerEvent) {
        self.pending_events.push(CorrelatedPlayerEvent::new(
            self.snapshot.media_instance_id,
            event,
        ));
    }

    /// Забирает normalized scrub events, не смешивая их с обычными player events.
    #[must_use]
    pub(crate) fn take_scrub_events(&mut self) -> Vec<ScrubEvent> {
        std::mem::take(&mut self.pending_scrub_events)
    }

    /// Добавляет scrub event, прикрепляя live diagnostics только к live-scrub context-ам.
    pub(crate) fn push_scrub_event_with_live_diagnostics(
        &mut self,
        event: ScrubEvent,
        live_scrub: Option<LiveScrubDiagnostics>,
    ) {
        let enriched_event = match live_scrub {
            Some(live_scrub) => event.with_live_scrub_diagnostics(live_scrub),
            None => event,
        };
        let diagnostics = match &enriched_event {
            ScrubEvent::Started(event) => event.diagnostics,
            ScrubEvent::Progress(event) => event.diagnostics,
            ScrubEvent::PreviewFrameReady(event) => event.diagnostics,
            ScrubEvent::ResumePending(event) => event.diagnostics,
            ScrubEvent::Committed(event) => event.diagnostics,
            ScrubEvent::MatchedPlayback(event) => event.diagnostics,
            ScrubEvent::Cancelled(event) => event.diagnostics,
            ScrubEvent::Failed(event) => event.diagnostics,
        };
        self.record_scrub_event_diagnostics(diagnostics);
        self.pending_scrub_events.push(enriched_event);
    }

    /// Переводит renderer resource ids в новое поколение после полного reset media pipeline.
    fn advance_render_generation(&mut self) {
        self.pipeline.advance_render_generation();
    }

    /// Обновляет оценку длительности video frame по очередному decoded PTS.
    pub fn observe_video_frame_pts(&mut self, pts: Duration) {
        self.pipeline.observe_decoded_video_frame_pts(pts);
    }

    /// Очищает video frame queue и present frame, освобождая texture slots.
    pub fn clear_video_frames(&mut self) {
        self.clear_seek_preroll_fallback_frame();
        let queued_resource_handles = self.pipeline.clear_video_queues();
        let present_resource_handle = self
            .pipeline
            .take_present_video_frame()
            .map(|frame| frame.resource_handle);

        for resource_handle in queued_resource_handles {
            self.release_video_texture(resource_handle);
        }
        if let Some(resource_handle) = present_resource_handle {
            self.release_video_texture(resource_handle);
        }
    }

    /// Очищает только очередь будущих video frames, сохраняя текущий кадр на экране.
    pub fn clear_queued_video_frames(&mut self) {
        self.clear_seek_preroll_fallback_frame();
        let queued_resource_handles = self.pipeline.clear_video_queues();

        for resource_handle in queued_resource_handles {
            self.release_video_texture(resource_handle);
        }
    }

    /// Сбрасывает decoded frames, оставшиеся в decoder→player канале от
    /// предыдущего stream config-а.
    ///
    /// `seek_generation` обнуляется при media reset, поэтому stale кадры
    /// прошлого media не отсекаются generation-проверкой и иначе дошли бы до
    /// contract validation (например, 10-bit HDR кадр против 8-bit SDR
    /// контракта). Вызывать только после остановки decoder-production
    /// (`clear_video_decoder_stream`), чтобы канал не пополнялся во время drain-а.
    pub(crate) fn discard_pending_decoded_video_frames(&mut self) {
        while let Some(frame) = self.pipeline.try_recv_decoded_video_frame() {
            self.release_video_texture(frame.resource_handle);
        }
    }

    /// Освобождает stale present frame, если final seek упёрся в texture pressure.
    ///
    /// Final seek не должен навсегда держать старый кадр, когда именно его
    /// texture/surface slot мешает decoder-у выдать target frame. Решение
    /// остаётся на уровне session: pipeline только отдаёт владение frame-ом, а
    /// render lease accounting и deferred release проходят через обычный
    /// `release_video_texture()` boundary.
    pub(crate) fn release_stale_present_frame_for_final_seek_texture_pressure(
        &mut self,
        min_available_texture_slots: usize,
    ) -> bool {
        if self.active_final_seek_target().is_none() || !self.snapshot.timeline.stale_frame {
            return false;
        }

        let Some(texture_slots) = self.pipeline.video_decoder_resource_snapshot() else {
            return false;
        };

        if texture_slots.available_slots() > min_available_texture_slots {
            return false;
        }

        let Some(stale_frame) = self.pipeline.take_present_video_frame() else {
            return false;
        };

        let frame_pts = stale_frame.pts;
        let resource_handle = stale_frame.resource_handle;
        self.release_video_texture(resource_handle);
        debug!(
            pts_ms = frame_pts.as_millis(),
            handle = resource_handle.0,
            available_texture_slots = texture_slots.available_slots(),
            min_available_texture_slots,
            "Final seek released stale present frame under texture pressure"
        );
        true
    }

    /// Устанавливает video backend через transactional decoder-handle swap.
    ///
    /// Новый handle становится active только на время проверки stream config/re-seek.
    /// При ошибке прежний handle возвращается в pipeline и повторно конфигурируется;
    /// исходная и rollback ошибки остаются раздельными в boundary error.
    pub fn install_video_backend_with_intent(
        &mut self,
        started_backend: StartedVideoBackend,
        intent: PlayerVideoBackendInstallIntent,
    ) -> Result<(), PlayerRuntimeApplyError> {
        if intent == PlayerVideoBackendInstallIntent::SettingsReconfigure
            && let Some(activity) = self.runtime_reconfigure_boundary_activity()
        {
            return Err(PlayerRuntimeApplyError::RuntimeBusy(activity));
        }

        let backend_id = started_backend.backend_id().to_owned();
        self.clear_active_seek_decoder_output_floor("video backend replacement")
            .map_err(|error| PlayerRuntimeApplyError::Fatal(error.to_string()))?;

        if self.pipeline.has_active_video_decoder() {
            self.clear_video_frames();
            self.advance_render_generation();
        }

        let previous_backend_id = self.active_video_backend_id.clone();
        let previous_decoder = self
            .pipeline
            .replace_video_decoder_thread_handle(started_backend.into_decoder_thread());
        self.active_video_backend_id = Some(backend_id);

        let configure_result = if self.has_pending_video_backend_reselection() {
            self.retry_pending_video_backend_reselection()
        } else {
            self.configure_active_video_decoder_stream()
        };

        if let Err(apply_error) = configure_result {
            let rollback_result = match previous_decoder {
                Some(previous_decoder) => {
                    self.pipeline
                        .replace_video_decoder_thread_handle(previous_decoder);
                    self.active_video_backend_id = previous_backend_id;
                    self.configure_active_video_decoder_stream()
                }
                None => {
                    self.active_video_backend_id = previous_backend_id;
                    Ok(())
                }
            };

            return match rollback_result {
                Ok(()) => Err(PlayerRuntimeApplyError::Fatal(apply_error.to_string())),
                Err(rollback_error) => Err(PlayerRuntimeApplyError::ApplyAndRollbackFailed {
                    apply_error: apply_error.to_string(),
                    rollback_error: rollback_error.to_string(),
                }),
            };
        }

        info!(
            backend = self.pipeline.video_backend_name(),
            "Video backend started"
        );
        Ok(())
    }

    /// Compatibility/startup boundary для обычной pipeline-demand установки backend-а.
    pub fn set_video_backend(&mut self, started_backend: StartedVideoBackend) {
        if let Err(error) = self.install_video_backend_with_intent(
            started_backend,
            PlayerVideoBackendInstallIntent::PipelineDemand,
        ) {
            self.mark_fatal_error(PlayerError::new(
                PlayerErrorKind::RuntimeError,
                error.to_string(),
            ));
        }
    }

    /// Отклоняет отложенный выбор video backend-а, когда shell не нашёл совместимый план.
    pub fn reject_pending_video_backend_with_reason(&mut self, reason: String) {
        self.reject_pending_video_backend(reason);
    }
}

impl Default for PlayerSession {
    /// Создаёт пустую session с default snapshot.
    fn default() -> Self {
        Self {
            snapshot: PlayerSnapshot::default(),
            current_source_position: Duration::ZERO,
            source_duration: None,
            playback_window: None,
            dynamic_timeline: DynamicTimelineRuntime::default(),
            playback_window_end_state: PlaybackWindowEndState::default(),
            pipeline: PlaybackPipeline::default(),
            audio_decoder_factory: missing_audio_decoder_factory(),
            audio_output_factory: missing_audio_output_factory(),
            audio_tempo_processor_factory: missing_audio_tempo_processor_factory(),
            last_nonzero_volume: None,
            diagnostics: PlaybackDiagnostics::default(),
            pending_events: Vec::new(),
            pending_scrub_events: Vec::new(),
            media_lifecycle: MediaLifecycleState::default(),
            staged_media_install: StagedMediaInstallRegistry::default(),
            staged_video_preflight_timeout: PlayerTickConfig::default()
                .staged_video_preflight_timeout,
            demux_retry: DemuxRetryRuntime::default(),
            playback_intent_control: Arc::new(PlaybackIntentControl::default()),
            shutdown_requested: false,
            eof_drain: EofDrainRuntime::default(),
            capabilities: None,
            active_video_backend_id: None,
            frame_server_config: frame_server_core::FrameServerConfig::default()
                .validate()
                .expect("default frame-server config must validate"),
            seek_runtime: SeekRuntimeState::default(),
            prepared_demux_seek: PreparedDemuxSeekRuntime::default(),
            pending_exact_timeline_seek: None,
            pending_installed_position_restore: None,
            installed_staged_position: None,
            prepared_seek_landing: PreparedSeekLandingRuntime,
            pending_video_backend_reselection: None,
            last_audio_starvation_warn_at: None,
            last_seen_audio_underrun_callbacks: 0,
            last_tick_observed_at: None,
        }
    }
}

#[cfg(test)]
mod tests;
