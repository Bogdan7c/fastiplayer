use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use capability_core::{SystemCapabilities, UnsupportedVideoRequirement, VideoCapabilityRejection};
use codec_core::{
    ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoCodec,
    VideoDecodeRequirement, Vp9MetadataSource, resolve_vp9_metadata,
};
use media_core::{MediaDuration, MediaTime, TrackInfo, TrackKind};
use tracing::{info, warn};
use webm_demux::Demuxer;

use crate::pipeline::{
    DEFAULT_VIDEO_FRAME_DURATION, MAX_OBSERVED_VIDEO_FRAME_DURATION,
    MIN_OBSERVED_VIDEO_FRAME_DURATION,
};
use crate::{
    AudioBufferSnapshot, BackendSnapshot, FrameCounters, MediaOpenRequest, MediaSource,
    MediaSummary, PlaybackPipeline, PlaybackState, PlayerCommand, PlayerError, PlayerErrorKind,
    PlayerEvent, PlayerResult, PlayerSnapshot, QualitySelection, QueueSnapshot, SeekRequest,
    TexturePoolSnapshot, TrackId, TrackSelectionSnapshot, TrackSummarySnapshot, VideoFrameSnapshot,
};

/// Dev-only режим, который разрешает VP9 Profile 2 HDR дойти до P010 zero-copy boundary.
const P010_BOUNDARY_DIAGNOSTIC_ENV_VAR: &str = "RUSTIPLAYER_DEV_VERIFY_P010_BOUNDARY";

/// Центральная session плеера: high-level state machine и владение playback pipeline.
pub struct PlayerSession {
    /// Последний базовый read-only snapshot без runtime diagnostics, зависящих от shell.
    snapshot: PlayerSnapshot,

    /// Media pipeline, перенесённый из `AppState` в Phase 3.
    pub pipeline: PlaybackPipeline,

    /// События, накопленные после последнего drain.
    pending_events: Vec<PlayerEvent>,

    /// Autoplay-флаг последнего open request до фактического media-open.
    pending_autoplay: bool,

    /// Был ли принят shutdown-запрос.
    shutdown_requested: bool,

    /// Флаг дорендера хвоста после EOF.
    pub draining_after_eof: bool,

    /// Последний системный capability report, полученный от shell/backend layer.
    capabilities: Option<SystemCapabilities>,
}

