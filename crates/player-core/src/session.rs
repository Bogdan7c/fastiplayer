use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use capability_core::SystemCapabilities;
use media_core::{MediaDuration, MediaTime, TimelineNotSeekableReason};
use tracing::{debug, info, trace, warn};

use crate::audio_boundary::{missing_audio_decoder_factory, missing_audio_output_factory};
use crate::pipeline::AudioEofDrainState;
use crate::seek_state::{PlaybackResumeIntent, SeekRuntimeState};
use crate::{
    AudioDecoderFactory, AudioOutputFactory, FrameCounters, PlaybackDiagnostics, PlaybackPipeline,
    PlaybackState, PlayerCommand, PlayerError, PlayerErrorKind, PlayerEvent, PlayerResult,
    PlayerSnapshot, QualitySelection, SeekRequest, StartedVideoBackend, TrackId,
};

mod audio_runtime;
mod capability_selection;
mod diagnostics_sink;
mod media_lifecycle;
mod render_leases;
mod seek_transaction;
mod snapshot_builder;
mod tick;

use self::media_lifecycle::MediaLifecycleState;
pub(crate) use self::render_leases::LeasedPresentFrame;
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

/// Точная причина, по которой EOF-drain ещё не может перейти в `Ended`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum EofDrainBlocker {
    /// Активный seek commit владеет текущим lifecycle, EOF не должен его перебивать.
    SeekCommit,

    /// Seek near EOF ждёт fallback video frame, которым владеет seek pipeline.
    SeekEofFallbackVideo,

    /// В pipeline ещё лежит preroll fallback frame, который должен быть обработан владельцем seek.
    SeekPrerollFallbackFrame,

    /// Уже считанные video packets ещё не отправлены или не отброшены decoder boundary.
    PendingVideoPackets { queued_packets: usize },

    /// Готовые video frames ещё ждут presentation/release.
    VideoPresentQueue { queued_frames: usize },

    /// Decoder thread ещё не подтвердил завершение ранее отправленных packets.
    VideoDecodeInFlight { in_flight_packets: usize },

    /// Уже считанные audio packets ещё не декодированы и не записаны в output.
    PendingAudioPackets { queued_packets: usize },

    /// Audio output ещё сообщает buffered tail.
    DrainingAudioOutput {
        /// Текущий уровень output buffer-а в миллисекундах.
        buffer_level_ms: f64,

        /// Был ли уже успешно запрошен запуск output stream-а.
        playback_requested: bool,

        /// Сколько времени audio clock не показывал нового progress-а.
        stalled_for: Duration,

        /// Порог, после которого stale output buffer считается зависшим.
        stall_timeout: Duration,
    },
}

/// Центральная session плеера: high-level state machine и владение playback pipeline.
pub struct PlayerSession {
    /// Последний базовый read-only snapshot без runtime diagnostics, зависящих от shell.
    snapshot: PlayerSnapshot,

    /// Media pipeline, перенесённый из `AppState` в Phase 3.
    pub(crate) pipeline: PlaybackPipeline,

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

    /// Флаг дорендера хвоста после EOF.
    pub draining_after_eof: bool,

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
            draining_after_eof = self.draining_after_eof,
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
        self.snapshot.playback_state == PlaybackState::Playing
            || (self.draining_after_eof && !self.has_active_seek_commit())
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

    /// Переводит session в EOF-drain, сохраняя demuxer открытым для replay/seek.
    pub fn enter_eof_drain(&mut self) {
        let previous_state = self.playback_state();
        debug!(
            previous_state = ?previous_state,
            current_position_ms = self.snapshot.current_position.as_secs_f64() * 1000.0,
            duration_ms = ?self
                .snapshot
                .duration
                .map(|duration| duration.as_secs_f64() * 1000.0),
            pending_audio_packets = self.pipeline.pending_audio_packet_len(),
            pending_video_packets = self.pipeline.pending_video_packet_len(),
            queued_video_frames = self.pipeline.video_present_queue_len(),
            video_decode_in_flight = self.pipeline.video_decode_in_flight_packets(),
            audio_eof_drain_state = ?self.pipeline.audio_eof_drain_state(),
            "Entering EOF drain"
        );
        self.set_playback_state(PlaybackState::Draining);

        if matches!(
            previous_state,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.start_eof_audio_tail_if_needed();
        }
    }

