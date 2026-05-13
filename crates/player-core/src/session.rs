use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use capability_core::{SystemCapabilities, UnsupportedVideoRequirement, VideoCapabilityRejection};
use codec_core::{
    ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoCodec,
    VideoDecodeRequirement, Vp9MetadataSource, resolve_vp9_metadata,
};
use media_core::{MediaDuration, MediaTime, TimelineNotSeekableReason, TrackInfo, TrackKind};
use tracing::{debug, info, warn};
use webm_demux::{DemuxSeekRequest, DemuxSeekability, Demuxer};

use crate::pipeline::{
    DEFAULT_VIDEO_FRAME_DURATION, MAX_OBSERVED_VIDEO_FRAME_DURATION,
    MIN_OBSERVED_VIDEO_FRAME_DURATION,
};
use crate::seek_controller::PlaybackResumeIntent;
use crate::{
    AudioBufferSnapshot, BackendSnapshot, FrameCounters, MediaOpenRequest, MediaSource,
    MediaSummary, PlaybackPipeline, PlaybackState, PlayerCommand, PlayerError, PlayerErrorKind,
    PlayerEvent, PlayerResult, PlayerSnapshot, QualitySelection, QueueSnapshot, SeekRequest,
    TexturePoolSnapshot, TrackId, TrackSelectionSnapshot, TrackSummarySnapshot, VideoFrameSnapshot,
};

/// Dev-only режим, который разрешает VP9 Profile 2 HDR дойти до P010 zero-copy boundary.
const P010_BOUNDARY_DIAGNOSTIC_ENV_VAR: &str = "RUSTIPLAYER_DEV_VERIFY_P010_BOUNDARY";

/// Тип seek transaction-а: финальный commit меняет playback position, preview только показывает кадр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekCommitKind {
    /// Обычный seek или завершение scrub-а с фиксацией позиции.
    Final,

    /// Live preview во время активного scrub-а без закрытия scrub state.
    Preview,
}

