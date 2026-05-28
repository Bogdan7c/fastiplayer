//! Playback tick и A/V scheduler.
//!
//! Этот модуль держит логику, которая раньше жила в `app-egui::main`:
//! чтение packets из demuxer, audio throttle, отправку video packets в decoder,
//! приём decoded frames, backpressure и выбор кадра для показа.

use std::time::{Duration, Instant};

use media_core::{DemuxReadEvent, PacketKeyframe, TrackId, TrackKind, TrackTimestamp};
use rustiplayer_config::AppConfig;
use tracing::{debug, trace, warn};

use super::{PlayerSession, audio_runtime::sanitize_audio_high_water_mark};
use crate::{
    PendingAudioPacket, PendingVideoPacket, PipelineLatencyStage, PipelinePauseReason,
    PlaybackState, PlayerError, PlayerErrorKind, WorkerFrameTimingSnapshot,
    WorkerWakeupDiagnosticsSnapshot, WorkerWakeupReason,
};

mod video_decoder_io;

use video_decoder_io::{
    VideoDecoderDecodeAheadLimits, VideoDecoderIoContext, VideoDecoderIoLimits,
    VideoDecoderTextureLimits, can_send_video_packet_to_decoder, drain_decoded_video_frames,
    has_texture_capacity_for_decode, run_video_decoder_io, send_pending_video_packets_to_decoder,
    texture_capacity_backpressure_reason,
};

/// Контекст одного playback tick.
#[derive(Debug, Clone, Copy)]
pub struct PlayerTickContext {
    /// Монотонное время shell на момент tick.
    pub now: Instant,

    /// Настройки scheduler/backpressure для текущего tick.
    pub config: PlayerTickConfig,

    /// Насколько worker опоздал относительно своего регулярного tick interval.
    pub tick_late_by: Duration,
}

impl PlayerTickContext {
    /// Создаёт tick context с production defaults.
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            now,
            config: PlayerTickConfig::default(),
            tick_late_by: Duration::ZERO,
        }
    }

    /// Создаёт tick context с явно переданным конфигом.
    #[must_use]
    pub const fn with_config(now: Instant, config: PlayerTickConfig) -> Self {
        Self {
            now,
            config,
            tick_late_by: Duration::ZERO,
        }
    }

    /// Создаёт tick context с worker timing diagnostics для adaptive catch-up.
    #[must_use]
    pub const fn with_timing(
        now: Instant,
        config: PlayerTickConfig,
        tick_late_by: Duration,
    ) -> Self {
        Self {
            now,
            config,
            tick_late_by,
        }
    }
}

/// Конфигурация playback tick, backpressure и A/V scheduler.
#[derive(Debug, Clone, Copy)]
pub struct PlayerTickConfig {
    /// Сколько container packets можно прочитать за один tick.
    pub max_demux_packets_per_tick: usize,

    /// Максимум decoded video frames в очереди presentation.
    pub max_video_present_queue: usize,

    /// Минимальный запас decoded frames, ниже которого считаем pipeline starvation-prone.
    pub min_video_present_queue: usize,

    /// Целевой запас decoded frames в очереди presentation.
    pub target_video_present_queue: usize,

    /// Минимальный запас свободных texture slots перед отправкой новых packets в decoder.
    pub min_texture_slots_available_for_decode: usize,

    /// Целевой запас свободных texture/surface slots для adaptive catch-up.
    pub target_texture_slots_available_for_decode: usize,

    /// Максимум сырых video packets между demuxer и decoder thread.
    pub max_pending_video_packets: usize,

    /// Временный bounded лимит video packets, пока audio buffer догоняет low-watermark.
    pub max_pending_video_packets_during_audio_catchup: usize,

    /// Максимум video packets, отправляемых decoder thread за один tick.
    pub max_video_packets_sent_per_tick: usize,

    /// Максимум decoded frames, принимаемых из decoder thread за один tick.
    pub max_decoded_video_frames_drained_per_tick: usize,

    /// Максимальный decode-ahead относительно audio clock.
    pub max_video_decode_ahead: Duration,

    /// Целевой steady-state decode-ahead относительно audio clock.
    pub target_video_decode_ahead: Duration,

    /// Дополнительное bounded окно catch-up work после базового tick.
    pub adaptive_catch_up_time_budget: Duration,

    /// Уровень audio buffer, выше которого audio packets временно не декодируются.
    pub audio_buffer_high_water_mark_ms: f64,

    /// Уровень audio buffer, ниже которого demux может читать сквозь video backpressure.
    pub audio_demux_low_water_mark_ms: f64,

    /// Минимальный audio buffer перед переходом autoplay из `Buffering` в `Playing`.
    pub audio_preroll_target_ms: f64,

    /// Максимальное время ожидания seek commit gates.
    pub seek_commit_timeout: Duration,

    /// Минимальный audio buffer перед resume после seek.
    pub seek_resume_audio_min_buffer_ms: f64,

    /// Soft timeout audio gate-а после того, как target video frame уже показан.
    pub seek_resume_audio_gate_timeout: Duration,

    /// Минимальный запас готовых video frames перед resume после seek.
    pub seek_resume_video_min_ready_frames: usize,

    /// Минимальная позиция audio clock, после которой stalled audio считается реальным.
    pub audio_stall_min_position: Duration,

    /// Длительность без движения audio clock, после которой звук считается stalled.
    pub audio_stall_timeout: Duration,

    /// Небольшой lead scheduler-а относительно audio clock в долях video frame.
    pub video_present_lead_frames: f64,

    /// Окно раннего показа sequential frame в долях video frame.
    pub video_present_window_frames: f64,

    /// Grace перед late-drop в долях video frame.
    pub video_late_drop_grace_frames: f64,
}

impl Default for PlayerTickConfig {
    /// Возвращает текущие MVP-лимиты, перенесённые из app layer в player-core.
    fn default() -> Self {
        Self {
            max_demux_packets_per_tick: 12,
            max_video_present_queue: 8,
            min_video_present_queue: 2,
            target_video_present_queue: 4,
            min_texture_slots_available_for_decode: 2,
            target_texture_slots_available_for_decode: 4,
            max_pending_video_packets: 32,
            max_pending_video_packets_during_audio_catchup: 240,
            max_video_packets_sent_per_tick: 8,
            max_decoded_video_frames_drained_per_tick: 8,
            max_video_decode_ahead: Duration::from_millis(500),
            target_video_decode_ahead: Duration::from_millis(250),
            adaptive_catch_up_time_budget: Duration::from_millis(4),
            audio_buffer_high_water_mark_ms: 200.0,
            audio_demux_low_water_mark_ms: 100.0,
            audio_preroll_target_ms: 50.0,
            seek_commit_timeout: Duration::from_millis(10_000),
            seek_resume_audio_min_buffer_ms: 50.0,
            seek_resume_audio_gate_timeout: Duration::from_millis(250),
            seek_resume_video_min_ready_frames: 3,
            audio_stall_min_position: Duration::from_millis(100),
            audio_stall_timeout: Duration::from_millis(250),
            video_present_lead_frames: 0.5,
            video_present_window_frames: 1.0,
            video_late_drop_grace_frames: 2.0,
        }
    }
}

impl From<&AppConfig> for PlayerTickConfig {
    /// Собирает runtime-лимиты playback из пользовательского TOML-config.
    ///
    /// Scheduler knobs читаются из `[video.scheduler]`; старые top-level video
    /// поля остаются max-границами для present queue, decode-ahead и bounded
    /// decoder queues.
    fn from(config: &AppConfig) -> Self {
        let defaults = Self::default();

        Self {
            max_demux_packets_per_tick: config.video.scheduler.demux_packets_per_tick,
            max_video_present_queue: config.video.present_queue_frames,
            min_video_present_queue: config.video.scheduler.present_queue_min_frames,
            target_video_present_queue: config.video.scheduler.present_queue_target_frames,
            min_texture_slots_available_for_decode: config.video.scheduler.surface_free_slots_min,
            target_texture_slots_available_for_decode: config
                .video
                .scheduler
                .surface_free_slots_target,
            max_pending_video_packets: config.video.decoder_packet_channel_frames,
            max_video_packets_sent_per_tick: config.video.scheduler.video_packets_per_tick,
            max_decoded_video_frames_drained_per_tick: config
                .video
                .scheduler
                .decoded_frames_per_tick,
            max_video_decode_ahead: Duration::from_millis(config.video.max_decode_ahead_ms),
            target_video_decode_ahead: Duration::from_millis(
                config.video.scheduler.decode_ahead_target_ms,
            ),
            adaptive_catch_up_time_budget: Duration::from_millis(
                config.video.scheduler.catch_up_budget_ms,
            ),
            audio_buffer_high_water_mark_ms: config.audio.buffer_target_ms as f64,
            audio_demux_low_water_mark_ms: (config.audio.buffer_target_ms as f64 * 0.5)
                .max(config.player.seek.resume_audio_min_buffer_ms as f64),
            audio_preroll_target_ms: config.player.seek.resume_audio_min_buffer_ms as f64,
            seek_commit_timeout: Duration::from_millis(config.player.seek.commit_timeout_ms),
            seek_resume_audio_min_buffer_ms: config.player.seek.resume_audio_min_buffer_ms as f64,
            seek_resume_audio_gate_timeout: Duration::from_millis(
                config.player.seek.resume_audio_gate_timeout_ms,
            ),
            seek_resume_video_min_ready_frames: config.player.seek.resume_video_min_ready_frames,
            ..defaults
        }
    }
}

impl PlayerTickConfig {
    /// Возвращает достижимый video preroll для seek resume с учётом размера presentation queue.
    #[must_use]
    pub(crate) fn effective_seek_resume_video_min_ready_frames(&self) -> usize {
        self.seek_resume_video_min_ready_frames
            .max(1)
            .min(video_present_queue_limit(self).saturating_add(1))
    }
}

/// Итог работы одного playback tick для shell-телеметрии.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerTickResult {
    /// Packets, прочитанные из demuxer за tick.
    pub demuxed_packets: Vec<PlayerTickPacket>,

    /// Количество decoded frames, принятых из decoder thread.
    pub decoded_video_frames: u64,

    /// Количество кадров, выбранных scheduler-ом для показа.
    pub video_frames_presented: u64,

    /// Количество tick-ов, где текущий кадр был повторён.
    pub video_frames_repeated: u64,

    /// Список кадров, удалённых scheduler/backpressure логикой.
    pub dropped_video_frames: Vec<PlayerVideoFrameDrop>,

    /// Список typed pipeline pauses за tick.
    pub pipeline_pauses: Vec<PlayerPipelinePause>,

    /// `true`, если demux чтение остановилось из-за backpressure.
    pub demux_backpressured: bool,
}

impl PlayerTickResult {
    /// Запоминает packet для внешней телеметрии без передачи codec bytes наружу.
    fn record_demuxed_packet(&mut self, packet: &media_core::Packet) {
        self.demuxed_packets.push(PlayerTickPacket {
            track_id: packet.track_id,
            kind: packet.kind,
            pts: packet.pts,
            track_pts: packet.track_pts,
            track_dts: packet.track_dts,
            size: packet.data.len(),
            byte_offset: packet.byte_offset,
            keyframe: packet.keyframe,
        });
    }

    /// Учитывает принятый decoded video frame.
    fn record_decoded_video_frame(&mut self) {
        self.decoded_video_frames = self.decoded_video_frames.saturating_add(1);
    }

    /// Учитывает кадр, выбранный для presentation.
    fn record_presented_video_frame(&mut self) {
        self.video_frames_presented = self.video_frames_presented.saturating_add(1);
    }

    /// Учитывает повтор текущего present frame.
    fn record_repeated_video_frame(&mut self) {
        self.video_frames_repeated = self.video_frames_repeated.saturating_add(1);
    }

    /// Учитывает удалённый video frame вместе с причиной.
    fn record_dropped_video_frame(&mut self, pts: Duration, reason: PlayerVideoDropReason) {
        self.dropped_video_frames
            .push(PlayerVideoFrameDrop { pts, reason });
    }

    /// Учитывает typed pipeline pause.
    fn record_pipeline_pause(&mut self, reason: PipelinePauseReason) {
        self.pipeline_pauses.push(PlayerPipelinePause { reason });
    }
}

/// Packet summary для shell-телеметрии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerTickPacket {
    /// ID media track, из которого пришёл packet.
    pub track_id: TrackId,

    /// Тип track: audio или video.
    pub kind: TrackKind,

    /// Presentation timestamp packet-а.
    pub pts: Duration,

    /// Исходный signed PTS demuxer-а в track time base, если доступен.
    pub track_pts: Option<TrackTimestamp>,

    /// Исходный signed DTS demuxer-а в track time base, если доступен.
    pub track_dts: Option<TrackTimestamp>,

    /// Размер codec payload в bytes.
    pub size: usize,

    /// Safe source byte offset для demux seek, если container adapter его сообщил.
    pub byte_offset: Option<u64>,

    /// Keyframe-классификация для video packets.
    pub keyframe: PacketKeyframe,
}

/// Public compatibility имя причины удаления video frame.
pub use crate::VideoDropReason as PlayerVideoDropReason;

/// Summary удалённого video frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerVideoFrameDrop {
    /// Presentation timestamp удалённого кадра.
    pub pts: Duration,

    /// Причина удаления кадра.
    pub reason: PlayerVideoDropReason,
}

/// Summary pipeline pause-а внутри tick telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerPipelinePause {
    /// Typed причина pause.
    pub reason: PipelinePauseReason,
}

/// Read-only план следующего playback wakeup-а worker-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlayerWorkerWakeupPlan {
    /// `None` означает, что worker может ждать только command/render/scrub events.
    pub(crate) delay: Option<Duration>,

    /// Машиночитаемая причина планирования.
    pub(crate) reason: WorkerWakeupReason,

    /// Сравнение media clock target и первого queued video frame.
    pub(crate) frame_timing: Option<WorkerFrameTimingSnapshot>,
}

/// Read-only snapshot scheduler clocks для active seek stall diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SchedulerTimingDiagnosticsSnapshot {
    /// Текущее значение audio clock без media base.
    pub(crate) audio_clock: Duration,

    /// Абсолютная media position, которую scheduler считает "сейчас".
    pub(crate) presentation_clock_position: Duration,

    /// Media target с учётом scheduler lead перед ближайшим present.
    pub(crate) target_media_time_for_present: Duration,
}

/// Собирает scheduler timing без изменения очередей и clock state.
pub(crate) fn scheduler_timing_diagnostics(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
) -> SchedulerTimingDiagnosticsSnapshot {
    let presentation_clock_position = session.presentation_clock_position_at(now);
    let target_media_time_for_present =
        target_media_time_for_present(session, tick_config, presentation_clock_position);

    SchedulerTimingDiagnosticsSnapshot {
        audio_clock: session.audio_clock_now(),
        presentation_clock_position,
        target_media_time_for_present,
    }
}

impl PlayerWorkerWakeupPlan {
    /// Планирует ожидание только внешнего события.
    #[must_use]
    const fn idle() -> Self {
        Self {
            delay: None,
            reason: WorkerWakeupReason::Idle,
            frame_timing: None,
        }
    }