impl PlayerSession {
    /// Создаёт пустую player session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Возвращает последний базовый immutable snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &PlayerSnapshot {
        &self.snapshot
    }

    /// Собирает актуальный snapshot для UI, renderer и desktop integration.
    #[must_use]
    pub fn snapshot_with_frame_counters(&self, frame_counters: FrameCounters) -> PlayerSnapshot {
        let mut snapshot = self.snapshot.clone();
        snapshot.playback_state = self.playback_state();
        snapshot.source_label = self.source_label();
        snapshot.media_title = self.media_title();
        snapshot.selected_tracks = self.track_selection_snapshot();
        snapshot.tracks = self.track_summary_snapshot();
        snapshot.active_backend = self.backend_snapshot();
        snapshot.current_video_frame = self.current_video_frame_snapshot();
        snapshot.render_generation = self.pipeline.render_generation;
        snapshot.video_frame_duration_estimate = self.pipeline.video_frame_duration_estimate;
        snapshot.audio_buffer = self.audio_buffer_snapshot();
        snapshot.queues = self.queue_snapshot();
        snapshot.frame_counters = frame_counters;
        snapshot
    }

    /// Сообщает, что session уже получила shutdown-запрос.
    #[must_use]
    pub const fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested
    }

    /// Устанавливает capability report и публикует событие для UI/log layer.
    pub fn set_system_capabilities(&mut self, capabilities: SystemCapabilities) {
        let summary = capabilities.detailed_report_text();
        self.snapshot.capability_summary = Some(summary.clone());
        self.pending_events
            .push(PlayerEvent::CapabilityScanCompleted(
                crate::CapabilitySummary { summary },
            ));
        self.capabilities = Some(capabilities);
    }

    /// Возвращает effective playback state с учётом EOF-drain режима.
    #[must_use]
    pub const fn playback_state(&self) -> PlaybackState {
        if self.draining_after_eof {
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
            PlaybackState::Playing | PlaybackState::Buffering
        )
    }

    /// Возвращает `true`, если scheduler может менять present frame.
    #[must_use]
    pub const fn can_present_video(&self) -> bool {
        matches!(
            self.snapshot.playback_state,
            PlaybackState::Playing | PlaybackState::Buffering
        ) || self.draining_after_eof
    }

    /// Возвращает `true`, если текущая session владеет открытым demuxer-ом.
    #[must_use]
    pub fn has_loaded_media_pipeline(&self) -> bool {
        self.pipeline.demuxer.is_some()
    }

    /// Возвращает путь текущего локального файла, если media было открыто с диска.
    #[must_use]
    pub fn current_file_path(&self) -> Option<&Path> {
        self.pipeline.file_path.as_deref()
    }

    /// Применяет команду к state machine.
    pub fn dispatch_command(&mut self, command: PlayerCommand) -> PlayerResult<()> {
        match command {
            PlayerCommand::OpenMedia(request) => self.open_media(request),
            PlayerCommand::Play => self.play(),
            PlayerCommand::Pause => self.pause(),
            PlayerCommand::TogglePlayback => self.toggle_playback(),
            PlayerCommand::Seek(request) => self.seek(request),
            PlayerCommand::BeginScrub => self.begin_scrub(),
            PlayerCommand::UpdateScrub(request) => self.update_scrub(request),
            PlayerCommand::EndScrub { policy } => self.end_scrub(policy),
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

    /// Переключает playback между `Playing` и `Paused`.
    pub fn toggle_playback(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        match self.snapshot.playback_state {
            PlaybackState::Playing => self.pause(),
            _ => self.play(),
        }
    }

    /// Обновляет позицию playback из clock/UI без накопления high-frequency событий.
    pub fn update_current_position(&mut self, position: Duration) {
        if self.snapshot.current_position == position {
            return;
        }

        self.snapshot
            .set_timeline_position(MediaTime::from_duration(position));
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

    /// Загружает локальный WebM/Matroska файл и передаёт demuxer во владение session.
    pub fn load_file(&mut self, path: &Path) {
        self.load_file_with_autoplay(path, false);
    }

    /// Загружает локальный WebM/Matroska файл с явной autoplay-политикой.
    pub fn load_file_with_autoplay(&mut self, path: &Path, autoplay: bool) {
        self.reset_media_state();

        let open_request =
            MediaOpenRequest::new(MediaSource::LocalFile(path.to_path_buf()), autoplay);
        if let Err(error) = self.dispatch_command(PlayerCommand::OpenMedia(open_request)) {
            self.record_recoverable_error(error);
            return;
        }

        match webm_demux::SymphoniaDemuxer::from_file(path) {
            Ok(demuxer) => {
                let tracks = demuxer.tracks().to_vec();
                let duration = demuxer.duration();
                info!(
                    path = %path.display(),
                    tracks = tracks.len(),
                    duration = ?duration,
                    "Файл загружен"
                );

                self.init_audio_pipeline(&tracks);
                if let Err(error) =
                    self.select_default_video_track(&tracks, "Поддерживаемый video track не найден")
                {
                    warn!(error = %error, "Video track rejected during local file load");
                    self.mark_fatal_error(error);
                    return;
                }
                self.pipeline.demuxer = Some(Box::new(demuxer));
                self.pipeline.file_path = Some(path.to_path_buf());
                self.pipeline.tracks = tracks;
                self.pipeline.source_label = None;
                self.clear_error();

                let summary = MediaSummary {
                    title: self.media_title(),
                    source_label: path.display().to_string(),
                    duration,
                };
                if let Err(error) = self.mark_media_opened(summary) {
                    self.record_recoverable_error(error);
                }
            }
            Err(error) => {
                warn!(error = %error, "Не удалось открыть файл");
                self.mark_fatal_error(PlayerError::new(
                    PlayerErrorKind::DemuxError,
                    format!("Ошибка: {error}"),
                ));
            }
        }
    }

    /// Загружает уже открытый demuxer для streaming source.
    pub fn load_demuxer(&mut self, label: String, demuxer: Box<dyn webm_demux::Demuxer + Send>) {
        self.load_demuxer_with_autoplay(label, demuxer, false);
    }

    /// Загружает уже открытый demuxer для streaming source с явной autoplay-политикой.
    pub fn load_demuxer_with_autoplay(
        &mut self,
        label: String,
        demuxer: Box<dyn webm_demux::Demuxer + Send>,
        autoplay: bool,
    ) {
        self.reset_media_state();

        let open_request =
            MediaOpenRequest::new(MediaSource::ExternalLabel(label.clone()), autoplay);
        if let Err(error) = self.dispatch_command(PlayerCommand::OpenMedia(open_request)) {
            self.record_recoverable_error(error);
            return;
        }

        let tracks = demuxer.tracks().to_vec();
        let duration = demuxer.duration();

        info!(
            source = %label,
            tracks = tracks.len(),
            duration = ?duration,
            "Streaming demuxer загружен"
        );

        self.init_audio_pipeline(&tracks);
        if let Err(error) = self.select_default_video_track(
            &tracks,
            "Поддерживаемый video track не найден в streaming demuxer",
        ) {
            warn!(error = %error, "Video track rejected during streaming media load");
            self.mark_fatal_error(error);
            return;
        }
        self.pipeline.demuxer = Some(demuxer);
        self.pipeline.file_path = None;
        self.pipeline.tracks = tracks;
        self.pipeline.source_label = Some(label.clone());
        self.clear_error();

        let summary = MediaSummary {
            title: Some(label.clone()),
            source_label: label,
            duration,
        };
        if let Err(error) = self.mark_media_opened(summary) {
            self.record_recoverable_error(error);
        }
    }

    /// Отмечает успешное открытие media внешним demux/source слоем.
    pub fn mark_media_opened(&mut self, summary: MediaSummary) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.snapshot.media_title = summary.title.clone();
        self.set_snapshot_duration(summary.duration);
        self.snapshot.source_label = Some(summary.source_label.clone());
        self.clear_error();
        self.pending_events.push(PlayerEvent::MediaOpened(summary));

        if self.pending_autoplay {
            self.begin_autoplay_preroll()?;
        } else {
            self.pause()?;
        }

        Ok(())
    }

    /// Отмечает fatal error от media pipeline.
    pub fn mark_fatal_error(&mut self, error: PlayerError) {
        self.snapshot.last_error = Some(error.clone());
        self.set_playback_state(PlaybackState::Failed);
        self.pending_events.push(PlayerEvent::FatalError(error));
    }

    /// Переводит session в EOF-drain и освобождает demuxer.
    pub fn enter_eof_drain(&mut self) {
        self.pipeline.demuxer = None;
        self.set_playback_state(PlaybackState::Draining);
    }

    /// Забирает накопленные события и очищает внутреннюю очередь.
    #[must_use]
    pub fn take_events(&mut self) -> Vec<PlayerEvent> {
        std::mem::take(&mut self.pending_events)
    }

    /// Полностью сбрасывает состояние текущего media.
    pub fn reset_media_state(&mut self) {
        self.set_playback_state(PlaybackState::Paused);
        self.clear_video_frames();
        self.advance_render_generation();

        if let Some(ref thread) = self.pipeline.video_decoder_thread
            && let Err(error) = thread.flush()
        {
            warn!(error = %error, "Не удалось сбросить video decoder thread");
        }

        self.pipeline.demuxer = None;
        self.pipeline.file_path = None;
        self.pipeline.tracks.clear();
        self.pipeline.source_label = None;
        self.pipeline.audio_decoder = None;
        self.pipeline.audio_output = None;
        self.pipeline.audio_track_id = None;
        self.pipeline.pending_audio_packets.clear();
        self.pipeline.video_track_id = None;
        self.pipeline.pending_video_packets.clear();
        self.pipeline.audio_clock = None;
        self.pipeline.video_frame_duration_estimate = DEFAULT_VIDEO_FRAME_DURATION;
        self.pipeline.last_decoded_video_pts = None;
        self.pipeline.last_audio_clock = Duration::ZERO;
        self.pipeline.last_audio_clock_change_at = std::time::Instant::now();
        self.pipeline.active_video_requirement = None;

        self.pending_autoplay = false;
        self.snapshot.source_label = None;
        self.snapshot.media_title = None;
        self.snapshot.clear_timeline();
        self.snapshot.selected_tracks = TrackSelectionSnapshot::default();
        self.snapshot.tracks.clear();
        self.clear_error();
    }

    /// Переводит renderer resource ids в новое поколение после полного reset media pipeline.
    fn advance_render_generation(&mut self) {
        self.pipeline.render_generation = self.pipeline.render_generation.wrapping_add(1);
    }

    /// Обновляет оценку длительности video frame по очередному decoded PTS.
    pub fn observe_video_frame_pts(&mut self, pts: Duration) {
        if let Some(previous_pts) = self.pipeline.last_decoded_video_pts {
            let observed_duration = pts.saturating_sub(previous_pts);
            if (MIN_OBSERVED_VIDEO_FRAME_DURATION..=MAX_OBSERVED_VIDEO_FRAME_DURATION)
                .contains(&observed_duration)
            {
                let old_micros = self.pipeline.video_frame_duration_estimate.as_micros() as u64;
                let new_micros = observed_duration.as_micros() as u64;
                let smoothed_micros = (old_micros.saturating_mul(7) + new_micros) / 8;
                self.pipeline.video_frame_duration_estimate =
                    Duration::from_micros(smoothed_micros.max(1));
            }
        }

        self.pipeline.last_decoded_video_pts = Some(pts);
    }

    /// Очищает video frame queue и present frame, освобождая texture slots.
    pub fn clear_video_frames(&mut self) {
        let queued_texture_handles = self
            .pipeline
            .video_frame_queue
            .drain(..)
            .map(|frame| frame.texture_handle)
            .collect::<Vec<_>>();
        let present_texture_handle = self
            .pipeline
            .present_video_frame
            .take()
            .map(|frame| frame.texture_handle);

        for texture_handle in queued_texture_handles {
            self.release_video_texture(texture_handle);
        }
        if let Some(texture_handle) = present_texture_handle {
            self.release_video_texture(texture_handle);
        }

        self.pipeline.video_frame_queue.clear();
    }

    /// Очищает только очередь будущих video frames, сохраняя текущий кадр на экране.
    pub fn clear_queued_video_frames(&mut self) {
        let queued_texture_handles = self
            .pipeline
            .video_frame_queue
            .drain(..)
            .map(|frame| frame.texture_handle)
            .collect::<Vec<_>>();

        for texture_handle in queued_texture_handles {
            self.release_video_texture(texture_handle);
        }

        self.pipeline.video_frame_queue.clear();
    }

    /// Регистрирует render lease для texture handle текущего поколения.
    pub(crate) fn register_render_lease(
        &mut self,
        render_generation: u64,
        texture_handle: video_core::FrameTextureHandle,
    ) -> bool {
        if render_generation != self.pipeline.render_generation {
            return false;
        }

        let lease_key = (render_generation, texture_handle.0);
        let lease_count = self
            .pipeline
            .leased_video_textures
            .entry(lease_key)
            .or_insert(0);
        *lease_count = lease_count.saturating_add(1);
        true
    }

    /// Снимает render lease и применяет отложенный texture release, если он уже был запрошен.
    #[cfg(test)]
    pub(crate) fn release_render_lease(
        &mut self,
        render_generation: u64,
        texture_handle: video_core::FrameTextureHandle,
    ) {
        self.release_render_lease_with_provider(render_generation, texture_handle, None);
    }

    /// Снимает render lease и релизит texture через provider поколения, создавшего кадр.
    pub(crate) fn release_render_lease_with_provider(
        &mut self,
        render_generation: u64,
        texture_handle: video_core::FrameTextureHandle,
        texture_provider: Option<&video_vaapi::VideoTextureViewProvider>,
    ) {
        let lease_key = (render_generation, texture_handle.0);

        let Some(lease_count) = self.pipeline.leased_video_textures.get_mut(&lease_key) else {
            return;
        };

        if *lease_count > 1 {
            *lease_count -= 1;
            return;
        }

        self.pipeline.leased_video_textures.remove(&lease_key);
        let release_was_deferred = self
            .pipeline
            .deferred_video_texture_releases
            .remove(&lease_key);
        if !release_was_deferred {
            return;
        }

        if let Some(texture_provider) = texture_provider {
            texture_provider.release_frame(texture_handle);
        } else if render_generation == self.pipeline.render_generation {
            self.release_video_texture_now(texture_handle);
        }
    }

    /// Освобождает texture handle сразу или откладывает release до завершения render lease.
    pub(crate) fn release_video_texture(&mut self, texture_handle: video_core::FrameTextureHandle) {
        let lease_key = (self.pipeline.render_generation, texture_handle.0);
        if self.pipeline.leased_video_textures.contains_key(&lease_key) {
            self.pipeline
                .deferred_video_texture_releases
                .insert(lease_key);
            return;
        }

        self.release_video_texture_now(texture_handle);
    }

    /// Непосредственно отдаёт texture slot обратно decoder thread.
    fn release_video_texture_now(&mut self, texture_handle: video_core::FrameTextureHandle) {
        if let Some(ref thread) = self.pipeline.video_decoder_thread {
            thread.release_frame(texture_handle);
        }
    }

    /// Обрабатывает audio packet: decode -> write to AudioOutput.
    pub fn process_audio_packet(&mut self, track_id: TrackId, encoded_audio_bytes: &[u8]) {
        if self.pipeline.audio_track_id != Some(track_id) {
            return;
        }

        if let Some(ref mut decoder) = self.pipeline.audio_decoder {
            match decoder.decode(encoded_audio_bytes) {
                Ok(samples) if !samples.is_empty() => {
                    if let Some(ref mut output) = self.pipeline.audio_output {
                        output.write_samples(&samples);
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(error = %error, "Ошибка декодирования audio packet");
                    self.set_runtime_error(format!("Audio decode error: {error}"));
                }
            }
        }
    }

    /// Process pending audio packets с throttle по buffer level.
    pub fn process_pending_audio_packets(&mut self) {
        self.process_pending_audio_packets_with_buffer_limit(
            crate::PlayerTickConfig::default().audio_buffer_high_water_mark_ms,
        );
    }

    /// Возвращает audio clock time для отображения в UI.
    #[must_use]
    pub fn audio_clock_secs(&self) -> Option<f64> {
        self.pipeline
            .audio_output
            .as_ref()
            .map(|output| output.clock().now_secs())
    }

    /// Возвращает уровень audio buffer в миллисекундах.
    #[must_use]
    pub fn audio_buffer_level_ms(&self) -> Option<f64> {
        self.pipeline
            .audio_output
            .as_ref()
            .map(|output| output.buffer_level_ms())
    }

    /// Возвращает текущее время audio clock.
    #[must_use]
    pub fn audio_clock_now(&self) -> Duration {
        self.pipeline
            .audio_clock
            .as_ref()
            .map(|clock| clock.now())
            .unwrap_or(Duration::ZERO)
    }

    /// Инициализирует video pipeline через VA-API decoder thread.
    pub fn init_video_pipeline(
        &mut self,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        let device = Arc::new(device.clone());
        let queue = Arc::new(queue.clone());

        match video_vaapi::VideoDecodeThread::new(device, queue, instance.clone(), adapter.clone())
        {
            Ok(thread) => {
                self.pipeline.video_backend = thread.backend_name();
                self.pipeline.video_decoder_thread = Some(thread);
                info!(
                    backend = self.pipeline.video_backend,
                    "Video decoder thread started"
                );
            }
            Err(error) => {
                warn!(error = %error, "VA-API decoder thread unavailable, no hardware decode");
            }
        }
    }

    /// Принимает open request и переводит session в `Opening`.
    fn open_media(&mut self, request: MediaOpenRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pending_autoplay = request.autoplay;
        self.snapshot.source_label = Some(request.source.label());
        self.snapshot.media_title = None;
        self.snapshot.clear_timeline();
        self.clear_error();
        self.pending_events
            .push(PlayerEvent::MediaOpenRequested(request));
        self.set_playback_state(PlaybackState::Opening);
        Ok(())
    }

    /// Переводит playback в `Playing` и запускает audio output.
    fn play(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.set_playback_state(PlaybackState::Playing);

        if let Some(ref mut output) = self.pipeline.audio_output {
            if let Err(error) = output.play() {
                warn!(error = %error, "Не удалось запустить audio");
                self.set_runtime_error(format!("Audio play error: {error}"));
            }
            self.pipeline.last_audio_clock = self.audio_clock_now();
            self.pipeline.last_audio_clock_change_at = std::time::Instant::now();
        }

        Ok(())
    }

    /// Запускает preroll перед autoplay, не включая audio stream раньше заполнения buffer.
    fn begin_autoplay_preroll(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pending_autoplay = false;
        self.set_playback_state(PlaybackState::Buffering);
        self.pipeline.last_audio_clock = self.audio_clock_now();
        self.pipeline.last_audio_clock_change_at = std::time::Instant::now();
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
            .audio_buffer_level_ms()
            .map(|level_ms| level_ms >= audio_preroll_target_ms.max(1.0))
            .unwrap_or(true);
        let video_ready =
            self.pipeline.video_track_id.is_none() || self.pipeline.present_video_frame.is_some();

        audio_ready && video_ready
    }

    /// Переводит playback в `Paused` и останавливает audio output.
    fn pause(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.set_playback_state(PlaybackState::Paused);
        self.clear_queued_video_frames();

        if let Some(ref mut output) = self.pipeline.audio_output
            && let Err(error) = output.pause()
        {
            warn!(error = %error, "Не удалось остановить audio");
            self.set_runtime_error(format!("Audio pause error: {error}"));
        }

        Ok(())
    }

    /// Фиксирует seek request в snapshot до переноса реального scheduler.
    fn seek(&mut self, request: SeekRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        let target_position = self.resolve_seek_target(request);
        self.snapshot.timeline.target_position = Some(target_position);
        self.publish_position_changed(target_position.as_duration());
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.stale_frame = false;
        self.pending_events
            .push(PlayerEvent::SeekRequested(request));
        Ok(())
    }

    /// Начинает interactive scrub на уровне контракта без запуска demux seek.
    fn begin_scrub(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.stale_frame = true;
        self.snapshot.timeline.target_position = Some(self.snapshot.timeline.current_position);
        Ok(())
    }

    /// Запоминает последнюю цель scrub без изменения текущей playback позиции.
    fn update_scrub(&mut self, request: SeekRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        let target_position = self.resolve_seek_target(request);
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.stale_frame = true;
        self.snapshot.timeline.target_position = Some(target_position);
        self.pending_events
            .push(PlayerEvent::SeekRequested(request));
        Ok(())
    }

    /// Завершает scrub и применяет последнюю цель согласно выбранной политике.
    fn end_scrub(&mut self, policy: crate::ScrubCommitPolicy) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        match policy {
            crate::ScrubCommitPolicy::CommitLatest => {
                if let Some(target_position) = self.snapshot.timeline.target_position {
                    self.publish_position_changed(target_position.as_duration());
                }
            }
        }
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.stale_frame = false;
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
        if let Some(ref mut output) = self.pipeline.audio_output {
            output.set_volume(volume);
        }
        Ok(())
    }

    /// Выбирает video track.
    fn select_video_track(&mut self, track_id: TrackId) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pipeline.video_track_id = Some(track_id);
        self.snapshot.selected_tracks.video_track = Some(track_id);
        self.pending_events
            .push(PlayerEvent::VideoTrackSelected(track_id));
        Ok(())
    }

    /// Выбирает audio track.
    fn select_audio_track(&mut self, track_id: TrackId) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pipeline.audio_track_id = Some(track_id);
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
        self.draining_after_eof = playback_state == PlaybackState::Draining;
        self.snapshot.playback_state = playback_state;

        if previous_state == playback_state {
            return;
        }

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
        request
            .target
            .resolve(self.snapshot.timeline.current_position)
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

    /// Ищет первый video track, который проходит capability-based selection.
    fn select_default_video_track(
        &mut self,
        tracks: &[TrackInfo],
        missing_message: &str,
    ) -> PlayerResult<()> {
        let video_tracks = tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .collect::<Vec<_>>();

        if video_tracks.is_empty() {
            info!("{missing_message}");
            return Ok(());
        }

        let mut last_rejection = None;
        for track in video_tracks {
            let Some(requirement) = video_requirement_from_track(track) else {
                last_rejection = Some(PlayerError::new(
                    PlayerErrorKind::UnsupportedVideoCodec,
                    format!(
                        "Video codec `{}` не поддерживается текущей capability model",
                        track.codec_id
                    ),
                ));
                continue;
            };

            match self.validate_video_decode_requirement(&requirement) {
                Ok(()) => {
                    self.activate_video_track(track, requirement);
                    return Ok(());
                }
                Err(error) => {
                    if self.can_defer_vp9_packet_refinement(&requirement) {
                        info!(
                            track_id = %track.id,
                            requirement = %requirement.describe(),
                            "VP9 video track выбран до bitstream refinement; strict capability check будет повторён перед decode"
                        );
                        self.activate_video_track(track, requirement);
                        return Ok(());
                    }

                    last_rejection = Some(error);
                }
            }
        }

        Err(last_rejection.unwrap_or_else(|| {
            PlayerError::new(PlayerErrorKind::UnsupportedVideoCodec, missing_message)
        }))
    }

    /// Активирует video track после обычной проверки или разрешённого deferred refinement.
    fn activate_video_track(&mut self, track: &TrackInfo, requirement: VideoDecodeRequirement) {
        self.pipeline.video_track_id = Some(track.id);
        self.pipeline.active_video_requirement = Some(requirement);
        self.snapshot.selected_tracks.video_track = Some(track.id);
        log_selected_video_track_metadata(track, self.pipeline.active_video_requirement.as_ref());
    }

    /// Разрешает отложить VP9 validation до первого packet header-а, если container неполный.
    fn can_defer_vp9_packet_refinement(&self, requirement: &VideoDecodeRequirement) -> bool {
        if !vp9_requirement_needs_packet_refinement(requirement) {
            return false;
        }

        self.capabilities.as_ref().is_some_and(|capabilities| {
            matches!(
                capabilities.check_video_requirement(requirement),
                Err(ref unsupported_requirement)
                    if unsupported_requirement_can_be_refined_by_vp9_packet_probe(
                        unsupported_requirement
                    )
            )
        })
    }

    /// Проверяет video stream requirement по последнему capability report.
    pub(crate) fn validate_video_decode_requirement(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> PlayerResult<()> {
        let Some(capabilities) = &self.capabilities else {
            return Ok(());
        };

        match capabilities.check_video_requirement(requirement) {
            Ok(_) => Ok(()),
            Err(production_error) => {
                if !p010_boundary_diagnostic_mode_enabled() {
                    return Err(player_error_from_unsupported_requirement(production_error));
                }

                capabilities
                    .check_video_requirement_for_p010_boundary_diagnostic(requirement)
                    .map(|_| {
                        warn!(
                            env_var = P010_BOUNDARY_DIAGNOSTIC_ENV_VAR,
                            requirement = %requirement.describe(),
                            "P010 boundary diagnostic mode bypassed production render selection"
                        );
                    })
                    .map_err(player_error_from_unsupported_requirement)
            }
        }
    }

    /// Уточняет active video requirement после bitstream probe.
    pub(crate) fn refine_active_video_requirement(
        &mut self,
        requirement: VideoDecodeRequirement,
    ) -> PlayerResult<()> {
        self.validate_video_decode_requirement(&requirement)?;
        self.pipeline.active_video_requirement = Some(requirement);
        Ok(())
    }

    /// Возвращает codec текущего video track по `TrackId`.
    pub(crate) fn video_codec_for_track(&self, track_id: TrackId) -> Option<VideoCodec> {
        self.pipeline
            .tracks
            .iter()
            .find(|track| track.id == track_id && track.kind == TrackKind::Video)
            .and_then(|track| VideoCodec::from_container_codec_id(&track.codec_id))
    }

    /// Возвращает VP9 container metadata source для active track refinement.
    pub(crate) fn vp9_container_metadata_source_for_track(
        &self,
        track_id: TrackId,
    ) -> Option<Vp9MetadataSource> {
        self.pipeline
            .tracks
            .iter()
            .find(|track| track.id == track_id && track.kind == TrackKind::Video)
            .and_then(vp9_metadata_source_from_track)
    }

    /// Инициализирует audio pipeline если есть Opus-compatible audio track.
    fn init_audio_pipeline(&mut self, tracks: &[TrackInfo]) {
        let audio_track = tracks.iter().find(|track| {
            track.kind == TrackKind::Audio
                && track.sample_rate.is_some()
                && track.channels.is_some()
        });

        let Some(track) = audio_track else {
            info!("Audio track не найден или параметры неизвестны — playback без звука");
            return;
        };

        let (Some(sample_rate), Some(channels)) = (track.sample_rate, track.channels) else {
            warn!(
                track_id = %track.id,
                "Audio track выбран без sample_rate/channels"
            );
            return;
        };

        info!(
            track_id = %track.id,
            codec = %track.codec_id,
            sample_rate,
            channels,
            "Инициализация audio pipeline"
        );

        match audio::OpusDecoder::new(sample_rate, channels) {
            Ok(decoder) => {
                self.pipeline.audio_decoder = Some(decoder);
            }
            Err(error) => {
                warn!(error = %error, "Не удалось создать Opus decoder");
                self.set_runtime_error(format!("Audio error: {error}"));
                return;
            }
        }

        match audio::AudioOutput::new(sample_rate, channels) {
            Ok(mut output) => {
                output.set_volume(self.snapshot.volume);
                self.pipeline.audio_output = Some(output);
            }
            Err(error) => {
                warn!(error = %error, "Не удалось создать AudioOutput");
                self.set_runtime_error(format!("Audio error: {error}"));
                return;
            }
        }

        self.pipeline.audio_track_id = Some(track.id);
        self.snapshot.selected_tracks.audio_track = Some(track.id);

        if let Some(ref output) = self.pipeline.audio_output {
            self.pipeline.audio_clock = Some(output.clock().clone());
        }

        info!("Audio pipeline инициализирован");
    }

    /// Формирует метку источника без раскрытия mutable demuxer state.
    fn source_label(&self) -> Option<String> {
        self.pipeline
            .file_path
            .as_ref()
            .map(|file_path| file_path.display().to_string())
            .or_else(|| self.pipeline.source_label.clone())
            .or_else(|| self.snapshot.source_label.clone())
    }

    /// Возвращает имя файла или streaming label как текущий media title.
    fn media_title(&self) -> Option<String> {
        self.pipeline
            .file_path
            .as_ref()
            .and_then(|file_path| file_path.file_name())
            .map(|file_name| file_name.to_string_lossy().into_owned())
            .or_else(|| self.snapshot.media_title.clone())
    }

    /// Собирает snapshot выбранных tracks.
    fn track_selection_snapshot(&self) -> TrackSelectionSnapshot {
        TrackSelectionSnapshot {
            video_track: self.pipeline.video_track_id,
            audio_track: self.pipeline.audio_track_id,
            subtitle_track: self.snapshot.selected_tracks.subtitle_track,
        }
    }

    /// Собирает compact track metadata для UI.
    fn track_summary_snapshot(&self) -> Vec<TrackSummarySnapshot> {
        self.pipeline
            .tracks
            .iter()
            .map(|track| TrackSummarySnapshot {
                id: track.id,
                kind: track.kind,
                codec_id: track.codec_id.clone(),
                sample_rate: track.sample_rate,
                channels: track.channels,
                video_color_summary: video_color_summary(track),
            })
            .collect()
    }

    /// Собирает snapshot активного backend и texture pool.
    fn backend_snapshot(&self) -> BackendSnapshot {
        BackendSnapshot {
            name: Some(self.pipeline.video_backend.to_string()),
            texture_pool: self.texture_pool_snapshot(),
        }
    }

    /// Конвертирует VA-API texture pool stats в core snapshot.
    fn texture_pool_snapshot(&self) -> Option<TexturePoolSnapshot> {
        self.pipeline
            .video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.texture_pool_stats())
            .map(|texture_stats| TexturePoolSnapshot {
                capacity: texture_stats.capacity,
                slots: texture_stats.slots,
                in_use: texture_stats.in_use,
            })
    }

    /// Описывает текущий кадр без передачи renderer-owned ресурсов.
    fn current_video_frame_snapshot(&self) -> Option<VideoFrameSnapshot> {
        self.pipeline
            .present_video_frame
            .as_ref()
            .map(|present_frame| VideoFrameSnapshot {
                render_generation: self.pipeline.render_generation,
                handle: present_frame.texture_handle.0,
                pts: present_frame.pts,
                width: present_frame.width,
                height: present_frame.height,
                render_width: present_frame.render_width,
                render_height: present_frame.render_height,
            })
    }

    /// Собирает snapshot audio buffer.
    fn audio_buffer_snapshot(&self) -> AudioBufferSnapshot {
        AudioBufferSnapshot {
            level: self
                .audio_buffer_level_ms()
                .and_then(|level_ms| optional_duration_from_seconds(level_ms / 1000.0)),
            underruns: self
                .pipeline
                .audio_clock
                .as_ref()
                .map(|audio_clock| audio_clock.underrun_callbacks())
                .unwrap_or(0),
        }
    }

    /// Собирает snapshot очередей без раскрытия их содержимого.
    fn queue_snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            pending_audio_packets: self.pipeline.pending_audio_packets.len(),
            pending_video_packets: self.pipeline.pending_video_packets.len(),
            decoded_video_frames: self.pipeline.video_frame_queue.len(),
        }
    }
}

