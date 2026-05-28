use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use capability_core::SystemCapabilities;
use media_core::{MediaDemuxError, MediaDuration, MediaTime, TimelineNotSeekableReason, TrackKind};
use tracing::{debug, info, trace, warn};

use crate::audio_boundary::{missing_audio_decoder_factory, missing_audio_output_factory};
use crate::pipeline::AudioEofDrainState;
use crate::seek_state::{
    PlaybackResumeIntent, SeekCommitState, SeekDemuxRequestError,
    demux_seek_request_for_transaction,
};
use crate::{
    ActiveSeekDiagnosticsSnapshot, AudioDecoderFactory, AudioOutputFactory, FrameCounters,
    PipelineQueueDepthSnapshot, PlaybackDiagnostics, PlaybackPipeline, PlaybackState,
    PlayerCommand, PlayerError, PlayerErrorKind, PlayerEvent, PlayerResult, PlayerSnapshot,
    QualitySelection, SeekAudioResumeInfo, SeekBootstrapDiagnosticsSnapshot, SeekCommitInfo,
    SeekProgressBlocker, SeekRequest, SeekTargetFramePresentation, StartedVideoBackend, TrackId,
};

mod audio_runtime;
mod capability_selection;
mod diagnostics_sink;
mod media_lifecycle;
mod render_leases;
mod snapshot_builder;
mod tick;

use self::audio_runtime::SeekAudioGateStatus;
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

/// Read-only срез gate-состояния, нужный только для диагностики active seek-а.
#[derive(Debug, Clone, Copy)]
struct SeekProgressGateSnapshot {
    /// Target frame уже стал текущим non-stale present frame.
    target_frame_presented: bool,

    /// Video gate больше не блокирует commit.
    video_gate_ready: bool,

    /// Typed status audio gate-а с причиной возможного ожидания.
    audio_gate_status: SeekAudioGateStatus,

    /// Сколько video frames уже готовы для post-seek resume.
    ready_video_frames: usize,

    /// Сколько video frames требуется текущей resume policy.
    required_video_frames: usize,
}

/// Решение commit gate policy без потери причины soft fallback-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeekCommitGateDecision {
    /// Gates ещё не готовы, seek transaction остаётся открытым.
    Waiting,

    /// Video и audio gates штатно готовы.
    Ready,

    /// Video gate готов, а audio gate превысил разрешённый soft timeout.
    AudioSoftFallback {
        /// Последний typed status audio gate-а перед fallback commit-ом.
        audio_gate_status: SeekAudioGateStatus,
    },
}

impl SeekCommitGateDecision {
    /// Возвращает `true`, если seek transaction можно закрывать сейчас.
    const fn allows_commit(self) -> bool {
        matches!(self, Self::Ready | Self::AudioSoftFallback { .. })
    }

    /// Возвращает audio blocker, если commit идёт через soft fallback.
    const fn audio_soft_fallback_status(self) -> Option<SeekAudioGateStatus> {
        match self {
            Self::AudioSoftFallback { audio_gate_status } => Some(audio_gate_status),
            Self::Waiting | Self::Ready => None,
        }
    }
}

/// Политика выбора playback position при закрытии final seek-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalSeekCommitPosition {
    /// Requested target становится clock base.
    Target { position: Duration },

    /// Обычный video seek коммитит первый свежий frame текущего seek generation-а.
    PresentedFrame { position: Duration },

    /// Near-EOF fallback коммитит PTS реально показанного fallback frame-а.
    EofFallbackFrame { position: Duration },
}

impl FinalSeekCommitPosition {
    /// Возвращает позицию, которую нужно опубликовать как committed playback position.
    const fn position(self) -> Duration {
        match self {
            Self::Target { position }
            | Self::PresentedFrame { position }
            | Self::EofFallbackFrame { position } => position,
        }
    }

    /// Stable label для structured logs и focused regression tests.
    const fn policy_name(self) -> &'static str {
        match self {
            Self::Target { .. } => "target",
            Self::PresentedFrame { .. } => "presented-frame",
            Self::EofFallbackFrame { .. } => "eof-fallback-frame",
        }
    }
}

/// Сколько первых demux packets после accepted seek попадает в debug trace.
const POST_SEEK_PACKET_TRACE_LIMIT: usize = 8;

/// Решение helper-а: нужно ли писать compact log для очередного post-seek packet-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PostSeekPacketTraceDecision {
    /// Номер packet-а в post-seek последовательности, начиная с 1.
    packet_index: usize,

    /// Это первый video packet, увиденный после accepted seek.
    first_video_packet: bool,
}

/// Минимальное диагностическое состояние одного seek trace-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SeekTraceState {
    /// Generation accepted seek transaction-а, для которого собирается trace.
    active_generation: Option<u64>,

    /// Сколько demux packets увидели после accepted seek.
    observed_post_seek_packets: usize,

    /// Сколько packet logs уже записали для текущего trace-а.
    logged_post_seek_packets: usize,

    /// Был ли уже зафиксирован первый post-seek video packet.
    first_video_packet_seen: bool,

    /// Был ли уже зафиксирован первый decoded frame после seek.
    first_decoded_frame_logged: bool,

    /// Был ли уже зафиксирован первый queued frame после seek.
    first_queued_frame_logged: bool,

    /// Был ли уже зафиксирован первый presented frame после seek.
    first_presented_frame_logged: bool,

    /// PTS первого presented frame текущего seek trace-а.
    first_presented_frame_position: Option<Duration>,

    /// Был ли уже зафиксирован первый TracksChanged marker после seek.
    first_track_list_update_logged: bool,
}

impl SeekTraceState {
    /// Начинает новый trace и забывает одноразовые markers предыдущего seek-а.
    fn begin(&mut self, generation: u64) {
        *self = Self {
            active_generation: Some(generation),
            ..Self::default()
        };
    }

    /// Завершает trace без изменения runtime state seek transaction-а.
    fn clear(&mut self) {
        *self = Self::default();
    }

    /// Учитывает demux packet и возвращает решение о compact logging.
    fn record_post_seek_packet(
        &mut self,
        packet_kind: TrackKind,
    ) -> Option<PostSeekPacketTraceDecision> {
        self.active_generation?;

        self.observed_post_seek_packets = self.observed_post_seek_packets.saturating_add(1);
        let first_video_packet = packet_kind == TrackKind::Video && !self.first_video_packet_seen;
        if first_video_packet {
            self.first_video_packet_seen = true;
        }

        let within_packet_trace_limit =
            self.logged_post_seek_packets < POST_SEEK_PACKET_TRACE_LIMIT;
        if !within_packet_trace_limit && !first_video_packet {
            return None;
        }

        self.logged_post_seek_packets = self.logged_post_seek_packets.saturating_add(1);
        Some(PostSeekPacketTraceDecision {
            packet_index: self.observed_post_seek_packets,
            first_video_packet,
        })
    }

