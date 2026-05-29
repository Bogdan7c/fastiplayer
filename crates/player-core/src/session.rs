use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use capability_core::SystemCapabilities;
use media_core::{MediaDuration, MediaTime};
use tracing::{debug, info, warn};

use crate::audio_boundary::{missing_audio_decoder_factory, missing_audio_output_factory};
use crate::seek_state::{PlaybackResumeIntent, SeekRuntimeState};
use crate::{
    AudioDecoderFactory, AudioOutputFactory, FrameCounters, PlaybackDiagnostics, PlaybackPipeline,
    PlaybackState, PlayerCommand, PlayerError, PlayerErrorKind, PlayerEvent, PlayerResult,
    PlayerSnapshot, QualitySelection, SeekRequest, StartedVideoBackend, TrackId,
};

mod audio_runtime;
mod capability_selection;
mod diagnostics_sink;
mod eof_drain;
mod media_lifecycle;
mod render_leases;
mod seek_transaction;
mod snapshot_builder;
mod tick;

use self::eof_drain::EofDrainRuntime;
use self::media_lifecycle::MediaLifecycleState;
pub(crate) use self::render_leases::{LeasedPresentFrame, PresentFrameIdentity};
pub use self::tick::{
    PlayerPipelinePause, PlayerTickConfig, PlayerTickContext, PlayerTickPacket, PlayerTickResult,
    PlayerVideoDropReason, PlayerVideoFrameDrop,
};
pub(crate) use self::tick::{
    PlayerWorkerWakeupPlan, SchedulerTimingDiagnosticsSnapshot, scheduler_timing_diagnostics,
};