/// Строит минимальное decode requirement из container track metadata.
fn video_requirement_from_track(track: &TrackInfo) -> Option<VideoDecodeRequirement> {
    let codec = VideoCodec::from_container_codec_id(&track.codec_id)?;
    if codec != VideoCodec::Vp9 {
        return Some(VideoDecodeRequirement::new(codec));
    }

    let Some(container_source) = vp9_metadata_source_from_track(track) else {
        return Some(VideoDecodeRequirement::new(codec));
    };

    Some(resolve_vp9_metadata(Some(container_source), None).requirement)
}

/// Возвращает `true`, если VP9 requirement ещё нуждается в header-level уточнении.
pub(crate) fn vp9_requirement_needs_packet_refinement(
    requirement: &VideoDecodeRequirement,
) -> bool {
    requirement.codec == VideoCodec::Vp9
        && (requirement.profile.is_none()
            || requirement.bit_depth.is_none()
            || requirement.chroma.is_none()
            || requirement.color.is_none())
}

/// Проверяет, что отказ относится к metadata, которую VP9 packet probe может уточнить.
fn unsupported_requirement_can_be_refined_by_vp9_packet_probe(
    unsupported_requirement: &UnsupportedVideoRequirement,
) -> bool {
    unsupported_requirement.requirement.codec == VideoCodec::Vp9
        && matches!(
            unsupported_requirement.rejections.first(),
            Some(VideoCapabilityRejection::InvalidHdrMetadata { .. })
                | Some(VideoCapabilityRejection::InsufficientStreamMetadata {
                    codec: VideoCodec::Vp9
                })
        )
}