    /// Возвращает `true` только для первого decoded frame текущего seek trace-а.
    fn record_first_decoded_frame(&mut self) -> bool {
        if self.active_generation.is_none() || self.first_decoded_frame_logged {
            return false;
        }

        self.first_decoded_frame_logged = true;
        true
    }

    /// Возвращает `true` только для первого queued frame текущего seek trace-а.
    fn record_first_queued_frame(&mut self) -> bool {
        if self.active_generation.is_none() || self.first_queued_frame_logged {
            return false;
        }

        self.first_queued_frame_logged = true;
        true
    }

    /// Возвращает `true` только для первого presented frame текущего seek trace-а.
    fn record_first_presented_frame(&mut self, frame_pts: Duration) -> bool {
        if self.active_generation.is_none() || self.first_presented_frame_logged {
            return false;
        }

        self.first_presented_frame_logged = true;
        self.first_presented_frame_position = Some(frame_pts);
        true
    }

    /// Возвращает PTS первого presented frame только для текущего generation-а.
    fn first_presented_frame_position_for_generation(&self, generation: u64) -> Option<Duration> {
        (self.active_generation == Some(generation))
            .then_some(self.first_presented_frame_position)
            .flatten()
    }

    /// Возвращает `true` только для первого TracksChanged marker-а текущего seek trace-а.
    fn record_first_track_list_update(&mut self) -> bool {
        if self.active_generation.is_none() || self.first_track_list_update_logged {
            return false;
        }

        self.first_track_list_update_logged = true;
        true
    }
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

    /// Активная операция точного seek commit-а, если player ждёт pre-roll/gates.
    seek_commit: Option<SeekCommitState>,

    /// Одноразовые markers baseline seek diagnostics без влияния на playback.
    seek_trace: SeekTraceState,

    /// Активен ли compatibility scrub без live-preview transaction.
    simple_scrub_active: bool,

    /// Последний request compatibility scrub API без live-preview transaction-а.
    simple_scrub_latest_request: Option<SeekRequest>,

