//! Playback tick и A/V scheduler.
//!
//! Этот модуль держит логику, которая раньше жила в `app-egui::main`:
//! чтение packets из demuxer, audio throttle, отправку video packets в decoder,
//! приём decoded frames, backpressure и выбор кадра для показа.

use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::{
    VideoCodec, VideoDecodeRequirement, VideoRequirementProbe, VideoRequirementRejection,
    probe_video_packet_requirement, resolve_video_metadata,
    video_requirement_needs_packet_refinement,
};
use media_core::{TrackId, TrackKind};
use rustiplayer_config::AppConfig;
use tracing::{trace, warn};

use crate::{
    DecodeSendError, DecodeThreadError, PendingAudioPacket, PendingVideoPacket,
    PipelineLatencyStage, PipelinePauseReason, PlaybackState, PlayerDecodePacket, PlayerError,
    PlayerErrorKind, PlayerSession, WorkerFrameTimingSnapshot, WorkerWakeupDiagnosticsSnapshot,
    WorkerWakeupReason,
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

    /// Максимальное время ожидания live preview seek gates.
    pub seek_preview_timeout: Duration,

    /// Минимальный audio buffer перед resume после seek.
    pub seek_resume_audio_min_buffer_ms: f64,

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
            seek_preview_timeout: Duration::from_millis(100),
            seek_resume_audio_min_buffer_ms: 50.0,
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
            seek_preview_timeout: Duration::from_millis(config.player.seek.live_preview_budget_ms),
            seek_resume_audio_min_buffer_ms: config.player.seek.resume_audio_min_buffer_ms as f64,
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

    /// Размер codec payload в bytes.
    pub size: usize,

    /// Safe source byte offset для demux seek, если container adapter его сообщил.
    pub byte_offset: Option<u64>,

    /// Признак keyframe для video packets.
    pub keyframe: bool,
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

        if let Some(front_frame_delay) = front_frame_scheduler_delay(self, tick_config, now) {
            return PlayerWorkerWakeupPlan::after(
                front_frame_delay,
                WorkerWakeupReason::FramePtsDeadline,
                frame_timing,
            );
        }

        if decoder_readiness_poll_needed(self, tick_config) {
            return PlayerWorkerWakeupPlan::after(
                decoder_readiness_poll_interval,
                WorkerWakeupReason::DecodeReadiness,
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
            self.process_pending_audio_packets_with_buffer_limit(
                tick_context.config.audio_buffer_high_water_mark_ms,
            );
        }

        process_pending_video_packets(self, tick_context, &mut tick_result);
        self.finish_seek_commit_if_ready(
            tick_context.now,
            tick_context.config.seek_commit_timeout,
            tick_context.config.seek_preview_timeout,
            tick_context.config.seek_resume_audio_min_buffer_ms,
            effective_seek_resume_video_min_ready_frames(&tick_context.config),
        );
        if let Err(error) =
            self.finish_autoplay_preroll_if_ready(tick_context.config.audio_preroll_target_ms)
        {
            self.mark_fatal_error(error);
        }

        tick_result
    }

    /// Обрабатывает pending audio packets до достижения high-water mark audio buffer.
    pub(crate) fn process_pending_audio_packets_with_buffer_limit(
        &mut self,
        high_water_mark_ms: f64,
    ) {
        let high_water_mark_ms = sanitize_audio_high_water_mark(high_water_mark_ms);

        if self.audio_buffer_level_ms().unwrap_or(0.0) > high_water_mark_ms {
            return;
        }

        while let Some(packet) = self.pipeline.pop_pending_audio_packet_front() {
            if self.audio_buffer_level_ms().unwrap_or(0.0) > high_water_mark_ms {
                self.pipeline.push_pending_audio_packet_front(packet);
                break;
            }

            self.process_audio_packet(
                packet.track_id,
                packet.pts,
                packet.generation,
                &packet.encoded_bytes,
            );
        }
    }

    /// Обновляет playback position один раз за tick.
    fn update_position_for_tick(&mut self, now: Instant) {
        if self.playback_state() != PlaybackState::Playing {
            return;
        }

        self.update_current_position(self.presentation_clock_position_at(now));
    }
}

