use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use capability_core::SystemCapabilities;
#[cfg(test)]
use codec_core::VideoCodec;
use codec_core::VideoDecodeRequirement;
use frame_server_core::{
    CancelScrubReason, LiveScrubDiagnostics, ScrubEvent, ValidatedFrameServerConfig,
};
use media_core::{MediaDuration, MediaTime};
use tracing::{debug, info, warn};
#[cfg(test)]
use video_core::VideoDecoderThreadHandle;

use crate::audio_boundary::{
    missing_audio_decoder_factory, missing_audio_output_factory,
    missing_audio_tempo_processor_factory,
};
#[cfg(test)]
use crate::decoder_boundary::PresentFrameResourceProviderHandle;
use crate::seek_state::{PlaybackResumeIntent, SeekRuntimeState};
use crate::{
    AudioDecoderFactory, AudioOutputFactory, AudioTempoProcessorFactory, FrameCounters,
    PlaybackDiagnostics, PlaybackPipeline, PlaybackState, PlayerCommand, PlayerCommandOutcome,
    PlayerError, PlayerErrorKind, PlayerEvent, PlayerResult, PlayerSnapshot, QualitySelection,
    SeekRequest, StartedVideoBackend, TrackId,
};

mod audio_runtime;
mod audio_tempo_rate_change;
mod audio_tempo_runtime;
mod capability_selection;
mod diagnostics_sink;
mod eof_drain;
mod media_lifecycle;
mod playback_rate;
mod prepared_seek;
mod render_leases;
mod scrub_driver;
mod seek_transaction;
mod snapshot_builder;
mod tick;