    /// Final seek near EOF завершился свежим frame-ом до requested target.
    seek_eof_fallback_video_position: Option<MediaTime>,
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
        self.draining_after_eof && self.seek_commit.is_none()
    }

    /// Запускает audio output для EOF tail-а, который был накоплен до завершения autoplay preroll.
    pub(crate) fn start_eof_audio_tail_if_needed(&mut self) {
        if !self.draining_after_eof || self.seek_commit.is_some() {
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
                seek_commit_active = self.seek_commit.is_some(),
                seek_eof_fallback_video = self.seek_eof_fallback_video_position.is_some(),
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
        if self.seek_commit.is_some() {
            return Some(EofDrainBlocker::SeekCommit);
        }

        if self.seek_eof_fallback_video_position.is_some() {
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

    /// Проверяет, есть ли активный seek commit для scheduler/gate логики.
    #[must_use]
    pub(crate) const fn has_active_seek_commit(&self) -> bool {
        self.seek_commit.is_some()
    }

    /// Возвращает активный seek commit для scheduler/gate логики.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn seek_commit(&self) -> Option<SeekCommitState> {
        self.seek_commit
    }

    /// Собирает подробный snapshot активного seek-а для throttled stall logs.
    #[must_use]
    pub(crate) fn active_seek_diagnostics(
        &self,
        now: Instant,
        tick_config: &PlayerTickConfig,
    ) -> Option<ActiveSeekDiagnosticsSnapshot> {
        let seek_commit = self.seek_commit?;
        let target_position = seek_commit.target_position.as_duration();
        let queues = self.diagnostic_queue_depths();
        let required_video_frames = self.required_seek_resume_video_ready_frames(
            seek_commit,
            tick_config.effective_seek_resume_video_min_ready_frames(),
        );
        let ready_video_frames = self.seek_ready_video_frame_count(seek_commit);
        let target_frame_presented = self.seek_presented_frame_ready(seek_commit);
        let video_gate_ready = self.seek_video_gate_ready(seek_commit, required_video_frames);
        let audio_gate_status =
            self.seek_audio_gate_status(seek_commit, tick_config.seek_resume_audio_min_buffer_ms);
        let audio_gate_ready = audio_gate_status.is_ready();
        let diagnostics_snapshot = self.diagnostics_snapshot_with_queues(queues);
        let gate_snapshot = SeekProgressGateSnapshot {
            target_frame_presented,
            video_gate_ready,
            audio_gate_status,
            ready_video_frames,
            required_video_frames,
        };
        let blocker = self.seek_progress_blocker(
            tick_config,
            queues,
            gate_snapshot,
            diagnostics_snapshot.seek_bootstrap,
        );

        Some(ActiveSeekDiagnosticsSnapshot {
            kind: "seek",
            generation: seek_commit.generation,
            pipeline_generation: self.pipeline.seek_generation(),
            selected_video_track_id: self.pipeline.selected_video_track_id(),
            selected_audio_track_id: self.pipeline.selected_audio_track_id(),
            age: now.saturating_duration_since(seek_commit.started_at),
            target: target_position,
            actual: seek_commit.actual_position.as_duration(),
            resume_intent: playback_resume_intent_name(seek_commit.resume_intent),
            blocker,
            video_gate_ready,
            audio_gate_ready,
            target_frame_presented,
            ready_video_frames,
            required_video_frames,
            present_frame_pts: self.pipeline.present_video_frame_pts(),
            front_queued_frame_pts: self
                .pipeline
                .front_queued_video_frame()
                .map(|frame| frame.pts),
            demuxing_active: self.is_demuxing_active(),
            draining_after_eof: self.draining_after_eof,
            stale_frame: self.snapshot.timeline.stale_frame,
            stale_generation_discards: diagnostics_snapshot.drops.stale_generation,
            seek_bootstrap: diagnostics_snapshot.seek_bootstrap,
            last_pause_reason: diagnostics_snapshot.pauses.last.map(|pause| pause.reason),
            queues,
        })
    }

    /// Пишет compact marker для первых demux packets активного seek-а.
    pub(crate) fn note_demux_packet_for_seek_trace(
        &mut self,
        packet: &media_core::Packet,
        packet_generation: u64,
    ) {
        let Some(seek_commit) = self.seek_commit else {
            return;
        };

        let Some(trace_decision) = self.seek_trace.record_post_seek_packet(packet.kind) else {
            return;
        };

        if trace_decision.first_video_packet {
            debug!(
                kind = "seek",
                target_ms = seek_commit.target_position.as_duration().as_millis(),
                actual_ms = seek_commit.actual_position.as_duration().as_millis(),
                active_seek_generation = seek_commit.generation,
                packet_generation,
                pipeline_generation = self.pipeline.seek_generation(),
                selected_video_track_id = ?self.pipeline.selected_video_track_id(),
                selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
                packet_index = trace_decision.packet_index,
                packet_track_id = %packet.track_id,
                packet_pts_ms = packet.pts.as_millis(),
                packet_keyframe = ?packet.keyframe,
                "First post-seek video packet observed"
            );
            return;
        }

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            active_seek_generation = seek_commit.generation,
            packet_generation,
            pipeline_generation = self.pipeline.seek_generation(),
            selected_video_track_id = ?self.pipeline.selected_video_track_id(),
            selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
            packet_index = trace_decision.packet_index,
            packet_track_id = %packet.track_id,
            packet_kind = ?packet.kind,
            packet_pts_ms = packet.pts.as_millis(),
            packet_keyframe = ?packet.keyframe,
            "Post-seek demux packet observed"
        );
    }

    /// Пишет marker первого decoded frame-а после accepted seek.
    pub(crate) fn note_decoded_video_frame_for_seek_trace(
        &mut self,
        frame_pts: Duration,
        frame_generation: u64,
    ) {
        let Some(seek_commit) = self.seek_commit else {
            return;
        };

        if !self.seek_trace.record_first_decoded_frame() {
            return;
        }

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            active_seek_generation = seek_commit.generation,
            pipeline_generation = self.pipeline.seek_generation(),
            frame_pts_ms = frame_pts.as_millis(),
            frame_generation,
            "First post-seek decoded frame observed"
        );
    }

    /// Пишет marker первого decoded frame-а, который дошёл до presentation queue.
    pub(crate) fn note_queued_video_frame_for_seek_trace(
        &mut self,
        frame_pts: Duration,
        frame_generation: u64,
    ) {
        let Some(seek_commit) = self.seek_commit else {
            return;
        };

        if !self.seek_trace.record_first_queued_frame() {
            return;
        }

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            active_seek_generation = seek_commit.generation,
            pipeline_generation = self.pipeline.seek_generation(),
            frame_pts_ms = frame_pts.as_millis(),
            frame_generation,
            present_queue_depth = self.pipeline.video_present_queue_len(),
            "First post-seek queued frame observed"
        );
    }

    /// Возвращает accepted seek clock base как временный clock, пока audio clock недоступен.
    ///
    /// Обычный final seek может стартовать с demux-safe actual позиции до target-а,
    /// поэтому scheduler должен сравнивать post-seek frames с той же базой, которую
    /// session применит к media/audio clocks после accepted demux point-а.
    #[must_use]
    pub(crate) fn seek_presentation_clock_override(&self) -> Option<Duration> {
        if self.pipeline.has_audio_clock() {
            return None;
        }

        self.seek_commit
            .map(|seek_commit| self.accepted_seek_clock_base(seek_commit))
    }

    /// Выбирает clock base, от которого audio/video должны идти во время accepted seek-а.
    fn accepted_seek_clock_base(&self, seek_commit: SeekCommitState) -> Duration {
        if self.final_seek_uses_decode_safe_clock_anchor(seek_commit) {
            return seek_commit.actual_position.as_duration();
        }

        seek_commit.target_position.as_duration()
    }

    /// Проверяет, что final seek может стартовать playback от demux-safe actual frame-а.
    fn final_seek_uses_decode_safe_clock_anchor(&self, _seek_commit: SeekCommitState) -> bool {
        self.pipeline.has_selected_video_track()
    }

    /// Перепривязывает clocks после accepted seek, когда demuxer уже сообщил actual point.
    fn reanchor_clocks_after_seek_accept(&mut self, seek_commit: SeekCommitState) {
        let clock_base = self.accepted_seek_clock_base(seek_commit);
        self.pipeline
            .reanchor_media_clock_for_seek(clock_base, Instant::now());

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            clock_base_ms = clock_base.as_millis(),
            generation = seek_commit.generation,
            pipeline_generation = self.pipeline.seek_generation(),
            "Seek media clock перепривязан после accepted demux point"
        );
    }

    /// Проверяет, нужно ли выбросить decoded frame как pre-roll до seek target.
    ///
    /// Final seek теперь всегда latency-first для video: первый свежий кадр текущего
    /// generation-а может попасть в scheduler и стать playback position.
    #[must_use]
    pub(crate) fn should_drop_decoded_frame_for_seek(&self, _frame_pts: Duration) -> bool {
        false
    }

    /// Возвращает target активного final seek-а, если такой transition сейчас идёт.
    #[must_use]
    pub(crate) fn active_final_seek_target(&self) -> Option<Duration> {
        self.seek_commit
            .map(|seek_commit| seek_commit.target_position.as_duration())
    }

    /// Проверяет, может ли active seek обойти обычное A/V scheduler window для queued frame-а.
    ///
    /// Final seek показывает первый свежий frame текущего generation-а, чтобы click seek
    /// не ждал exact target frame после decode-safe demux preroll.
    #[must_use]
    pub(crate) fn active_seek_frame_ready_for_scheduler(&self, frame_pts: Duration) -> bool {
        let Some(seek_commit) = self.seek_commit else {
            return false;
        };

        self.final_seek_frame_ready_for_scheduler(seek_commit, frame_pts)
    }

    /// Проверяет, может ли final seek показать queued frame в обход обычного scheduler window.
    fn final_seek_frame_ready_for_scheduler(
        &self,
        seek_commit: SeekCommitState,
        _frame_pts: Duration,
    ) -> bool {
        !self.final_seek_visible_frame_ready(seek_commit)
    }

    /// Проверяет, можно ли сохранить pre-target frame как EOF fallback текущего seek-а.
    #[must_use]
    pub(crate) fn can_keep_seek_preroll_fallback(&self, frame_pts: Duration) -> bool {
        self.active_final_seek_target()
            .is_some_and(|target_position| frame_pts < target_position)
    }

    /// Заменяет EOF fallback frame и возвращает старый frame для явного release/drop учёта.
    pub(crate) fn replace_seek_preroll_fallback_frame(
        &mut self,
        frame: video_core::DecodedFrame,
    ) -> Option<video_core::DecodedFrame> {
        self.pipeline
            .replace_seek_preroll_fallback_video_frame(frame)
    }

    /// Забирает EOF fallback frame, если scheduler решил, что target frame уже не придёт.
    pub(crate) fn take_seek_preroll_fallback_frame(&mut self) -> Option<video_core::DecodedFrame> {
        self.pipeline.take_seek_preroll_fallback_video_frame()
    }

    /// Освобождает EOF fallback frame при новом seek/reset/точном target frame-е.
    pub(crate) fn clear_seek_preroll_fallback_frame(&mut self) {
        if let Some(frame) = self.pipeline.clear_seek_preroll_fallback_video_frame() {
            self.release_video_texture(frame.texture_handle);
        }
    }

    /// Отмечает, что final seek near EOF показал свежий fallback frame текущего transition-а.
    pub(crate) fn note_presented_seek_eof_fallback_frame(&mut self, frame_pts: Duration) {
        let Some(seek_commit) = self.seek_commit else {
            return;
        };

        let first_post_seek_presented_frame =
            self.seek_trace.record_first_presented_frame(frame_pts);
        let present_frame_generation = self
            .pipeline
            .present_video_frame()
            .map(|frame| frame.generation);
        if first_post_seek_presented_frame {
            debug!(
                kind = "seek",
                target_ms = seek_commit.target_position.as_duration().as_millis(),
                actual_ms = seek_commit.actual_position.as_duration().as_millis(),
                active_seek_generation = seek_commit.generation,
                pipeline_generation = self.pipeline.seek_generation(),
                frame_pts_ms = frame_pts.as_millis(),
                frame_generation = ?present_frame_generation,
                eof_fallback = true,
                stale_frame = self.snapshot.timeline.stale_frame,
                "First post-seek presented frame observed"
            );
        }

        self.seek_eof_fallback_video_position = Some(MediaTime::from_duration(frame_pts));
        self.snapshot.timeline.stale_frame = false;
    }

    /// Отмечает, что свежий video frame текущего seek-а уже стал текущим кадром presentation.
    pub(crate) fn note_presented_frame_for_seek(&mut self, frame_pts: Duration) {
        let Some(seek_commit) = self.seek_commit else {
            return;
        };

        let first_post_seek_presented_frame =
            self.seek_trace.record_first_presented_frame(frame_pts);
        let present_frame_generation = self
            .pipeline
            .present_video_frame()
            .map(|frame| frame.generation);
        if first_post_seek_presented_frame {
            debug!(
                kind = "seek",
                target_ms = seek_commit.target_position.as_duration().as_millis(),
                actual_ms = seek_commit.actual_position.as_duration().as_millis(),
                active_seek_generation = seek_commit.generation,
                pipeline_generation = self.pipeline.seek_generation(),
                frame_pts_ms = frame_pts.as_millis(),
                frame_generation = ?present_frame_generation,
                stale_frame = self.snapshot.timeline.stale_frame,
                "First post-seek presented frame observed"
            );
        }

        trace!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            frame_pts_ms = frame_pts.as_millis(),
            generation = seek_commit.generation,
            "Seek transaction увидел presented frame"
        );

        self.seek_eof_fallback_video_position = None;
        self.snapshot.timeline.stale_frame = false;
        if first_post_seek_presented_frame {
            self.pending_events
                .push(PlayerEvent::SeekTargetFramePresented(
                    SeekTargetFramePresentation {
                        target_position: seek_commit.target_position.as_duration(),
                        frame_pts,
                    },
                ));
        }
    }

    /// Завершает seek commit, когда video/audio gates готовы, или применяет timeout.
    ///
    /// Timeout защищает только реально ожидающие transition-ы: если gates уже
    /// готовы на этом tick-е, commit должен победить даже после истечения budget.
    #[cfg(test)]
    fn finish_seek_commit_if_ready_for_tests(
        &mut self,
        now: Instant,
        commit_timeout: Duration,
        resume_audio_min_buffer_ms: f64,
        resume_audio_gate_timeout: Duration,
        resume_video_min_ready_frames: usize,
    ) {
        let tick_config = PlayerTickConfig {
            seek_commit_timeout: commit_timeout,
            seek_resume_audio_min_buffer_ms: resume_audio_min_buffer_ms,
            seek_resume_audio_gate_timeout: resume_audio_gate_timeout,
            seek_resume_video_min_ready_frames: resume_video_min_ready_frames,
            ..PlayerTickConfig::default()
        };

        self.finish_seek_commit_if_ready(now, &tick_config);
    }

    /// Завершает seek commit по фактическому tick config, чтобы diagnostics и gates
    /// видели один и тот же набор scheduler/backpressure knobs.
    pub(crate) fn finish_seek_commit_if_ready(
        &mut self,
        now: Instant,
        tick_config: &PlayerTickConfig,
    ) {
        let Some(seek_commit) = self.seek_commit else {
            return;
        };

        let resume_video_min_ready_frames =
            tick_config.effective_seek_resume_video_min_ready_frames();
        let gate_decision = self.seek_commit_gate_decision(
            seek_commit,
            now,
            tick_config.seek_resume_audio_min_buffer_ms,
            tick_config.seek_resume_audio_gate_timeout,
            resume_video_min_ready_frames,
        );
        if gate_decision.allows_commit() {
            if let Some(audio_gate_status) = gate_decision.audio_soft_fallback_status() {
                warn!(
                    target_ms = seek_commit.target_position.as_duration().as_millis(),
                    actual_ms = seek_commit.actual_position.as_duration().as_millis(),
                    generation = seek_commit.generation,
                    audio_gate_status = ?audio_gate_status,
                    audio_gate_timeout_ms = tick_config.seek_resume_audio_gate_timeout.as_millis(),
                    "Final seek commit продолжен через audio gate soft fallback"
                );
                self.record_seek_audio_soft_fallback(
                    seek_commit,
                    audio_gate_status,
                    tick_config.seek_resume_audio_gate_timeout,
                );
            }

            self.complete_seek_commit(seek_commit);
            return;
        }

        if now.saturating_duration_since(seek_commit.started_at) < tick_config.seek_commit_timeout {
            return;
        }

        let timeout_blocker = self.seek_timeout_blocker_from_active_diagnostics(now, tick_config);
        self.fail_seek_commit_on_timeout(seek_commit, timeout_blocker);
    }

    /// Публикует recoverable диагностику, когда final seek отпущен без готового audio gate-а.
    fn record_seek_audio_soft_fallback(
        &mut self,
        seek_commit: SeekCommitState,
        audio_gate_status: SeekAudioGateStatus,
        resume_audio_gate_timeout: Duration,
    ) {
        let blocker = audio_gate_status
            .blocker()
            .unwrap_or(SeekProgressBlocker::WaitingForAudioPreroll);
        let error = PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!(
                "Final seek resumed without ready audio after {} ms: target={} ms, blocker={}",
                resume_audio_gate_timeout.as_millis(),
                seek_commit.target_position.as_duration().as_millis(),
                blocker.metric_name()
            ),
        );

        self.record_recoverable_error(error);
    }

    /// Проверяет video/audio gates и возвращает причину, если commit разрешён soft fallback-ом.
    fn seek_commit_gate_decision(
        &self,
        seek_commit: SeekCommitState,
        now: Instant,
        resume_audio_min_buffer_ms: f64,
        resume_audio_gate_timeout: Duration,
        resume_video_min_ready_frames: usize,
    ) -> SeekCommitGateDecision {
        if !self.seek_video_gate_ready(seek_commit, resume_video_min_ready_frames) {
            return SeekCommitGateDecision::Waiting;
        }

        let audio_gate_status =
            self.seek_audio_gate_status(seek_commit, resume_audio_min_buffer_ms);
        if audio_gate_status.is_ready() {
            return SeekCommitGateDecision::Ready;
        }

        if self.seek_audio_gate_soft_fallback_ready(
            seek_commit,
            now,
            audio_gate_status,
            resume_audio_gate_timeout,
        ) {
            return SeekCommitGateDecision::AudioSoftFallback { audio_gate_status };
        }

        SeekCommitGateDecision::Waiting
    }

    /// Берёт текущий blocker из active seek diagnostics до очистки timeout-состояния.
    fn seek_timeout_blocker_from_active_diagnostics(
        &self,
        now: Instant,
        tick_config: &PlayerTickConfig,
    ) -> SeekProgressBlocker {
        self.active_seek_diagnostics(now, tick_config)
            .map(|diagnostics| diagnostics.blocker)
            .unwrap_or(SeekProgressBlocker::Unknown)
    }

    /// Возвращает `true`, если audio gate можно отпустить без бесконечного удержания seek-а.
    fn seek_audio_gate_soft_fallback_ready(
        &self,
        seek_commit: SeekCommitState,
        now: Instant,
        audio_gate_status: SeekAudioGateStatus,
        resume_audio_gate_timeout: Duration,
    ) -> bool {
        if seek_commit.resume_intent != PlaybackResumeIntent::Play {
            return false;
        }

        if !self.pipeline.has_selected_audio_track() {
            return false;
        }

        if !self.pipeline.has_selected_video_track() {
            return false;
        }

        if !audio_gate_status.can_soft_fallback() {
            return false;
        }

        now.saturating_duration_since(seek_commit.started_at) >= resume_audio_gate_timeout
    }

    /// Video gate готов, когда текущая seek policy увидела нужный frame.
    fn seek_video_gate_ready(
        &self,
        seek_commit: SeekCommitState,
        resume_video_min_ready_frames: usize,
    ) -> bool {
        if !self.pipeline.has_selected_video_track() {
            return true;
        }

        if self.final_seek_visible_frame_ready(seek_commit) {
            return true;
        }

        let target_position = seek_commit.target_position.as_duration();
        let target_frame_presented = self.pipeline.present_video_frame_covers(target_position)
            && !self.snapshot.timeline.stale_frame;
        let eof_fallback_presented =
            self.seek_eof_fallback_video_ready(seek_commit, target_position);

        if eof_fallback_presented {
            return true;
        }

        if !target_frame_presented {
            return false;
        }

        let required_ready_frames = self
            .required_seek_resume_video_ready_frames(seek_commit, resume_video_min_ready_frames);

        self.seek_ready_video_frame_count(seek_commit) >= required_ready_frames
    }

    /// Проверяет, что текущая seek policy уже получила non-stale present frame.
    fn seek_presented_frame_ready(&self, seek_commit: SeekCommitState) -> bool {
        if self.final_seek_visible_frame_ready(seek_commit) {
            return true;
        }

        let target_position = seek_commit.target_position.as_duration();
        self.pipeline.present_video_frame_covers(target_position)
            && !self.snapshot.timeline.stale_frame
    }

    /// Проверяет, что обычный final seek уже показал свежий frame текущего generation-а.
    fn final_seek_visible_frame_ready(&self, seek_commit: SeekCommitState) -> bool {
        if self.snapshot.timeline.stale_frame || !self.pipeline.has_selected_video_track() {
            return false;
        }

        self.pipeline
            .present_video_frame_pts_for_generation(seek_commit.generation)
            .is_some()
    }

    /// Классифицирует текущую причину, по которой active seek ещё не закрыл gates.
    fn seek_progress_blocker(
        &self,
        tick_config: &PlayerTickConfig,
        queues: PipelineQueueDepthSnapshot,
        gate_snapshot: SeekProgressGateSnapshot,
        seek_bootstrap: SeekBootstrapDiagnosticsSnapshot,
    ) -> SeekProgressBlocker {
        let audio_gate_ready = gate_snapshot.audio_gate_status.is_ready();
        if gate_snapshot.video_gate_ready && audio_gate_ready {
            return SeekProgressBlocker::ReadyToCommit;
        }

        if let Some(audio_blocker @ SeekProgressBlocker::WaitingForAudioClear) =
            gate_snapshot.audio_gate_status.blocker()
        {
            return audio_blocker;
        }

        if !self.pipeline.has_selected_video_track() {
            return gate_snapshot
                .audio_gate_status
                .blocker()
                .unwrap_or(SeekProgressBlocker::WaitingForAudioPreroll);
        }

        if let Some(texture_slots) = queues.texture_slots
            && texture_slots.available_slots() <= tick_config.min_texture_slots_available_for_decode
        {
            if texture_slots.waiting_gpu_completion > 0
                || texture_slots.waiting_decoder_reuse > 0
                || queues.active_render_leases > 0
                || queues.deferred_render_releases > 0
            {
                return SeekProgressBlocker::WaitingForGpuRelease;
            }

            return SeekProgressBlocker::WaitingForFreeSurface;
        }

        if !gate_snapshot.target_frame_presented {
            return self.video_target_frame_blocker(queues, seek_bootstrap);
        }

        if gate_snapshot.ready_video_frames < gate_snapshot.required_video_frames {
            return SeekProgressBlocker::WaitingForVideoResumePreroll;
        }

        if !audio_gate_ready {
            return gate_snapshot
                .audio_gate_status
                .blocker()
                .unwrap_or(SeekProgressBlocker::WaitingForAudioPreroll);
        }

        SeekProgressBlocker::Unknown
    }

    /// Уточняет blocker для состояния, где seek ещё не показал target frame.
    fn video_target_frame_blocker(
        &self,
        queues: PipelineQueueDepthSnapshot,
        seek_bootstrap: SeekBootstrapDiagnosticsSnapshot,
    ) -> SeekProgressBlocker {
        let waiting_for_decode_start_after_drops = self.pipeline.video_decoder_needs_keyframe()
            && seek_bootstrap.dropped_until_keyframe > 0;

        if waiting_for_decode_start_after_drops {
            return SeekProgressBlocker::WaitingForPostFlushKeyframe;
        }

        if queues.decoder_send_queue_depth > 0 || queues.decoder_in_flight_packets > 0 {
            return SeekProgressBlocker::WaitingForDecoderOutput;
        }

        if queues.pending_video_packets > 0 {
            return SeekProgressBlocker::WaitingForDecoderInput;
        }

        if self
            .pipeline
            .front_queued_video_frame()
            .is_some_and(|frame| self.active_seek_frame_ready_for_scheduler(frame.pts))
        {
            return SeekProgressBlocker::ReadyForScheduler;
        }

        if self.pipeline.has_present_video_frame() || !self.pipeline.video_present_queue_is_empty()
        {
            return SeekProgressBlocker::WaitingForScheduler;
        }

        if self.is_demuxing_active() && !self.draining_after_eof {
            return SeekProgressBlocker::WaitingForDemux;
        }

        SeekProgressBlocker::WaitingForVideoTargetFrame
    }

    /// Возвращает требуемый video preroll для конкретного seek transaction-а.
    fn required_seek_resume_video_ready_frames(
        &self,
        seek_commit: SeekCommitState,
        resume_video_min_ready_frames: usize,
    ) -> usize {
        match seek_commit.resume_intent {
            PlaybackResumeIntent::Play if self.pipeline.has_selected_audio_track() => 1,
            PlaybackResumeIntent::Play => resume_video_min_ready_frames.max(1),
            _ => 1,
        }
    }

    /// Считает current frame и уже декодированные future frames для seek resume.
    fn seek_ready_video_frame_count(&self, seek_commit: SeekCommitState) -> usize {
        let target_position = seek_commit.target_position.as_duration();
        let current_frame_ready = self.seek_presented_frame_ready(seek_commit);
        let queued_ready_frames = self
            .pipeline
            .queued_video_frames()
            .filter(|frame| frame.pts >= target_position)
            .count();

        usize::from(current_frame_ready) + queued_ready_frames
    }

    /// EOF fallback готов только если показан свежий frame текущего final seek transition-а.
    fn seek_eof_fallback_video_ready(
        &self,
        _seek_commit: SeekCommitState,
        target_position: Duration,
    ) -> bool {
        if !self.draining_after_eof {
            return false;
        }

        let Some(fallback_position) = self.seek_eof_fallback_video_position else {
            return false;
        };
        let fallback_position = fallback_position.as_duration();
        if fallback_position >= target_position || self.snapshot.timeline.stale_frame {
            return false;
        }

        self.pipeline.present_video_frame_matches(fallback_position)
    }

    /// Audio gate готов после clear ack, runtime decoder/output и минимального preroll.
    ///
    /// Paused final seek не включает audio stream, поэтому после clear ack
    /// он не ждёт decoder/output. Final resume в `Playing` ждёт selected audio path:
    /// unsupported/disabled audio должен быть явно снят с selection policy-слоем.
    #[cfg(test)]
    fn seek_audio_gate_ready(
        &self,
        seek_commit: SeekCommitState,
        resume_audio_min_buffer_ms: f64,
    ) -> bool {
        self.seek_audio_gate_status(seek_commit, resume_audio_min_buffer_ms)
            .is_ready()
    }

    /// Успешно закрывает seek transaction и применяет сохранённый resume intent.
    fn complete_seek_commit(&mut self, seek_commit: SeekCommitState) {
        self.complete_final_seek_commit(seek_commit);
    }

    /// Выбирает committed position без смешивания requested target и EOF fallback frame-а.
    fn final_seek_commit_position(&self, seek_commit: SeekCommitState) -> FinalSeekCommitPosition {
        if let Some(position) = self.final_seek_eof_fallback_commit_position(seek_commit) {
            return FinalSeekCommitPosition::EofFallbackFrame { position };
        }

        if let Some(position) = self.final_seek_presented_frame_commit_position(seek_commit) {
            return FinalSeekCommitPosition::PresentedFrame { position };
        }

        FinalSeekCommitPosition::Target {
            position: seek_commit.target_position.as_duration(),
        }
    }

    /// Обычный video seek фиксирует первый реально показанный pre-target frame.
    fn final_seek_presented_frame_commit_position(
        &self,
        seek_commit: SeekCommitState,
    ) -> Option<Duration> {
        if self.snapshot.timeline.stale_frame {
            return None;
        }

        if !self.pipeline.has_selected_video_track() {
            return None;
        }

        let frame_position = self
            .seek_trace
            .first_presented_frame_position_for_generation(seek_commit.generation)
            .or_else(|| {
                self.pipeline
                    .present_video_frame_pts_for_generation(seek_commit.generation)
            })?;
        let target_position = seek_commit.target_position.as_duration();

        (frame_position < target_position).then_some(frame_position)
    }

    /// EOF fallback считается committed position только если этот frame реально сейчас показан.
    fn final_seek_eof_fallback_commit_position(
        &self,
        seek_commit: SeekCommitState,
    ) -> Option<Duration> {
        if !self.draining_after_eof {
            return None;
        }

        if !self.pipeline.has_selected_video_track() || self.snapshot.timeline.stale_frame {
            return None;
        }

        let fallback_position = self.seek_eof_fallback_video_position?.as_duration();
        if fallback_position >= seek_commit.target_position.as_duration() {
            return None;
        }

        self.pipeline
            .present_video_frame_matches(fallback_position)
            .then_some(fallback_position)
    }

    /// Закрывает финальный seek и публикует новую playback позицию.
    fn complete_final_seek_commit(&mut self, seek_commit: SeekCommitState) {
        let commit_position = self.final_seek_commit_position(seek_commit);
        let playback_position = commit_position.position();

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            committed_ms = playback_position.as_millis(),
            commit_position_policy = commit_position.policy_name(),
            generation = seek_commit.generation,
            pipeline_generation = self.pipeline.seek_generation(),
            resume_intent = ?seek_commit.resume_intent,
            "Final seek commit завершён"
        );
        self.seek_commit = None;
        self.seek_trace.clear();
        self.simple_scrub_active = false;
        self.simple_scrub_latest_request = None;
        self.seek_eof_fallback_video_position = None;
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.stale_frame = false;
        self.pipeline.set_media_clock_base(playback_position);
        self.pipeline.clear_monotonic_media_clock();
        self.publish_position_changed(playback_position);
        self.pending_events
            .push(PlayerEvent::SeekCommitted(SeekCommitInfo {
                target_position: seek_commit.target_position.as_duration(),
                actual_position: seek_commit.actual_position.as_duration(),
                resume_intent: seek_commit.resume_intent,
            }));

        match seek_commit.resume_intent {
            PlaybackResumeIntent::Pause => {
                self.pause_audio_output_for_seek();
                self.set_playback_state(PlaybackState::Paused);
            }
            PlaybackResumeIntent::Play => {
                self.resume_audio_output_after_seek(playback_position);
                let observed_at = Instant::now();
                let audio_now = self.audio_clock_now();
                self.pipeline
                    .reset_audio_clock_sample(audio_now, observed_at);
                self.set_playback_state(PlaybackState::Playing);
                self.anchor_monotonic_media_clock_if_needed(observed_at);
            }
        }

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            committed_ms = playback_position.as_millis(),
            commit_position_policy = commit_position.policy_name(),
            generation = seek_commit.generation,
            pipeline_generation = self.pipeline.seek_generation(),
            resume_intent = ?seek_commit.resume_intent,
            playback_state = ?self.snapshot.playback_state,
            "Final seek resume intent applied"
        );
    }

    /// Запускает audio output после seek и различает success/error/absent output.
    fn resume_audio_output_after_seek(&mut self, target_position: Duration) {
        let Some(play_result) = self.pipeline.play_audio_output() else {
            return;
        };

        match play_result {
            Ok(()) => {
                self.pending_events
                    .push(PlayerEvent::AudioResumedAfterSeek(SeekAudioResumeInfo {
                        target_position,
                    }));
            }
            Err(error) => {
                warn!(error = %error, "Не удалось запустить audio после seek");
                let player_error = PlayerError::new(
                    PlayerErrorKind::AudioDeviceUnavailable,
                    format!("Audio play after seek error: {error}"),
                );
                self.record_recoverable_error(player_error);
            }
        }
    }

    /// Прерывает seek transaction по timeout как recoverable error и оставляет media paused.
    fn fail_seek_commit_on_timeout(
        &mut self,
        seek_commit: SeekCommitState,
        timeout_blocker: SeekProgressBlocker,
    ) {
        self.fail_final_seek_commit_on_timeout(seek_commit, timeout_blocker);
    }

    /// Прерывает финальный seek transaction по timeout как recoverable error.
    fn fail_final_seek_commit_on_timeout(
        &mut self,
        seek_commit: SeekCommitState,
        timeout_blocker: SeekProgressBlocker,
    ) {
        self.seek_commit = None;
        self.seek_trace.clear();
        self.simple_scrub_active = false;
        self.simple_scrub_latest_request = None;
        self.seek_eof_fallback_video_position = None;
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        // После timeout старый present frame остаётся на экране, но уже не принадлежит
        // закрытому final seek-у. Поэтому fresh можно считать только frame, который
        // действительно покрывает target завершённой transaction.
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame()
            && !self.present_frame_covers_target(seek_commit.target_position.as_duration());
        self.pause_audio_output_for_seek();
        self.set_playback_state(PlaybackState::Paused);

        warn!(
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            generation = seek_commit.generation,
            blocker = timeout_blocker.metric_name(),
            blocker_kind = ?timeout_blocker,
            "Final seek commit остановлен по timeout"
        );

        let error = PlayerError::new(
            PlayerErrorKind::SeekTimeout,
            format!(
                "Seek commit timeout after target={} ms, actual demux={} ms, blocker={}",
                seek_commit.target_position.as_duration().as_millis(),
                seek_commit.actual_position.as_duration().as_millis(),
                timeout_blocker.metric_name()
            ),
        );
        self.record_recoverable_error(error);
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
        if let Some(seek_commit) = self.seek_commit.as_mut() {
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
        if let Some(seek_commit) = self.seek_commit.as_mut() {
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

    /// Запускает настоящий seek transaction через текущий demuxer.
    fn seek(&mut self, request: SeekRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        let target_position = self.resolve_seek_target(request);
        self.pending_events
            .push(PlayerEvent::SeekRequested(request));
        self.simple_scrub_active = false;
        self.simple_scrub_latest_request = None;
        self.snapshot.timeline.scrubbing = false;

        if !self.pipeline.has_demuxer() {
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Seek невозможен: media pipeline ещё не открыт",
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        let resume_intent = PlaybackResumeIntent::from_playback_state(self.playback_state());
        self.start_seek_transaction(target_position, request.mode, resume_intent)
    }

    /// Начинает compatibility scrub на уровне контракта без запуска demux seek.
    fn begin_scrub(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.simple_scrub_active = true;
        self.simple_scrub_latest_request = None;
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.stale_frame = false;
        self.snapshot.timeline.target_position = Some(self.snapshot.timeline.current_position);
        Ok(())
    }

    /// Запоминает последнюю цель scrub без изменения текущей playback позиции.
    fn update_scrub(&mut self, request: SeekRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.simple_scrub_active = true;
        self.store_simple_scrub_request(request);
        Ok(())
    }

    /// Сохраняет preview request как latest target без запуска demux preview-mode seek-а.
    fn preview_scrub(&mut self, request: SeekRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.simple_scrub_active = true;
        self.store_simple_scrub_request(request);
        Ok(())
    }

    /// Запоминает target compatibility scrub API только для последующего final seek.
    fn store_simple_scrub_request(&mut self, request: SeekRequest) {
        let target_position = self.resolve_seek_target(request);
        self.simple_scrub_latest_request = Some(request);
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.target_position = Some(target_position);
        if !self.snapshot.timeline.seeking {
            self.snapshot.timeline.stale_frame = false;
        }
    }

    /// Завершает compatibility scrub и превращает latest target в обычный final seek.
    fn end_scrub(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        if !self.simple_scrub_active {
            self.finish_simple_scrub_without_seek();
            return Ok(());
        }

        let latest_request = self.simple_scrub_latest_request.take();
        self.simple_scrub_active = false;
        self.finish_simple_scrub_without_seek();
        if let Some(request) = latest_request {
            self.seek(request)?;
        }
        Ok(())
    }

    /// Сбрасывает только lightweight scrub-флаги, не трогая текущий active seek.
    fn finish_simple_scrub_without_seek(&mut self) {
        self.simple_scrub_active = false;
        self.simple_scrub_latest_request = None;
        self.snapshot.timeline.scrubbing = false;
        if !self.snapshot.timeline.seeking {
            self.snapshot.timeline.target_position = None;
            self.snapshot.timeline.stale_frame = false;
        }
    }

    /// Проверяет, что текущий video frame уже соответствует target; audio-only media проходит.
    fn present_frame_covers_target(&self, target_position: Duration) -> bool {
        if !self.pipeline.has_selected_video_track() {
            return true;
        }

        self.pipeline.present_video_frame_covers(target_position)
    }

    /// Сбрасывает video decoder перед seek и делает flush явной fail-fast границей.
    fn reset_video_decoder_for_seek(&self) -> Result<(), PlayerError> {
        self.pipeline
            .flush_video_decoder_thread()
            .map_err(player_error_from_decoder_flush_error)
    }

    /// Завершает seek transaction до demux seek, если decoder не подтвердил flush.
    fn fail_seek_transaction_on_decoder_flush(&mut self, error: PlayerError) {
        self.seek_commit = None;
        self.seek_trace.clear();
        self.seek_eof_fallback_video_position = None;
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.stale_frame = true;
        self.snapshot.timeline.target_position = None;
        self.simple_scrub_active = false;
        self.simple_scrub_latest_request = None;
        self.snapshot.timeline.scrubbing = false;
        self.set_playback_state(PlaybackState::Paused);
        self.record_recoverable_error(error);
    }

    /// Выполняет синхронную часть seek transaction и оставляет commit gates на tick.
    fn start_seek_transaction(
        &mut self,
        target_position: MediaTime,
        seek_mode: crate::SeekMode,
        resume_intent: PlaybackResumeIntent,
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

        if !self.pipeline.has_demuxer() {
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Seek невозможен: media pipeline ещё не открыт",
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        let target_duration = target_position.as_duration();
        let demux_seek_request = match demux_seek_request_for_transaction(
            self.pipeline.has_selected_video_track(),
            target_duration,
            seek_mode,
        ) {
            Ok(request) => request,
            Err(error) => {
                self.record_recoverable_error(player_error_from_seek_demux_request_error(error));
                return Ok(());
            }
        };

        self.pause_audio_output_for_seek();
        if let Err(error) = self.reset_video_decoder_for_seek() {
            self.fail_seek_transaction_on_decoder_flush(error);
            return Ok(());
        }

        self.set_playback_state(PlaybackState::Seeking);
        let generation = self.pipeline.begin_seek_generation();
        let has_video = self.pipeline.has_selected_video_track();

        self.pipeline.clear_pending_packets_for_seek();
        self.pipeline.reset_decoder_state_for_seek(has_video);
        if has_video {
            self.record_video_decoder_bootstrap_started();
        }
        self.seek_eof_fallback_video_position = None;
        self.clear_seek_preroll_fallback_frame();
        self.clear_queued_video_frames();
        self.pipeline.reset_clocks_for_seek(target_duration);
        self.snapshot.timeline.target_position = Some(target_position);
        self.snapshot.timeline.seeking = true;
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame();

        if let Some(Err(error)) = self.pipeline.reset_audio_decoder() {
            let player_error = PlayerError::new(
                PlayerErrorKind::RuntimeError,
                format!("Audio decoder reset failed during seek: {error}"),
            );
            self.record_recoverable_error(player_error);
        }

        if let Some(clear_result) = self.pipeline.clear_audio_output_for_seek(generation) {
            match clear_result {
                Ok(ack_generation) => {
                    self.pipeline.mark_audio_buffer_clear_ack(ack_generation);
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
            self.pipeline.mark_audio_buffer_clear_ack(generation);
            self.pipeline.reset_audio_clock();
        }

        let seek_result = {
            debug!(
                kind = "seek",
                target_ms = target_duration.as_millis(),
                demux_mode = ?demux_seek_request.mode,
                generation,
                selected_video_track_id = ?self.pipeline.selected_video_track_id(),
                selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
                "Starting demux seek transaction"
            );
            let Some(seek_result) = self.pipeline.seek_demuxer(demux_seek_request) else {
                self.seek_trace.clear();
                return Ok(());
            };
            seek_result
        };

        match seek_result {
            Ok(result) => {
                debug!(
                    kind = "seek",
                    target_ms = target_duration.as_millis(),
                    actual_ms = result.actual_position.as_duration().as_millis(),
                    actual_track_timestamp = ?result.actual_track_timestamp,
                    demux_mode = ?demux_seek_request.mode,
                    generation,
                    pipeline_generation = self.pipeline.seek_generation(),
                    selected_video_track_id = ?self.pipeline.selected_video_track_id(),
                    selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
                    "Demux seek transaction accepted"
                );
                self.seek_trace.begin(generation);
                let seek_commit = SeekCommitState {
                    generation,
                    target_position,
                    actual_position: result.actual_position,
                    started_at: Instant::now(),
                    resume_intent,
                };
                self.reanchor_clocks_after_seek_accept(seek_commit);
                self.seek_commit = Some(seek_commit);
                Ok(())
            }
            Err(error) => {
                self.seek_commit = None;
                self.seek_trace.clear();
                self.simple_scrub_active = false;
                self.simple_scrub_latest_request = None;
                self.snapshot.timeline.scrubbing = false;
                self.snapshot.timeline.seeking = false;
                self.snapshot.timeline.stale_frame = false;
                self.set_playback_state(PlaybackState::Paused);
                self.snapshot.timeline.target_position = None;
                let player_error = player_error_from_demux_seek_error(error);
                self.record_recoverable_error(player_error);
                Ok(())
            }
        }
    }

    /// Останавливает audio stream для seek, не меняя high-level playback state.
    fn pause_audio_output_for_seek(&mut self) {
        if let Some(Err(error)) = self.pipeline.pause_audio_output() {
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
            seek_commit: None,
            seek_trace: SeekTraceState::default(),
            simple_scrub_active: false,
            simple_scrub_latest_request: None,
            seek_eof_fallback_video_position: None,
        }
    }
}

/// Мапит ошибку demux seek в player error без смешивания unavailable/timeout/demux.
fn player_error_from_demux_seek_error(error: anyhow::Error) -> PlayerError {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<MediaDemuxError>()
            .is_some_and(MediaDemuxError::is_seek_unavailable)
    }) {
        return PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            format!("Seek failed: {error}"),
        );
    }

    PlayerError::new(PlayerErrorKind::DemuxError, format!("Seek failed: {error}"))
}

/// Мапит ошибку выбора seek policy до mutating-части transaction-а.
fn player_error_from_seek_demux_request_error(error: SeekDemuxRequestError) -> PlayerError {
    match error {
        SeekDemuxRequestError::UnsupportedSeekMode { mode } => PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            format!("Seek mode {mode:?} пока не поддерживается текущим demux contract"),
        ),
    }
}

/// Мапит failed decoder flush в typed player error без продолжения seek transaction.
fn player_error_from_decoder_flush_error(error: anyhow::Error) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::DecoderFlushFailed,
        format!("Video decoder flush failed before seek: {error}"),
    )
}

/// Возвращает stable label для resume intent-а после seek.
fn playback_resume_intent_name(intent: PlaybackResumeIntent) -> &'static str {
    match intent {
        PlaybackResumeIntent::Pause => "pause",
        PlaybackResumeIntent::Play => "play",
    }
}

#[cfg(test)]
mod tests;
