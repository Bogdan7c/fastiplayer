use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use capability_core::{SystemCapabilities, UnsupportedVideoRequirement, VideoCapabilityRejection};
use codec_core::{VideoCodec, VideoDecodeRequirement};
use media_core::{TrackInfo, TrackKind};
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
        matches!(self.snapshot.playback_state, PlaybackState::Playing)
    }

    /// Возвращает `true`, если scheduler может менять present frame.
    #[must_use]
    pub const fn can_present_video(&self) -> bool {
        matches!(self.snapshot.playback_state, PlaybackState::Playing) || self.draining_after_eof
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

        self.snapshot.current_position = position;
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
        self.reset_media_state();

        let open_request = MediaOpenRequest::new(MediaSource::LocalFile(path.to_path_buf()), false);
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
    pub fn load_demuxer(&mut self, label: String, demuxer: Box<dyn webm_demux::Demuxer>) {
        self.reset_media_state();

        let open_request = MediaOpenRequest::new(MediaSource::ExternalLabel(label.clone()), false);
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
        self.snapshot.duration = summary.duration;
        self.snapshot.source_label = Some(summary.source_label.clone());
        self.clear_error();
        self.pending_events.push(PlayerEvent::MediaOpened(summary));

        if self.pending_autoplay {
            self.play()?;
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
        self.snapshot.duration = None;
        self.snapshot.current_position = Duration::ZERO;
        self.snapshot.selected_tracks = TrackSelectionSnapshot::default();
        self.snapshot.tracks.clear();
        self.clear_error();
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
        if let Some(ref thread) = self.pipeline.video_decoder_thread {
            for frame in self.pipeline.video_frame_queue.drain(..) {
                thread.release_frame(frame.texture_handle);
            }
            if let Some(ref frame) = self.pipeline.present_video_frame {
                thread.release_frame(frame.texture_handle);
            }
        }
        self.pipeline.video_frame_queue.clear();
        self.pipeline.present_video_frame = None;
    }

    /// Очищает только очередь будущих video frames, сохраняя текущий кадр на экране.
    pub fn clear_queued_video_frames(&mut self) {
        if let Some(ref thread) = self.pipeline.video_decoder_thread {
            for frame in self.pipeline.video_frame_queue.drain(..) {
                thread.release_frame(frame.texture_handle);
            }
        }
        self.pipeline.video_frame_queue.clear();
    }

    /// Обрабатывает audio packet: decode -> write to AudioOutput.
    pub fn process_audio_packet(&mut self, track_id: TrackId, data: &[u8]) {
        if self.pipeline.audio_track_id != Some(track_id) {
            return;
        }

        if let Some(ref mut decoder) = self.pipeline.audio_decoder {
            match decoder.decode(data) {
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
        self.snapshot.duration = None;
        self.snapshot.current_position = Duration::ZERO;
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
        self.publish_position_changed(request.position);
        self.pending_events
            .push(PlayerEvent::SeekRequested(request));
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
                    self.pipeline.video_track_id = Some(track.id);
                    self.pipeline.active_video_requirement = Some(requirement);
                    self.snapshot.selected_tracks.video_track = Some(track.id);
                    return Ok(());
                }
                Err(error) => {
                    last_rejection = Some(error);
                }
            }
        }

        Err(last_rejection.unwrap_or_else(|| {
            PlayerError::new(PlayerErrorKind::UnsupportedVideoCodec, missing_message)
        }))
    }

    /// Проверяет video stream requirement по последнему capability report.
    pub(crate) fn validate_video_decode_requirement(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> PlayerResult<()> {
        let Some(capabilities) = &self.capabilities else {
            return Ok(());
        };

        capabilities
            .check_video_requirement(requirement)
            .map(|_| ())
            .map_err(player_error_from_unsupported_requirement)
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
    Some(VideoDecodeRequirement::new(codec))
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
        Some(VideoCapabilityRejection::UnsupportedFormat { .. }) if error.requirement.hdr => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::UnsupportedRenderFormat { .. }) if error.requirement.hdr => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::UnsupportedRenderFormat { .. }) => {
            PlayerErrorKind::UnsupportedRenderFormat
        }
        Some(VideoCapabilityRejection::NoAvailableBackend)
        | Some(VideoCapabilityRejection::UnsupportedFormat { .. })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MediaSource, PlayerCommand};
    use capability_core::{BackendCapabilities, BackendDriverInfo, BackendProbeStatus};
    use codec_core::{
        BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat, VideoProfile,
        Vp9Profile,
    };

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
                export_paths: Vec::new(),
                diagnostics: Vec::new(),
            }],
            render_backends: Vec::new(),
        }
    }

    #[test]
    fn default_session_starts_idle() {
        let session = PlayerSession::new();

        assert_eq!(session.snapshot().playback_state, PlaybackState::Idle);
        assert_eq!(session.snapshot().current_position, Duration::ZERO);
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
    fn open_media_waits_for_external_media_open_confirmation() {
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

        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
        assert_eq!(session.snapshot().duration, Some(Duration::from_secs(10)));
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
    }
}