/// Собирает VP9 resolver source из typed video metadata track-а.
fn vp9_metadata_source_from_track(track: &TrackInfo) -> Option<Vp9MetadataSource> {
    let video = track.video.as_ref()?;
    let mut source = Vp9MetadataSource::container();
    source.profile = video.profile;
    source.bit_depth = video.bit_depth;
    source.chroma = video.chroma;
    source.width = video.coded_width;
    source.height = video.coded_height;
    if let Some(color) = &video.color {
        source = source.with_color(color.clone());
    }
    Some(source)
}

/// Пишет resolved VP9/container metadata в logs без codec logic в UI.
fn log_selected_video_track_metadata(
    track: &TrackInfo,
    active_requirement: Option<&VideoDecodeRequirement>,
) {
    let Some(video_metadata) = track.video.as_ref() else {
        return;
    };

    info!(
        track_id = %track.id,
        codec = %track.codec_id,
        width = ?video_metadata.coded_width,
        height = ?video_metadata.coded_height,
        bit_depth = ?video_metadata.bit_depth,
        chroma = ?video_metadata.chroma,
        color = ?video_metadata.color,
        requirement = ?active_requirement,
        "Video track metadata resolved from container"
    );
}

/// Формирует compact color summary для media info panel.
fn video_color_summary(track: &TrackInfo) -> Option<String> {
    let color = track.video.as_ref()?.color.as_ref()?;
    Some(format!(
        "{} {} {} {}",
        display_primaries(color.primaries),
        display_transfer(color.transfer),
        display_matrix(color.matrix),
        display_range(color.range)
    ))
}