    /// Планирует bounded timeout до следующей meaningful работы.
    #[must_use]
    const fn after(
        delay: Duration,
        reason: WorkerWakeupReason,
        frame_timing: Option<WorkerFrameTimingSnapshot>,
    ) -> Self {
        Self {
            delay: Some(delay),
            reason,
            frame_timing,
        }
    }

    /// Конвертирует план в snapshot diagnostics после фактического wakeup-а.
    #[must_use]
    pub(crate) const fn diagnostics(
        self,
        tick_late_by: Duration,
    ) -> WorkerWakeupDiagnosticsSnapshot {
        WorkerWakeupDiagnosticsSnapshot {
            reason: Some(self.reason),
            planned_delay: self.delay,
            tick_late_by,
            frame_timing: self.frame_timing,
        }
    }
}

impl PlayerSession {
    /// Вычисляет следующий worker wakeup из состояния media pipeline.
    ///
    /// Эта функция не меняет pipeline: она только выбирает, когда worker должен
    /// снова вызвать `tick()`. Video cadence берётся из PTS/audio clock, а
    /// короткий decoder poll используется только как readiness fallback, потому
    /// что decoder thread сейчас отдаёт frames через неблокирующий `try_recv_frame()`.
    #[must_use]
    pub(crate) fn worker_wakeup_plan(
        &self,
        now: Instant,
        tick_config: &PlayerTickConfig,
        decoder_readiness_poll_interval: Duration,
        coarse_progress_interval: Duration,
    ) -> PlayerWorkerWakeupPlan {
        if !self.playback_state().is_playback_active() {
            return PlayerWorkerWakeupPlan::idle();
        }

        let frame_timing = front_frame_timing(self, tick_config, now);

        if front_frame_ready_for_scheduler(self, tick_config, now) {
            return PlayerWorkerWakeupPlan::after(
                Duration::ZERO,
                WorkerWakeupReason::FrameReady,
                frame_timing,
            );
        }

        if immediate_pipeline_work_available(self, tick_config) {
            let reason = if matches!(
                self.playback_state(),
                PlaybackState::Buffering | PlaybackState::Seeking
            ) {
                WorkerWakeupReason::SeekOrPreroll
            } else {
                WorkerWakeupReason::PipelineWorkReady
            };
            return PlayerWorkerWakeupPlan::after(Duration::ZERO, reason, frame_timing);
        }

        let audio_refill_delay = audio_buffer_refill_wakeup_delay(self, tick_config);
        if let Some(front_frame_delay) = front_frame_scheduler_delay(self, tick_config, now) {
            if let Some(audio_refill_delay) =
                audio_refill_delay.filter(|delay| *delay < front_frame_delay)
            {
                return PlayerWorkerWakeupPlan::after(
                    audio_refill_delay,
                    WorkerWakeupReason::PipelineWorkReady,
                    frame_timing,
                );
            }

            return PlayerWorkerWakeupPlan::after(
                front_frame_delay,
                WorkerWakeupReason::FramePtsDeadline,
                frame_timing,
            );
        }

        if decoder_readiness_poll_needed(self, tick_config) {
            if let Some(audio_refill_delay) =
                audio_refill_delay.filter(|delay| *delay < decoder_readiness_poll_interval)
            {
                return PlayerWorkerWakeupPlan::after(
                    audio_refill_delay,
                    WorkerWakeupReason::PipelineWorkReady,
                    frame_timing,
                );
            }

            return PlayerWorkerWakeupPlan::after(
                decoder_readiness_poll_interval,
                WorkerWakeupReason::DecodeReadiness,
                frame_timing,
            );
        }

        if let Some(audio_refill_delay) = audio_refill_delay {
            return PlayerWorkerWakeupPlan::after(
                audio_refill_delay,
                WorkerWakeupReason::PipelineWorkReady,
                frame_timing,
            );
        }

        if seek_transition_needs_progress(self) {
            return PlayerWorkerWakeupPlan::after(
                coarse_progress_interval,
                WorkerWakeupReason::SeekOrPreroll,
                frame_timing,
            );
        }

        if self.eof_drain_needs_progress() {
            return PlayerWorkerWakeupPlan::after(
                coarse_progress_interval,
                WorkerWakeupReason::CoarseProgress,
                frame_timing,
            );
        }

        if active_pipeline_needs_coarse_progress(self) {
            return PlayerWorkerWakeupPlan::after(
                coarse_progress_interval,
                WorkerWakeupReason::CoarseProgress,
                frame_timing,
            );
        }

        PlayerWorkerWakeupPlan::idle()
    }
}

impl PlayerSession {
    /// Выполняет один playback tick.
    ///
    /// Shell вызывает этот метод один раз на redraw. Метод намеренно не рендерит
    /// и не знает про egui: он только продвигает media pipeline и возвращает
    /// компактный результат для телеметрии.
    #[must_use]
    pub fn tick(&mut self, tick_context: PlayerTickContext) -> PlayerTickResult {
        let mut tick_result = PlayerTickResult::default();

        self.update_position_for_tick(tick_context.now);

        if self.is_demuxing_active() && self.pipeline.has_demuxer() {
            read_demux_packets(
                self,
                &tick_context.config,
                &mut tick_result,
                tick_context.config.max_demux_packets_per_tick,
                None,
            );
        }

        if self.is_demuxing_active() || self.is_eof_draining() {
            self.process_pending_audio_packets_with_buffer_limit(
                tick_context.config.audio_buffer_high_water_mark_ms,
            );
            self.start_eof_audio_tail_if_needed();
        }

        process_pending_video_packets(self, tick_context, &mut tick_result);
        self.finish_seek_commit_if_ready(tick_context.now, &tick_context.config);
        if let Err(error) =
            self.finish_autoplay_preroll_if_ready(tick_context.config.audio_preroll_target_ms)
        {
            self.mark_fatal_error(error);
        }
        self.finish_eof_drain_if_ready(tick_context.now, tick_context.config.audio_stall_timeout);

        tick_result
    }

    /// Обновляет playback position один раз за tick.
    fn update_position_for_tick(&mut self, now: Instant) {
        if self.playback_state() != PlaybackState::Playing && !self.eof_drain_needs_progress() {
            return;
        }

        let playback_position = self.presentation_clock_position_at(now);
        if self.pipeline.has_audio_clock() {
            self.pipeline
                .note_audio_clock_sample(self.audio_clock_now(), now);
        }
        self.update_current_position(playback_position);
    }
}

/// Нормализует low-water mark для audio catch-up demux.
fn sanitize_audio_demux_low_water_mark(low_water_mark_ms: f64) -> f64 {
    if low_water_mark_ms.is_finite() && low_water_mark_ms > 0.0 {
        low_water_mark_ms
    } else {
        PlayerTickConfig::default().audio_demux_low_water_mark_ms
    }
}