    /// Проверяет, должен ли worker продолжать bounded wakeup для EOF-drain state machine.
    #[must_use]
    pub(crate) fn eof_drain_needs_progress(&self) -> bool {
        self.draining_after_eof && !self.has_active_seek_commit()
    }

    /// Запускает audio output для EOF tail-а, который был накоплен до завершения autoplay preroll.
    pub(crate) fn start_eof_audio_tail_if_needed(&mut self) {
        if !self.draining_after_eof || self.has_active_seek_commit() {
            return;
        }

        let AudioEofDrainState::DrainingOutput {
            playback_requested: false,
            ..
        } = self.pipeline.audio_eof_drain_state()
        else {
            return;
        };

        if let Some(play_result) = self.pipeline.play_audio_output() {
            if let Err(error) = play_result {
                warn!(error = %error, "Не удалось запустить audio tail после EOF");
                self.set_runtime_error(format!("Audio EOF drain play error: {error}"));
                self.pipeline.clear_audio_output();
                self.pipeline.clear_audio_clock();
                return;
            }

            let observed_at = Instant::now();
            let audio_now = self.audio_clock_now();
            self.pipeline
                .reset_audio_clock_sample(audio_now, observed_at);
        }
    }

    /// Завершает EOF-drain, когда audio/video очереди полностью отдали buffered tail.
    pub(crate) fn finish_eof_drain_if_ready(
        &mut self,
        now: Instant,
        audio_stall_timeout: Duration,
    ) -> bool {
        if !self.draining_after_eof {
            return false;
        }

        if !self.eof_drain_ready_to_end(now, audio_stall_timeout) {
            let blocker = self.eof_drain_blocker(now, audio_stall_timeout);
            trace!(
                blocker = ?blocker,
                playback_state = ?self.playback_state(),
                current_position_ms = self.snapshot.current_position.as_secs_f64() * 1000.0,
                duration_ms = ?self
                    .snapshot
                    .duration
                    .map(|duration| duration.as_secs_f64() * 1000.0),
                pending_audio_packets = self.pipeline.pending_audio_packet_len(),
                pending_video_packets = self.pipeline.pending_video_packet_len(),
                queued_video_frames = self.pipeline.video_present_queue_len(),
                video_decode_in_flight = self.pipeline.video_decode_in_flight_packets(),
                seek_commit_active = self.has_active_seek_commit(),
                seek_eof_fallback_video = self.seek_runtime.has_eof_fallback_video_position(),
                audio_eof_drain_state = ?self.pipeline.audio_eof_drain_state(),
                audio_clock_now_ms = self.audio_clock_now().as_secs_f64() * 1000.0,
                audio_clock_stalled_for_ms =
                    self.pipeline.audio_clock_stalled_for(now).as_secs_f64() * 1000.0,
                "EOF drain waiting"
            );
            return false;
        }

        let end_position = self
            .snapshot
            .duration
            .unwrap_or_else(|| self.presentation_clock_position_at(now));
        debug!(
            end_position_ms = end_position.as_secs_f64() * 1000.0,
            previous_position_ms = self.snapshot.current_position.as_secs_f64() * 1000.0,
            audio_eof_drain_state = ?self.pipeline.audio_eof_drain_state(),
            "EOF drain finished; entering Ended"
        );
        self.update_current_position(end_position);
        self.set_playback_state(PlaybackState::Ended);
        true
    }

    /// Проверяет только условия завершения drain; сам state меняет `finish_eof_drain_if_ready`.
    fn eof_drain_ready_to_end(&self, now: Instant, audio_stall_timeout: Duration) -> bool {
        self.eof_drain_blocker(now, audio_stall_timeout).is_none()
    }