/// Возвращает stable label для primaries.
fn display_primaries(primaries: ColorPrimaries) -> &'static str {
    match primaries {
        ColorPrimaries::Bt709 => "BT.709",
        ColorPrimaries::Bt2020 => "BT.2020",
        ColorPrimaries::Smpte170m => "SMPTE 170M",
        ColorPrimaries::Bt470Bg => "BT.470BG",
        ColorPrimaries::Unknown => "primaries unknown",
    }
}

/// Возвращает stable label для transfer function.
fn display_transfer(transfer: TransferFunction) -> &'static str {
    match transfer {
        TransferFunction::Bt709 => "BT.709",
        TransferFunction::Srgb => "sRGB",
        TransferFunction::Pq => "PQ",
        TransferFunction::Hlg => "HLG",
        TransferFunction::Unknown => "transfer unknown",
    }
}

/// Возвращает stable label для matrix coefficients.
fn display_matrix(matrix: MatrixCoefficients) -> &'static str {
    match matrix {
        MatrixCoefficients::Bt601 => "BT.601",
        MatrixCoefficients::Bt709 => "BT.709 matrix",
        MatrixCoefficients::Bt2020 => "BT.2020 matrix",
        MatrixCoefficients::Unknown => "matrix unknown",
    }
}

/// Возвращает stable label для range.
fn display_range(range: ColorRange) -> &'static str {
    match range {
        ColorRange::Limited => "limited",
        ColorRange::Full => "full",
        ColorRange::Unknown => "range unknown",
    }
}