/// Возвращает delay до момента, когда audio buffer снова можно пополнять.
fn audio_buffer_refill_wakeup_delay(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> Option<Duration> {
    if !audio_refill_work_can_be_scheduled(session) {
        return None;
    }

    let audio_buffer_level_ms = session.audio_buffer_level_ms()?;
    if !audio_buffer_level_ms.is_finite() {
        return None;
    }

    let high_water_mark_ms =
        sanitize_audio_high_water_mark(tick_config.audio_buffer_high_water_mark_ms);
    if audio_buffer_level_ms <= high_water_mark_ms {
        return None;
    }

    let delay_seconds = (audio_buffer_level_ms - high_water_mark_ms) / 1000.0;
    Some(Duration::from_secs_f64(delay_seconds))
}

/// Проверяет, есть ли audio work, которое станет runnable после снижения buffer level.
fn audio_refill_work_can_be_scheduled(session: &PlayerSession) -> bool {
    if !session.pipeline.has_selected_audio_track() {
        return false;
    }

    if session.is_demuxing_active() && session.pipeline.has_demuxer() {
        return true;
    }

    session.eof_drain_needs_progress() && !session.pipeline.pending_audio_packet_is_empty()
}

/// Возвращает bounded лимит video packets для audio catch-up режима.
fn audio_catchup_pending_video_limit(tick_config: &PlayerTickConfig) -> usize {
    tick_config
        .max_pending_video_packets_during_audio_catchup
        .max(tick_config.max_pending_video_packets)
}

/// Проверяет, нужно ли временно приоритизировать demux ради заполнения audio buffer.
fn audio_demux_catchup_needed(session: &PlayerSession, tick_config: &PlayerTickConfig) -> bool {
    audio_demux_catchup_needed_for_level(
        session.pipeline.has_selected_audio_track(),
        session.audio_buffer_level_ms(),
        tick_config.audio_demux_low_water_mark_ms,
    )
}

/// Чистая часть audio catch-up policy для unit-тестов без CPAL device.
fn audio_demux_catchup_needed_for_level(
    audio_track_selected: bool,
    audio_buffer_level_ms: Option<f64>,
    low_water_mark_ms: f64,
) -> bool {
    if !audio_track_selected {
        return false;
    }

    let Some(audio_buffer_level_ms) = audio_buffer_level_ms else {
        return false;
    };

    audio_buffer_level_ms.is_finite()
        && audio_buffer_level_ms < sanitize_audio_demux_low_water_mark(low_water_mark_ms)
}

/// Проверяет bootstrap-фазу, когда audio track уже выбран, но output ещё не создан.
fn selected_audio_bootstrap_needs_demux(session: &PlayerSession) -> bool {
    session.pipeline.has_selected_audio_track()
        && session.audio_buffer_level_ms().is_none()
        && session.pipeline.pending_audio_packet_is_empty()
}

/// Проверяет audio read-ahead policy перед чтением следующего demux packet-а.
fn audio_read_ahead_blocks_demux(session: &PlayerSession, tick_config: &PlayerTickConfig) -> bool {
    if !session.pipeline.has_selected_audio_track() {
        return false;
    }

    match session.audio_buffer_level_ms() {
        Some(audio_buffer_level_ms) => {
            !audio_buffer_level_ms.is_finite()
                || audio_buffer_level_ms
                    > sanitize_audio_high_water_mark(tick_config.audio_buffer_high_water_mark_ms)
        }
        None => !session.pipeline.pending_audio_packet_is_empty(),
    }
}

/// Проверяет bounded video backlog, который разрешён audio bootstrap/catch-up режимам.
fn audio_priority_pending_video_limit_allows_demux(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    session.pipeline.pending_video_packet_len() < audio_catchup_pending_video_limit(tick_config)
}

/// Проверяет, исчерпано ли bounded окно adaptive catch-up.
fn catch_up_deadline_reached(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

/// Читает новые packets из demuxer в пределах бюджета текущего tick.
fn read_demux_packets(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
    packet_budget: usize,
    catch_up_deadline: Option<Instant>,
) -> usize {
    let mut packets_read = 0usize;

    for _ in 0..packet_budget {
        if catch_up_deadline_reached(catch_up_deadline) {
            break;
        }

        let prioritize_audio_catchup = audio_demux_catchup_needed(session, tick_config);
        if prioritize_audio_catchup
            && session.pipeline.pending_video_packet_len() >= tick_config.max_pending_video_packets
        {
            trace!(
                pending_video_packets = session.pipeline.pending_video_packet_len(),
                catchup_video_packet_limit = audio_catchup_pending_video_limit(tick_config),
                audio_buffer_ms = session.audio_buffer_level_ms().unwrap_or(0.0),
                low_water_mark_ms =
                    sanitize_audio_demux_low_water_mark(tick_config.audio_demux_low_water_mark_ms),
                "Demux audio catch-up: reading through video pressure"
            );
        }

        if !can_read_next_demux_packet_with_audio_priority(
            session,
            tick_config,
            prioritize_audio_catchup,
        ) {
            tick_result.demux_backpressured = true;
            let pause_reason =
                demux_backpressure_reason(session, tick_config, prioritize_audio_catchup)
                    .unwrap_or(PipelinePauseReason::DemuxBackpressure);
            record_pipeline_pause(session, tick_result, pause_reason);
            trace!(
                pause_reason = ?pause_reason,
                pending_audio_packets = session.pipeline.pending_audio_packet_len(),
                pending_video_packets = session.pipeline.pending_video_packet_len(),
                queued_video_frames = session.pipeline.video_present_queue_len(),
                audio_buffer_ms = ?session.audio_buffer_level_ms(),
                "Demux backpressure: waiting for downstream capacity"
            );
            break;
        }

        let demux_read_started_at = Instant::now();
        let Some(event_result) = session.pipeline.demux_next_event() else {
            break;
        };
        session.record_pipeline_latency(
            PipelineLatencyStage::DemuxRead,
            demux_read_started_at.elapsed(),
            None,
            None,
        );

        match event_result {
            Ok(DemuxReadEvent::Packet(packet)) => {
                tick_result.record_demuxed_packet(&packet);
                route_demuxed_packet(session, packet);
                packets_read += 1;
            }
            Ok(DemuxReadEvent::EndOfStream) => {
                debug!(
                    current_position_ms =
                        session.snapshot().current_position.as_secs_f64() * 1000.0,
                    duration_ms = ?session
                        .snapshot()
                        .duration
                        .map(|duration| duration.as_secs_f64() * 1000.0),
                    pending_audio_packets = session.pipeline.pending_audio_packet_len(),
                    pending_video_packets = session.pipeline.pending_video_packet_len(),
                    queued_video_frames = session.pipeline.video_present_queue_len(),
                    video_decode_in_flight = session.pipeline.video_decode_in_flight_packets(),
                    audio_buffer_ms = ?session.audio_buffer_level_ms(),
                    audio_clock_now_ms = session.audio_clock_now().as_secs_f64() * 1000.0,
                    "Demux reported EOF; entering drain"
                );
                session.enter_eof_drain();
                break;
            }
            Ok(DemuxReadEvent::TracksChanged(track_update)) => {
                session.handle_demux_track_list_update(track_update);
                if session.playback_state() == PlaybackState::Failed {
                    break;
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, "Ошибка чтения packet");
                session.mark_fatal_error(PlayerError::new(
                    PlayerErrorKind::DemuxError,
                    format!("Ошибка чтения packet: {error}"),
                ));
                break;
            }
        }
    }

    packets_read
}

/// Перекладывает packet из demuxer в соответствующую pending queue.
fn route_demuxed_packet(session: &mut PlayerSession, packet: media_core::Packet) {
    let generation = session.pipeline.seek_generation();
    session.note_demux_packet_for_seek_trace(&packet, generation);

    match packet.kind {
        TrackKind::Audio => {
            let packet_timing = audio_packet_timing_from_media_packet(&packet);
            let pending_packet = PendingAudioPacket::with_timing(
                packet.track_id,
                packet.pts,
                packet.dts,
                packet.duration,
                packet_timing,
                generation,
                packet.data,
            );
            session
                .pipeline
                .enqueue_pending_audio_packet(pending_packet);
        }
        TrackKind::Video => {
            let pending_packet = PendingVideoPacket::new(
                packet.track_id,
                packet.pts,
                generation,
                packet.data,
                packet.keyframe,
            );
            session
                .pipeline
                .enqueue_pending_video_packet(pending_packet);
        }
    }
}

/// Собирает audio decoder timing из raw metadata, которую demuxer сохранил рядом с media time.
fn audio_packet_timing_from_media_packet(
    packet: &media_core::Packet,
) -> audio_core::AudioPacketTiming {
    let Some(track_pts) = packet.track_pts else {
        return audio_core::AudioPacketTiming::unknown();
    };

    if track_pts.track_id != packet.track_id {
        warn!(
            packet_track_id = %packet.track_id,
            timing_track_id = %track_pts.track_id,
            "Audio packet raw PTS принадлежит другому track; decoder timing помечен unknown"
        );
        return audio_core::AudioPacketTiming::unknown();
    }

    let Some(time_base) = audio_time_base_from_media_time_base(track_pts.time_base) else {
        warn!(
            packet_track_id = %packet.track_id,
            time_base = ?track_pts.time_base,
            "Audio packet raw PTS имеет некорректную time base; decoder timing помечен unknown"
        );
        return audio_core::AudioPacketTiming::unknown();
    };

    let dts_units = audio_packet_dts_units(packet, track_pts);
    let duration_units = audio_packet_duration_units(packet, track_pts);

    audio_core::AudioPacketTiming::from_track_units(
        time_base,
        track_pts.units.get(),
        dts_units,
        duration_units,
    )
}

/// Конвертирует media-core time base в audio boundary time base без Symphonia types.
fn audio_time_base_from_media_time_base(
    time_base: media_core::TimeBase,
) -> Option<audio_core::AudioPacketTimeBase> {
    audio_core::AudioPacketTimeBase::new(time_base.numer, time_base.denom)
}

/// Возвращает raw DTS units только если DTS согласован с PTS track owner/timebase.
fn audio_packet_dts_units(packet: &media_core::Packet, track_pts: TrackTimestamp) -> Option<i64> {
    let track_dts = packet.track_dts?;

    if track_dts.track_id != packet.track_id || track_dts.time_base != track_pts.time_base {
        warn!(
            packet_track_id = %packet.track_id,
            dts_track_id = %track_dts.track_id,
            pts_time_base = ?track_pts.time_base,
            dts_time_base = ?track_dts.time_base,
            "Audio packet raw DTS не согласован с PTS; DTS не передан decoder boundary"
        );
        return None;
    }

    Some(track_dts.units.get())
}

/// Возвращает raw duration units только если duration согласована с PTS track owner/timebase.
fn audio_packet_duration_units(
    packet: &media_core::Packet,
    track_pts: TrackTimestamp,
) -> Option<u64> {
    let track_duration = packet.track_duration?;

    if track_duration.track_id != packet.track_id || track_duration.time_base != track_pts.time_base
    {
        warn!(
            packet_track_id = %packet.track_id,
            duration_track_id = %track_duration.track_id,
            pts_time_base = ?track_pts.time_base,
            duration_time_base = ?track_duration.time_base,
            "Audio packet raw duration не согласована с PTS; duration не передана decoder boundary"
        );
        return None;
    }

    Some(track_duration.units.get())
}

/// Проверяет demux admission с явно переданным audio-priority флагом.
fn can_read_next_demux_packet_with_audio_priority(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    prioritize_audio_catchup: bool,
) -> bool {
    if audio_read_ahead_blocks_demux(session, tick_config) {
        return false;
    }

    if prioritize_audio_catchup || selected_audio_bootstrap_needs_demux(session) {
        return audio_priority_pending_video_limit_allows_demux(session, tick_config);
    }

    if !has_texture_capacity_for_decode(session, video_decoder_texture_limits(tick_config)) {
        return false;
    }

    if session.pipeline.pending_video_packet_len() >= tick_config.max_pending_video_packets {
        return false;
    }

    seek_admission_active(session) || available_video_present_slots(session, tick_config) > 0
}

/// Возвращает typed причину demux backpressure вместо generic "нет места".
fn demux_backpressure_reason(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    prioritize_audio_catchup: bool,
) -> Option<PipelinePauseReason> {
    if audio_read_ahead_blocks_demux(session, tick_config) {
        return Some(PipelinePauseReason::DemuxBackpressure);
    }

    if prioritize_audio_catchup || selected_audio_bootstrap_needs_demux(session) {
        return (!audio_priority_pending_video_limit_allows_demux(session, tick_config))
            .then_some(PipelinePauseReason::WaitingForDemuxAudioPriority);
    }

    if let Some(reason) =
        texture_capacity_backpressure_reason(session, video_decoder_texture_limits(tick_config))
    {
        return Some(reason);
    }

    if session.pipeline.pending_video_packet_len() >= tick_config.max_pending_video_packets {
        return Some(PipelinePauseReason::DemuxBackpressure);
    }

    (!seek_admission_active(session) && available_video_present_slots(session, tick_config) == 0)
        .then_some(PipelinePauseReason::WaitingForPresentQueue)
}

/// Возвращает количество свободных мест в presentation queue.
fn available_video_present_slots(session: &PlayerSession, tick_config: &PlayerTickConfig) -> usize {
    video_present_queue_limit(tick_config)
        .saturating_sub(session.pipeline.video_present_queue_len())
}

/// Возвращает безопасный лимит presentation queue.
fn video_present_queue_limit(tick_config: &PlayerTickConfig) -> usize {
    tick_config.max_video_present_queue.max(1)
}

/// Возвращает безопасный минимум presentation queue.
fn video_present_queue_min(tick_config: &PlayerTickConfig) -> usize {
    tick_config
        .min_video_present_queue
        .max(1)
        .min(video_present_queue_limit(tick_config))
}

/// Возвращает codec-neutral target presentation queue для steady-state playback.
fn video_present_queue_target(tick_config: &PlayerTickConfig) -> usize {
    tick_config
        .target_video_present_queue
        .max(video_present_queue_min(tick_config))
        .min(video_present_queue_limit(tick_config))
}

/// Проверяет, что seek transaction сейчас должен продвигаться независимо от обычных present slots.
fn seek_admission_active(session: &PlayerSession) -> bool {
    session.has_active_seek_commit()
}

/// Проверяет, блокирует ли normal playback admission работу video pipeline-а.
fn normal_present_queue_blocks_video_admission(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    !seek_admission_active(session)
        && session.can_present_video()
        && available_video_present_slots(session, tick_config) == 0
}

/// Возвращает безопасный максимум decode-ahead относительно audio clock.
fn video_decode_ahead_limit(tick_config: &PlayerTickConfig) -> Duration {
    tick_config
        .max_video_decode_ahead
        .max(Duration::from_millis(1))
}

/// Возвращает steady-state target decode-ahead относительно audio clock.
fn video_decode_ahead_target(tick_config: &PlayerTickConfig) -> Duration {
    tick_config
        .target_video_decode_ahead
        .max(Duration::from_millis(1))
        .min(video_decode_ahead_limit(tick_config))
}

/// Возвращает безопасный минимальный reserve surface/import slots.
fn texture_slot_min_watermark(tick_config: &PlayerTickConfig) -> usize {
    tick_config.min_texture_slots_available_for_decode
}

/// Возвращает безопасный target reserve surface/import slots.
fn texture_slot_target_watermark(tick_config: &PlayerTickConfig) -> usize {
    tick_config
        .target_texture_slots_available_for_decode
        .max(texture_slot_min_watermark(tick_config))
}

/// Формирует typed texture limits для decoder I/O без передачи всего config-а.
fn video_decoder_texture_limits(tick_config: &PlayerTickConfig) -> VideoDecoderTextureLimits {
    VideoDecoderTextureLimits::new(texture_slot_min_watermark(tick_config))
}

/// Формирует typed decode-ahead limits для конкретного режима admission.
fn video_decoder_decode_ahead_limits(
    tick_config: &PlayerTickConfig,
    admission_limit: Duration,
) -> VideoDecoderDecodeAheadLimits {
    VideoDecoderDecodeAheadLimits::new(admission_limit, video_decode_ahead_limit(tick_config))
}

/// Формирует полный набор лимитов для одного decoder I/O прохода.
fn video_decoder_io_limits(
    tick_config: &PlayerTickConfig,
    max_frames_to_drain: usize,
    max_packets_to_send: usize,
    decode_ahead_limit: Duration,
) -> VideoDecoderIoLimits {
    VideoDecoderIoLimits::new(
        max_frames_to_drain,
        max_packets_to_send,
        video_present_queue_limit(tick_config),
        video_decoder_texture_limits(tick_config),
        video_decoder_decode_ahead_limits(tick_config, decode_ahead_limit),
    )
}

/// Возвращает достижимый video preroll для seek resume с учётом размера presentation queue.
#[cfg(test)]
fn effective_seek_resume_video_min_ready_frames(tick_config: &PlayerTickConfig) -> usize {
    tick_config.effective_seek_resume_video_min_ready_frames()
}

/// Удаляет лишние кадры, если presentation queue стала больше безопасного лимита.
fn trim_video_present_queue(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
) {
    let queue_limit = video_present_queue_limit(tick_config);

    while session.pipeline.video_present_queue_len() > queue_limit {
        let Some(frame) = session.pipeline.pop_queued_video_frame_front() else {
            break;
        };

        release_video_texture(session, frame.texture_handle);
        record_video_drop(
            session,
            tick_result,
            frame.pts,
            PlayerVideoDropReason::QueueOverflow,
        );
        tracing::debug!(
            pts_ms = frame.pts.as_millis(),
            "Dropping frame: queue overflow protection"
        );
    }
}

/// Добавляет duration без panic при переполнении.
fn saturating_duration_add(timestamp: Duration, offset: Duration) -> Duration {
    timestamp.checked_add(offset).unwrap_or(Duration::MAX)
}

/// Возвращает безопасный неотрицательный множитель для `Duration::mul_f64`.
fn finite_non_negative_factor(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value >= 0.0 {
        value
    } else {
        fallback
    }
}

/// Возвращает signed delta между двумя media timestamps в микросекундах.
fn duration_delta_micros(left: Duration, right: Duration) -> i128 {
    let delta = if left >= right {
        left.saturating_sub(right)
            .as_micros()
            .min(i128::MAX as u128) as i128
    } else {
        right
            .saturating_sub(left)
            .as_micros()
            .min(i128::MAX as u128) as i128
    };

    if left >= right { delta } else { -delta }
}

/// Рассчитывает media time, под который выбираем frame для ближайшего present.
fn target_media_time_for_present(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    presentation_now: Duration,
) -> Duration {
    let present_lead = video_present_lead(session, tick_config);

    saturating_duration_add(presentation_now, present_lead)
}

/// Возвращает lead scheduler-а перед PTS кадра.
///
/// Это не video cadence и не fixed display tick: lead считается как доля
/// наблюдаемой длительности кадра, чтобы worker успел передать frame render
/// thread-у до ближайшего vsync.
fn video_present_lead(session: &PlayerSession, tick_config: &PlayerTickConfig) -> Duration {
    let lead_frames = finite_non_negative_factor(
        tick_config.video_present_lead_frames,
        PlayerTickConfig::default().video_present_lead_frames,
    );
    session
        .pipeline
        .video_frame_duration_estimate()
        .mul_f64(lead_frames)
}

/// Собирает diagnostics по первому queued frame без мутации очереди.
fn front_frame_timing(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
) -> Option<WorkerFrameTimingSnapshot> {
    let front_frame = session.pipeline.front_queued_video_frame()?;
    let presentation_now = session.presentation_clock_position_at(now);
    let target_media_time = target_media_time_for_present(session, tick_config, presentation_now);

    Some(WorkerFrameTimingSnapshot {
        front_frame_pts: front_frame.pts,
        target_media_time,
        front_frame_delta_from_target_us: duration_delta_micros(front_frame.pts, target_media_time),
    })
}

/// Проверяет, должен ли scheduler немедленно обработать первый queued frame.
fn front_frame_ready_for_scheduler(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
) -> bool {
    let Some(front_frame) = session.pipeline.front_queued_video_frame() else {
        return false;
    };

    if session.active_seek_frame_ready_for_scheduler(front_frame.pts) {
        return true;
    }

    let presentation_now = session.presentation_clock_position_at(now);
    let target_media_time = target_media_time_for_present(session, tick_config, presentation_now);
    let present_window = video_present_window(session, tick_config);
    let late_drop_grace = video_late_drop_grace(session, tick_config);

    should_drop_front_frame_as_late(
        session.pipeline.front_and_next_queued_video_frames(),
        target_media_time,
        late_drop_grace,
    ) || !should_wait_for_front_frame(front_frame.pts, target_media_time, present_window)
}

/// Возвращает задержку до момента, когда scheduler должен подготовить первый queued frame.
///
/// Worker просыпается не ровно на PTS, а на `PTS - present_lead`: иначе
/// `Condvar`/`select!` timeout, планировщик ОС и handoff в render thread
/// систематически публикуют frame уже после нужного redraw-а.
fn front_frame_scheduler_delay(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
) -> Option<Duration> {
    let front_frame = session.pipeline.front_queued_video_frame()?;
    let presentation_now = session.presentation_clock_position_at(now);
    let scheduler_media_deadline = front_frame
        .pts
        .saturating_sub(video_present_lead(session, tick_config));

    Some(scheduler_media_deadline.saturating_sub(presentation_now))
}

/// Проверяет, есть ли работа, которую tick может выполнить без ожидания media PTS.
fn immediate_pipeline_work_available(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    if pending_audio_work_available(session, tick_config) {
        return true;
    }

    let demux_available = demux_work_available(session, tick_config);
    if !seek_admission_active(session)
        && session.can_present_video()
        && session.pipeline.video_present_queue_len() >= video_present_queue_target(tick_config)
        && !demux_available
    {
        return false;
    }

    if pending_video_work_available(session, tick_config) {
        return true;
    }

    demux_available
}

/// Проверяет, может ли worker прямо сейчас протолкнуть audio packets.
fn pending_audio_work_available(session: &PlayerSession, tick_config: &PlayerTickConfig) -> bool {
    if session.pipeline.pending_audio_packet_is_empty() {
        return false;
    }

    session.audio_buffer_level_ms().unwrap_or(0.0)
        <= sanitize_audio_high_water_mark(tick_config.audio_buffer_high_water_mark_ms)
}

/// Проверяет, может ли worker прямо сейчас отправить или списать video packet.
fn pending_video_work_available(session: &PlayerSession, tick_config: &PlayerTickConfig) -> bool {
    let Some(packet) = session.pipeline.front_pending_video_packet() else {
        return false;
    };

    if !session.pipeline.can_send_video_decode_packets() {
        return true;
    }

    if normal_present_queue_blocks_video_admission(session, tick_config) {
        return false;
    }

    can_send_video_packet_to_decoder(
        session,
        video_decoder_texture_limits(tick_config),
        video_decoder_decode_ahead_limits(tick_config, video_decode_ahead_target(tick_config)),
        packet.pts,
    )
}

/// Проверяет, может ли demuxer прочитать следующий packet без downstream overflow.
fn demux_work_available(session: &PlayerSession, tick_config: &PlayerTickConfig) -> bool {
    if !session.is_demuxing_active() || !session.pipeline.has_demuxer() {
        return false;
    }

    let prioritize_audio_catchup = audio_demux_catchup_needed(session, tick_config);
    let selected_audio_bootstrap = selected_audio_bootstrap_needs_demux(session);
    if session.pipeline.has_selected_video_track()
        && session.pipeline.video_present_queue_len() >= video_present_queue_target(tick_config)
        && !prioritize_audio_catchup
        && !selected_audio_bootstrap
        && !seek_admission_active(session)
    {
        return false;
    }

    can_read_next_demux_packet_with_audio_priority(session, tick_config, prioritize_audio_catchup)
}

/// Проверяет, нужен ли короткий poll decoded-frame readiness.
fn decoder_readiness_poll_needed(session: &PlayerSession, tick_config: &PlayerTickConfig) -> bool {
    let Some(decoder_packet_queue_depth) = session.pipeline.video_decoder_packet_queue_depth()
    else {
        return false;
    };

    decoder_readiness_poll_needed_for_state(
        session.pipeline.has_selected_video_track(),
        session.pipeline.video_present_queue_len(),
        video_present_queue_target(tick_config),
        !session.pipeline.pending_video_packet_is_empty(),
        session.pipeline.has_demuxer(),
        decoder_packet_queue_depth > 0,
        session.pipeline.video_decode_in_flight_packets() > 0,
        session.has_active_seek_commit(),
    )
}

/// Чистая часть decoder readiness policy без реального GPU decoder thread.
fn decoder_readiness_poll_needed_for_state(
    video_track_selected: bool,
    video_frame_queue_len: usize,
    video_present_queue_target: usize,
    worker_has_pending_video_packets: bool,
    worker_has_demuxer: bool,
    decoder_has_queued_packets: bool,
    decoder_has_in_flight_packets: bool,
    seek_commit_active: bool,
) -> bool {
    let decoder_has_seek_in_flight_packets = seek_commit_active && decoder_has_in_flight_packets;
    let worker_has_decode_inputs = worker_has_pending_video_packets
        || worker_has_demuxer
        || decoder_has_queued_packets
        || decoder_has_seek_in_flight_packets;
    if !worker_has_decode_inputs {
        return false;
    }

    if decoder_has_seek_in_flight_packets {
        return video_track_selected;
    }

    if video_frame_queue_len < video_present_queue_target {
        return video_track_selected;
    }

    worker_has_pending_video_packets
}

/// Проверяет, должен ли активный seek получить bounded wakeup хотя бы для timeout/gates.
fn seek_transition_needs_progress(session: &PlayerSession) -> bool {
    session.has_active_seek_commit()
}

/// Проверяет, нужен ли редкий progress wakeup без точного PTS deadline-а.
fn active_pipeline_needs_coarse_progress(session: &PlayerSession) -> bool {
    matches!(
        session.playback_state(),
        PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Seeking
    )
}

/// Возвращает допустимое окно выбора кадра вокруг target media time.
fn video_present_window(session: &PlayerSession, tick_config: &PlayerTickConfig) -> Duration {
    let window_frames = finite_non_negative_factor(
        tick_config.video_present_window_frames,
        PlayerTickConfig::default().video_present_window_frames,
    );

    session
        .pipeline
        .video_frame_duration_estimate()
        .mul_f64(window_frames)
}

/// Возвращает допустимое опоздание кадра перед forced catch-up drop.
fn video_late_drop_grace(session: &PlayerSession, tick_config: &PlayerTickConfig) -> Duration {
    let grace_frames = finite_non_negative_factor(
        tick_config.video_late_drop_grace_frames,
        PlayerTickConfig::default().video_late_drop_grace_frames,
    );

    session
        .pipeline
        .video_frame_duration_estimate()
        .mul_f64(grace_frames)
}

/// Проверяет, нужно ли дропнуть первый queued frame как реально устаревший.
fn should_drop_front_frame_as_late(
    front_and_next_frames: Option<(&video_core::DecodedFrame, &video_core::DecodedFrame)>,
    target_media_time: Duration,
    late_drop_grace: Duration,
) -> bool {
    let Some((front_frame, next_frame)) = front_and_next_frames else {
        // Без кадра-замены причина опоздания: starvation, а не настоящий late drop.
        return false;
    };

    let latest_front_pts = saturating_duration_add(front_frame.pts, late_drop_grace);
    if target_media_time <= latest_front_pts {
        return false;
    }

    next_frame.pts <= target_media_time
}

/// Проверяет, слишком ли рано показывать первый queued frame.
fn should_wait_for_front_frame(
    frame_pts: Duration,
    target_media_time: Duration,
    _present_window: Duration,
) -> bool {
    frame_pts > target_media_time
}

/// Освобождает texture handle через decoder thread, если он ещё существует.
fn release_video_texture(
    session: &mut PlayerSession,
    texture_handle: video_core::FrameTextureHandle,
) {
    session.release_video_texture(texture_handle);
}

/// Удаляет первый queued frame и записывает причину drop.
fn drop_front_queued_video_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    reason: PlayerVideoDropReason,
) -> bool {
    let Some(frame) = session.pipeline.pop_queued_video_frame_front() else {
        return false;
    };

    let frame_pts = frame.pts;
    release_video_texture(session, frame.texture_handle);
    record_video_drop(session, tick_result, frame_pts, reason);
    tracing::debug!(
        pts_ms = frame_pts.as_millis(),
        ?reason,
        "Dropping queued video frame"
    );
    true
}

/// Сохраняет самый поздний pre-target frame для final seek near EOF.
fn replace_seek_preroll_fallback_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    frame: video_core::DecodedFrame,
) {
    if let Some(replaced_frame) = session.replace_seek_preroll_fallback_frame(frame) {
        release_video_texture(session, replaced_frame.texture_handle);
        record_video_drop(
            session,
            tick_result,
            replaced_frame.pts,
            PlayerVideoDropReason::SeekPreroll,
        );
    }
}