/// Runtime state одного commit seek-а внутри playback pipeline.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SeekCommitState {
    /// Поколение packets/frames, валидное для этой операции.
    pub generation: u64,

    /// Цель commit-а на нормализованной media timeline.
    pub target_position: MediaTime,

    /// Фактическая позиция, на которую container переставил demuxer.
    pub actual_position: MediaTime,

    /// Момент старта операции для timeout policy.
    pub started_at: Instant,

    /// Playback-состояние, которое нужно применить после прохождения gates.
    pub resume_intent: PlaybackResumeIntent,

    /// Поведение завершения commit-а.
    pub kind: SeekCommitKind,
}

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

    /// Активная операция точного seek commit-а, если player ждёт pre-roll/gates.
    seek_commit: Option<SeekCommitState>,

    /// Последний свежий preview-кадр, реально показанный во время текущего scrub.
    last_visible_preview_position: Option<MediaTime>,
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
            PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Seeking
        )
    }

    /// Возвращает `true`, если scheduler может менять present frame.
    #[must_use]
    pub const fn can_present_video(&self) -> bool {
        matches!(
            self.snapshot.playback_state,
            PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Seeking
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
            PlayerCommand::PreviewScrub(request) => self.preview_scrub(request),
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
                let seekability = demuxer.seekability();
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
                self.apply_demux_seekability(seekability);
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
        let seekability = demuxer.seekability();

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
        self.apply_demux_seekability(seekability);
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
        self.pipeline.video_decoder_needs_keyframe = true;
        self.pipeline.audio_clock = None;
        self.pipeline.media_clock_base = Duration::ZERO;
        self.pipeline.seek_generation = 0;
        self.pipeline.audio_buffer_clear_generation = 0;
        self.pipeline.video_frame_duration_estimate = DEFAULT_VIDEO_FRAME_DURATION;
        self.pipeline.last_decoded_video_pts = None;
        self.pipeline.last_audio_clock = Duration::ZERO;
        self.pipeline.last_audio_clock_change_at = std::time::Instant::now();
        self.pipeline.active_video_requirement = None;

        self.pending_autoplay = false;
        self.seek_commit = None;
        self.last_visible_preview_position = None;
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
    pub fn process_audio_packet(
        &mut self,
        track_id: TrackId,
        packet_pts: Duration,
        generation: u64,
        encoded_audio_bytes: &[u8],
    ) {
        if self.pipeline.audio_track_id != Some(track_id) {
            return;
        }

        if generation != self.pipeline.seek_generation {
            return;
        }

        if let Some(ref mut decoder) = self.pipeline.audio_decoder {
            match decoder.decode(encoded_audio_bytes) {
                Ok(samples) if !samples.is_empty() => {
                    let sample_rate = decoder.sample_rate();
                    let channels = decoder.channels();
                    let samples = trim_decoded_audio_to_clock_base(
                        &samples,
                        packet_pts,
                        self.pipeline.media_clock_base,
                        sample_rate,
                        channels,
                    );
                    if samples.is_empty() {
                        return;
                    }

                    if let Some(ref mut output) = self.pipeline.audio_output {
                        output.write_samples(samples);
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

    /// Возвращает активный seek commit для scheduler/gate логики.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn seek_commit(&self) -> Option<SeekCommitState> {
        self.seek_commit
    }

    /// Проверяет, нужно ли выбросить decoded frame как pre-roll до seek target.
    ///
    /// Final seek остаётся точным: кадры до пользовательской позиции не попадают
    /// в presentation queue. Preview seek, наоборот, может показывать такие
    /// кадры как live feedback во время scrub, пока decoder догоняет target.
    #[must_use]
    pub(crate) fn should_drop_decoded_frame_for_seek(&self, frame_pts: Duration) -> bool {
        self.seek_commit.is_some_and(|seek_commit| {
            seek_commit.kind == SeekCommitKind::Final
                && frame_pts < seek_commit.target_position.as_duration()
        })
    }

    /// Отмечает, что target video frame уже стал текущим кадром presentation.
    pub(crate) fn note_presented_frame_for_seek(&mut self, frame_pts: Duration) {
        let Some(seek_commit) = self.seek_commit else {
            return;
        };

        match seek_commit.kind {
            SeekCommitKind::Preview => {
                self.last_visible_preview_position = Some(MediaTime::from_duration(frame_pts));
                self.snapshot.timeline.stale_frame = false;
            }
            SeekCommitKind::Final if frame_pts >= seek_commit.target_position.as_duration() => {
                self.snapshot.timeline.stale_frame = false;
            }
            SeekCommitKind::Final => {}
        }
    }

    /// Завершает seek commit, когда video/audio gates готовы, или применяет timeout.
    pub(crate) fn finish_seek_commit_if_ready(
        &mut self,
        now: Instant,
        commit_timeout: Duration,
        preview_timeout: Duration,
        resume_audio_min_buffer_ms: f64,
        resume_video_min_ready_frames: usize,
    ) {
        let Some(seek_commit) = self.seek_commit else {
            return;
        };

        let active_timeout = match seek_commit.kind {
            SeekCommitKind::Final => commit_timeout,
            SeekCommitKind::Preview => preview_timeout,
        };

        if now.saturating_duration_since(seek_commit.started_at) >= active_timeout {
            self.fail_seek_commit_on_timeout(seek_commit);
            return;
        }

        if !self.seek_commit_gates_ready(
            seek_commit,
            resume_audio_min_buffer_ms,
            resume_video_min_ready_frames,
        ) {
            return;
        }

        self.complete_seek_commit(seek_commit);
    }

    /// Переопределяет resume intent у уже запущенного seek transaction-а.
    ///
    /// Worker использует это после `EndScrub`: сама session видит временную
    /// pause-команду, которой scrub заглушил audio, а исходное желание
    /// пользователя продолжить playback хранится выше, в `SeekController`.
    pub(crate) fn override_active_seek_resume_intent(
        &mut self,
        resume_intent: PlaybackResumeIntent,
    ) -> bool {
        let Some(seek_commit) = self.seek_commit.as_mut() else {
            return false;
        };

        seek_commit.resume_intent = resume_intent;
        self.set_playback_state(PlaybackState::Seeking);

        if resume_intent == PlaybackResumeIntent::Pause {
            self.pause_audio_output_for_seek();
        }

        true
    }

    /// Проверяет video/audio gates для текущего seek commit-а.
    fn seek_commit_gates_ready(
        &self,
        seek_commit: SeekCommitState,
        resume_audio_min_buffer_ms: f64,
        resume_video_min_ready_frames: usize,
    ) -> bool {
        self.seek_video_gate_ready(seek_commit, resume_video_min_ready_frames)
            && self.seek_audio_gate_ready(seek_commit, resume_audio_min_buffer_ms)
    }

    /// Video gate готов, когда target frame показан и перед resume есть небольшой запас кадров.
    fn seek_video_gate_ready(
        &self,
        seek_commit: SeekCommitState,
        resume_video_min_ready_frames: usize,
    ) -> bool {
        if self.pipeline.video_track_id.is_none() {
            return true;
        }

        let target_position = seek_commit.target_position.as_duration();
        let target_frame_presented = self
            .pipeline
            .present_video_frame
            .as_ref()
            .is_some_and(|frame| frame.pts >= target_position)
            && !self.snapshot.timeline.stale_frame;

        if !target_frame_presented {
            return false;
        }

        let required_ready_frames = self
            .required_seek_resume_video_ready_frames(seek_commit, resume_video_min_ready_frames);

        self.seek_ready_video_frame_count(target_position) >= required_ready_frames
    }

    /// Возвращает требуемый video preroll для конкретного seek transaction-а.
    fn required_seek_resume_video_ready_frames(
        &self,
        seek_commit: SeekCommitState,
        resume_video_min_ready_frames: usize,
    ) -> usize {
        match (seek_commit.kind, seek_commit.resume_intent) {
            (SeekCommitKind::Final, PlaybackResumeIntent::Play)
                if self.pipeline.audio_track_id.is_some() =>
            {
                1
            }
            (SeekCommitKind::Final, PlaybackResumeIntent::Play) => {
                resume_video_min_ready_frames.max(1)
            }
            _ => 1,
        }
    }

    /// Считает target/current frame и уже декодированные future frames для seek resume.
    fn seek_ready_video_frame_count(&self, target_position: Duration) -> usize {
        let current_frame_ready = self
            .pipeline
            .present_video_frame
            .as_ref()
            .is_some_and(|frame| {
                frame.pts >= target_position && !self.snapshot.timeline.stale_frame
            });
        let queued_ready_frames = self
            .pipeline
            .video_frame_queue
            .iter()
            .filter(|frame| frame.pts >= target_position)
            .count();

        usize::from(current_frame_ready) + queued_ready_frames
    }

    /// Audio gate готов после очистки buffer; video seek не ждёт audio preroll.
    ///
    /// Для видео пользователь должен сразу увидеть и запустить target frame, а
    /// audio догоняет через обычный demux/decode path. Audio-only media сохраняет
    /// старую защиту: перед resume нужен минимальный buffer.
    fn seek_audio_gate_ready(
        &self,
        seek_commit: SeekCommitState,
        resume_audio_min_buffer_ms: f64,
    ) -> bool {
        if self.pipeline.audio_track_id.is_none() {
            return true;
        }

        if self.pipeline.audio_buffer_clear_generation < seek_commit.generation {
            return false;
        }

        if seek_commit.kind == SeekCommitKind::Preview {
            return true;
        }

        if seek_commit.resume_intent == PlaybackResumeIntent::Pause {
            return true;
        }

        if self.pipeline.video_track_id.is_some() {
            return true;
        }

        self.audio_buffer_level_ms()
            .map(|level_ms| level_ms >= resume_audio_min_buffer_ms.max(1.0))
            .unwrap_or(true)
    }

    /// Успешно закрывает seek transaction и применяет сохранённый resume intent.
    fn complete_seek_commit(&mut self, seek_commit: SeekCommitState) {
        match seek_commit.kind {
            SeekCommitKind::Final => self.complete_final_seek_commit(seek_commit),
            SeekCommitKind::Preview => self.complete_preview_seek_commit(seek_commit),
        }
    }

    /// Закрывает финальный seek и публикует новую playback позицию.
    fn complete_final_seek_commit(&mut self, seek_commit: SeekCommitState) {
        self.seek_commit = None;
        self.last_visible_preview_position = None;
        self.pipeline.media_clock_base = seek_commit.target_position.as_duration();
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.stale_frame = false;
        self.publish_position_changed(seek_commit.target_position.as_duration());

        match seek_commit.resume_intent {
            PlaybackResumeIntent::Pause => {
                self.pause_audio_output_for_seek();
                self.set_playback_state(PlaybackState::Paused);
            }
            PlaybackResumeIntent::Play => {
                if let Some(ref mut output) = self.pipeline.audio_output
                    && let Err(error) = output.play()
                {
                    warn!(error = %error, "Не удалось запустить audio после seek");
                    self.set_runtime_error(format!("Audio play after seek error: {error}"));
                }
                self.pipeline.last_audio_clock = self.audio_clock_now();
                self.pipeline.last_audio_clock_change_at = Instant::now();
                self.set_playback_state(PlaybackState::Playing);
            }
        }
    }

    /// Закрывает live preview seek, оставляя interactive scrub активным.
    fn complete_preview_seek_commit(&mut self, seek_commit: SeekCommitState) {
        self.seek_commit = None;
        self.snapshot.timeline.target_position = Some(seek_commit.target_position);
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.stale_frame = false;
        self.pause_audio_output_for_seek();
        self.set_playback_state(PlaybackState::Paused);
    }

    /// Прерывает seek transaction по timeout как recoverable error и оставляет media paused.
    fn fail_seek_commit_on_timeout(&mut self, seek_commit: SeekCommitState) {
        match seek_commit.kind {
            SeekCommitKind::Final => self.fail_final_seek_commit_on_timeout(seek_commit),
            SeekCommitKind::Preview => self.fail_preview_seek_commit_on_timeout(seek_commit),
        }
    }

    /// Прерывает финальный seek transaction по timeout как recoverable error.
    fn fail_final_seek_commit_on_timeout(&mut self, seek_commit: SeekCommitState) {
        self.seek_commit = None;
        self.last_visible_preview_position = None;
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.stale_frame = false;
        self.pause_audio_output_for_seek();
        self.set_playback_state(PlaybackState::Paused);

        let error = PlayerError::new(
            PlayerErrorKind::SeekTimeout,
            format!(
                "Seek commit timeout after target={} ms, actual demux={} ms",
                seek_commit.target_position.as_duration().as_millis(),
                seek_commit.actual_position.as_duration().as_millis()
            ),
        );
        self.record_recoverable_error(error);
    }

    /// Прерывает только live preview: scrub остаётся живым, финальный commit ещё возможен.
    fn fail_preview_seek_commit_on_timeout(&mut self, seek_commit: SeekCommitState) {
        self.seek_commit = None;
        self.snapshot.timeline.target_position = Some(seek_commit.target_position);
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.stale_frame = false;
        self.pause_audio_output_for_seek();
        self.set_playback_state(PlaybackState::Paused);
        debug!(
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            "Live scrub preview seek timed out"
        );
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
        if let Some(seek_commit) = self.seek_commit.as_mut() {
            seek_commit.resume_intent = PlaybackResumeIntent::Play;
            self.set_playback_state(PlaybackState::Seeking);
            return Ok(());
        }

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
        if let Some(seek_commit) = self.seek_commit.as_mut() {
            seek_commit.resume_intent = PlaybackResumeIntent::Pause;
            self.pause_audio_output_for_seek();
            self.set_playback_state(PlaybackState::Seeking);
            return Ok(());
        }

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

    /// Запускает настоящий seek transaction через текущий demuxer.
    fn seek(&mut self, request: SeekRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        let target_position = self.resolve_seek_target(request);
        self.pending_events
            .push(PlayerEvent::SeekRequested(request));

        if self.pipeline.demuxer.is_none() {
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Seek невозможен: media pipeline ещё не открыт",
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        self.start_seek_transaction(target_position, SeekCommitKind::Final)
    }

    /// Начинает interactive scrub на уровне контракта без запуска demux seek.
    fn begin_scrub(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.stale_frame = true;
        self.snapshot.timeline.target_position = Some(self.snapshot.timeline.current_position);
        self.last_visible_preview_position = None;
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

    /// Делает live preview seek для активного scrub-а без фиксации playback позиции.
    fn preview_scrub(&mut self, request: SeekRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        let target_position = self.resolve_seek_target(request);
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.stale_frame = true;
        self.snapshot.timeline.target_position = Some(target_position);
        self.start_seek_transaction(target_position, SeekCommitKind::Preview)
    }

    /// Завершает scrub и применяет последнюю цель согласно выбранной политике.
    fn end_scrub(&mut self, policy: crate::ScrubCommitPolicy) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        match policy {
            crate::ScrubCommitPolicy::CommitLatest => {
                if let Some(target_position) = self.snapshot.timeline.target_position {
                    if !self.promote_preview_seek_to_final(target_position) {
                        self.start_seek_transaction(target_position, SeekCommitKind::Final)?;
                    }
                }
            }
        }
        self.snapshot.timeline.scrubbing = false;
        Ok(())
    }

    /// Пытается превратить уже выполненную preview-работу в final commit без повторного seek.
    fn promote_preview_seek_to_final(&mut self, target_position: MediaTime) -> bool {
        if self.complete_visible_preview_seek_as_final() {
            return true;
        }

        if self.promote_active_preview_seek_to_final(target_position) {
            return true;
        }

        self.complete_ready_preview_seek_as_final(target_position)
    }

    /// Scrub с уже показанным preview-кадром коммитится от видимой позиции без ожидания target.
    fn complete_visible_preview_seek_as_final(&mut self) -> bool {
        if !self.snapshot.timeline.scrubbing {
            return false;
        }

        if self
            .seek_commit
            .is_some_and(|seek_commit| seek_commit.kind != SeekCommitKind::Preview)
        {
            return false;
        }

        let Some(visible_position) = self.last_visible_preview_position else {
            return false;
        };

        if !self.present_frame_matches_position(visible_position.as_duration()) {
            return false;
        }

        let generation = self
            .seek_commit
            .map(|seek_commit| seek_commit.generation)
            .unwrap_or(self.pipeline.seek_generation);
        let actual_position = self
            .seek_commit
            .map(|seek_commit| seek_commit.actual_position)
            .unwrap_or(visible_position);
        let promoted_seek_commit = SeekCommitState {
            generation,
            target_position: visible_position,
            actual_position,
            started_at: Instant::now(),
            resume_intent: PlaybackResumeIntent::Pause,
            kind: SeekCommitKind::Final,
        };
        self.complete_final_seek_commit(promoted_seek_commit);
        true
    }

    /// Активный preview с тем же target становится final transaction-ом.
    fn promote_active_preview_seek_to_final(&mut self, target_position: MediaTime) -> bool {
        let Some(seek_commit) = self.seek_commit.as_mut() else {
            return false;
        };

        if seek_commit.kind != SeekCommitKind::Preview
            || seek_commit.target_position != target_position
        {
            return false;
        }

        seek_commit.kind = SeekCommitKind::Final;
        seek_commit.resume_intent = PlaybackResumeIntent::Pause;
        seek_commit.started_at = Instant::now();
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.seeking = true;
        self.snapshot.timeline.target_position = Some(target_position);
        self.drop_queued_video_frames_before_target(target_position.as_duration());
        true
    }

    /// Завершённый preview с уже показанным target frame коммитится сразу.
    fn complete_ready_preview_seek_as_final(&mut self, target_position: MediaTime) -> bool {
        if !self.completed_preview_can_commit_as_final(target_position) {
            return false;
        }

        let promoted_seek_commit = SeekCommitState {
            generation: self.pipeline.seek_generation,
            target_position,
            actual_position: target_position,
            started_at: Instant::now(),
            resume_intent: PlaybackResumeIntent::Pause,
            kind: SeekCommitKind::Final,
        };
        self.complete_final_seek_commit(promoted_seek_commit);
        true
    }

    /// Проверяет, что preview действительно дошёл до target frame и не является stale UI state.
    fn completed_preview_can_commit_as_final(&self, target_position: MediaTime) -> bool {
        self.seek_commit.is_none()
            && self.snapshot.timeline.scrubbing
            && self.snapshot.timeline.target_position == Some(target_position)
            && !self.snapshot.timeline.seeking
            && !self.snapshot.timeline.stale_frame
            && self.present_frame_covers_target(target_position.as_duration())
    }

    /// Проверяет, что текущий video frame уже соответствует target; audio-only media проходит.
    fn present_frame_covers_target(&self, target_position: Duration) -> bool {
        if self.pipeline.video_track_id.is_none() {
            return true;
        }

        self.pipeline
            .present_video_frame
            .as_ref()
            .is_some_and(|frame| frame.pts >= target_position)
    }

    /// Проверяет, что текущий present frame ровно тот preview-кадр, который хотим зафиксировать.
    fn present_frame_matches_position(&self, position: Duration) -> bool {
        self.pipeline.video_track_id.is_some()
            && self
                .pipeline
                .present_video_frame
                .as_ref()
                .is_some_and(|frame| frame.pts == position)
    }

    /// Убирает pre-target кадры из future queue при переходе preview -> final.
    fn drop_queued_video_frames_before_target(&mut self, target_position: Duration) {
        let mut retained_frames = std::collections::VecDeque::new();

        while let Some(frame) = self.pipeline.video_frame_queue.pop_front() {
            if frame.pts < target_position {
                self.release_video_texture(frame.texture_handle);
            } else {
                retained_frames.push_back(frame);
            }
        }

        self.pipeline.video_frame_queue = retained_frames;
    }

    /// Выполняет синхронную часть seek transaction и оставляет commit gates на tick.
    fn start_seek_transaction(
        &mut self,
        target_position: MediaTime,
        commit_kind: SeekCommitKind,
    ) -> PlayerResult<()> {
        if !self.snapshot.timeline.seekable {
            let reason = self
                .snapshot
                .timeline
                .not_seekable_reason
                .unwrap_or(TimelineNotSeekableReason::UnknownTimeline);
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                format!("Seek невозможен: timeline не seekable ({reason:?})"),
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        if self.pipeline.demuxer.is_none() {
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Seek невозможен: media pipeline ещё не открыт",
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        let resume_intent = match commit_kind {
            SeekCommitKind::Final => {
                self.last_visible_preview_position = None;
                PlaybackResumeIntent::from_playback_state(self.playback_state())
            }
            SeekCommitKind::Preview => PlaybackResumeIntent::Pause,
        };
        let target_duration = target_position.as_duration();

        self.set_playback_state(PlaybackState::Seeking);
        self.pause_audio_output_for_seek();
        self.pipeline.seek_generation = self.pipeline.seek_generation.saturating_add(1);
        let generation = self.pipeline.seek_generation;

        if let Some(ref thread) = self.pipeline.video_decoder_thread
            && let Err(error) = thread.flush()
        {
            let player_error = PlayerError::new(
                PlayerErrorKind::RuntimeError,
                format!("Video decoder flush failed during seek: {error}"),
            );
            self.record_recoverable_error(player_error);
        }

        self.pipeline.pending_audio_packets.clear();
        self.pipeline.pending_video_packets.clear();
        self.pipeline.video_decoder_needs_keyframe = self.pipeline.video_track_id.is_some();
        self.clear_queued_video_frames();
        self.pipeline.last_decoded_video_pts = None;
        self.pipeline.media_clock_base = target_duration;
        self.pipeline.last_audio_clock = Duration::ZERO;
        self.pipeline.last_audio_clock_change_at = Instant::now();
        self.snapshot.timeline.target_position = Some(target_position);
        self.snapshot.timeline.seeking = true;
        self.snapshot.timeline.stale_frame = self.pipeline.present_video_frame.is_some();

        if let Some(ref mut decoder) = self.pipeline.audio_decoder
            && let Err(error) = decoder.reset()
        {
            let player_error = PlayerError::new(
                PlayerErrorKind::RuntimeError,
                format!("Opus decoder reset failed during seek: {error}"),
            );
            self.record_recoverable_error(player_error);
        }

        if let Some(ref mut output) = self.pipeline.audio_output {
            match output.clear_buffer_for_seek(generation) {
                Ok(ack_generation) => {
                    self.pipeline.audio_buffer_clear_generation = ack_generation;
                }
                Err(error) => {
                    let player_error = PlayerError::new(
                        PlayerErrorKind::AudioDeviceUnavailable,
                        format!("Audio buffer clear failed during seek: {error}"),
                    );
                    self.record_recoverable_error(player_error);
                }
            }
        } else {
            self.pipeline.audio_buffer_clear_generation = generation;
            if let Some(ref clock) = self.pipeline.audio_clock {
                clock.reset();
            }
        }

        let seek_result = {
            let Some(demuxer) = self.pipeline.demuxer.as_mut() else {
                return Ok(());
            };
            let demux_seek_request = demux_seek_request_for_transaction(
                commit_kind,
                self.pipeline.video_track_id.is_some(),
                target_duration,
            );
            demuxer.seek_with_request(demux_seek_request)
        };

        match seek_result {
            Ok(result) => {
                self.seek_commit = Some(SeekCommitState {
                    generation,
                    target_position,
                    actual_position: result.actual_position,
                    started_at: Instant::now(),
                    resume_intent,
                    kind: commit_kind,
                });
                Ok(())
            }
            Err(error) => {
                self.seek_commit = None;
                self.snapshot.timeline.seeking = false;
                self.snapshot.timeline.stale_frame = false;
                self.set_playback_state(PlaybackState::Paused);
                self.snapshot.timeline.target_position = match commit_kind {
                    SeekCommitKind::Final => None,
                    SeekCommitKind::Preview => Some(target_position),
                };
                let player_error = player_error_from_demux_seek_error(error);
                self.record_recoverable_error(player_error);
                Ok(())
            }
        }
    }

    /// Останавливает audio stream для seek, не меняя high-level playback state.
    fn pause_audio_output_for_seek(&mut self) {
        if let Some(ref mut output) = self.pipeline.audio_output
            && let Err(error) = output.pause()
        {
            warn!(error = %error, "Не удалось остановить audio перед seek");
            self.set_runtime_error(format!("Audio pause before seek error: {error}"));
        }
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

    /// Применяет seekability demuxer/source stack-а к player timeline.
    fn apply_demux_seekability(&mut self, seekability: DemuxSeekability) {
        match seekability {
            DemuxSeekability::Seekable => {}
            DemuxSeekability::NotSeekable { reason } => {
                self.snapshot.timeline.seekable = false;
                self.snapshot.timeline.seekable_range = None;
                self.snapshot.timeline.not_seekable_reason = Some(reason);
            }
        }
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

/// Выбирает demux seek mode для текущего seek transaction-а.
///
/// При video track после decoder flush нельзя начинать decode с inter-frame.
/// Поэтому demuxer должен поставить чтение на decode-safe точку до target, а
/// точность commit-а остаётся в player-core: pre-roll/drop доводит кадры до
/// исходной пользовательской позиции.
fn demux_seek_request_for_transaction(
    commit_kind: SeekCommitKind,
    has_video_track: bool,
    target_duration: Duration,
) -> DemuxSeekRequest {
    if has_video_track {
        return DemuxSeekRequest::decode_point_before(target_duration);
    }

    match commit_kind {
        SeekCommitKind::Final => DemuxSeekRequest::accurate(target_duration),
        SeekCommitKind::Preview => DemuxSeekRequest::preview(target_duration),
    }
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
            seek_commit: None,
            last_visible_preview_position: None,
        }
    }
}

/// Безопасно создаёт `Duration` только из finite и неотрицательных секунд.
fn optional_duration_from_seconds(seconds: f64) -> Option<Duration> {
    Duration::try_from_secs_f64(seconds).ok()
}

/// Возвращает срез audio samples, который начинается не раньше текущей media clock base.
fn trim_decoded_audio_to_clock_base(
    samples: &[f32],
    packet_pts: Duration,
    media_clock_base: Duration,
    sample_rate: u32,
    channels: u32,
) -> &[f32] {
    if packet_pts >= media_clock_base || sample_rate == 0 || channels == 0 {
        return samples;
    }

    let channel_count = channels as usize;
    let frame_count = samples.len() / channel_count;
    if frame_count == 0 {
        return &[];
    }

    let trim_duration = media_clock_base.saturating_sub(packet_pts);
    let trim_frames = duration_to_audio_frames(trim_duration, sample_rate);
    if trim_frames >= frame_count {
        return &[];
    }

    let trim_samples = trim_frames.saturating_mul(channel_count);
    &samples[trim_samples..]
}

/// Конвертирует duration в количество audio frames с округлением вниз.
fn duration_to_audio_frames(duration: Duration, sample_rate: u32) -> usize {
    let frames = duration.as_nanos().saturating_mul(u128::from(sample_rate)) / 1_000_000_000u128;

    frames.min(usize::MAX as u128) as usize
}

/// Мапит ошибку demux seek в player error без смешивания unavailable/timeout/demux.
fn player_error_from_demux_seek_error(error: anyhow::Error) -> PlayerError {
    if error
        .downcast_ref::<webm_demux::DemuxError>()
        .is_some_and(webm_demux::DemuxError::is_seek_unavailable)
    {
        return PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            format!("Seek failed: {error}"),
        );
    }

    PlayerError::new(PlayerErrorKind::DemuxError, format!("Seek failed: {error}"))
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
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{
        MediaSource, PendingAudioPacket, PendingVideoPacket, PlayerCommand, ScrubCommitPolicy,
        SeekMode, SeekTarget,
    };
    use bytes::Bytes;
    use capability_core::{
        BackendCapabilities, BackendDriverInfo, BackendProbeStatus, P010StorageLayout,
        VideoExportPath,
    };
    use codec_core::{
        BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat, VideoProfile,
        Vp9Profile,
    };
    use render_core::RenderCapabilities;
    use webm_demux::{DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer};

    /// Fake demuxer для проверки player-core transaction без реального WebM/GPU.
    struct FakeDemuxer {
        tracks: Vec<TrackInfo>,
        duration: Option<Duration>,
        packets: VecDeque<media_core::Packet>,
        seek_log: Arc<Mutex<Vec<Duration>>>,
        seek_request_log: Option<Arc<Mutex<Vec<DemuxSeekRequest>>>>,
        seekability: DemuxSeekability,
    }

    impl FakeDemuxer {
        /// Создаёт fake demuxer с явными tracks и shared seek log.
        fn new(
            tracks: Vec<TrackInfo>,
            duration: Option<Duration>,
            seek_log: Arc<Mutex<Vec<Duration>>>,
        ) -> Self {
            Self {
                tracks,
                duration,
                packets: VecDeque::new(),
                seek_log,
                seek_request_log: None,
                seekability: DemuxSeekability::Seekable,
            }
        }

        /// Задаёт seekability для сценариев с playback-only source.
        fn with_seekability(mut self, seekability: DemuxSeekability) -> Self {
            self.seekability = seekability;
            self
        }

        /// Подключает отдельный log полных demux seek request-ов.
        fn with_seek_request_log(
            mut self,
            seek_request_log: Arc<Mutex<Vec<DemuxSeekRequest>>>,
        ) -> Self {
            self.seek_request_log = Some(seek_request_log);
            self
        }
    }

    impl Demuxer for FakeDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &self.tracks
        }

        fn duration(&self) -> Option<Duration> {
            self.duration
        }

        fn seekability(&self) -> DemuxSeekability {
            self.seekability
        }

        fn next_packet(&mut self) -> anyhow::Result<Option<media_core::Packet>> {
            Ok(self.packets.pop_front())
        }

        fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            self.seek_log
                .lock()
                .expect("seek log mutex should not be poisoned")
                .push(timestamp);
            Ok(DemuxSeekResult {
                requested_position: MediaTime::from_duration(timestamp),
                actual_position: MediaTime::from_duration(timestamp),
                actual_track_timestamp: None,
            })
        }

        fn seek_with_request(
            &mut self,
            request: DemuxSeekRequest,
        ) -> anyhow::Result<DemuxSeekResult> {
            if let Some(ref seek_request_log) = self.seek_request_log {
                seek_request_log
                    .lock()
                    .expect("seek request log mutex should not be poisoned")
                    .push(request);
            }

            self.seek(request.timestamp)
        }
    }

    /// Создаёт минимальный track summary для fake media.
    fn fake_track(track_id: u32, kind: TrackKind) -> TrackInfo {
        TrackInfo {
            id: TrackId::new(track_id),
            kind,
            codec_id: match kind {
                TrackKind::Video => "V_VP9".to_string(),
                TrackKind::Audio => "A_OPUS".to_string(),
            },
            codec_private: None,
            time_base: media_core::TimeBase::new(1, 1_000),
            duration: Some(Duration::from_secs(30)),
            sample_rate: (kind == TrackKind::Audio).then_some(48_000),
            channels: (kind == TrackKind::Audio).then_some(2),
            video: None,
        }
    }

    /// Подключает fake demuxer без инициализации CPAL/VA-API ресурсов.
    fn install_fake_media(
        session: &mut PlayerSession,
        tracks: Vec<TrackInfo>,
    ) -> Arc<Mutex<Vec<Duration>>> {
        install_fake_media_with_seekability(session, tracks, DemuxSeekability::Seekable)
    }

    /// Подключает fake demuxer с заданной seekability.
    fn install_fake_media_with_seekability(
        session: &mut PlayerSession,
        tracks: Vec<TrackInfo>,
        seekability: DemuxSeekability,
    ) -> Arc<Mutex<Vec<Duration>>> {
        let seek_log = Arc::new(Mutex::new(Vec::new()));
        let demuxer = FakeDemuxer::new(
            tracks.clone(),
            Some(Duration::from_secs(30)),
            Arc::clone(&seek_log),
        )
        .with_seekability(seekability);

        session.pipeline.demuxer = Some(Box::new(demuxer));
        session.pipeline.tracks = tracks.clone();
        session.pipeline.video_track_id = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .map(|track| track.id);
        session.pipeline.audio_track_id = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .map(|track| track.id);
        session.set_snapshot_duration(Some(Duration::from_secs(30)));
        session.apply_demux_seekability(seekability);
        session.set_playback_state(PlaybackState::Paused);

        seek_log
    }

    /// Подключает fake demuxer и возвращает log полных demux seek request-ов.
    fn install_fake_media_with_seek_request_log(
        session: &mut PlayerSession,
        tracks: Vec<TrackInfo>,
    ) -> Arc<Mutex<Vec<DemuxSeekRequest>>> {
        let seek_log = Arc::new(Mutex::new(Vec::new()));
        let seek_request_log = Arc::new(Mutex::new(Vec::new()));
        let demuxer = FakeDemuxer::new(tracks.clone(), Some(Duration::from_secs(30)), seek_log)
            .with_seek_request_log(Arc::clone(&seek_request_log));

        session.pipeline.demuxer = Some(Box::new(demuxer));
        session.pipeline.tracks = tracks.clone();
        session.pipeline.video_track_id = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .map(|track| track.id);
        session.pipeline.audio_track_id = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .map(|track| track.id);
        session.set_snapshot_duration(Some(Duration::from_secs(30)));
        session.apply_demux_seekability(DemuxSeekability::Seekable);
        session.set_playback_state(PlaybackState::Paused);

        seek_request_log
    }

    /// Создаёт decoded frame без реальных GPU resources.
    fn decoded_frame_for_tests(pts: Duration, handle: u64) -> video_core::DecodedFrame {
        video_core::DecodedFrame {
            pts,
            format: video_core::DecodedPixelFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            memory_path: video_core::FrameMemoryPath::CpuUpload,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: codec_core::VideoColorMetadata::sdr_bt709_limited(),
            texture_handle: video_core::FrameTextureHandle(handle),
        }
    }

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
    fn seek_command_sets_target_and_commits_position_after_gates() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, Vec::new());
        let request = SeekRequest::absolute(MediaTime::from_millis(1_500));

        session
            .dispatch_command(PlayerCommand::Seek(request))
            .unwrap();

        assert_eq!(session.snapshot().current_position, Duration::ZERO);
        assert_eq!(
            session.snapshot().timeline.target_position,
            Some(MediaTime::from_millis(1_500))
        );
        assert!(session.snapshot().timeline.seeking);

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

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
        install_fake_media(&mut session, Vec::new());
        session.update_current_position(Duration::from_secs(10));
        let request = SeekRequest {
            target: SeekTarget::Relative(Duration::from_secs(5)),
            mode: crate::SeekMode::Accurate,
        };

        session
            .dispatch_command(PlayerCommand::Seek(request))
            .unwrap();
        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().current_position, Duration::from_secs(15));
    }

    #[test]
    fn scrub_commands_track_latest_target_and_commit_it() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, Vec::new());
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
        assert_eq!(session.snapshot().current_position, Duration::ZERO);

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().current_position, Duration::from_secs(7));
    }

    #[test]
    fn preview_scrub_seek_keeps_scrub_active_without_committing_position() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(&mut session, Vec::new());
        let request = SeekRequest::absolute(MediaTime::from_secs(7));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();

        assert_eq!(
            seek_log.lock().expect("seek log lock").as_slice(),
            &[Duration::from_secs(7)]
        );
        assert_eq!(
            session.seek_commit().map(|seek_commit| seek_commit.kind),
            Some(SeekCommitKind::Preview)
        );

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert!(session.snapshot().timeline.scrubbing);
        assert_eq!(
            session.snapshot().timeline.target_position,
            Some(MediaTime::from_secs(7))
        );
        assert_eq!(session.snapshot().current_position, Duration::ZERO);
        assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    }

    #[test]
    fn preview_scrub_seek_passes_target_to_demuxer() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();

        assert_eq!(
            seek_log.lock().expect("seek log lock").as_slice(),
            &[Duration::from_secs(8)]
        );
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| seek_commit.target_position),
            Some(MediaTime::from_secs(8))
        );
        assert_eq!(
            session.seek_commit().map(|seek_commit| seek_commit.kind),
            Some(SeekCommitKind::Preview)
        );
    }

    #[test]
    fn preview_seek_keeps_pre_target_frames_for_live_feedback() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();

        assert!(!session.should_drop_decoded_frame_for_seek(Duration::from_millis(7_900)));

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatest,
            })
            .unwrap();

        assert!(session.should_drop_decoded_frame_for_seek(Duration::from_millis(7_900)));
    }

    #[test]
    fn end_scrub_promotes_active_preview_without_second_demux_seek() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();
        session
            .pipeline
            .video_frame_queue
            .push_back(decoded_frame_for_tests(Duration::from_millis(7_900), 41));

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatest,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].mode,
            webm_demux::DemuxSeekMode::DecodePointBefore
        );
        assert_eq!(
            session.seek_commit().map(|seek_commit| seek_commit.kind),
            Some(SeekCommitKind::Final)
        );
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(session.pipeline.video_frame_queue.is_empty());
    }

    #[test]
    fn end_scrub_commits_visible_preview_frame_without_waiting_for_exact_target() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();
        session.pipeline.present_video_frame =
            Some(decoded_frame_for_tests(Duration::from_millis(7_900), 42));
        session.note_presented_frame_for_seek(Duration::from_millis(7_900));

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatest,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert!(session.seek_commit().is_none());
        assert_eq!(
            session.snapshot().current_position,
            Duration::from_millis(7_900)
        );
        assert_eq!(
            session.pipeline.media_clock_base,
            Duration::from_millis(7_900)
        );
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(!session.snapshot().timeline.seeking);
        assert!(!session.should_drop_decoded_frame_for_seek(Duration::from_millis(7_916)));
    }

    #[test]
    fn end_scrub_keeps_last_visible_preview_when_latest_target_was_not_previewed() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let visible_request = SeekRequest::absolute(MediaTime::from_secs(8));
        let unpresented_request = SeekRequest::absolute(MediaTime::from_secs(9));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(visible_request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(visible_request))
            .unwrap();
        session.pipeline.present_video_frame =
            Some(decoded_frame_for_tests(Duration::from_millis(7_900), 42));
        session.note_presented_frame_for_seek(Duration::from_millis(7_900));

        session
            .dispatch_command(PlayerCommand::UpdateScrub(unpresented_request))
            .unwrap();
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| seek_commit.target_position),
            Some(MediaTime::from_secs(8))
        );
        assert_eq!(
            session.snapshot().timeline.target_position,
            Some(MediaTime::from_secs(9))
        );

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatest,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert!(session.seek_commit().is_none());
        assert_eq!(
            session.snapshot().current_position,
            Duration::from_millis(7_900)
        );
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn end_scrub_commits_ready_preview_without_second_demux_seek() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();
        session.pipeline.present_video_frame =
            Some(decoded_frame_for_tests(Duration::from_secs(8), 42));
        session.note_presented_frame_for_seek(Duration::from_secs(8));
        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatest,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(session.snapshot().current_position, Duration::from_secs(8));
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(!session.snapshot().timeline.seeking);
        assert!(session.seek_commit().is_none());
    }

    #[test]
    fn keyframe_before_seek_keeps_demuxer_target_on_requested_position() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest {
                target: SeekTarget::Absolute(MediaTime::from_secs(8)),
                mode: SeekMode::KeyframeBefore,
            }))
            .unwrap();

        assert_eq!(
            seek_log.lock().expect("seek log lock").as_slice(),
            &[Duration::from_secs(8)]
        );
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| seek_commit.target_position),
            Some(MediaTime::from_secs(8))
        );
    }

    #[test]
    fn accurate_video_seek_passes_requested_target_to_demuxer() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(8),
            )))
            .unwrap();

        assert_eq!(
            seek_log.lock().expect("seek log lock").as_slice(),
            &[Duration::from_secs(8)]
        );
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| seek_commit.target_position),
            Some(MediaTime::from_secs(8))
        );
    }

    #[test]
    fn seek_transaction_passes_demux_request_without_runtime_index_hint() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(8),
            )))
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].timestamp, Duration::from_secs(8));
        assert_eq!(
            requests[0].mode,
            webm_demux::DemuxSeekMode::DecodePointBefore
        );
    }

    #[test]
    fn not_seekable_demuxer_marks_timeline_and_blocks_seek() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media_with_seekability(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable,
            },
        );

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(5),
            )))
            .unwrap();

        assert!(!session.snapshot().timeline.seekable);
        assert_eq!(
            session.snapshot().timeline.not_seekable_reason,
            Some(TimelineNotSeekableReason::SourceNotSeekable)
        );
        assert!(seek_log.lock().expect("seek log lock").is_empty());
        assert_eq!(
            session
                .snapshot()
                .last_error
                .as_ref()
                .map(|error| &error.kind),
            Some(&PlayerErrorKind::SeekUnavailable)
        );
    }

    #[test]
    fn seek_transaction_clears_pending_packets_and_calls_demux_seek() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(
            &mut session,
            vec![
                fake_track(1, TrackKind::Video),
                fake_track(2, TrackKind::Audio),
            ],
        );
        session
            .pipeline
            .pending_audio_packets
            .push_back(PendingAudioPacket::new(
                TrackId::new(2),
                Duration::ZERO,
                session.pipeline.seek_generation,
                Bytes::from_static(&[1, 2, 3]),
            ));
        session
            .pipeline
            .pending_video_packets
            .push_back(PendingVideoPacket::new(
                TrackId::new(1),
                Duration::ZERO,
                session.pipeline.seek_generation,
                Bytes::from_static(&[4, 5, 6]),
                true,
            ));
        session.pipeline.video_decoder_needs_keyframe = false;

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(5),
            )))
            .unwrap();

        assert!(session.pipeline.pending_audio_packets.is_empty());
        assert!(session.pipeline.pending_video_packets.is_empty());
        assert_eq!(
            *seek_log
                .lock()
                .expect("seek log mutex should not be poisoned"),
            vec![Duration::from_secs(5)]
        );
        assert_eq!(session.pipeline.seek_generation, 1);
        assert!(session.pipeline.video_decoder_needs_keyframe);
        assert!(session.seek_commit().is_some());
    }

    #[test]
    fn commit_timeout_pauses_and_reports_recoverable_seek_error() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(5),
            )))
            .unwrap();
        let timeout_now = Instant::now() + Duration::from_secs(11);

        session.finish_seek_commit_if_ready(
            timeout_now,
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
        assert!(matches!(
            session
                .snapshot()
                .last_error
                .as_ref()
                .map(|error| &error.kind),
            Some(PlayerErrorKind::SeekTimeout)
        ));
    }

    #[test]
    fn paused_before_scrub_stays_paused_after_commit() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, Vec::new());

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatest,
            })
            .unwrap();

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn playing_before_scrub_resumes_after_gates() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, Vec::new());

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session.dispatch_command(PlayerCommand::Pause).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatest,
            })
            .unwrap();
        session.dispatch_command(PlayerCommand::Play).unwrap();

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn explicit_scrub_resume_intent_survives_temporary_pause() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, Vec::new());

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session.dispatch_command(PlayerCommand::Pause).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatest,
            })
            .unwrap();

        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| seek_commit.resume_intent),
            Some(PlaybackResumeIntent::Pause)
        );
        assert!(session.override_active_seek_resume_intent(PlaybackResumeIntent::Play));

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn no_audio_media_seek_resumes_after_target_video_frame() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        let target_frame = decoded_frame_for_tests(Duration::from_secs(6), 42);
        session.pipeline.present_video_frame = Some(target_frame);
        session.note_presented_frame_for_seek(Duration::from_secs(6));

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn playing_seek_waits_for_configured_video_preroll_before_resume() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        session.pipeline.present_video_frame =
            Some(decoded_frame_for_tests(Duration::from_secs(6), 42));
        session.note_presented_frame_for_seek(Duration::from_secs(6));

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            3,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Seeking);
        assert!(session.snapshot().timeline.seeking);

        session
            .pipeline
            .video_frame_queue
            .push_back(decoded_frame_for_tests(Duration::from_millis(6_016), 43));
        session
            .pipeline
            .video_frame_queue
            .push_back(decoded_frame_for_tests(Duration::from_millis(6_033), 44));

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            3,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn playing_video_seek_with_audio_resumes_after_target_frame_without_audio_preroll() {
        let mut session = PlayerSession::new();
        install_fake_media(
            &mut session,
            vec![
                fake_track(1, TrackKind::Video),
                fake_track(2, TrackKind::Audio),
            ],
        );

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        session.pipeline.present_video_frame =
            Some(decoded_frame_for_tests(Duration::from_secs(6), 42));
        session.note_presented_frame_for_seek(Duration::from_secs(6));

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            3,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn seek_generation_drops_pre_roll_frames_before_target() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();

        assert!(session.should_drop_decoded_frame_for_seek(Duration::from_millis(5_999)));
        assert!(!session.should_drop_decoded_frame_for_seek(Duration::from_secs(6)));
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