/// Переводит structured capability error в player error model.
fn player_error_from_unsupported_requirement(error: UnsupportedVideoRequirement) -> PlayerError {
    let kind = match error.rejections.first() {
        Some(VideoCapabilityRejection::UnsupportedCodec { .. }) => {
            PlayerErrorKind::UnsupportedVideoCodec
        }
        Some(VideoCapabilityRejection::UnsupportedProfile { .. }) => {
            PlayerErrorKind::UnsupportedVideoProfile
        }
        Some(VideoCapabilityRejection::UnsupportedBitDepth { .. }) => {
            PlayerErrorKind::UnsupportedVideoBitDepth
        }
        Some(VideoCapabilityRejection::UnsupportedChroma { .. }) => {
            PlayerErrorKind::UnsupportedVideoChroma
        }
        Some(VideoCapabilityRejection::UnsupportedHdrRenderer { .. }) => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::InvalidHdrMetadata { .. }) => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::P010NotRenderable { .. }) if error.requirement.hdr => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::NoAvailableRenderer)
        | Some(VideoCapabilityRejection::UnsupportedDeviceExportPath { .. })
        | Some(VideoCapabilityRejection::UnsupportedP010StorageLayout { .. })
        | Some(VideoCapabilityRejection::UnsupportedRenderFrameFormat { .. })
        | Some(VideoCapabilityRejection::P010NotRenderable { .. }) => {
            PlayerErrorKind::UnsupportedRenderFormat
        }
        Some(VideoCapabilityRejection::NoAvailableBackend)
        | Some(VideoCapabilityRejection::UnsupportedDecodeFormat { .. })
        | Some(VideoCapabilityRejection::InsufficientStreamMetadata { .. })
        | None => PlayerErrorKind::HardwareDecoderUnavailable,
    };

    PlayerError::new(kind, error.user_message())
}