#[cfg(test)]
use self::audio_runtime::{
    AudioAutoplayReadiness, AudioDecoderInitSpec, audio_decoder_init_spec_from_tracks,
    classify_autoplay_audio_readiness, classify_seek_audio_gate,
};
use self::eof_drain::EofDrainRuntime;
use self::media_lifecycle::MediaLifecycleState;
use self::prepared_seek::PreparedSeekLandingRuntime;
pub(crate) use self::render_leases::{LeasedPresentFrame, PresentFrameIdentity};
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
    pending_events: Vec<PlayerEvent>,

    /// Scrub/SeekLanding события для отдельного S16 worker boundary.
    pending_scrub_events: Vec<ScrubEvent>,

    /// State, принадлежащий media lifecycle boundary.
    media_lifecycle: MediaLifecycleState,

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
            started_at: Instant::now(),
            resume_intent: PlaybackResumeIntent::Pause,
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
            PlayerCommand::EndScrub {
                policy: _,
                live_scrub,
            } => self.end_scrub(live_scrub),
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

    /// Переключает playback между пользовательскими смыслами "сейчас слышно/идёт" и "пауза".
    pub fn toggle_playback(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.cancel_active_scrub_for_external_command(CancelScrubReason::UserCancelled);
        if self.playback_state().is_playback_active() {
            // EOF drain тоже active: audio tail ещё может звучать, поэтому toggle обязан ставить pause.
            self.pause()
        } else {
            self.play()
        }
    }

    /// Обновляет позицию playback из clock/UI без накопления high-frequency событий.
    pub fn update_current_position(&mut self, position: Duration) {
        // Публичный setter остаётся явной lifecycle-операцией: caller меняет
        // позицию и тем самым создаёт новый no-audio anchor в одной точке времени.
        let observed_at = Instant::now();
        self.publish_clock_sample(position);
        self.reanchor_no_audio_clock(position, observed_at);
    }

    /// Публикует измеренную clock position без изменения clock ownership или anchor-а.
    fn publish_clock_sample(&mut self, position: Duration) {
        if self.snapshot.current_position == position {
            return;
        }

        self.snapshot
            .set_timeline_position(MediaTime::from_duration(position));
    }

    /// Явно перепривязывает только no-audio clock на lifecycle boundary.
    fn reanchor_no_audio_clock(&mut self, position: Duration, observed_at: Instant) {
        if self.snapshot.playback_state == PlaybackState::Playing
            && !self.pipeline.has_audio_clock()
        {
            self.pipeline.start_monotonic_media_clock(
                position,
                observed_at,
                self.snapshot.playback_rate,
            );
        }
    }

    /// Возвращает media clock position на monotonic момент `now`.
    ///
    /// Audio clock остаётся главным источником времени. Если audio clock отсутствует,
    /// Playing/EOF-drain без audio используют внутренний monotonic anchor, а не частоту worker tick-а.
    #[must_use]
    pub(crate) fn presentation_clock_position_at(&self, now: Instant) -> Duration {
        if self.pipeline.has_audio_clock() {
            return self.audio_media_clock_position();
        }

        if let Some(seek_target_position) = self.seek_presentation_clock_override() {
            return seek_target_position;
        }

        if self.monotonic_media_clock_drives_position()
            && let Some(position) = self.pipeline.monotonic_media_position(now)
        {
            return position;
        }

        self.snapshot.current_position
    }

    /// Проецирует ближайшую wall-задержку в media position выбранного clock source.
    #[must_use]
    pub(crate) fn presentation_media_position_after_wall_delay(
        &self,
        now: Instant,
        wall_delay: Duration,
    ) -> Duration {
        if self.pipeline.has_audio_clock() {
            return self
                .pipeline
                .media_position_after_audio_output_delay(wall_delay);
        }

        if self.seek_presentation_clock_override().is_none()
            && self.monotonic_media_clock_drives_position()
            && let Some(position) = self
                .pipeline
                .monotonic_media_position_after_wall_delay(now, wall_delay)
        {
            return position;
        }

        let current_media_position = self.presentation_clock_position_at(now);
        let media_delta = self
            .snapshot
            .playback_rate
            .scale_wall_delta_to_media_delta(wall_delay);
        current_media_position
            .checked_add(media_delta)
            .unwrap_or(Duration::MAX)
    }

    /// Переводит absolute media deadline в wall delay без clock-эвристик scheduler-а.
    #[must_use]
    pub(crate) fn wall_delay_until_media_deadline(
        &self,
        now: Instant,
        media_deadline: Duration,
    ) -> Duration {
        if self.pipeline.has_audio_clock() {
            return self
                .pipeline
                .audio_output_delay_until_media_deadline(media_deadline);
        }

        if self.seek_presentation_clock_override().is_none()
            && self.monotonic_media_clock_drives_position()
            && let Some(wall_delay) = self
                .pipeline
                .monotonic_wall_delay_until_media_deadline(now, media_deadline)
        {
            return wall_delay;
        }

        let current_media_position = self.presentation_clock_position_at(now);
        let media_delta = media_deadline.saturating_sub(current_media_position);
        self.snapshot
            .playback_rate
            .scale_media_delta_to_wall_delay(media_delta)
    }

    /// Проверяет, может ли no-audio monotonic clock сейчас двигать user-visible position.
    fn monotonic_media_clock_drives_position(&self) -> bool {
        self.snapshot.playback_state == PlaybackState::Playing || self.eof_drain_needs_progress()
    }

    /// Синхронизирует snapshot position с monotonic fallback clock без изменения playback state.
    fn sync_monotonic_media_clock_position(&mut self, now: Instant) {
        if self.pipeline.has_audio_clock() {
            return;
        }

        let position = self.presentation_clock_position_at(now);
        self.publish_clock_sample(position);
    }

    /// Запускает или перезапускает no-audio media clock от текущей snapshot position.
    fn anchor_monotonic_media_clock_if_needed(&mut self, now: Instant) {
        if self.pipeline.has_audio_clock() {
            self.pipeline.clear_monotonic_media_clock();
            return;
        }

        self.pipeline.start_monotonic_media_clock(
            self.snapshot.current_position,
            now,
            self.snapshot.playback_rate,
        );
    }

    /// Останавливает no-audio media clock, предварительно сохранив актуальную позицию.
    fn clear_monotonic_media_clock_anchor(&mut self, now: Instant) {
        self.sync_monotonic_media_clock_position(now);
        self.pipeline.clear_monotonic_media_clock();
    }

    /// Публикует уже frozen presentation position перед обычной Pause.
    fn freeze_current_position_for_pause(&mut self, current_position: Duration) {
        self.publish_clock_sample(current_position);
        self.pipeline.clear_monotonic_media_clock();
    }

    /// Возвращает абсолютную media position по audio clock.
    fn audio_media_clock_position(&self) -> Duration {
        self.pipeline.media_position_from_audio_clock()
    }

    /// Добавляет delta к текущей позиции без panic при переполнении.
    pub fn advance_position(&mut self, delta: Duration) {
        let next_position = self
            .snapshot
            .current_position
            .checked_add(delta)
            .unwrap_or(Duration::MAX);
        self.update_current_position(next_position);
    }

    /// Отмечает fatal error от media pipeline.
    pub fn mark_fatal_error(&mut self, error: PlayerError) {
        self.snapshot.last_error = Some(error.clone());
        self.set_playback_state(PlaybackState::Failed);
        self.pending_events.push(PlayerEvent::FatalError(error));
    }

    /// Забирает накопленные события и очищает внутреннюю очередь.
    #[must_use]
    pub fn take_events(&mut self) -> Vec<PlayerEvent> {
        std::mem::take(&mut self.pending_events)
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

    /// Устанавливает video backend, уже запущенный shell composition root-ом.
    pub fn set_video_backend(&mut self, started_backend: StartedVideoBackend) {
        let backend_id = started_backend.backend_id().to_owned();
        if let Err(error) = self.clear_active_seek_decoder_output_floor("video backend replacement")
        {
            self.mark_fatal_error(error);
            return;
        }

        // Перед подменой backend-а освобождаем video-кадры старого backend-а через ЕГО
        // resource provider и продвигаем render generation. Иначе удержанный present-кадр
        // (например P010 DMA-BUF от VA-API) пережил бы свап и был бы отдан новому
        // materializer-у другого класса (host-upload) → `Missing render resources`. Бамп
        // render generation помечает кадры/lease нового backend-а свежим поколением, а
        // старые отсекаются штатной generation-проверкой на renderer boundary. Делается
        // только при уже активном decoder-е, чтобы первый install при старте не плодил
        // лишний generation bump.
        if self.pipeline.has_active_video_decoder() {
            self.clear_video_frames();
            self.advance_render_generation();
        }

        self.active_video_backend_id = Some(backend_id);
        self.pipeline
            .set_video_decoder_thread_handle(started_backend.into_decoder_thread());
        info!(
            backend = self.pipeline.video_backend_name(),
            "Video backend started"
        );

        // Если видео ждало совместимого backend-а — активируем отложенный трек на новом
        // backend-е; иначе переконфигурируем уже выбранный active stream (горячая смена).
        if self.has_pending_video_backend_reselection() {
            self.retry_pending_video_backend_reselection();
        } else if let Err(error) = self.configure_active_video_decoder_stream() {
            warn!(
                error = %error,
                "Video backend failed to configure active stream after startup"
            );
            self.mark_fatal_error(error);
        }
    }

    /// Отклоняет отложенный выбор video backend-а, когда shell не нашёл совместимый план.
    pub fn reject_pending_video_backend_with_reason(&mut self, reason: String) {
        self.reject_pending_video_backend(reason);
    }

    /// Переводит playback в `Playing` и запускает audio output.
    fn play(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.cancel_active_scrub_for_external_command(CancelScrubReason::UserCancelled);
        if let Some(seek_commit) = self.seek_runtime.active_commit_mut() {
            seek_commit.resume_intent = PlaybackResumeIntent::Play;
            self.set_playback_state(PlaybackState::Seeking);
            return Ok(());
        }

        if self.should_replay_from_eof_on_play() {
            return self.restart_playback_after_eof();
        }

        self.set_playback_state(PlaybackState::Playing);

        if let Some(play_result) = self.pipeline.play_audio_output() {
            if let Err(error) = play_result {
                warn!(error = %error, "Не удалось запустить audio");
                self.set_runtime_error(format!("Audio play error: {error}"));
            }
            let observed_at = Instant::now();
            let audio_now = self.audio_clock_now();
            self.pipeline
                .reset_audio_clock_sample(audio_now, observed_at);
        }
        self.anchor_monotonic_media_clock_if_needed(Instant::now());

        Ok(())
    }

    /// Запускает preroll перед autoplay, не включая audio stream раньше заполнения buffer.
    fn begin_autoplay_preroll(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.media_lifecycle.clear_pending_autoplay();
        self.set_playback_state(PlaybackState::Buffering);
        let observed_at = Instant::now();
        let audio_now = self.audio_clock_now();
        self.pipeline
            .reset_audio_clock_sample(audio_now, observed_at);
        Ok(())
    }

    /// Завершает autoplay preroll, когда audio/video уже готовы к слышимому старту.
    pub(crate) fn finish_autoplay_preroll_if_ready(
        &mut self,
        audio_preroll_target_ms: f64,
    ) -> PlayerResult<bool> {
        if self.snapshot.playback_state != PlaybackState::Buffering {
            return Ok(false);
        }

        if !self.autoplay_preroll_ready(audio_preroll_target_ms) {
            return Ok(false);
        }

        self.play()?;
        Ok(true)
    }

    /// Проверяет минимальный readiness для перехода из `Buffering` в `Playing`.
    fn autoplay_preroll_ready(&self, audio_preroll_target_ms: f64) -> bool {
        let audio_ready = self
            .autoplay_audio_readiness(audio_preroll_target_ms)
            .is_ready();
        let video_ready = self.autoplay_video_preroll_ready();

        audio_ready && video_ready
    }

    /// Проверяет video gate autoplay-preroll без раскрытия очередей pipeline.
    fn autoplay_video_preroll_ready(&self) -> bool {
        if !self.pipeline.has_selected_video_track() {
            return true;
        }

        self.pipeline.has_present_video_frame() || !self.pipeline.video_present_queue_is_empty()
    }

    /// Переводит playback в `Paused` и останавливает audio output.
    fn pause(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.cancel_active_scrub_for_external_command(CancelScrubReason::UserCancelled);
        if let Some(seek_commit) = self.seek_runtime.active_commit_mut() {
            seek_commit.resume_intent = PlaybackResumeIntent::Pause;
            self.pause_audio_output_for_seek();
            self.set_playback_state(PlaybackState::Seeking);
            return Ok(());
        }

        // Output owner возвращает timing, зафиксированный под callback mutex.
        // Для video-only тот же lifecycle фиксируется по monotonic anchor-у.
        let audio_pause_result = self.pipeline.pause_audio_output_and_capture_clock();
        let paused_media_position = match audio_pause_result {
            Some(Ok(captured_clock)) => captured_clock.media_position(),
            Some(Err(error)) => {
                warn!(error = %error, "Не удалось остановить audio");
                return Err(PlayerError::new(
                    PlayerErrorKind::RuntimeError,
                    format!("Audio pause error: {error}"),
                ));
            }
            None => self.presentation_clock_position_at(Instant::now()),
        };
        self.freeze_current_position_for_pause(paused_media_position);
        self.set_playback_state(PlaybackState::Paused);
        self.clear_queued_video_frames();

        Ok(())
    }

    /// Останавливает текущий media и сбрасывает timeline без завершения session.
    fn stop(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.reset_media_state();
        self.set_playback_state(PlaybackState::Stopped);
        Ok(())
    }

    /// Валидирует и устанавливает громкость.
    fn set_volume(&mut self, volume: f32) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.validate_volume_command_value("volume", volume)?;
        self.remember_last_nonzero_volume(volume);
        self.apply_audio_volume(volume);
        Ok(())
    }

    /// Переключает mute и восстанавливает предыдущую слышимую громкость.
    fn toggle_mute(&mut self, fallback_volume: f32) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.validate_volume_command_value("fallback_volume", fallback_volume)?;

        if self.is_current_volume_audible() {
            self.remember_last_nonzero_volume(self.snapshot.volume);
            self.apply_audio_volume(0.0);
            return Ok(());
        }

        let restored_volume = self
            .last_nonzero_volume
            .filter(|volume| Self::is_rememberable_volume(*volume))
            .or_else(|| Self::is_rememberable_volume(fallback_volume).then_some(fallback_volume))
            .unwrap_or(1.0);

        self.remember_last_nonzero_volume(restored_volume);
        self.apply_audio_volume(restored_volume);
        Ok(())
    }

    /// Проверяет значение volume-like команды и пишет recoverable error без изменения state.
    fn validate_volume_command_value(&mut self, name: &str, volume: f32) -> PlayerResult<()> {
        if volume.is_finite() && (0.0..=1.0).contains(&volume) {
            return Ok(());
        }

        let error = PlayerError::new(
            PlayerErrorKind::InvalidCommand,
            format!("{name} must be finite and within 0.0..=1.0, got {volume}"),
        );
        self.record_recoverable_error(error.clone());
        Err(error)
    }

    /// Запоминает только громкость, которая не схлопнется в muted-состояние.
    fn remember_last_nonzero_volume(&mut self, volume: f32) {
        if Self::is_rememberable_volume(volume) {
            self.last_nonzero_volume = Some(volume);
        }
    }

    /// Применяет громкость к snapshot и активному output-у, не меняя remembered-volume.
    fn apply_audio_volume(&mut self, volume: f32) {
        self.snapshot.volume = volume;
        self.snapshot.muted = !Self::is_rememberable_volume(volume);
        let _output_was_present = self.pipeline.set_audio_output_volume(volume);
    }

    /// Текущий snapshot считается слышимым только если mute-флаг и число согласованы.
    fn is_current_volume_audible(&self) -> bool {
        !self.snapshot.muted && Self::is_rememberable_volume(self.snapshot.volume)
    }

    /// Практический non-zero: совпадает с существующей EPSILON mute-семантикой snapshot-а.
    fn is_rememberable_volume(volume: f32) -> bool {
        volume > f32::EPSILON
    }

    /// Выбирает video track.
    fn select_video_track(&mut self, track_id: TrackId) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.select_requested_video_track(track_id)?;
        self.pending_events
            .push(PlayerEvent::VideoTrackSelected(track_id));
        Ok(())
    }

    /// Выбирает audio track.
    fn select_audio_track(&mut self, track_id: TrackId) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pipeline.select_audio_track(track_id);
        self.snapshot.selected_tracks.audio_track = Some(track_id);
        self.pending_events
            .push(PlayerEvent::AudioTrackSelected(track_id));
        Ok(())
    }

    /// Выбирает subtitle track или отключает субтитры.
    fn select_subtitle_track(&mut self, track_id: Option<TrackId>) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.snapshot.selected_tracks.subtitle_track = track_id;
        self.pending_events
            .push(PlayerEvent::SubtitleTrackSelected(track_id));
        Ok(())
    }

    /// Фиксирует выбор качества как событие для будущего source/service слоя.
    fn select_quality(&mut self, selection: QualitySelection) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pending_events
            .push(PlayerEvent::QualitySelectionChanged(selection));
        Ok(())
    }

    /// Запрашивает reload config без чтения файлов внутри player-core.
    fn reload_config(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pending_events.push(PlayerEvent::ConfigReloadRequested);
        Ok(())
    }

    /// Обновляет session-owned frame-server policy без сброса текущего SeekLanding.
    pub(crate) fn apply_frame_server_policy_config(
        &mut self,
        frame_server_config: ValidatedFrameServerConfig,
    ) {
        self.frame_server_config = frame_server_config;
    }

    /// Возвращает read-only session-owned frame-server policy snapshot.
    #[cfg(test)]
    pub(crate) const fn frame_server_policy_config(&self) -> ValidatedFrameServerConfig {
        self.frame_server_config
    }

    /// Переводит session в stopped state.
    fn shutdown(&mut self) -> PlayerResult<()> {
        self.shutdown_requested = true;
        self.pending_events.push(PlayerEvent::ShutdownRequested);
        self.set_playback_state(PlaybackState::Stopped);
        Ok(())
    }

    /// Запрещает команды после shutdown, кроме самого idempotent shutdown.
    fn ensure_not_shutdown(&mut self) -> PlayerResult<()> {
        if !self.shutdown_requested {
            return Ok(());
        }

        let error = PlayerError::new(
            PlayerErrorKind::InvalidCommand,
            "player session is already shut down",
        );
        self.record_recoverable_error(error.clone());
        Err(error)
    }

    /// Обновляет playback state и публикует событие только при реальном изменении.
    fn set_playback_state(&mut self, playback_state: PlaybackState) {
        let previous_state = self.playback_state();
        self.eof_drain.sync_from_playback_state(playback_state);
        self.snapshot.playback_state = playback_state;

        if previous_state == playback_state {
            return;
        }

        debug!(
            previous_state = ?previous_state,
            playback_state = ?playback_state,
            draining_after_eof = self.is_eof_draining(),
            current_position_ms = self.snapshot.current_position.as_secs_f64() * 1000.0,
            duration_ms = ?self
                .snapshot
                .duration
                .map(|duration| duration.as_secs_f64() * 1000.0),
            "Playback state changed"
        );

        self.pending_events
            .push(PlayerEvent::PlaybackStateChanged(playback_state));
    }

    /// Сохраняет recoverable error в snapshot и event queue.
    fn record_recoverable_error(&mut self, error: PlayerError) {
        self.snapshot.last_error = Some(error.clone());
        self.pending_events
            .push(PlayerEvent::RecoverableError(error));
    }

    /// Публикует редкое явное изменение позиции, например seek.
    fn publish_position_changed(&mut self, position: Duration) {
        self.update_current_position(position);
        self.pending_events
            .push(PlayerEvent::PositionChanged(position));
    }

    /// Разрешает seek target в абсолютную media-позицию без изменения runtime seek policy.
    fn resolve_seek_target(&self, request: SeekRequest) -> MediaTime {
        let target_position = request
            .target
            .resolve(self.snapshot.timeline.current_position);

        self.snapshot
            .timeline
            .seekable_range
            .map(|range| target_position.clamp_to(range))
            .unwrap_or(target_position)
    }

    /// Синхронно обновляет legacy `Duration` и typed timeline duration.
    fn set_snapshot_duration(&mut self, duration: Option<Duration>) {
        self.snapshot
            .set_timeline_duration(duration.map(MediaDuration::from_duration));
    }

    /// Сохраняет runtime error как user-facing ошибку.
    fn set_runtime_error(&mut self, message: String) {
        let error = PlayerError::new(PlayerErrorKind::RuntimeError, message);
        self.snapshot.last_error = Some(error.clone());
        self.pending_events
            .push(PlayerEvent::RecoverableError(error));
    }

    /// Очищает последнюю ошибку после успешного media action.
    fn clear_error(&mut self) {
        self.snapshot.last_error = None;
    }
}

impl Default for PlayerSession {
    /// Создаёт пустую session с default snapshot.
    fn default() -> Self {
        Self {
            snapshot: PlayerSnapshot::default(),
            pipeline: PlaybackPipeline::default(),
            audio_decoder_factory: missing_audio_decoder_factory(),
            audio_output_factory: missing_audio_output_factory(),
            audio_tempo_processor_factory: missing_audio_tempo_processor_factory(),
            last_nonzero_volume: None,
            diagnostics: PlaybackDiagnostics::default(),
            pending_events: Vec::new(),
            pending_scrub_events: Vec::new(),
            media_lifecycle: MediaLifecycleState::default(),
            shutdown_requested: false,
            eof_drain: EofDrainRuntime::default(),
            capabilities: None,
            active_video_backend_id: None,
            frame_server_config: frame_server_core::FrameServerConfig::default()
                .validate()
                .expect("default frame-server config must validate"),
            seek_runtime: SeekRuntimeState::default(),
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