/// Удаляет EOF fallback, когда точный target frame уже найден.
fn drop_seek_preroll_fallback_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
) {
    if let Some(frame) = session.take_seek_preroll_fallback_frame() {
        release_video_texture(session, frame.texture_handle);
        record_video_drop(
            session,
            tick_result,
            frame.pts,
            PlayerVideoDropReason::SeekPreroll,
        );
    }
}

/// Делает первый queued frame текущим present frame.
fn present_front_queued_video_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
) -> bool {
    let Some(frame) = session.pipeline.pop_queued_video_frame_front() else {
        return false;
    };

    tracing::debug!(
        pts_ms = frame.pts.as_millis(),
        "Presenting scheduled video frame"
    );
    let frame_pts = frame.pts;
    if let Some(old_frame) = session.pipeline.replace_present_video_frame(frame) {
        release_video_texture(session, old_frame.texture_handle);
    }
    session.note_presented_frame_for_seek(frame_pts);
    tick_result.record_presented_video_frame();
    true
}

/// Показывает свежий pre-target frame, если final seek дошёл до EOF без target frame-а.
fn present_seek_preroll_fallback_after_eof(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
) -> bool {
    if !seek_preroll_fallback_ready_after_eof(session) {
        return false;
    }

    let Some(frame) = session.take_seek_preroll_fallback_frame() else {
        return false;
    };

    let frame_pts = frame.pts;
    tracing::debug!(
        pts_ms = frame_pts.as_millis(),
        "Presenting final seek EOF fallback frame"
    );
    if let Some(old_frame) = session.pipeline.replace_present_video_frame(frame) {
        release_video_texture(session, old_frame.texture_handle);
    }
    session.note_presented_seek_eof_fallback_frame(frame_pts);
    tick_result.record_presented_video_frame();
    true
}

/// Проверяет, что после EOF больше нет decoder work, способного дать точный target frame.
fn seek_preroll_fallback_ready_after_eof(session: &PlayerSession) -> bool {
    if session.active_final_seek_target().is_none() || !session.is_eof_draining() {
        return false;
    }

    if !session.pipeline.has_seek_preroll_fallback_video_frame() {
        return false;
    }

    if !session.pipeline.video_present_queue_is_empty()
        || !session.pipeline.pending_video_packet_is_empty()
        || session.pipeline.video_decode_in_flight_packets() > 0
    {
        return false;
    }

    session
        .pipeline
        .video_decoder_packet_queue_depth()
        .is_none_or(|packet_queue_depth| packet_queue_depth == 0)
}

/// Повторно показывает текущий кадр и учитывает это в telemetry result.
fn repeat_present_video_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    pause_reason: Option<PipelinePauseReason>,
) {
    if session.pipeline.has_present_video_frame() {
        tick_result.record_repeated_video_frame();
        session.record_repeated_video_frame();
    }
    if session.pipeline.has_selected_video_track()
        && session.playback_state() == PlaybackState::Playing
        && let Some(pause_reason) = pause_reason
    {
        record_pipeline_pause(session, tick_result, pause_reason);
    }
}

/// Остаток bounded adaptive work внутри одного tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AdaptiveCatchUpBudget {
    /// Сколько дополнительных demux packets ещё можно прочитать.
    demux_packets: usize,

    /// Сколько дополнительных video packets ещё можно отправить decoder thread-у.
    video_packets: usize,

    /// Сколько дополнительных decoded frames ещё можно принять из decoder thread-а.
    decoded_frames: usize,
}

impl AdaptiveCatchUpBudget {
    /// Проверяет, остался ли хоть один вид catch-up work.
    #[must_use]
    const fn has_work(self) -> bool {
        self.demux_packets > 0 || self.video_packets > 0 || self.decoded_frames > 0
    }
}

/// Возвращает deadline дополнительного catch-up окна.
fn adaptive_catch_up_deadline(now: Instant, tick_config: &PlayerTickConfig) -> Option<Instant> {
    if tick_config.adaptive_catch_up_time_budget.is_zero() {
        return None;
    }

    now.checked_add(tick_config.adaptive_catch_up_time_budget)
}

/// Считает, сколько frame intervals worker потерял из-за задержки tick-а.
fn delayed_frame_count(session: &PlayerSession, tick_late_by: Duration) -> usize {
    if tick_late_by.is_zero() {
        return 0;
    }

    let frame_nanos = session
        .pipeline
        .video_frame_duration_estimate()
        .as_nanos()
        .max(1);
    let late_nanos = tick_late_by.as_nanos();
    let delayed_frames = late_nanos.saturating_add(frame_nanos.saturating_sub(1)) / frame_nanos;

    delayed_frames.min(usize::MAX as u128) as usize
}

/// Считает frame deficit, который adaptive catch-up должен попытаться закрыть.
fn adaptive_catch_up_frame_need(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_late_by: Duration,
) -> usize {
    let queue_depth = session.pipeline.video_present_queue_len();
    let target_queue_depth = video_present_queue_target(tick_config);
    let target_deficit = target_queue_depth.saturating_sub(queue_depth);
    let min_deficit = video_present_queue_min(tick_config).saturating_sub(queue_depth);
    let delayed_frames = delayed_frame_count(session, tick_late_by);
    let delayed_target_deficit = target_queue_depth
        .saturating_add(delayed_frames)
        .min(video_present_queue_limit(tick_config))
        .saturating_sub(queue_depth);

    target_deficit.max(min_deficit).max(delayed_target_deficit)
}

/// Проверяет, нужен ли adaptive catch-up и есть ли куда складывать decoded frames.
fn adaptive_catch_up_needed(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_late_by: Duration,
) -> bool {
    if !session.can_present_video() {
        return false;
    }

    if available_video_present_slots(session, tick_config) == 0 {
        return false;
    }

    adaptive_catch_up_frame_need(session, tick_config, tick_late_by) > 0
}

/// Формирует operation budgets для catch-up из user-configured базовых budgets.
fn adaptive_catch_up_budget(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_late_by: Duration,
) -> AdaptiveCatchUpBudget {
    let frame_need = adaptive_catch_up_frame_need(session, tick_config, tick_late_by)
        .min(available_video_present_slots(session, tick_config));

    AdaptiveCatchUpBudget {
        demux_packets: tick_config
            .max_demux_packets_per_tick
            .saturating_add(frame_need),
        video_packets: tick_config
            .max_video_packets_sent_per_tick
            .saturating_add(frame_need)
            .min(available_video_present_slots(session, tick_config)),
        decoded_frames: tick_config
            .max_decoded_video_frames_drained_per_tick
            .saturating_add(frame_need)
            .min(available_video_present_slots(session, tick_config)),
    }
}

/// Проверяет, есть ли запас surface/import slots для дополнительного decode work.
fn has_texture_capacity_for_catch_up(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    if !session.pipeline.can_send_video_decode_packets() {
        return false;
    }

    let Some(stats) = session.pipeline.video_decoder_resource_snapshot() else {
        return true;
    };

    stats.available_slots() > texture_slot_target_watermark(tick_config)
}

/// Делает один проход adaptive catch-up без смешивания scheduling и rendering.
fn run_adaptive_catch_up_pass(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
    budget: &mut AdaptiveCatchUpBudget,
    deadline: Instant,
) -> bool {
    let mut made_progress = false;

    if budget.decoded_frames > 0
        && session.pipeline.can_receive_decoded_video_frames()
        && available_video_present_slots(session, tick_config) > 0
    {
        let drain_budget = budget
            .decoded_frames
            .min(tick_config.max_decoded_video_frames_drained_per_tick);
        let drain_limits = video_decoder_io_limits(
            tick_config,
            drain_budget,
            0,
            video_decode_ahead_limit(tick_config),
        );
        let drained_frames =
            drain_decoded_video_frames(session, tick_result, drain_limits, Some(deadline));
        budget.decoded_frames = budget.decoded_frames.saturating_sub(drained_frames);
        made_progress |= drained_frames > 0;
    }

    if budget.video_packets > 0
        && session.is_demuxing_active()
        && has_texture_capacity_for_catch_up(session, tick_config)
    {
        let send_budget = budget
            .video_packets
            .min(tick_config.max_video_packets_sent_per_tick);
        let send_limits = video_decoder_io_limits(
            tick_config,
            0,
            send_budget,
            video_decode_ahead_limit(tick_config),
        );
        let sent_packets = send_pending_video_packets_to_decoder(
            session,
            tick_result,
            send_limits,
            Some(deadline),
        );
        budget.video_packets = budget.video_packets.saturating_sub(sent_packets);
        made_progress |= sent_packets > 0;
    }

    if budget.demux_packets > 0 && session.is_demuxing_active() {
        let demux_budget = budget
            .demux_packets
            .min(tick_config.max_demux_packets_per_tick);
        let demuxed_packets = read_demux_packets(
            session,
            tick_config,
            tick_result,
            demux_budget,
            Some(deadline),
        );
        budget.demux_packets = budget.demux_packets.saturating_sub(demuxed_packets);
        made_progress |= demuxed_packets > 0;

        if demuxed_packets > 0 {
            session.process_pending_audio_packets_with_buffer_limit(
                tick_config.audio_buffer_high_water_mark_ms,
            );
        }
    }

    made_progress
}