impl Default for PlayerSession {
    /// Создаёт пустую session с default snapshot.
    fn default() -> Self {
        Self {
            snapshot: PlayerSnapshot::default(),
            pipeline: PlaybackPipeline::default(),
            pending_events: Vec::new(),
            pending_autoplay: false,
            shutdown_requested: false,
            draining_after_eof: false,
            capabilities: None,
        }
    }
}

/// Безопасно создаёт `Duration` только из finite и неотрицательных секунд.
fn optional_duration_from_seconds(seconds: f64) -> Option<Duration> {
    Duration::try_from_secs_f64(seconds).ok()
}

/// Возвращает `true`, если ручной Phase 9 P010 boundary diagnostic mode включён.
fn p010_boundary_diagnostic_mode_enabled() -> bool {
    std::env::var(P010_BOUNDARY_DIAGNOSTIC_ENV_VAR)
        .ok()
        .as_deref()
        .is_some_and(is_enabled_env_value)
}

/// Разбирает env flag без неявного включения от случайного значения.
fn is_enabled_env_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MediaSource, PlayerCommand, ScrubCommitPolicy, SeekTarget};
    use capability_core::{
        BackendCapabilities, BackendDriverInfo, BackendProbeStatus, P010StorageLayout,
        VideoExportPath,
    };
    use codec_core::{
        BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat, VideoProfile,
        Vp9Profile,
    };
    use render_core::RenderCapabilities;

    fn capabilities_with_vp9_profile0() -> SystemCapabilities {
        SystemCapabilities {
            schema_version: capability_core::CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds: 1,
            video_backends: vec![BackendCapabilities {
                backend_id: DecodeBackendId::vaapi(),
                display_name: "VA-API".to_string(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                supported_video_decode_formats: vec![SupportedVideoDecodeFormat {
                    codec: VideoCodec::Vp9,
                    profile: VideoProfile::Vp9(Vp9Profile::Profile0),
                    bit_depth: BitDepth::Eight,
                    chroma: ChromaSubsampling::Yuv420,
                    max_width: Some(1920),
                    max_height: Some(1080),
                    max_fps: None,
                    hdr_input: false,
                    backend: DecodeBackendId::vaapi(),
                }],
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                export_paths: vec![VideoExportPath::DmaBuf],
                p010_storage_layouts: Vec::new(),
                diagnostics: Vec::new(),
            }],
            render_backends: vec![RenderCapabilities::wgpu_nv12(Some(4096))],
        }
    }

    fn capabilities_with_phase10_vp9_profile2_hdr() -> SystemCapabilities {
        SystemCapabilities {
            schema_version: capability_core::CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds: 1,
            video_backends: vec![BackendCapabilities {
                backend_id: DecodeBackendId::vaapi(),
                display_name: "VA-API".to_string(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                supported_video_decode_formats: vec![SupportedVideoDecodeFormat {
                    codec: VideoCodec::Vp9,
                    profile: VideoProfile::Vp9(Vp9Profile::Profile2),
                    bit_depth: BitDepth::Ten,
                    chroma: ChromaSubsampling::Yuv420,
                    max_width: Some(4096),
                    max_height: Some(4096),
                    max_fps: None,
                    hdr_input: true,
                    backend: DecodeBackendId::vaapi(),
                }],
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                export_paths: vec![VideoExportPath::DmaBuf],
                p010_storage_layouts: vec![P010StorageLayout::BaselineSeparateLayer],
                diagnostics: Vec::new(),
            }],
            render_backends: vec![RenderCapabilities::wgpu_p010_bt2446c(Some(4096))],
        }
    }

    fn bt2020_pq_limited() -> codec_core::VideoColorMetadata {
        codec_core::VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            TransferFunction::Pq,
            None,
        )
    }

    #[test]
    fn default_session_starts_idle() {
        let session = PlayerSession::new();

        assert_eq!(session.snapshot().playback_state, PlaybackState::Idle);
        assert_eq!(session.snapshot().current_position, Duration::ZERO);
        assert_eq!(
            session.snapshot().timeline.current_position,
            MediaTime::ZERO
        );
    }

    #[test]
    fn seek_command_updates_legacy_and_typed_timeline_position() {
        let mut session = PlayerSession::new();
        let request = SeekRequest::absolute(MediaTime::from_millis(1_500));

        session
            .dispatch_command(PlayerCommand::Seek(request))
            .unwrap();

        assert_eq!(
            session.snapshot().current_position,
            Duration::from_millis(1_500)
        );
        assert_eq!(
            session.snapshot().timeline.current_position,
            MediaTime::from_millis(1_500)
        );
        assert!(session.take_events().iter().any(
            |event| matches!(event, PlayerEvent::SeekRequested(accepted) if *accepted == request)
        ));
    }

    #[test]
    fn relative_seek_target_resolves_from_current_timeline_position() {
        let mut session = PlayerSession::new();
        session.update_current_position(Duration::from_secs(10));
        let request = SeekRequest {
            target: SeekTarget::Relative(Duration::from_secs(5)),
            mode: crate::SeekMode::Accurate,
        };

        session
            .dispatch_command(PlayerCommand::Seek(request))
            .unwrap();

        assert_eq!(session.snapshot().current_position, Duration::from_secs(15));
    }

    #[test]
    fn scrub_commands_track_latest_target_and_commit_it() {
        let mut session = PlayerSession::new();
        let request = SeekRequest::absolute(MediaTime::from_secs(7));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();

        assert!(session.snapshot().timeline.scrubbing);
        assert!(session.snapshot().timeline.stale_frame);
        assert_eq!(
            session.snapshot().timeline.target_position,
            Some(MediaTime::from_secs(7))
        );

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatest,
            })
            .unwrap();

        assert!(!session.snapshot().timeline.scrubbing);
        assert_eq!(session.snapshot().current_position, Duration::from_secs(7));
    }

    #[test]
    fn play_pause_commands_update_snapshot_and_events() {
        let mut session = PlayerSession::new();

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.dispatch_command(PlayerCommand::Pause).unwrap();

        let events = session.take_events();
        assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
        assert!(events.contains(&PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)));
        assert!(events.contains(&PlayerEvent::PlaybackStateChanged(PlaybackState::Paused)));
    }

    #[test]
    fn invalid_volume_is_reported_and_preserves_previous_value() {
        let mut session = PlayerSession::new();

        let result = session.dispatch_command(PlayerCommand::SetVolume(1.5));

        assert!(result.is_err());
        assert_eq!(session.snapshot().volume, 1.0);
        assert!(session.snapshot().last_error.is_some());
        assert!(
            session
                .take_events()
                .iter()
                .any(|event| matches!(event, PlayerEvent::RecoverableError(_)))
        );
    }

    #[test]
    fn autoplay_open_media_enters_buffering_before_play() {
        let mut session = PlayerSession::new();
        let request = MediaOpenRequest::new(MediaSource::ExternalLabel("sample".into()), true);

        session
            .dispatch_command(PlayerCommand::OpenMedia(request))
            .unwrap();
        assert_eq!(session.snapshot().playback_state, PlaybackState::Opening);

        session
            .mark_media_opened(MediaSummary {
                title: Some("Sample".into()),
                source_label: "sample".into(),
                duration: Some(Duration::from_secs(10)),
            })
            .unwrap();

        assert_eq!(session.snapshot().playback_state, PlaybackState::Buffering);
        assert!(session.is_demuxing_active());
        assert!(session.can_present_video());
        assert!(
            session
                .finish_autoplay_preroll_if_ready(50.0)
                .expect("preroll finish should not fail")
        );
        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
        assert_eq!(session.snapshot().duration, Some(Duration::from_secs(10)));
    }

    #[test]
    fn old_generation_render_release_does_not_touch_current_generation() {
        let mut session = PlayerSession::new();
        let old_generation = session.pipeline.render_generation;
        let old_handle = video_core::FrameTextureHandle(5);

        assert!(session.register_render_lease(old_generation, old_handle));
        session.release_video_texture(old_handle);
        session.reset_media_state();
        let new_generation = session.pipeline.render_generation;

        session.release_render_lease(old_generation, old_handle);

        assert!(new_generation > old_generation);
        assert!(session.pipeline.leased_video_textures.is_empty());
        assert!(session.pipeline.deferred_video_texture_releases.is_empty());
    }

    #[test]
    fn eof_drain_state_is_visible_in_snapshot() {
        let mut session = PlayerSession::new();

        session.enter_eof_drain();

        assert_eq!(session.playback_state(), PlaybackState::Draining);
        assert!(session.can_present_video());
    }

    #[test]
    fn capability_report_updates_snapshot_and_event_queue() {
        let mut session = PlayerSession::new();

        session.set_system_capabilities(capabilities_with_vp9_profile0());

        assert!(session.snapshot().capability_summary.is_some());
        assert!(
            session
                .take_events()
                .iter()
                .any(|event| matches!(event, PlayerEvent::CapabilityScanCompleted(_)))
        );
    }

    #[test]
    fn unsupported_profile_returns_player_error_before_decode() {
        let mut session = PlayerSession::new();
        session.set_system_capabilities(capabilities_with_vp9_profile0());
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2));

        let error = session
            .validate_video_decode_requirement(&requirement)
            .expect_err("VP9 profile2 must be rejected by profile0-only capabilities");

        assert_eq!(error.kind, PlayerErrorKind::UnsupportedVideoProfile);
        assert!(error.message.contains("profile VP9 Profile 2"));
        assert!(!session.can_defer_vp9_packet_refinement(&requirement));
    }

    #[test]
    fn incomplete_vp9_hdr_container_metadata_waits_for_packet_refinement() {
        let mut session = PlayerSession::new();
        session.set_system_capabilities(capabilities_with_phase10_vp9_profile2_hdr());
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_resolution(3840, 2160)
            .with_color(bt2020_pq_limited());

        let error = session
            .validate_video_decode_requirement(&requirement)
            .expect_err("container-only VP9 HDR metadata is not strict enough yet");

        assert_eq!(error.kind, PlayerErrorKind::UnsupportedHdrMode);
        assert!(vp9_requirement_needs_packet_refinement(&requirement));
        assert!(session.can_defer_vp9_packet_refinement(&requirement));
    }

    #[test]
    fn p010_boundary_diagnostic_env_parser_requires_explicit_enabled_value() {
        for enabled_value in ["1", "true", "TRUE", "yes", "on", " on "] {
            assert!(is_enabled_env_value(enabled_value));
        }

        for disabled_value in ["", "0", "false", "no", "off", "debug"] {
            assert!(!is_enabled_env_value(disabled_value));
        }
    }
}