/// Нормализует high-water mark, чтобы внешний некорректный config не ломал audio throttle.
fn sanitize_audio_high_water_mark(high_water_mark_ms: f64) -> f64 {
    if high_water_mark_ms.is_finite() && high_water_mark_ms > 0.0 {
        high_water_mark_ms
    } else {
        PlayerTickConfig::default().audio_buffer_high_water_mark_ms
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

/// Возвращает bounded лимит video packets для audio catch-up режима.
fn audio_catchup_pending_video_limit(tick_config: &PlayerTickConfig) -> usize {
    tick_config
        .max_pending_video_packets_during_audio_catchup
        .max(tick_config.max_pending_video_packets)
}

/// Проверяет, нужно ли временно приоритизировать demux ради заполнения audio buffer.
fn audio_demux_catchup_needed(session: &PlayerSession, tick_config: &PlayerTickConfig) -> bool {
    audio_demux_catchup_needed_for_level(
        session.pipeline.audio_track_id.is_some(),
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
                pending_video_packets = session.pipeline.pending_video_packet_len(),
                queued_video_frames = session.pipeline.video_present_queue_len(),
                "Demux backpressure: waiting for decoder/presentation"
            );
            break;
        }

        let demux_read_started_at = Instant::now();
        let Some(packet_result) = session.pipeline.demux_next_packet() else {
            break;
        };
        session.record_pipeline_latency(
            PipelineLatencyStage::DemuxRead,
            demux_read_started_at.elapsed(),
            None,
            None,
        );

        match packet_result {
            Ok(Some(packet)) => {
                tick_result.record_demuxed_packet(&packet);
                route_demuxed_packet(session, packet);
                packets_read += 1;
            }
            Ok(None) => {
                session.enter_eof_drain();
                break;
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
    let generation = session.pipeline.seek_generation;

    match packet.kind {
        TrackKind::Audio => {
            let pending_packet =
                PendingAudioPacket::new(packet.track_id, packet.pts, generation, packet.data);
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

/// Проверяет demux admission с явно переданным audio-priority флагом.
fn can_read_next_demux_packet_with_audio_priority(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    prioritize_audio_catchup: bool,
) -> bool {
    if prioritize_audio_catchup {
        return session.pipeline.pending_video_packet_len()
            < audio_catchup_pending_video_limit(tick_config);
    }

    if !has_texture_capacity_for_decode(session, tick_config) {
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
    if prioritize_audio_catchup {
        return (session.pipeline.pending_video_packet_len()
            >= audio_catchup_pending_video_limit(tick_config))
        .then_some(PipelinePauseReason::WaitingForDemuxAudioPriority);
    }

    if let Some(reason) = texture_capacity_backpressure_reason(session, tick_config) {
        return Some(reason);
    }

    if session.pipeline.pending_video_packet_len() >= tick_config.max_pending_video_packets {
        return Some(PipelinePauseReason::DemuxBackpressure);
    }

    (!seek_admission_active(session) && available_video_present_slots(session, tick_config) == 0)
        .then_some(PipelinePauseReason::WaitingForPresentQueue)
}

/// Проверяет, можно ли отправить video packet в decoder thread без чрезмерного decode-ahead.
fn can_send_video_packet_to_decoder(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    packet_pts: Duration,
    decode_ahead_limit: Duration,
) -> bool {
    if !has_texture_capacity_for_decode(session, tick_config) {
        return false;
    }

    if session.pipeline.audio_track_id.is_none() || session.pipeline.audio_clock.is_none() {
        return true;
    }

    let audio_now = session.audio_clock_now();
    let media_audio_now = saturating_duration_add(session.pipeline.media_clock_base, audio_now);
    let packet_lead = packet_pts.saturating_sub(media_audio_now);

    packet_lead <= decode_ahead_limit.min(video_decode_ahead_limit(tick_config))
}

/// Проверяет запас texture slots для ещё одного decoded frame.
fn has_texture_capacity_for_decode(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    texture_capacity_backpressure_reason(session, tick_config).is_none()
}

/// Диагностирует, почему texture/surface pool не даёт отправить новый packet.
fn texture_capacity_backpressure_reason(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> Option<PipelinePauseReason> {
    let Some(stats) = session.pipeline.video_decoder_resource_snapshot() else {
        return None;
    };

    let available_slots = stats.available_slots();
    if available_slots > texture_slot_min_watermark(tick_config) {
        return None;
    }

    trace!(
        texture_slots = stats.slots,
        texture_in_use = stats.in_use,
        texture_capacity = stats.capacity,
        available_slots,
        reserve = texture_slot_min_watermark(tick_config),
        waiting_gpu_completion = stats.waiting_gpu_completion,
        waiting_decoder_reuse = stats.waiting_decoder_reuse,
        "Video backpressure: waiting for texture slots"
    );

    if stats.waiting_gpu_completion > 0 || stats.waiting_decoder_reuse > 0 {
        Some(PipelinePauseReason::WaitingForGpuRelease)
    } else {
        Some(PipelinePauseReason::WaitingForFreeSurface)
    }
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

/// Возвращает bounded budget приёма decoded frames для текущего admission mode.
fn decoded_frame_receive_budget(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    max_frames_to_drain: usize,
) -> usize {
    if seek_admission_active(session) {
        return max_frames_to_drain;
    }

    if session.can_present_video() {
        return available_video_present_slots(session, tick_config).min(max_frames_to_drain);
    }

    max_frames_to_drain
}

/// Возвращает bounded budget отправки packets в decoder для текущего admission mode.
fn video_packet_send_present_admission_budget(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    max_packets_to_send: usize,
) -> usize {
    if seek_admission_active(session) {
        return max_packets_to_send;
    }

    available_video_present_slots(session, tick_config).min(max_packets_to_send)
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

/// Возвращает достижимый video preroll для seek resume с учётом размера presentation queue.
fn effective_seek_resume_video_min_ready_frames(tick_config: &PlayerTickConfig) -> usize {
    tick_config.effective_seek_resume_video_min_ready_frames()
}

/// Кладёт decoded frame в presentation queue, сохраняя фиксированный размер очереди.
fn enqueue_decoded_video_frame(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
    frame: video_core::DecodedFrame,
) {
    let queue_limit = video_present_queue_limit(tick_config);

    while session.pipeline.video_present_queue_len() >= queue_limit {
        let Some(stale_frame) = session.pipeline.pop_queued_video_frame_front() else {
            break;
        };

        release_video_texture(session, stale_frame.texture_handle);
        record_video_drop(
            session,
            tick_result,
            stale_frame.pts,
            PlayerVideoDropReason::QueueOverflow,
        );
        tracing::debug!(
            pts_ms = stale_frame.pts.as_millis(),
            queue_limit,
            "Dropping oldest queued video frame before enqueue"
        );
    }

    session.pipeline.enqueue_queued_video_frame(frame);
}

/// Забирает fatal decoder-thread error и переводит его в состояние player session.
fn drain_video_decoder_thread_error(session: &mut PlayerSession) -> bool {
    let mut fatal_decoder_error = None;

    while let Some(error) = session.pipeline.try_recv_video_decoder_error() {
        fatal_decoder_error = Some(error);
    }

    let Some(error) = fatal_decoder_error else {
        return false;
    };

    session.pipeline.clear_pending_video_packets();
    session.pipeline.reset_video_decode_in_flight();
    session.mark_fatal_error(player_error_from_decode_thread_error(&error));
    true
}

/// Забирает typed diagnostics events от decoder/backend boundary.
fn drain_video_decoder_thread_diagnostics(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
) {
    loop {
        let diagnostic_event = session.pipeline.try_recv_video_decoder_diagnostic_event();
        let Some(event) = diagnostic_event else {
            break;
        };
        match event {
            video_core::VideoDecoderDiagnosticEvent::FrameDropped { pts, reason } => {
                let drop_reason = match reason {
                    video_core::VideoDecoderDropReason::ReadyQueueOverflow => {
                        PlayerVideoDropReason::QueueOverflow
                    }
                };
                record_video_drop(session, tick_result, pts, drop_reason);
            }
            video_core::VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure } => {
                session.record_decoded_frame_publish_pressure(pressure);
            }
        }
    }
}

/// Мапит fail-closed ошибку decoder thread в player error model.
fn player_error_from_decode_thread_error(error: &DecodeThreadError) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::RuntimeError,
        format!("Video decoder thread failed: {}", error.message()),
    )
}

/// Забирает готовые кадры из decoder thread и кладёт их в presentation queue.
fn drain_decoded_video_frames(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
    max_frames_to_drain: usize,
    catch_up_deadline: Option<Instant>,
) -> usize {
    drain_completed_video_decode_packets(session);
    drain_video_decoder_thread_diagnostics(session, tick_result);

    if drain_video_decoder_thread_error(session) {
        return 0;
    }

    if !session.pipeline.can_receive_decoded_video_frames() {
        if !session.pipeline.pending_video_packet_is_empty() {
            tracing::warn!(
                count = session.pipeline.pending_video_packet_len(),
                "No video decoder thread — dropping video packets"
            );
        }
        while let Some(packet) = session.pipeline.pop_pending_video_packet_front() {
            record_video_drop(
                session,
                tick_result,
                packet.pts,
                PlayerVideoDropReason::DecoderStarvation,
            );
        }
        session.pipeline.reset_video_decode_in_flight();
        return 0;
    }

    let playback_can_present = session.can_present_video();
    let receive_budget = decoded_frame_receive_budget(session, tick_config, max_frames_to_drain);

    if receive_budget == 0 {
        return 0;
    }

    let mut decoded_frames = Vec::new();
    for _ in 0..receive_budget {
        if catch_up_deadline_reached(catch_up_deadline) {
            break;
        }
        let Some(frame) = session.pipeline.try_recv_decoded_video_frame() else {
            break;
        };
        decoded_frames.push(frame);
    }

    if drain_video_decoder_thread_error(session) {
        for frame in decoded_frames {
            release_video_texture(session, frame.texture_handle);
        }
        return 0;
    }
    drain_video_decoder_thread_diagnostics(session, tick_result);

    let drained_frame_count = decoded_frames.len();
    for frame in decoded_frames {
        tracing::debug!(
            pts_ms = frame.pts.as_millis(),
            format = %frame.format,
            bit_depth = %frame.bit_depth,
            memory_path = %frame.memory_path,
            width = frame.width,
            height = frame.height,
            "Video frame decoded"
        );
        tick_result.record_decoded_video_frame();
        session.record_decoded_frame_diagnostics(&frame);

        let frame_pts = frame.pts;
        if session.should_drop_decoded_frame_for_seek(frame_pts) {
            if session.can_keep_seek_preroll_fallback(frame_pts) {
                replace_seek_preroll_fallback_frame(session, tick_result, frame);
            } else {
                release_video_texture(session, frame.texture_handle);
                record_video_drop(
                    session,
                    tick_result,
                    frame_pts,
                    PlayerVideoDropReason::SeekPreroll,
                );
            }
            tracing::debug!(
                pts_ms = frame_pts.as_millis(),
                "Retaining or dropping pre-roll video frame before seek target"
            );
            continue;
        }

        drop_seek_preroll_fallback_frame(session, tick_result);
        session.observe_video_frame_pts(frame.pts);

        if playback_can_present {
            enqueue_decoded_video_frame(session, tick_config, tick_result, frame);
        } else {
            release_video_texture(session, frame.texture_handle);
            record_video_drop(
                session,
                tick_result,
                frame.pts,
                PlayerVideoDropReason::Paused,
            );
            tracing::debug!("Dropping decoded frame received while playback is paused");
        }
    }

    drained_frame_count
}

/// Синхронизирует player-side in-flight счётчик с packet ack-ами decoder thread-а.
fn drain_completed_video_decode_packets(session: &mut PlayerSession) {
    let completed_packet_count = session.pipeline.drain_completed_video_decode_packet_count();

    session
        .pipeline
        .note_video_packets_completed_by_decoder(completed_packet_count);
}

/// Отправляет ограниченное число pending video packets в decoder thread.
fn send_pending_video_packets_to_decoder(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
    max_packets_to_send: usize,
    decode_ahead_limit: Duration,
    catch_up_deadline: Option<Instant>,
) -> usize {
    if !session.pipeline.can_send_video_decode_packets() {
        return 0;
    }

    let mut sent_packets = 0usize;
    let present_admission_budget =
        video_packet_send_present_admission_budget(session, tick_config, max_packets_to_send);

    while sent_packets < max_packets_to_send {
        if catch_up_deadline_reached(catch_up_deadline) {
            break;
        }

        if present_admission_budget == 0 {
            record_pipeline_pause(
                session,
                tick_result,
                PipelinePauseReason::WaitingForPresentQueue,
            );
            break;
        }
        if sent_packets >= present_admission_budget {
            break;
        }

        let Some(packet) = session.pipeline.front_pending_video_packet() else {
            break;
        };
        let packet_track_id = packet.track_id;
        let packet_pts = packet.pts;
        let packet_generation = packet.generation;
        let packet_keyframe = packet.keyframe;
        let encoded_bytes = packet.encoded_bytes.clone();

        if let Some(drop_reason) = pending_video_packet_generation_drop_reason(
            session.pipeline.seek_generation,
            packet_generation,
        ) {
            session.pipeline.pop_pending_video_packet_front();
            record_video_drop(session, tick_result, packet_pts, drop_reason);
            continue;
        }

        if session.pipeline.video_track_id != Some(packet_track_id) {
            session.pipeline.pop_pending_video_packet_front();
            continue;
        }

        if !accept_video_packet_for_decoder_bootstrap(session, packet_keyframe, packet_pts) {
            session.pipeline.pop_pending_video_packet_front();
            record_video_drop(
                session,
                tick_result,
                packet_pts,
                PlayerVideoDropReason::SeekPreroll,
            );
            continue;
        }

        let packet_probe = PendingVideoPacketProbe {
            track_id: packet_track_id,
            encoded_bytes,
        };
        if !validate_pending_video_packet_before_decode(session, &packet_probe) {
            break;
        }

        if let Some(reason) = texture_capacity_backpressure_reason(session, tick_config) {
            record_pipeline_pause(session, tick_result, reason);
            break;
        }

        if !can_send_video_packet_to_decoder(session, tick_config, packet_pts, decode_ahead_limit) {
            trace!(
                pts_ms = packet_pts.as_millis(),
                audio_ms = session.audio_clock_now().as_millis(),
                decode_ahead_limit_ms = decode_ahead_limit.as_millis(),
                max_ahead_ms = video_decode_ahead_limit(tick_config).as_millis(),
                "A/V sync: holding video packet to limit decode-ahead"
            );
            break;
        }

        trace!(
            pts_ms = packet_pts.as_millis(),
            encoded_len = packet_probe.encoded_bytes.len(),
            keyframe = packet_keyframe,
            "Sending video packet to decoder thread"
        );

        let resolved_color = session
            .pipeline
            .active_video_requirement
            .as_ref()
            .and_then(|requirement| requirement.color.clone());
        let decode_packet = PlayerDecodePacket {
            track_id: packet_track_id,
            pts: packet_pts,
            encoded_bytes: packet_probe.encoded_bytes,
            keyframe: packet_keyframe,
            resolved_color,
        };

        match session.pipeline.send_video_decode_packet(decode_packet) {
            None => break,
            Some(Ok(())) => {
                session.pipeline.pop_pending_video_packet_front();
                session.pipeline.note_video_packet_sent_to_decoder();
            }
            Some(Err(DecodeSendError::Backpressure(reason))) => {
                tracing::debug!(reason = %reason, "Decoder packet channel backpressure");
                record_pipeline_pause(
                    session,
                    tick_result,
                    PipelinePauseReason::DecoderPacketQueueFull,
                );
                break;
            }
            Some(Err(DecodeSendError::Fatal(error))) => {
                tracing::warn!(error = %error, "Failed to send packet to decoder thread");
                session.mark_fatal_error(PlayerError::new(
                    PlayerErrorKind::RuntimeError,
                    format!("Video decoder thread stopped before accepting packet: {error}"),
                ));
                break;
            }
        }

        sent_packets += 1;
    }

    sent_packets
}

/// Пропускает inter-frames, пока decoder после flush ждёт новый keyframe.
fn accept_video_packet_for_decoder_bootstrap(
    session: &mut PlayerSession,
    packet_keyframe: bool,
    packet_pts: Duration,
) -> bool {
    if !session.pipeline.video_decoder_needs_keyframe() {
        return true;
    }

    if !packet_keyframe {
        trace!(
            pts_ms = packet_pts.as_millis(),
            "Dropping video packet until decoder receives post-flush keyframe"
        );
        return false;
    }

    session.pipeline.mark_video_decoder_bootstrapped();
    true
}

/// Отделяет stale seek generation от late-drop policy.
fn pending_video_packet_generation_drop_reason(
    current_generation: u64,
    packet_generation: u64,
) -> Option<PlayerVideoDropReason> {
    (packet_generation != current_generation).then_some(PlayerVideoDropReason::StaleGeneration)
}

/// Минимальный view pending packet-а для bitstream capability validation.
struct PendingVideoPacketProbe {
    /// Track ID нужен, чтобы найти container codec.
    track_id: TrackId,

    /// Codec payload нужен adapter-у для чтения header-level requirement.
    encoded_bytes: Bytes,
}

/// Проверяет profile/format до отправки packet-а в hardware decoder.
fn validate_pending_video_packet_before_decode(
    session: &mut PlayerSession,
    packet: &PendingVideoPacketProbe,
) -> bool {
    if session
        .pipeline
        .active_video_requirement
        .as_ref()
        .is_some_and(|requirement| !video_requirement_needs_packet_refinement(requirement))
    {
        return true;
    }

    let requirement = match video_requirement_from_packet(session, packet) {
        Ok(Some(requirement)) => requirement,
        Ok(None) => return true,
        Err(error) => {
            warn!(error = %error, "Video stream rejected by packet requirement probe");
            session.mark_fatal_error(error);
            session.pipeline.clear_pending_video_packets();
            return false;
        }
    };

    match session.refine_active_video_requirement(requirement) {
        Ok(()) => true,
        Err(error) => {
            warn!(error = %error, "Video stream rejected before hardware decode");
            session.mark_fatal_error(error);
            session.pipeline.clear_pending_video_packets();
            false
        }
    }
}

/// Читает codec header через adapter registry и строит уточнённое requirement.
fn video_requirement_from_packet_data(
    codec: VideoCodec,
    packet_data: &[u8],
    container_source: Option<codec_core::VideoMetadataSource>,
) -> Result<Option<VideoDecodeRequirement>, PlayerError> {
    match probe_video_packet_requirement(codec, packet_data) {
        VideoRequirementProbe::Candidate(candidate) => Ok(Some(
            resolve_video_metadata(codec, container_source, Some(candidate)).requirement,
        )),
        VideoRequirementProbe::Rejected(rejection) => {
            Err(player_error_from_requirement_rejection(rejection))
        }
        VideoRequirementProbe::Recoverable(uncertainty) => {
            trace!(
                ?uncertainty,
                "Video requirement probe skipped before decode"
            );
            Ok(None)
        }
    }
}

/// Переводит codec adapter reject в player error без generic hardware wording.
fn player_error_from_requirement_rejection(rejection: VideoRequirementRejection) -> PlayerError {
    let kind = match rejection {
        VideoRequirementRejection::UnsupportedBitDepth { .. } => {
            PlayerErrorKind::UnsupportedVideoBitDepth
        }
        VideoRequirementRejection::UnsupportedChroma { .. } => {
            PlayerErrorKind::UnsupportedVideoChroma
        }
        VideoRequirementRejection::UnsupportedCodecAdapter { .. } => {
            PlayerErrorKind::UnsupportedVideoCodec
        }
    };

    PlayerError::new(kind, rejection.user_message())
}

/// Возвращает codec-specific requirement probe через adapter registry.
fn video_requirement_from_packet(
    session: &PlayerSession,
    packet: &PendingVideoPacketProbe,
) -> Result<Option<VideoDecodeRequirement>, PlayerError> {
    let Some(codec) = session.video_codec_for_track(packet.track_id) else {
        return Ok(None);
    };

    video_requirement_from_packet_data(
        codec,
        &packet.encoded_bytes,
        session.video_metadata_source_for_track(packet.track_id),
    )
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
    let present_lead = session
        .pipeline
        .video_frame_duration_estimate
        .mul_f64(lead_frames);

    present_lead
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

    if active_final_seek_frame_ready(session, front_frame.pts) {
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

/// Проверяет, что первый queued frame уже может закрыть active final seek.
fn active_final_seek_frame_ready(session: &PlayerSession, frame_pts: Duration) -> bool {
    session
        .active_final_seek_target()
        .is_some_and(|target_position| frame_pts >= target_position)
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
    if !seek_admission_active(session)
        && session.can_present_video()
        && session.pipeline.video_present_queue_len() >= video_present_queue_target(tick_config)
    {
        return false;
    }

    if pending_audio_work_available(session, tick_config) {
        return true;
    }

    if pending_video_work_available(session, tick_config) {
        return true;
    }

    demux_work_available(session, tick_config)
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
        tick_config,
        packet.pts,
        video_decode_ahead_target(tick_config),
    )
}

/// Проверяет, может ли demuxer прочитать следующий packet без downstream overflow.
fn demux_work_available(session: &PlayerSession, tick_config: &PlayerTickConfig) -> bool {
    if !session.is_demuxing_active() || !session.pipeline.has_demuxer() {
        return false;
    }

    let prioritize_audio_catchup = audio_demux_catchup_needed(session, tick_config);
    if session.pipeline.video_track_id.is_some()
        && session.pipeline.video_present_queue_len() >= video_present_queue_target(tick_config)
        && !prioritize_audio_catchup
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
        session.pipeline.video_track_id.is_some(),
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
        .video_frame_duration_estimate
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
        .video_frame_duration_estimate
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
    if session.active_final_seek_target().is_none() || !session.draining_after_eof {
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
    if session.pipeline.video_track_id.is_some()
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
        .video_frame_duration_estimate
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
        let drained_frames = drain_decoded_video_frames(
            session,
            tick_config,
            tick_result,
            drain_budget,
            Some(deadline),
        );
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
        let sent_packets = send_pending_video_packets_to_decoder(
            session,
            tick_config,
            tick_result,
            send_budget,
            video_decode_ahead_limit(tick_config),
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

    let base_drain_budget = if session.can_present_video() {
        tick_config.max_decoded_video_frames_drained_per_tick
    } else {
        usize::MAX
    };
    drain_decoded_video_frames(session, tick_config, tick_result, base_drain_budget, None);

    let playback_can_present = session.can_present_video();
    if !playback_can_present {
        return;
    }

    if session.is_demuxing_active() {
        send_pending_video_packets_to_decoder(
            session,
            tick_config,
            tick_result,
            tick_config.max_video_packets_sent_per_tick,
            video_decode_ahead_target(tick_config),
            None,
        );
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
    if audio_now != session.pipeline.last_audio_clock {
        session.pipeline.last_audio_clock = audio_now;
        session.pipeline.last_audio_clock_change_at = tick_context.now;
    }

    let audio_stall_elapsed = tick_context
        .now
        .saturating_duration_since(session.pipeline.last_audio_clock_change_at);
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
    let force_present_for_final_seek = active_final_seek_frame_ready(session, frame.pts);
    if !force_present_for_final_seek
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
        force_present_for_final_seek,
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
    use std::collections::VecDeque;

    use super::*;
    use crate::{PlayerCommand, PlayerSession};

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
                session.pipeline.seek_generation,
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
        let frame_duration = session.pipeline.video_frame_duration_estimate;

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
            session.pipeline.video_frame_duration_estimate,
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
            session.pipeline.video_frame_duration_estimate
        ));
    }

    #[test]
    fn seek_generation_transition_is_not_counted_as_late_drop() {
        assert_eq!(
            pending_video_packet_generation_drop_reason(2, 1),
            Some(PlayerVideoDropReason::StaleGeneration)
        );
        assert_eq!(pending_video_packet_generation_drop_reason(2, 2), None);
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
                    session.pipeline.seek_generation,
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
                    session.pipeline.seek_generation,
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
                    session.pipeline.seek_generation,
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
        let packet = media_core::Packet::new(
            TrackId::new(2),
            TrackKind::Audio,
            Duration::from_millis(42),
            None,
            false,
            payload.clone(),
        );

        route_demuxed_packet(&mut session, packet);

        let pending_packet = session
            .pipeline
            .pop_pending_audio_packet_front()
            .expect("audio packet должен попасть в pending audio queue");

        assert_eq!(pending_packet.track_id, TrackId::new(2));
        assert_eq!(pending_packet.pts, Duration::from_millis(42));
        assert_eq!(pending_packet.generation, session.pipeline.seek_generation);
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
        assert_eq!(pending_packet.generation, session.pipeline.seek_generation);
        assert!(pending_packet.keyframe);
        assert_eq!(pending_packet.encoded_bytes.as_ptr(), payload_ptr);
        assert_eq!(&pending_packet.encoded_bytes[..], &[0x82, 0x49, 0x83, 0x42]);
    }

    #[test]
    fn decoder_bootstrap_drops_interframes_until_keyframe() {
        let mut session = PlayerSession::new();
        session.pipeline.require_video_decoder_keyframe();

        assert!(!accept_video_packet_for_decoder_bootstrap(
            &mut session,
            false,
            Duration::from_millis(10)
        ));
        assert!(session.pipeline.video_decoder_needs_keyframe());

        assert!(accept_video_packet_for_decoder_bootstrap(
            &mut session,
            true,
            Duration::from_millis(20)
        ));
        assert!(!session.pipeline.video_decoder_needs_keyframe());

        assert!(accept_video_packet_for_decoder_bootstrap(
            &mut session,
            false,
            Duration::from_millis(30)
        ));
    }
}