/// Догоняет pipeline после короткого latency spike, но только в bounded окне.
fn run_adaptive_catch_up(
    session: &mut PlayerSession,
    tick_context: PlayerTickContext,
    tick_result: &mut PlayerTickResult,
) {
    if !adaptive_catch_up_needed(session, &tick_context.config, tick_context.tick_late_by) {
        return;
    }

    let Some(deadline) = adaptive_catch_up_deadline(tick_context.now, &tick_context.config) else {
        return;
    };

    let mut budget =
        adaptive_catch_up_budget(session, &tick_context.config, tick_context.tick_late_by);

    while budget.has_work()
        && !catch_up_deadline_reached(Some(deadline))
        && adaptive_catch_up_needed(session, &tick_context.config, tick_context.tick_late_by)
    {
        let made_progress = run_adaptive_catch_up_pass(
            session,
            &tick_context.config,
            tick_result,
            &mut budget,
            deadline,
        );

        if !made_progress {
            break;
        }
    }
}

/// Обрабатывает pending video packets: приём кадров, backpressure и A/V sync.
fn process_pending_video_packets(
    session: &mut PlayerSession,
    tick_context: PlayerTickContext,
    tick_result: &mut PlayerTickResult,
) {
    let tick_config = &tick_context.config;

    session.release_stale_present_frame_for_final_seek_texture_pressure(
        texture_slot_min_watermark(tick_config),
    );

    let base_drain_budget = if session.can_present_video() {
        tick_config.max_decoded_video_frames_drained_per_tick
    } else {
        usize::MAX
    };
    let decoder_io_limits = video_decoder_io_limits(
        tick_config,
        base_drain_budget,
        tick_config.max_video_packets_sent_per_tick,
        video_decode_ahead_target(tick_config),
    );
    let decoder_io_progress = run_video_decoder_io(
        session,
        tick_result,
        decoder_io_limits,
        VideoDecoderIoContext::new(None, true, true),
    );
    trace!(
        decoded_frames_drained = decoder_io_progress.decoded_frames_drained,
        packets_sent = decoder_io_progress.packets_sent,
        "Decoder I/O pass completed"
    );

    let playback_can_present = session.can_present_video();
    if !playback_can_present {
        return;
    }

    run_adaptive_catch_up(session, tick_context, tick_result);
    trim_video_present_queue(session, tick_config, tick_result);
    let scheduler_started_at = Instant::now();

    if present_seek_preroll_fallback_after_eof(session, tick_result) {
        session.record_pipeline_latency(
            PipelineLatencyStage::WorkerScheduler,
            scheduler_started_at.elapsed(),
            None,
            None,
        );
        return;
    }

    if !session.pipeline.video_present_queue_is_empty() {
        tracing::debug!(
            queue_len = session.pipeline.video_present_queue_len(),
            "A/V sync: processing frame queue"
        );
    }

    let audio_now = session.audio_clock_now();
    session
        .pipeline
        .note_audio_clock_sample(audio_now, tick_context.now);

    let audio_stall_elapsed = session.pipeline.audio_clock_stalled_for(tick_context.now);
    let audio_stalled = audio_now >= tick_config.audio_stall_min_position
        && audio_stall_elapsed >= tick_config.audio_stall_timeout;

    if audio_stalled {
        tracing::debug!(
            audio_ms = audio_now.as_secs_f64() * 1000.0,
            stalled_ms = audio_stall_elapsed.as_millis(),
            queue_len = session.pipeline.video_present_queue_len(),
            "A/V sync: audio stalled"
        );

        if !present_front_queued_video_frame(session, tick_result) {
            repeat_present_video_frame(
                session,
                tick_result,
                Some(PipelinePauseReason::DecoderStarvation),
            );
        }
        session.record_pipeline_latency(
            PipelineLatencyStage::WorkerScheduler,
            scheduler_started_at.elapsed(),
            None,
            None,
        );
        return;
    }

    let presentation_now = session.presentation_clock_position_at(tick_context.now);
    let target_media_time = target_media_time_for_present(session, tick_config, presentation_now);
    let present_window = video_present_window(session, tick_config);
    let late_drop_grace = video_late_drop_grace(session, tick_config);

    while should_drop_front_frame_as_late(
        session.pipeline.front_and_next_queued_video_frames(),
        target_media_time,
        late_drop_grace,
    ) {
        if session
            .pipeline
            .front_queued_video_frame()
            .is_some_and(|frame| session.active_seek_frame_ready_for_scheduler(frame.pts))
        {
            break;
        }

        if !drop_front_queued_video_frame(session, tick_result, PlayerVideoDropReason::Late) {
            break;
        }
    }

    let Some(frame) = session.pipeline.front_queued_video_frame() else {
        repeat_present_video_frame(
            session,
            tick_result,
            Some(PipelinePauseReason::DecoderStarvation),
        );
        session.record_pipeline_latency(
            PipelineLatencyStage::WorkerScheduler,
            scheduler_started_at.elapsed(),
            None,
            None,
        );
        return;
    };

    let diff_ms = frame.pts.as_secs_f64() * 1000.0 - target_media_time.as_secs_f64() * 1000.0;
    let force_present_for_seek = session.active_seek_frame_ready_for_scheduler(frame.pts);
    if !force_present_for_seek
        && should_wait_for_front_frame(frame.pts, target_media_time, present_window)
    {
        trace!(
            pts_ms = frame.pts.as_millis(),
            target_ms = target_media_time.as_millis(),
            diff_ms,
            window_ms = present_window.as_millis(),
            "A/V scheduler: waiting for target media time"
        );
        record_pipeline_pause(session, tick_result, PipelinePauseReason::SyncWaiting);
        repeat_present_video_frame(session, tick_result, None);
        session.record_pipeline_latency(
            PipelineLatencyStage::WorkerScheduler,
            scheduler_started_at.elapsed(),
            None,
            None,
        );
        return;
    }

    tracing::debug!(
        pts_ms = frame.pts.as_millis(),
        audio_ms = audio_now.as_millis(),
        clock_ms = presentation_now.as_millis(),
        target_ms = target_media_time.as_millis(),
        diff_ms,
        window_ms = present_window.as_millis(),
        force_present_for_seek,
        "A/V scheduler: frame selected"
    );
    present_front_queued_video_frame(session, tick_result);
    session.record_pipeline_latency(
        PipelineLatencyStage::WorkerScheduler,
        scheduler_started_at.elapsed(),
        None,
        None,
    );

    run_adaptive_catch_up(session, tick_context, tick_result);
}

/// Записывает drop одновременно в tick telemetry и session diagnostics.
fn record_video_drop(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    pts: Duration,
    reason: PlayerVideoDropReason,
) {
    session.record_video_drop(Some(pts), reason);
    tick_result.record_dropped_video_frame(pts, reason);
}