    /// Возвращает первый blocker EOF-drain в порядке ownership/lifecycle зависимостей.
    fn eof_drain_blocker(
        &self,
        now: Instant,
        audio_stall_timeout: Duration,
    ) -> Option<EofDrainBlocker> {
        if self.has_active_seek_commit() {
            return Some(EofDrainBlocker::SeekCommit);
        }

        if self.seek_runtime.has_eof_fallback_video_position() {
            return Some(EofDrainBlocker::SeekEofFallbackVideo);
        }

        if self.pipeline.has_seek_preroll_fallback_video_frame() {
            return Some(EofDrainBlocker::SeekPrerollFallbackFrame);
        }

        let pending_video_packets = self.pipeline.pending_video_packet_len();
        if pending_video_packets > 0 {
            return Some(EofDrainBlocker::PendingVideoPackets {
                queued_packets: pending_video_packets,
            });
        }

        let queued_video_frames = self.pipeline.video_present_queue_len();
        if queued_video_frames > 0 {
            return Some(EofDrainBlocker::VideoPresentQueue {
                queued_frames: queued_video_frames,
            });
        }

        let in_flight_packets = self.pipeline.video_decode_in_flight_packets();
        if in_flight_packets > 0 {
            return Some(EofDrainBlocker::VideoDecodeInFlight { in_flight_packets });
        }

        match self.pipeline.audio_eof_drain_state() {
            AudioEofDrainState::PendingPackets { queued_packets } => {
                Some(EofDrainBlocker::PendingAudioPackets { queued_packets })
            }
            AudioEofDrainState::DrainingOutput {
                buffer_level_ms,
                playback_requested,
            } if !self.eof_audio_output_stalled(now, audio_stall_timeout) => {
                Some(EofDrainBlocker::DrainingAudioOutput {
                    buffer_level_ms,
                    playback_requested,
                    stalled_for: self.pipeline.audio_clock_stalled_for(now),
                    stall_timeout: audio_stall_timeout,
                })
            }
            AudioEofDrainState::DrainingOutput { .. }
            | AudioEofDrainState::NoSelectedAudio
            | AudioEofDrainState::NoOutput
            | AudioEofDrainState::DrainedOutput { .. } => None,
        }
    }

    /// Проверяет зависший audio output после EOF без чтения concrete CPAL state-а.
    fn eof_audio_output_stalled(&self, now: Instant, audio_stall_timeout: Duration) -> bool {
        self.pipeline.audio_clock_stalled_for(now) >= audio_stall_timeout
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

        if self.draining_after_eof || self.snapshot.playback_state == PlaybackState::Ended {
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

    /// Запускает повторное воспроизведение после штатного EOF через обычный seek pipeline.
    fn restart_playback_after_eof(&mut self) -> PlayerResult<()> {
        if !self.pipeline.has_demuxer() {
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Replay невозможен: media pipeline уже закрыт",
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        if !self.snapshot.timeline.seekable {
            let reason = self
                .snapshot
                .timeline
                .not_seekable_reason
                .unwrap_or(TimelineNotSeekableReason::UnknownTimeline);
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                format!("Replay невозможен: timeline не seekable ({reason:?})"),
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        self.start_seek_transaction(
            MediaTime::ZERO,
            crate::SeekMode::Accurate,
            PlaybackResumeIntent::Play,
        )
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
        self.pipeline
            .select_video_track_preserving_active_requirement(track_id);
        self.snapshot.selected_tracks.video_track = Some(track_id);
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
        self.draining_after_eof = playback_state == PlaybackState::Draining;
        self.snapshot.playback_state = playback_state;

        if previous_state == playback_state {
            return;
        }

        debug!(
            previous_state = ?previous_state,
            playback_state = ?playback_state,
            draining_after_eof = self.draining_after_eof,
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
            draining_after_eof: false,
            capabilities: None,
            seek_runtime: SeekRuntimeState::default(),
        }
    }
}

#[cfg(test)]
mod tests;