#[cfg(test)]
use self::audio_runtime::{
    AudioAutoplayReadiness, AudioDecoderInitSpec, audio_decoder_init_spec_from_tracks,
    classify_autoplay_audio_readiness, classify_seek_audio_gate,
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

    /// Codec/render-neutral diagnostics aggregator для текущего media pipeline.
    diagnostics: PlaybackDiagnostics,

    /// События, накопленные после последнего drain.
    pending_events: Vec<PlayerEvent>,

    /// State, принадлежащий media lifecycle boundary.
    media_lifecycle: MediaLifecycleState,

    /// Был ли принят shutdown-запрос.
    shutdown_requested: bool,

    /// Runtime EOF-drain boundary: наружу выходит только через intent methods.
    eof_drain: EofDrainRuntime,

    /// Последний системный capability report, полученный от shell/backend layer.
    capabilities: Option<SystemCapabilities>,

    /// Runtime state seek transaction/scrub/trace markers, которым владеет session.
    seek_runtime: SeekRuntimeState,
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
    pub const fn is_demuxing_active(&self) -> bool {
        matches!(
            self.snapshot.playback_state,
            PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Seeking
        )
    }

    /// Возвращает `true`, если scheduler может менять present frame.
    #[must_use]
    pub const fn can_present_video(&self) -> bool {
        matches!(
            self.snapshot.playback_state,
            PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Seeking
        ) || self.is_eof_draining()
    }

    /// Возвращает `true`, если текущая session владеет открытым demuxer-ом.
    #[must_use]
    pub fn has_loaded_media_pipeline(&self) -> bool {
        self.pipeline.has_demuxer()
    }

    /// Возвращает путь текущего локального файла, если media было открыто с диска.
    #[must_use]
    pub fn current_file_path(&self) -> Option<&Path> {
        self.pipeline.source_file_path()
    }

    /// Применяет команду к state machine.
    pub fn dispatch_command(&mut self, command: PlayerCommand) -> PlayerResult<()> {
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

        match command {
            PlayerCommand::OpenMedia(request) => self.open_media(request),
            PlayerCommand::Play => self.play(),
            PlayerCommand::Pause => self.pause(),
            PlayerCommand::TogglePlayback => self.toggle_playback(),
            PlayerCommand::Seek(request) => self.seek(request),
            PlayerCommand::BeginScrub => self.begin_scrub(),
            PlayerCommand::UpdateScrub(request) => self.update_scrub(request),
            PlayerCommand::PreviewScrub(request) => self.preview_scrub(request),
            PlayerCommand::EndScrub { policy: _ } => self.end_scrub(),
            PlayerCommand::Stop => self.stop(),
            PlayerCommand::SetVolume(volume) => self.set_volume(volume),
            PlayerCommand::SelectVideoTrack(track_id) => self.select_video_track(track_id),
            PlayerCommand::SelectAudioTrack(track_id) => self.select_audio_track(track_id),
            PlayerCommand::SelectSubtitleTrack(track_id) => self.select_subtitle_track(track_id),
            PlayerCommand::SelectQuality(selection) => self.select_quality(selection),
            PlayerCommand::ReloadConfig => self.reload_config(),
            PlayerCommand::Shutdown => self.shutdown(),
        }
    }

    /// Переключает playback между пользовательскими смыслами "сейчас слышно/идёт" и "пауза".
    pub fn toggle_playback(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        if self.playback_state().is_playback_active() {
            // EOF drain тоже active: audio tail ещё может звучать, поэтому toggle обязан ставить pause.
            self.pause()
        } else {
            self.play()
        }
    }

    /// Обновляет позицию playback из clock/UI без накопления high-frequency событий.
    pub fn update_current_position(&mut self, position: Duration) {
        if self.snapshot.current_position == position {
            return;
        }

        self.snapshot
            .set_timeline_position(MediaTime::from_duration(position));

        if self.snapshot.playback_state == PlaybackState::Playing
            && !self.pipeline.has_audio_clock()
        {
            self.pipeline
                .start_monotonic_media_clock(position, Instant::now());
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
        self.update_current_position(position);
    }

    /// Запускает или перезапускает no-audio media clock от текущей snapshot position.
    fn anchor_monotonic_media_clock_if_needed(&mut self, now: Instant) {
        if self.pipeline.has_audio_clock() {
            self.pipeline.clear_monotonic_media_clock();
            return;
        }

        self.pipeline
            .start_monotonic_media_clock(self.snapshot.current_position, now);
    }

    /// Останавливает no-audio media clock, предварительно сохранив актуальную позицию.
    fn clear_monotonic_media_clock_anchor(&mut self, now: Instant) {
        self.sync_monotonic_media_clock_position(now);
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
        let queued_texture_handles = self.pipeline.clear_video_queues();
        let present_texture_handle = self
            .pipeline
            .take_present_video_frame()
            .map(|frame| frame.texture_handle);

        for texture_handle in queued_texture_handles {
            self.release_video_texture(texture_handle);
        }
        if let Some(texture_handle) = present_texture_handle {
            self.release_video_texture(texture_handle);
        }
    }

    /// Очищает только очередь будущих video frames, сохраняя текущий кадр на экране.
    pub fn clear_queued_video_frames(&mut self) {
        self.clear_seek_preroll_fallback_frame();
        let queued_texture_handles = self.pipeline.clear_video_queues();

        for texture_handle in queued_texture_handles {
            self.release_video_texture(texture_handle);
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
        let texture_handle = stale_frame.texture_handle;
        self.release_video_texture(texture_handle);
        debug!(
            pts_ms = frame_pts.as_millis(),
            handle = texture_handle.0,
            available_texture_slots = texture_slots.available_slots(),
            min_available_texture_slots,
            "Final seek released stale present frame under texture pressure"
        );
        true
    }

    /// Устанавливает video backend, уже запущенный shell composition root-ом.
    pub fn set_video_backend(&mut self, started_backend: StartedVideoBackend) {
        self.pipeline
            .set_video_decoder_thread_handle(started_backend.into_decoder_thread());
        info!(
            backend = self.pipeline.video_backend_name(),
            "Video backend started"
        );
    }

    /// Переводит playback в `Playing` и запускает audio output.
    fn play(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
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
        let video_ready =
            !self.pipeline.has_selected_video_track() || self.pipeline.has_present_video_frame();

        audio_ready && video_ready
    }

    /// Переводит playback в `Paused` и останавливает audio output.
    fn pause(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.clear_monotonic_media_clock_anchor(Instant::now());
        if let Some(seek_commit) = self.seek_runtime.active_commit_mut() {
            seek_commit.resume_intent = PlaybackResumeIntent::Pause;
            self.pause_audio_output_for_seek();
            self.set_playback_state(PlaybackState::Seeking);
            return Ok(());
        }

        self.set_playback_state(PlaybackState::Paused);
        self.clear_queued_video_frames();

        if let Some(Err(error)) = self.pipeline.pause_audio_output() {
            warn!(error = %error, "Не удалось остановить audio");
            self.set_runtime_error(format!("Audio pause error: {error}"));
        }

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
        if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
            let error = PlayerError::new(
                PlayerErrorKind::InvalidCommand,
                format!("volume must be finite and within 0.0..=1.0, got {volume}"),
            );
            self.record_recoverable_error(error.clone());
            return Err(error);
        }

        self.snapshot.volume = volume;
        self.snapshot.muted = volume <= f32::EPSILON;
        let _output_was_present = self.pipeline.set_audio_output_volume(volume);
        Ok(())
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
            diagnostics: PlaybackDiagnostics::default(),
            pending_events: Vec::new(),
            media_lifecycle: MediaLifecycleState::default(),
            shutdown_requested: false,
            eof_drain: EofDrainRuntime::default(),
            capabilities: None,
            seek_runtime: SeekRuntimeState::default(),
        }
    }
}

#[cfg(test)]
mod tests;