/// Записывает pipeline pause одновременно в tick telemetry и session diagnostics.
fn record_pipeline_pause(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    reason: PipelinePauseReason,
) {
    session.record_pipeline_pause(reason);
    tick_result.record_pipeline_pause(reason);
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use bytes::Bytes;
    use codec_core::{VideoCodec, VideoDecodeRequirement};

    use super::video_decoder_io::{
        accept_video_packet_for_decoder_bootstrap, drain_decoded_video_frames,
        pending_video_packet_generation_drop_reason, player_error_from_decode_thread_error,
        send_pending_video_packets_to_decoder,
    };
    use super::*;
    use crate::{
        DecodeBackpressureReason, DecodeSendError, DecodeThreadError, FrameCounters,
        PlaybackPipeline, PlayerAudioClock, PlayerAudioOutput, PlayerCommand, PlayerDecodePacket,
        PlayerSession, PresentFrameResourceProviderHandle,
    };

    /// Empty demuxer для проверки admission без реального container backend-а.
    struct EmptyDemuxer {
        /// Track list хранится внутри fake, чтобы выполнить `Demuxer::tracks` contract.
        tracks: Vec<media_core::TrackInfo>,
    }

    impl EmptyDemuxer {
        /// Создаёт demuxer без packets: admission tests не читают из него данные.
        fn new() -> Self {
            Self { tracks: Vec::new() }
        }
    }

    impl media_core::Demuxer for EmptyDemuxer {
        /// Возвращает стабильный track list для boundary contract-а.
        fn tracks(&self) -> &[media_core::TrackInfo] {
            &self.tracks
        }

        /// Duration в этих тестах не участвует в admission policy.
        fn duration(&self) -> Option<Duration> {
            None
        }

        /// Packet-ов нет: тесты проверяют только `demux_work_available`.
        fn next_packet(&mut self) -> anyhow::Result<Option<media_core::Packet>> {
            Ok(None)
        }

        /// Seek возвращает requested point, чтобы fake оставался честным `Demuxer`.
        fn seek(&mut self, timestamp: Duration) -> anyhow::Result<media_core::DemuxSeekResult> {
            Ok(media_core::DemuxSeekResult {
                requested_position: media_core::MediaTime::from_duration(timestamp),
                actual_position: media_core::MediaTime::from_duration(timestamp),
                actual_track_timestamp: None,
            })
        }
    }

    /// Fake clock для output boundary; admission tests читают только buffer level.
    struct FixedAudioClock;

    impl PlayerAudioClock for FixedAudioClock {
        /// Clock position в этих тестах не участвует.
        fn now(&self) -> Duration {
            Duration::ZERO
        }

        /// Reset не имеет side effects для admission policy.
        fn reset(&self) {}

        /// Underrun diagnostics в этих тестах не участвуют.
        fn underrun_callbacks(&self) -> u64 {
            0
        }
    }

    /// Fake audio output с фиксированным buffer level для high-water admission tests.
    struct FixedAudioOutput {
        /// Clock нужен output boundary, хотя demux admission его не читает.
        clock: Arc<FixedAudioClock>,

        /// Уровень audio buffer, который видит `PlayerSession::audio_buffer_level_ms`.
        buffer_level_ms: f64,
    }

    impl FixedAudioOutput {
        /// Создаёт output с заданным buffer level.
        fn new(buffer_level_ms: f64) -> Self {
            Self {
                clock: Arc::new(FixedAudioClock),
                buffer_level_ms,
            }
        }
    }

    impl PlayerAudioOutput for FixedAudioOutput {
        /// Тестовый output принимает samples и сообщает количество записанных значений.
        fn write_samples(&mut self, samples: &[f32]) -> u64 {
            samples.len() as u64
        }

        /// Запуск stream-а в admission tests не имеет side effects.
        fn play(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        /// Pause в admission tests не имеет side effects.
        fn pause(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        /// Clear подтверждает ровно тот generation, который попросил caller.
        fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
            Ok(generation)
        }

        /// Volume не влияет на demux admission.
        fn set_volume(&mut self, _volume: f32) {}

        /// Возвращает scripted buffer level.
        fn buffer_level_ms(&self) -> f64 {
            self.buffer_level_ms
        }

        /// Возвращает fake clock для соблюдения audio output boundary.
        fn clock(&self) -> Arc<dyn PlayerAudioClock> {
            let clock: Arc<dyn PlayerAudioClock> = self.clock.clone();
            clock
        }
    }

    /// Подключает empty demuxer и оставляет track selection под контролем теста.
    fn install_empty_demuxer(session: &mut PlayerSession) {
        session.pipeline.install_opened_media(
            Box::new(EmptyDemuxer::new()),
            None,
            None,
            Vec::new(),
        );
    }

    /// Подключает fake output с явно заданным buffer level.
    fn install_fixed_audio_output(session: &mut PlayerSession, buffer_level_ms: f64) {
        session
            .pipeline
            .install_audio_output_for_tests(Box::new(FixedAudioOutput::new(buffer_level_ms)));
    }

    /// Добавляет один pending audio packet на выбранной generation.
    fn enqueue_pending_audio_packet(session: &mut PlayerSession, track_id: TrackId) {
        let generation = session.pipeline.seek_generation();
        session
            .pipeline
            .enqueue_pending_audio_packet(PendingAudioPacket::new(
                track_id,
                Duration::ZERO,
                None,
                None,
                generation,
                Bytes::from_static(b"audio"),
            ));
    }

    /// Создаёт минимальный VP9 video track для packet refinement tests.
    fn video_track_for_tests(track_id: u32) -> media_core::TrackInfo {
        media_core::TrackInfo {
            id: TrackId::new(track_id),
            kind: TrackKind::Video,
            codec_id: "V_VP9".to_string(),
            codec_private: None,
            time_base: media_core::TimeBase::new(1, 1_000),
            duration: Some(Duration::from_secs(30)),
            sample_rate: None,
            channels: None,
            video: None,
        }
    }

    /// Создаёт test frame без реальных GPU resources с явным decoded contract.
    fn decoded_frame_with_format(
        pts: Duration,
        handle: u64,
        format: video_core::DecodedPixelFormat,
    ) -> video_core::DecodedFrame {
        let (bit_depth, memory_path) = match format {
            video_core::DecodedPixelFormat::Nv12 => (
                codec_core::BitDepth::Eight,
                video_core::FrameMemoryPath::DmaBufZeroCopy,
            ),
            video_core::DecodedPixelFormat::P010 => (
                codec_core::BitDepth::Ten,
                video_core::FrameMemoryPath::DmaBufZeroCopy,
            ),
            video_core::DecodedPixelFormat::Rgba8 => {
                panic!("RGBA8 is not a production decoded video test format")
            }
        };

        video_core::DecodedFrame {
            generation: 0,
            pts,
            format,
            bit_depth,
            chroma: codec_core::ChromaSubsampling::Yuv420,
            memory_path,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: codec_core::VideoColorMetadata::sdr_bt709_limited(),
            texture_handle: video_core::FrameTextureHandle(handle),
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

    /// Создаёт текущий production NV12 test frame без привязки scheduler assertions к формату.
    fn decoded_frame(pts: Duration, handle: u64) -> video_core::DecodedFrame {
        decoded_frame_with_format(pts, handle, video_core::DecodedPixelFormat::Nv12)
    }

    /// Формирует decoder I/O limits для focused tests без чтения config внутри scheduler-а.
    fn decoder_io_limits_for_tests(
        max_frames_to_drain: usize,
        max_packets_to_send: usize,
    ) -> VideoDecoderIoLimits {
        video_decoder_io_limits(
            &PlayerTickConfig::default(),
            max_frames_to_drain,
            max_packets_to_send,
            Duration::from_millis(250),
        )
    }

    /// Добавляет pending video packet для выбранного track-а текущей generation.
    fn enqueue_selected_video_packet(
        session: &mut PlayerSession,
        pts: Duration,
        encoded_bytes: Bytes,
        keyframe: impl Into<PacketKeyframe>,
    ) {
        session
            .pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(1),
                pts,
                session.pipeline.seek_generation(),
                encoded_bytes,
                keyframe,
            ));
    }

    /// Fake decoder, который записывает отправленные packets без реального backend-а.
    #[derive(Clone, Default)]
    struct RecordingVideoDecoderThread {
        /// Packet log нужен тесту, чтобы отличить drop от отправки в decoder.
        sent_packets: Arc<Mutex<Vec<PlayerDecodePacket>>>,

        /// Scripted send results позволяют различить backpressure и fatal send.
        send_results: Arc<Mutex<VecDeque<Result<(), DecodeSendError>>>>,

        /// Очередь decoded frames для проверки decoder receive boundary.
        decoded_frames: Arc<Mutex<VecDeque<video_core::DecodedFrame>>>,

        /// Texture handles, которые session вернула decoder boundary.
        released_handles: Arc<Mutex<Vec<video_core::FrameTextureHandle>>>,
    }

    impl RecordingVideoDecoderThread {
        /// Создаёт пустой fake decoder для проверки routing/drop behavior.
        fn new() -> Self {
            Self::default()
        }

        /// Возвращает snapshot отправленных packets без раскрытия mutex наружу.
        fn sent_packets(&self) -> Vec<PlayerDecodePacket> {
            self.sent_packets
                .lock()
                .expect("recording decoder packet log lock")
                .clone()
        }

        /// Кладёт synthetic decoded frame в receive queue fake decoder-а.
        fn push_decoded_frame(&self, frame: video_core::DecodedFrame) {
            self.decoded_frames
                .lock()
                .expect("recording decoder frame queue lock")
                .push_back(frame);
        }

        /// Задаёт результат следующей отправки packet-а в decoder boundary.
        fn push_send_result(&self, result: Result<(), DecodeSendError>) {
            self.send_results
                .lock()
                .expect("recording decoder send result lock")
                .push_back(result);
        }

        /// Возвращает handles, освобождённые через decoder release path.
        fn released_handles(&self) -> Vec<video_core::FrameTextureHandle> {
            self.released_handles
                .lock()
                .expect("recording decoder release log lock")
                .clone()
        }
    }

    impl video_core::VideoDecoderThreadHandle for RecordingVideoDecoderThread {
        type ResourceProvider = PresentFrameResourceProviderHandle;

        fn backend_name(&self) -> &'static str {
            "Recording fake decoder"
        }

        fn send_packet(&self, packet: PlayerDecodePacket) -> Result<(), DecodeSendError> {
            let scripted_result = self
                .send_results
                .lock()
                .expect("recording decoder send result lock")
                .pop_front();
            if let Some(result) = scripted_result {
                if result.is_ok() {
                    self.sent_packets
                        .lock()
                        .expect("recording decoder packet log lock")
                        .push(packet);
                }
                return result;
            }

            self.sent_packets
                .lock()
                .expect("recording decoder packet log lock")
                .push(packet);
            Ok(())
        }

        fn release_frame(&self, handle: video_core::FrameTextureHandle) {
            self.released_handles
                .lock()
                .expect("recording decoder release log lock")
                .push(handle);
        }

        fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
            self.decoded_frames
                .lock()
                .expect("recording decoder frame queue lock")
                .pop_front()
        }

        fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
            None
        }

        fn try_recv_error(&self) -> Option<DecodeThreadError> {
            None
        }

        fn flush(&self) -> anyhow::Result<()> {
            Ok(())
        }

        fn resource_provider(&self) -> PresentFrameResourceProviderHandle {
            panic!("recording fake decoder does not provide renderer resources")
        }

        fn decoder_resource_snapshot(&self) -> Option<crate::DecoderResourceSnapshot> {
            None
        }

        fn packet_queue_depth(&self) -> usize {
            0
        }

        fn drain_completed_packet_count(&self) -> usize {
            0
        }
    }

    /// Создаёт capabilities, где аппаратный backend принимает только VP9 Profile 0.
    fn capabilities_with_vp9_profile0_for_decoder_io() -> capability_core::SystemCapabilities {
        let backend_id = codec_core::DecodeBackendId::new("decoder_io_test_backend")
            .expect("test backend id должен быть валидным");

        capability_core::SystemCapabilities {
            schema_version: capability_core::CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds: 1,
            video_backends: vec![capability_core::BackendCapabilities {
                backend_id: backend_id.clone(),
                display_name: "Decoder I/O test backend".to_string(),
                status: capability_core::BackendProbeStatus::Available,
                driver: capability_core::BackendDriverInfo::default(),
                supported_video_decode_formats: vec![codec_core::SupportedVideoDecodeFormat {
                    codec: VideoCodec::Vp9,
                    profile: codec_core::VideoProfile::Vp9(codec_core::Vp9Profile::Profile0),
                    bit_depth: codec_core::BitDepth::Eight,
                    chroma: codec_core::ChromaSubsampling::Yuv420,
                    max_width: Some(1920),
                    max_height: Some(1080),
                    max_fps: None,
                    hdr_input: false,
                    backend: backend_id,
                }],
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                export_paths: vec![capability_core::VideoExportPath::DmaBuf],
                p010_storage_layouts: Vec::new(),
                diagnostics: Vec::new(),
            }],
            render_backends: vec![render_core::RenderCapabilities::wgpu_nv12(Some(4096))],
        }
    }

    /// Собирает минимальный VP9 keyframe header для packet refinement tests.
    fn vp9_profile2_10bit_keyframe_for_tests() -> Bytes {
        Bytes::from_static(&[0x92, 0x49, 0x83, 0x42, 0x50, 0x77, 0xf8, 0x43, 0x78])
    }

    #[test]
    fn scheduler_presents_first_ready_frame() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(Duration::ZERO, 1));

        let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

        assert_eq!(tick_result.video_frames_presented, 1);
        assert_eq!(
            session
                .pipeline
                .present_video_frame()
                .map(|frame| frame.pts),
            Some(Duration::ZERO)
        );
        assert!(session.pipeline.video_present_queue_is_empty());
    }

    #[test]
    fn scheduler_waits_for_future_frame() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(Duration::from_secs(1), 1));

        let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

        assert_eq!(tick_result.video_frames_presented, 0);
        assert!(session.pipeline.present_video_frame().is_none());
        assert_eq!(session.pipeline.video_present_queue_len(), 1);
    }

    #[test]
    fn scheduler_presents_frame_at_pts_minus_present_lead() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.update_current_position(Duration::from_millis(8));
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(Duration::from_millis(16), 1));

        let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

        assert_eq!(tick_result.video_frames_presented, 1);
        assert_eq!(
            session
                .pipeline
                .present_video_frame()
                .map(|frame| frame.pts),
            Some(Duration::from_millis(16))
        );
    }

    #[test]
    fn scheduler_uses_fallback_position_when_audio_clock_is_absent() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.update_current_position(Duration::from_millis(100));
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(Duration::from_millis(100), 1));

        let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

        assert_eq!(tick_result.video_frames_presented, 1);
        assert_eq!(
            session
                .pipeline
                .present_video_frame()
                .map(|frame| frame.pts),
            Some(Duration::from_millis(100))
        );
        assert!(session.pipeline.video_present_queue_is_empty());
    }

    #[test]
    fn worker_wakeup_for_5994_cadence_uses_pts_minus_present_lead_deadline() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.update_current_position(Duration::ZERO);
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(Duration::from_micros(16_683), 1));

        let plan = session.worker_wakeup_plan(
            Instant::now(),
            &PlayerTickConfig::default(),
            Duration::from_millis(2),
            Duration::from_millis(250),
        );

        assert_eq!(plan.reason, WorkerWakeupReason::FramePtsDeadline);
        let planned_delay = plan.delay.expect("queued frame should create deadline");
        assert!(
            planned_delay > Duration::from_millis(6),
            "worker woke too early for a PTS-derived lead deadline: {planned_delay:?}"
        );
        assert!(
            planned_delay < Duration::from_millis(12),
            "worker must not wait until the exact PTS and miss render handoff: {planned_delay:?}"
        );
    }

    #[test]
    fn worker_wakeup_for_24_and_30_fps_is_not_fixed_60hz_polling() {
        for frame_duration in [Duration::from_millis(33), Duration::from_millis(41)] {
            let mut session = PlayerSession::new();
            session.dispatch_command(PlayerCommand::Play).unwrap();
            session.update_current_position(Duration::ZERO);
            session.pipeline.enqueue_queued_video_frame(decoded_frame(
                frame_duration,
                frame_duration.as_micros() as u64,
            ));

            let plan = session.worker_wakeup_plan(
                Instant::now(),
                &PlayerTickConfig::default(),
                Duration::from_millis(2),
                Duration::from_millis(250),
            );

            assert_eq!(plan.reason, WorkerWakeupReason::FramePtsDeadline);
            assert!(
                plan.delay
                    .is_some_and(|delay| delay > Duration::from_millis(20))
            );
        }
    }

    #[test]
    fn worker_wakeup_does_not_busy_spin_when_present_queue_is_healthy() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.update_current_position(Duration::ZERO);
        let tick_config = PlayerTickConfig {
            target_video_present_queue: 4,
            max_video_present_queue: 8,
            ..PlayerTickConfig::default()
        };

        for frame_index in 0..video_present_queue_target(&tick_config) {
            session.pipeline.enqueue_queued_video_frame(decoded_frame(
                Duration::from_millis(100 + frame_index as u64 * 17),
                frame_index as u64,
            ));
        }
        session
            .pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(1),
                Duration::from_millis(180),
                session.pipeline.seek_generation(),
                Bytes::from_static(b"future-video"),
                true,
            ));

        let plan = session.worker_wakeup_plan(
            Instant::now(),
            &tick_config,
            Duration::from_millis(2),
            Duration::from_millis(250),
        );

        assert_eq!(plan.reason, WorkerWakeupReason::FramePtsDeadline);
        assert!(plan.delay.is_some_and(|delay| !delay.is_zero()));
    }

    #[test]
    fn worker_wakeup_uses_audio_refill_deadline_before_coarse_progress() {
        let mut session = PlayerSession::new();
        let audio_track_id = TrackId::new(2);
        let tick_config = PlayerTickConfig {
            audio_buffer_high_water_mark_ms: 200.0,
            ..PlayerTickConfig::default()
        };

        install_empty_demuxer(&mut session);
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.pipeline.select_audio_track(audio_track_id);
        install_fixed_audio_output(&mut session, 250.0);

        let plan = session.worker_wakeup_plan(
            Instant::now(),
            &tick_config,
            Duration::from_millis(2),
            Duration::from_millis(250),
        );

        assert_eq!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
        assert_eq!(plan.delay, Some(Duration::from_millis(50)));
    }

    #[test]
    fn worker_wakeup_prefers_audio_refill_deadline_over_later_video_frame() {
        let mut session = PlayerSession::new();
        let audio_track_id = TrackId::new(2);
        let tick_config = PlayerTickConfig {
            audio_buffer_high_water_mark_ms: 200.0,
            ..PlayerTickConfig::default()
        };

        install_empty_demuxer(&mut session);
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.update_current_position(Duration::ZERO);
        session.pipeline.select_audio_track(audio_track_id);
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(Duration::from_millis(500), 1));
        install_fixed_audio_output(&mut session, 250.0);

        let plan = session.worker_wakeup_plan(
            Instant::now(),
            &tick_config,
            Duration::from_millis(2),
            Duration::from_millis(250),
        );

        assert_eq!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
        assert_eq!(plan.delay, Some(Duration::from_millis(50)));
    }

    #[test]
    fn worker_wakeup_uses_audio_refill_deadline_while_draining_pending_audio_tail() {
        let mut session = PlayerSession::new();
        let audio_track_id = TrackId::new(2);
        let tick_config = PlayerTickConfig {
            audio_buffer_high_water_mark_ms: 200.0,
            ..PlayerTickConfig::default()
        };

        install_empty_demuxer(&mut session);
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.pipeline.select_audio_track(audio_track_id);
        enqueue_pending_audio_packet(&mut session, audio_track_id);
        install_fixed_audio_output(&mut session, 250.0);
        session.enter_eof_drain();

        let plan = session.worker_wakeup_plan(
            Instant::now(),
            &tick_config,
            Duration::from_millis(2),
            Duration::from_millis(250),
        );

        assert_eq!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
        assert_eq!(plan.delay, Some(Duration::from_millis(50)));
    }

    #[test]
    fn worker_wakeup_records_front_frame_pts_diff_for_diagnostics() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.update_current_position(Duration::from_millis(10));
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(Duration::from_millis(30), 1));

        let plan = session.worker_wakeup_plan(
            Instant::now(),
            &PlayerTickConfig::default(),
            Duration::from_millis(2),
            Duration::from_millis(250),
        );
        let diagnostics = plan.diagnostics(Duration::from_millis(3));

        assert_eq!(
            diagnostics.reason,
            Some(WorkerWakeupReason::FramePtsDeadline)
        );
        assert_eq!(diagnostics.tick_late_by, Duration::from_millis(3));
        assert!(diagnostics.frame_timing.is_some_and(|timing| {
            timing.front_frame_pts == Duration::from_millis(30)
                && timing.front_frame_delta_from_target_us > 0
        }));
    }

    #[test]
    fn decoder_readiness_poll_keeps_seek_decoder_inflight_from_idling() {
        let poll_needed = decoder_readiness_poll_needed_for_state(
            true,
            0,
            video_present_queue_target(&PlayerTickConfig::default()),
            false,
            false,
            false,
            true,
            true,
        );

        assert!(poll_needed);
    }

    #[test]
    fn decoder_readiness_poll_keeps_seek_decoder_inflight_when_present_queue_is_full() {
        let tick_config = PlayerTickConfig::default();
        let present_queue_target = video_present_queue_target(&tick_config);
        let poll_needed = decoder_readiness_poll_needed_for_state(
            true,
            present_queue_target,
            present_queue_target,
            false,
            false,
            false,
            true,
            true,
        );

        assert!(poll_needed);
    }

    #[test]
    fn decoder_readiness_poll_ignores_decoder_inflight_outside_seek() {
        let poll_needed = decoder_readiness_poll_needed_for_state(
            true,
            0,
            video_present_queue_target(&PlayerTickConfig::default()),
            false,
            false,
            false,
            true,
            false,
        );

        assert!(!poll_needed);
    }

    #[test]
    fn scheduler_late_drop_requires_next_frame_to_be_ready() {
        let mut queue = VecDeque::new();
        queue.push_back(decoded_frame(Duration::from_millis(0), 1));
        queue.push_back(decoded_frame(Duration::from_millis(33), 2));

        let should_drop = should_drop_front_frame_as_late(
            queue.front().zip(queue.get(1)),
            Duration::from_millis(70),
            Duration::from_millis(16),
        );

        assert!(should_drop);
    }

    #[test]
    fn scheduler_keeps_late_frame_without_replacement() {
        let mut queue = VecDeque::new();
        queue.push_back(decoded_frame(Duration::from_millis(0), 1));

        let should_drop = should_drop_front_frame_as_late(
            queue.front().zip(queue.get(1)),
            Duration::from_millis(70),
            Duration::from_millis(16),
        );

        assert!(!should_drop);
    }

    #[test]
    fn scheduler_5994_vs_60hz_phase_difference_is_not_late_drop() {
        let mut queue = VecDeque::new();
        queue.push_back(decoded_frame(Duration::ZERO, 1));
        queue.push_back(decoded_frame(Duration::from_micros(16_683), 2));

        let should_drop = should_drop_front_frame_as_late(
            queue.front().zip(queue.get(1)),
            Duration::from_micros(16_667),
            Duration::from_micros(33_366),
        );

        assert!(!should_drop);
    }

    #[test]
    fn scheduler_repeats_current_frame_when_queue_is_empty() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .pipeline
            .set_present_video_frame(decoded_frame(Duration::ZERO, 1));

        let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

        assert_eq!(tick_result.video_frames_repeated, 1);
        assert_eq!(
            session
                .pipeline
                .present_video_frame()
                .map(|frame| frame.pts),
            Some(Duration::ZERO)
        );
        assert_eq!(
            session
                .snapshot_with_frame_counters(crate::FrameCounters::default())
                .diagnostics
                .repeated_video_frames,
            1
        );
        assert_eq!(
            session
                .snapshot_with_frame_counters(crate::FrameCounters::default())
                .diagnostics
                .drops
                .late,
            0
        );
    }

    #[test]
    fn scheduler_preserves_p010_boundary_frame_without_format_branching() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame_with_format(
                Duration::ZERO,
                10,
                video_core::DecodedPixelFormat::P010,
            ));

        let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

        assert_eq!(tick_result.video_frames_presented, 1);
        assert_eq!(
            session
                .pipeline
                .present_video_frame()
                .map(|frame| frame.format),
            Some(video_core::DecodedPixelFormat::P010)
        );
        assert!(session.pipeline.video_present_queue_is_empty());
    }

    #[test]
    fn decoder_thread_error_maps_to_runtime_player_error() {
        let decode_thread_error = DecodeThreadError::new("P010 DMA-BUF zero-copy import failed");

        let player_error = player_error_from_decode_thread_error(&decode_thread_error);

        assert_eq!(player_error.kind, PlayerErrorKind::RuntimeError);
        assert!(
            player_error
                .message
                .contains("P010 DMA-BUF zero-copy import failed")
        );
    }

    #[test]
    fn audio_demux_catchup_requires_selected_audio_and_low_buffer() {
        assert!(audio_demux_catchup_needed_for_level(true, Some(40.0), 50.0));
        assert!(!audio_demux_catchup_needed_for_level(
            true,
            Some(60.0),
            50.0
        ));
        assert!(!audio_demux_catchup_needed_for_level(
            false,
            Some(40.0),
            50.0
        ));
        assert!(!audio_demux_catchup_needed_for_level(true, None, 50.0));
    }

    #[test]
    fn scheduler_requests_catch_up_after_one_delayed_tick() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        let tick_config = PlayerTickConfig::default();
        let frame_duration = session.pipeline.video_frame_duration_estimate();

        for frame_index in 0..video_present_queue_target(&tick_config) {
            session.pipeline.enqueue_queued_video_frame(decoded_frame(
                frame_duration.mul_f64(frame_index as f64),
                frame_index as u64,
            ));
        }

        assert_eq!(
            adaptive_catch_up_frame_need(&session, &tick_config, frame_duration),
            1
        );
        assert!(adaptive_catch_up_needed(
            &session,
            &tick_config,
            frame_duration
        ));
    }

    #[test]
    fn scheduler_allows_decoder_burst_after_delay() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        let tick_config = PlayerTickConfig {
            max_decoded_video_frames_drained_per_tick: 2,
            max_video_present_queue: 8,
            min_video_present_queue: 2,
            target_video_present_queue: 4,
            ..PlayerTickConfig::default()
        };

        let budget = adaptive_catch_up_budget(
            &session,
            &tick_config,
            session.pipeline.video_frame_duration_estimate(),
        );

        assert!(budget.decoded_frames > tick_config.max_decoded_video_frames_drained_per_tick);
        assert_eq!(budget.decoded_frames, 7);
    }

    #[test]
    fn scheduler_requests_catch_up_when_present_queue_is_near_empty() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        let tick_config = PlayerTickConfig {
            min_video_present_queue: 2,
            target_video_present_queue: 4,
            max_video_present_queue: 8,
            ..PlayerTickConfig::default()
        };
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(Duration::ZERO, 1));

        assert_eq!(
            adaptive_catch_up_frame_need(&session, &tick_config, Duration::ZERO),
            3
        );
        assert!(adaptive_catch_up_needed(
            &session,
            &tick_config,
            Duration::ZERO
        ));
    }

    #[test]
    fn scheduler_does_not_catch_up_when_present_queue_is_full() {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        let tick_config = PlayerTickConfig {
            max_video_present_queue: 4,
            min_video_present_queue: 2,
            target_video_present_queue: 3,
            ..PlayerTickConfig::default()
        };

        for frame_index in 0..video_present_queue_limit(&tick_config) {
            session.pipeline.enqueue_queued_video_frame(decoded_frame(
                Duration::from_millis(frame_index as u64),
                frame_index as u64,
            ));
        }

        assert!(!adaptive_catch_up_needed(
            &session,
            &tick_config,
            session.pipeline.video_frame_duration_estimate()
        ));
    }

    #[test]
    fn seek_generation_transition_is_not_counted_as_late_drop() {
        let mut pipeline = PlaybackPipeline::default();
        let stale_generation = pipeline.seek_generation();
        let current_generation = pipeline.begin_seek_generation();

        assert_eq!(
            pending_video_packet_generation_drop_reason(&pipeline, stale_generation),
            Some(PlayerVideoDropReason::StaleGeneration)
        );
        assert_eq!(
            pending_video_packet_generation_drop_reason(&pipeline, current_generation),
            None
        );
    }

    #[test]
    fn seek_video_preroll_is_capped_by_present_queue_capacity() {
        let tick_config = PlayerTickConfig {
            max_video_present_queue: 1,
            seek_resume_video_min_ready_frames: 8,
            ..PlayerTickConfig::default()
        };

        assert_eq!(
            effective_seek_resume_video_min_ready_frames(&tick_config),
            2
        );
    }

    #[test]
    fn audio_demux_catchup_uses_bounded_video_packet_limit() {
        let mut session = PlayerSession::new();
        let tick_config = PlayerTickConfig {
            max_pending_video_packets: 2,
            max_pending_video_packets_during_audio_catchup: 4,
            ..PlayerTickConfig::default()
        };

        for packet_index in 0..tick_config.max_pending_video_packets {
            session
                .pipeline
                .enqueue_pending_video_packet(PendingVideoPacket::new(
                    TrackId::new(1),
                    Duration::from_millis(packet_index as u64),
                    session.pipeline.seek_generation(),
                    Bytes::new(),
                    false,
                ));
        }

        assert!(!can_read_next_demux_packet_with_audio_priority(
            &session,
            &tick_config,
            false
        ));
        assert!(can_read_next_demux_packet_with_audio_priority(
            &session,
            &tick_config,
            true
        ));

        for packet_index in tick_config.max_pending_video_packets
            ..tick_config.max_pending_video_packets_during_audio_catchup
        {
            session
                .pipeline
                .enqueue_pending_video_packet(PendingVideoPacket::new(
                    TrackId::new(1),
                    Duration::from_millis(packet_index as u64),
                    session.pipeline.seek_generation(),
                    Bytes::new(),
                    false,
                ));
        }

        assert!(!can_read_next_demux_packet_with_audio_priority(
            &session,
            &tick_config,
            true
        ));
    }

    #[test]
    fn selected_audio_without_output_allows_demux_until_first_audio_packet() {
        let mut session = PlayerSession::new();
        let audio_track_id = TrackId::new(2);
        let tick_config = PlayerTickConfig {
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 2,
            ..PlayerTickConfig::default()
        };

        install_empty_demuxer(&mut session);
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.pipeline.select_audio_track(audio_track_id);
        session
            .pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(1),
                Duration::ZERO,
                session.pipeline.seek_generation(),
                Bytes::new(),
                true,
            ));

        assert!(can_read_next_demux_packet_with_audio_priority(
            &session,
            &tick_config,
            false
        ));
        assert!(demux_work_available(&session, &tick_config));

        enqueue_pending_audio_packet(&mut session, audio_track_id);

        assert!(!can_read_next_demux_packet_with_audio_priority(
            &session,
            &tick_config,
            false
        ));
        assert!(!demux_work_available(&session, &tick_config));
    }

    #[test]
    fn high_water_audio_blocks_more_demux_when_pending_audio_waits() {
        let mut session = PlayerSession::new();
        let audio_track_id = TrackId::new(2);
        let tick_config = PlayerTickConfig {
            audio_buffer_high_water_mark_ms: 100.0,
            ..PlayerTickConfig::default()
        };

        install_empty_demuxer(&mut session);
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.pipeline.select_audio_track(audio_track_id);
        install_fixed_audio_output(&mut session, 250.0);
        enqueue_pending_audio_packet(&mut session, audio_track_id);

        assert!(!can_read_next_demux_packet_with_audio_priority(
            &session,
            &tick_config,
            false
        ));
        assert!(!demux_work_available(&session, &tick_config));
        assert_eq!(
            demux_backpressure_reason(&session, &tick_config, false),
            Some(PipelinePauseReason::DemuxBackpressure)
        );
    }

    #[test]
    fn audio_only_high_water_blocks_demux_when_pending_audio_is_empty() {
        let mut session = PlayerSession::new();
        let audio_track_id = TrackId::new(2);
        let tick_config = PlayerTickConfig {
            audio_buffer_high_water_mark_ms: 100.0,
            ..PlayerTickConfig::default()
        };

        install_empty_demuxer(&mut session);
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.pipeline.select_audio_track(audio_track_id);
        install_fixed_audio_output(&mut session, 250.0);

        assert!(session.pipeline.pending_audio_packet_is_empty());
        assert!(!can_read_next_demux_packet_with_audio_priority(
            &session,
            &tick_config,
            false
        ));
        assert!(!demux_work_available(&session, &tick_config));
        assert_eq!(
            demux_backpressure_reason(&session, &tick_config, false),
            Some(PipelinePauseReason::DemuxBackpressure)
        );
    }

    #[test]
    fn low_water_audio_catchup_still_allows_bounded_demux() {
        let mut session = PlayerSession::new();
        let audio_track_id = TrackId::new(2);
        let tick_config = PlayerTickConfig {
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 2,
            audio_buffer_high_water_mark_ms: 300.0,
            audio_demux_low_water_mark_ms: 100.0,
            ..PlayerTickConfig::default()
        };

        install_empty_demuxer(&mut session);
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.pipeline.select_audio_track(audio_track_id);
        install_fixed_audio_output(&mut session, 40.0);
        session
            .pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(1),
                Duration::ZERO,
                session.pipeline.seek_generation(),
                Bytes::new(),
                true,
            ));

        assert!(audio_demux_catchup_needed(&session, &tick_config));
        assert!(!can_read_next_demux_packet_with_audio_priority(
            &session,
            &tick_config,
            false
        ));
        assert!(can_read_next_demux_packet_with_audio_priority(
            &session,
            &tick_config,
            true
        ));
        assert!(demux_work_available(&session, &tick_config));
    }

    #[test]
    fn video_queue_pressure_does_not_block_selected_audio_bootstrap() {
        let mut session = PlayerSession::new();
        let tick_config = PlayerTickConfig {
            max_video_present_queue: 1,
            min_video_present_queue: 1,
            target_video_present_queue: 1,
            ..PlayerTickConfig::default()
        };

        install_empty_demuxer(&mut session);
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.pipeline.select_audio_track(TrackId::new(2));
        session.pipeline.select_video_track(
            TrackId::new(1),
            VideoDecodeRequirement::new(VideoCodec::Vp9),
        );
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(Duration::ZERO, 1));

        assert!(demux_work_available(&session, &tick_config));
        assert!(immediate_pipeline_work_available(&session, &tick_config));
    }

    #[test]
    fn video_only_demux_admission_still_waits_for_present_queue() {
        let mut session = PlayerSession::new();
        let tick_config = PlayerTickConfig {
            max_video_present_queue: 1,
            min_video_present_queue: 1,
            target_video_present_queue: 1,
            ..PlayerTickConfig::default()
        };

        install_empty_demuxer(&mut session);
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.pipeline.select_video_track(
            TrackId::new(1),
            VideoDecodeRequirement::new(VideoCodec::Vp9),
        );
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(Duration::ZERO, 1));

        assert!(!can_read_next_demux_packet_with_audio_priority(
            &session,
            &tick_config,
            false
        ));
        assert!(!demux_work_available(&session, &tick_config));
        assert!(!immediate_pipeline_work_available(&session, &tick_config));
        assert_eq!(
            demux_backpressure_reason(&session, &tick_config, false),
            Some(PipelinePauseReason::WaitingForPresentQueue)
        );
    }

    #[test]
    fn demux_backpressure_reports_present_queue_reason() {
        let mut session = PlayerSession::new();
        let tick_config = PlayerTickConfig {
            max_video_present_queue: 1,
            ..PlayerTickConfig::default()
        };
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(Duration::ZERO, 1));

        let reason = demux_backpressure_reason(&session, &tick_config, false);

        assert_eq!(reason, Some(PipelinePauseReason::WaitingForPresentQueue));
    }

    #[test]
    fn demux_backpressure_reports_audio_priority_reason() {
        let mut session = PlayerSession::new();
        let tick_config = PlayerTickConfig {
            max_pending_video_packets: 2,
            max_pending_video_packets_during_audio_catchup: 4,
            ..PlayerTickConfig::default()
        };

        for packet_index in 0..audio_catchup_pending_video_limit(&tick_config) {
            session
                .pipeline
                .enqueue_pending_video_packet(PendingVideoPacket::new(
                    TrackId::new(1),
                    Duration::from_millis(packet_index as u64),
                    session.pipeline.seek_generation(),
                    Bytes::new(),
                    false,
                ));
        }

        let reason = demux_backpressure_reason(&session, &tick_config, true);

        assert_eq!(
            reason,
            Some(PipelinePauseReason::WaitingForDemuxAudioPriority)
        );
    }

    #[test]
    fn read_demux_packets_without_demuxer_is_noop() {
        let mut session = PlayerSession::new();
        let tick_config = PlayerTickConfig::default();
        let mut tick_result = PlayerTickResult::default();

        session.dispatch_command(PlayerCommand::Play).unwrap();

        let packets_read = read_demux_packets(
            &mut session,
            &tick_config,
            &mut tick_result,
            tick_config.max_demux_packets_per_tick,
            None,
        );

        assert_eq!(packets_read, 0);
        assert!(tick_result.demuxed_packets.is_empty());
        assert!(!tick_result.demux_backpressured);
        assert!(session.snapshot().last_error.is_none());
    }

    #[test]
    fn route_demuxed_audio_packet_preserves_shared_payload_and_metadata() {
        let mut session = PlayerSession::new();
        let payload = Bytes::from(vec![0x4f, 0x70, 0x75, 0x73]);
        let payload_ptr = payload.as_ptr();
        let time_base = media_core::TimeBase::new(1, 48_000).expect("valid audio time base");
        let packet = media_core::Packet::new(
            TrackId::new(2),
            TrackKind::Audio,
            Duration::from_millis(42),
            None,
            false,
            payload.clone(),
        )
        .with_track_timestamps(
            Some(media_core::TrackTimestamp::new(
                TrackId::new(2),
                2_016,
                time_base,
            )),
            Some(media_core::TrackTimestamp::new(
                TrackId::new(2),
                1_920,
                time_base,
            )),
        )
        .with_track_duration(media_core::TrackDuration::new(
            TrackId::new(2),
            960,
            time_base,
        ));

        route_demuxed_packet(&mut session, packet);

        let pending_packet = session
            .pipeline
            .pop_pending_audio_packet_front()
            .expect("audio packet должен попасть в pending audio queue");

        assert_eq!(pending_packet.track_id, TrackId::new(2));
        assert_eq!(pending_packet.pts, Duration::from_millis(42));
        assert_eq!(
            pending_packet.generation,
            session.pipeline.seek_generation()
        );
        assert_eq!(pending_packet.timing.pts_units(), 2_016);
        assert_eq!(pending_packet.timing.dts_units(), Some(1_920));
        assert_eq!(pending_packet.timing.duration_units(), Some(960));
        assert_eq!(
            pending_packet
                .timing
                .time_base()
                .expect("audio pending packet должен сохранить raw time base")
                .denom(),
            48_000
        );
        assert_eq!(pending_packet.encoded_bytes.as_ptr(), payload_ptr);
        assert_eq!(&pending_packet.encoded_bytes[..], b"Opus");
    }

    #[test]
    fn route_demuxed_video_packet_preserves_shared_payload_keyframe_and_pts() {
        let mut session = PlayerSession::new();
        let payload = Bytes::from(vec![0x82, 0x49, 0x83, 0x42]);
        let payload_ptr = payload.as_ptr();
        let packet = media_core::Packet::new(
            TrackId::new(1),
            TrackKind::Video,
            Duration::from_millis(120),
            None,
            true,
            payload.clone(),
        );

        route_demuxed_packet(&mut session, packet);

        let pending_packet = session
            .pipeline
            .pop_pending_video_packet_front()
            .expect("video packet должен попасть в pending video queue");

        assert_eq!(pending_packet.track_id, TrackId::new(1));
        assert_eq!(pending_packet.pts, Duration::from_millis(120));
        assert_eq!(
            pending_packet.generation,
            session.pipeline.seek_generation()
        );
        assert_eq!(pending_packet.keyframe, PacketKeyframe::Keyframe);
        assert_eq!(pending_packet.encoded_bytes.as_ptr(), payload_ptr);
        assert_eq!(&pending_packet.encoded_bytes[..], &[0x82, 0x49, 0x83, 0x42]);
    }

    #[test]
    fn absent_decoder_send_is_noop_and_drain_drops_pending_packets() {
        let mut session = PlayerSession::new();
        let mut tick_result = PlayerTickResult::default();

        session.pipeline.select_video_track(
            TrackId::new(1),
            VideoDecodeRequirement::new(VideoCodec::Vp9),
        );
        enqueue_selected_video_packet(
            &mut session,
            Duration::from_millis(120),
            Bytes::from_static(b"pending-without-decoder"),
            true,
        );

        let sent_packets = send_pending_video_packets_to_decoder(
            &mut session,
            &mut tick_result,
            decoder_io_limits_for_tests(0, 1),
            None,
        );

        assert_eq!(sent_packets, 0);
        assert!(!session.pipeline.pending_video_packet_is_empty());
        assert!(tick_result.dropped_video_frames.is_empty());

        let drained_frames = drain_decoded_video_frames(
            &mut session,
            &mut tick_result,
            decoder_io_limits_for_tests(1, 0),
            None,
        );

        assert_eq!(drained_frames, 0);
        assert!(session.pipeline.pending_video_packet_is_empty());
        assert_eq!(
            tick_result.dropped_video_frames,
            vec![PlayerVideoFrameDrop {
                pts: Duration::from_millis(120),
                reason: PlayerVideoDropReason::DecoderStarvation,
            }]
        );
    }

    #[test]
    fn pending_video_packet_from_unselected_track_is_dropped_before_decoder_send() {
        let mut session = PlayerSession::new();
        let decoder_thread = RecordingVideoDecoderThread::new();
        let mut tick_result = PlayerTickResult::default();

        session
            .pipeline
            .set_video_decoder_thread(decoder_thread.clone());
        session.pipeline.select_video_track(
            TrackId::new(1),
            VideoDecodeRequirement::new(VideoCodec::Vp9),
        );
        session
            .pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(2),
                Duration::from_millis(120),
                session.pipeline.seek_generation(),
                Bytes::from_static(b"foreign-video-track"),
                true,
            ));

        let sent_packets = send_pending_video_packets_to_decoder(
            &mut session,
            &mut tick_result,
            decoder_io_limits_for_tests(0, 1),
            None,
        );

        assert_eq!(sent_packets, 0);
        assert!(session.pipeline.pending_video_packet_is_empty());
        assert!(decoder_thread.sent_packets().is_empty());
        assert!(tick_result.dropped_video_frames.is_empty());
    }

    #[test]
    fn stale_pending_video_packet_is_dropped_as_stale_generation_before_decoder_send() {
        let mut session = PlayerSession::new();
        let decoder_thread = RecordingVideoDecoderThread::new();
        let mut tick_result = PlayerTickResult::default();
        let stale_generation = session.pipeline.seek_generation();

        session
            .pipeline
            .set_video_decoder_thread(decoder_thread.clone());
        session.pipeline.select_video_track(
            TrackId::new(1),
            VideoDecodeRequirement::new(VideoCodec::Vp9),
        );
        session.pipeline.begin_seek_generation();
        session
            .pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(1),
                Duration::from_millis(120),
                stale_generation,
                Bytes::from_static(b"stale-video"),
                true,
            ));

        let sent_packets = send_pending_video_packets_to_decoder(
            &mut session,
            &mut tick_result,
            decoder_io_limits_for_tests(0, 1),
            None,
        );

        assert_eq!(sent_packets, 0);
        assert!(session.pipeline.pending_video_packet_is_empty());
        assert!(decoder_thread.sent_packets().is_empty());
        assert_eq!(
            tick_result.dropped_video_frames,
            vec![PlayerVideoFrameDrop {
                pts: Duration::from_millis(120),
                reason: PlayerVideoDropReason::StaleGeneration,
            }]
        );
    }

    #[test]
    fn decoder_packet_send_backpressure_records_typed_pause_reason() {
        let mut session = PlayerSession::new();
        let decoder_thread = RecordingVideoDecoderThread::new();
        let mut tick_result = PlayerTickResult::default();

        decoder_thread.push_send_result(Err(DecodeSendError::Backpressure(
            DecodeBackpressureReason::PacketQueueFull {
                queued_packets: 4,
                capacity: 4,
            },
        )));
        session
            .pipeline
            .set_video_decoder_thread(decoder_thread.clone());
        session.pipeline.select_video_track(
            TrackId::new(1),
            VideoDecodeRequirement::new(VideoCodec::Vp9),
        );
        enqueue_selected_video_packet(
            &mut session,
            Duration::from_millis(120),
            Bytes::from_static(b"backpressured-video"),
            true,
        );

        let sent_packets = send_pending_video_packets_to_decoder(
            &mut session,
            &mut tick_result,
            decoder_io_limits_for_tests(0, 1),
            None,
        );

        assert_eq!(sent_packets, 0);
        assert!(!session.pipeline.pending_video_packet_is_empty());
        assert!(decoder_thread.sent_packets().is_empty());
        assert_eq!(
            tick_result.pipeline_pauses,
            vec![PlayerPipelinePause {
                reason: PipelinePauseReason::DecoderPacketQueueFull,
            }]
        );
    }

    #[test]
    fn decoder_packet_send_fatal_error_marks_session_failed() {
        let mut session = PlayerSession::new();
        let decoder_thread = RecordingVideoDecoderThread::new();
        let mut tick_result = PlayerTickResult::default();

        decoder_thread.push_send_result(Err(DecodeSendError::Fatal(DecodeThreadError::new(
            "fatal send failure",
        ))));
        session
            .pipeline
            .set_video_decoder_thread(decoder_thread.clone());
        session.pipeline.select_video_track(
            TrackId::new(1),
            VideoDecodeRequirement::new(VideoCodec::Vp9),
        );
        enqueue_selected_video_packet(
            &mut session,
            Duration::from_millis(120),
            Bytes::from_static(b"fatal-video"),
            true,
        );

        let sent_packets = send_pending_video_packets_to_decoder(
            &mut session,
            &mut tick_result,
            decoder_io_limits_for_tests(0, 1),
            None,
        );

        assert_eq!(sent_packets, 0);
        assert!(!session.pipeline.pending_video_packet_is_empty());
        assert!(decoder_thread.sent_packets().is_empty());
        let last_error = session
            .snapshot()
            .last_error
            .as_ref()
            .expect("fatal decoder send должен записать last_error");
        assert_eq!(last_error.kind, PlayerErrorKind::RuntimeError);
        assert!(last_error.message.contains("fatal send failure"));
    }

    #[test]
    fn stale_decoded_video_frame_is_dropped_before_presentation_queue() {
        let mut session = PlayerSession::new();
        let decoder_thread = RecordingVideoDecoderThread::new();
        let mut tick_result = PlayerTickResult::default();
        let stale_generation = session.pipeline.seek_generation();

        session
            .pipeline
            .set_video_decoder_thread(decoder_thread.clone());
        session.pipeline.select_video_track(
            TrackId::new(1),
            VideoDecodeRequirement::new(VideoCodec::Vp9),
        );
        session.pipeline.begin_seek_generation();
        let mut stale_frame = decoded_frame(Duration::from_millis(120), 77);
        stale_frame.generation = stale_generation;
        decoder_thread.push_decoded_frame(stale_frame);

        let drained_frames = drain_decoded_video_frames(
            &mut session,
            &mut tick_result,
            decoder_io_limits_for_tests(1, 0),
            None,
        );

        assert_eq!(drained_frames, 1);
        assert!(session.pipeline.video_present_queue_is_empty());
        assert_eq!(
            decoder_thread.released_handles(),
            vec![video_core::FrameTextureHandle(77)]
        );
        assert_eq!(
            tick_result.dropped_video_frames,
            vec![PlayerVideoFrameDrop {
                pts: Duration::from_millis(120),
                reason: PlayerVideoDropReason::StaleGeneration,
            }]
        );
    }

    #[test]
    fn unknown_keyframe_bootstrap_packet_is_sent_as_decoder_decode_start() {
        let mut session = PlayerSession::new();
        let decoder_thread = RecordingVideoDecoderThread::new();
        let mut tick_result = PlayerTickResult::default();

        session
            .pipeline
            .set_video_decoder_thread(decoder_thread.clone());
        session.pipeline.select_video_track(
            TrackId::new(1),
            VideoDecodeRequirement::new(VideoCodec::Vp9),
        );
        session.pipeline.require_video_decoder_keyframe();
        session
            .pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(1),
                Duration::from_millis(120),
                session.pipeline.seek_generation(),
                Bytes::from_static(b"unknown-keyframe"),
                PacketKeyframe::Unknown,
            ));

        let sent_packets = send_pending_video_packets_to_decoder(
            &mut session,
            &mut tick_result,
            decoder_io_limits_for_tests(0, 1),
            None,
        );
        let decoder_packets = decoder_thread.sent_packets();

        assert_eq!(sent_packets, 1);
        assert_eq!(decoder_packets.len(), 1);
        assert!(decoder_packets[0].keyframe);
        assert!(!session.pipeline.video_decoder_needs_keyframe());
        let diagnostics = session
            .snapshot_with_frame_counters(FrameCounters::default())
            .diagnostics
            .seek_bootstrap;
        assert_eq!(diagnostics.dropped_until_keyframe, 0);
        assert_eq!(
            diagnostics.first_accepted_keyframe,
            Some(PacketKeyframe::Unknown)
        );
    }

    #[test]
    fn packet_refinement_rejection_stops_before_decoder_send() {
        let mut session = PlayerSession::new();
        let decoder_thread = RecordingVideoDecoderThread::new();
        let mut tick_result = PlayerTickResult::default();

        session.set_system_capabilities(capabilities_with_vp9_profile0_for_decoder_io());
        session
            .pipeline
            .apply_demux_track_list_update(vec![video_track_for_tests(1)]);
        session
            .pipeline
            .set_video_decoder_thread(decoder_thread.clone());
        session.pipeline.select_video_track(
            TrackId::new(1),
            VideoDecodeRequirement::new(VideoCodec::Vp9),
        );
        enqueue_selected_video_packet(
            &mut session,
            Duration::from_millis(120),
            vp9_profile2_10bit_keyframe_for_tests(),
            true,
        );

        let sent_packets = send_pending_video_packets_to_decoder(
            &mut session,
            &mut tick_result,
            decoder_io_limits_for_tests(0, 1),
            None,
        );

        assert_eq!(sent_packets, 0);
        assert!(session.pipeline.pending_video_packet_is_empty());
        assert!(decoder_thread.sent_packets().is_empty());
        let last_error = session
            .snapshot()
            .last_error
            .as_ref()
            .expect("rejected bitstream refinement должен стать fatal before send");
        assert_eq!(last_error.kind, PlayerErrorKind::UnsupportedVideoProfile);
        assert!(last_error.message.contains("profile VP9 Profile 2"));
    }

    #[test]
    fn decoder_bootstrap_drops_interframes_until_keyframe() {
        let mut session = PlayerSession::new();
        session.pipeline.require_video_decoder_keyframe();

        assert!(!accept_video_packet_for_decoder_bootstrap(
            &mut session,
            PacketKeyframe::NotKeyframe,
            Duration::from_millis(10)
        ));
        assert!(session.pipeline.video_decoder_needs_keyframe());
        assert_eq!(
            session
                .snapshot_with_frame_counters(FrameCounters::default())
                .diagnostics
                .seek_bootstrap
                .dropped_until_keyframe,
            1
        );

        assert!(accept_video_packet_for_decoder_bootstrap(
            &mut session,
            PacketKeyframe::Keyframe,
            Duration::from_millis(20)
        ));
        assert!(!session.pipeline.video_decoder_needs_keyframe());
        let diagnostics = session
            .snapshot_with_frame_counters(FrameCounters::default())
            .diagnostics
            .seek_bootstrap;
        assert_eq!(diagnostics.dropped_until_keyframe, 1);
        assert_eq!(
            diagnostics.first_accepted_keyframe,
            Some(PacketKeyframe::Keyframe)
        );

        assert!(accept_video_packet_for_decoder_bootstrap(
            &mut session,
            PacketKeyframe::NotKeyframe,
            Duration::from_millis(30)
        ));
    }

    #[test]
    fn decoder_bootstrap_accepts_unknown_keyframe_as_decode_start() {
        let mut session = PlayerSession::new();
        session.pipeline.require_video_decoder_keyframe();

        assert!(accept_video_packet_for_decoder_bootstrap(
            &mut session,
            PacketKeyframe::Unknown,
            Duration::from_millis(10)
        ));
        assert!(!session.pipeline.video_decoder_needs_keyframe());
    }
}
