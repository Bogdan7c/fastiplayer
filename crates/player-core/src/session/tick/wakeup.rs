//! Worker wakeup planning for session tick.
//!
//! Модуль собирает read-only deadline plan из текущего pipeline state. Он не
//! мутирует session и поэтому сохраняет fairness: уже выбранный audio refill
//! deadline не сдвигается render-feedback событиями наружного worker loop.

use std::time::{Duration, Instant};

use super::{
    PlayerTickConfig,
    demux_admission::{
        audio_demux_catchup_needed, can_read_next_demux_packet_with_audio_priority,
        selected_audio_bootstrap_needs_demux,
    },
    presentation_scheduler::{
        front_frame_ready_for_scheduler, front_frame_scheduler_delay, front_frame_timing,
        host_upload_ready_queue_capacity, normal_present_queue_blocks_video_admission,
        seek_admission_active, video_decode_ahead_limit, video_decode_ahead_target,
        video_decoder_decode_ahead_limits, video_decoder_texture_limits,
        video_present_queue_target,
    },
    video_decoder_io::{
        can_send_video_packet_to_decoder, pending_video_packet_requires_decoder_send_capacity,
    },
};
use crate::{
    PlaybackState, WorkerFrameTimingSnapshot, WorkerWakeupDiagnosticsSnapshot, WorkerWakeupReason,
    pipeline::{VideoDecoderActivityStatus, VideoDecoderSendBackpressure},
    session::PlayerSession,
    session::audio_runtime::sanitize_audio_high_water_mark,
};

/// Read-only план следующего playback wakeup-а worker-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlayerWorkerWakeupPlan {
    /// `None` означает, что worker может ждать только command/render/scrub events.
    pub(crate) delay: Option<Duration>,

    /// Машиночитаемая причина планирования.
    pub(crate) reason: WorkerWakeupReason,

    /// Сравнение media clock target и первого queued video frame.
    pub(crate) frame_timing: Option<WorkerFrameTimingSnapshot>,

    /// `true`, если worker может ждать neutral decoder activity до fallback deadline-а.
    pub(crate) wait_for_decoder_activity: bool,
}

impl PlayerWorkerWakeupPlan {
    /// Планирует ожидание только внешнего события.
    #[must_use]
    const fn idle() -> Self {
        Self {
            delay: None,
            reason: WorkerWakeupReason::Idle,
            frame_timing: None,
            wait_for_decoder_activity: false,
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
            wait_for_decoder_activity: false,
        }
    }

    /// Планирует timeout, но разрешает worker-у проснуться раньше от decoder activity.
    #[must_use]
    const fn after_with_decoder_activity(
        delay: Duration,
        reason: WorkerWakeupReason,
        frame_timing: Option<WorkerFrameTimingSnapshot>,
        wait_for_decoder_activity: bool,
    ) -> Self {
        Self {
            delay: Some(delay),
            reason,
            frame_timing,
            wait_for_decoder_activity,
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
        let decoder_activity_status = self.pipeline.video_decoder_activity_status();

        self.worker_wakeup_plan_with_decoder_activity_status(
            now,
            tick_config,
            decoder_readiness_poll_interval,
            coarse_progress_interval,
            &decoder_activity_status,
        )
    }

    /// Вычисляет worker wakeup, используя activity status, снятый caller-ом до planning.
    ///
    /// Worker вызывает этот overload после собственного snapshot-а, чтобы закрыть
    /// окно lost wakeup между decoder activity snapshot и входом в `select!`.
    #[must_use]
    pub(crate) fn worker_wakeup_plan_with_decoder_activity_status(
        &self,
        now: Instant,
        tick_config: &PlayerTickConfig,
        decoder_readiness_poll_interval: Duration,
        coarse_progress_interval: Duration,
        decoder_activity_status: &VideoDecoderActivityStatus,
    ) -> PlayerWorkerWakeupPlan {
        if !self.playback_state().is_playback_active() && !self.seek_landing_decode_active() {
            return prefer_earlier_staged_preflight(self, now, PlayerWorkerWakeupPlan::idle());
        }

        let frame_timing = front_frame_timing(self, tick_config, now);

        if front_frame_ready_for_scheduler(self, tick_config, now) {
            return PlayerWorkerWakeupPlan::after(
                Duration::ZERO,
                WorkerWakeupReason::FrameReady,
                frame_timing,
            );
        }

        if immediate_pipeline_work_available(self, tick_config, now) {
            let reason = if matches!(
                self.playback_state(),
                PlaybackState::Buffering | PlaybackState::Seeking
            ) || self.seek_landing_decode_active()
            {
                WorkerWakeupReason::SeekOrPreroll
            } else {
                WorkerWakeupReason::PipelineWorkReady
            };
            return PlayerWorkerWakeupPlan::after(Duration::ZERO, reason, frame_timing);
        }

        let audio_refill_delay = audio_buffer_refill_wakeup_delay(self, tick_config);
        let front_frame_delay = front_frame_scheduler_delay(self, tick_config, now);

        if decoder_readiness_poll_needed(self, tick_config) {
            let existing_plan = decoder_readiness_wakeup_plan(
                audio_refill_delay,
                front_frame_delay,
                decoder_readiness_poll_interval,
                frame_timing,
                decoder_activity_status.can_wait_for_activity(),
            );
            return prefer_earlier_demux_retry(self, tick_config, now, existing_plan);
        }

        if let Some(front_frame_delay) = front_frame_delay {
            if let Some(audio_refill_delay) =
                audio_refill_delay.filter(|delay| *delay < front_frame_delay)
            {
                let existing_plan = PlayerWorkerWakeupPlan::after(
                    audio_refill_delay,
                    WorkerWakeupReason::PipelineWorkReady,
                    frame_timing,
                );
                return prefer_earlier_demux_retry(self, tick_config, now, existing_plan);
            }

            let existing_plan = PlayerWorkerWakeupPlan::after(
                front_frame_delay,
                WorkerWakeupReason::FramePtsDeadline,
                frame_timing,
            );
            return prefer_earlier_demux_retry(self, tick_config, now, existing_plan);
        }

        if let Some(audio_refill_delay) = audio_refill_delay {
            let existing_plan = PlayerWorkerWakeupPlan::after(
                audio_refill_delay,
                WorkerWakeupReason::PipelineWorkReady,
                frame_timing,
            );
            return prefer_earlier_demux_retry(self, tick_config, now, existing_plan);
        }

        if seek_transition_needs_progress(self) {
            let existing_plan = PlayerWorkerWakeupPlan::after(
                coarse_progress_interval,
                WorkerWakeupReason::SeekOrPreroll,
                frame_timing,
            );
            return prefer_earlier_demux_retry(self, tick_config, now, existing_plan);
        }

        if self.eof_drain_needs_progress() {
            let existing_plan = PlayerWorkerWakeupPlan::after(
                coarse_progress_interval,
                WorkerWakeupReason::CoarseProgress,
                frame_timing,
            );
            return prefer_earlier_demux_retry(self, tick_config, now, existing_plan);
        }

        if active_pipeline_needs_coarse_progress(self) {
            let existing_plan = PlayerWorkerWakeupPlan::after(
                coarse_progress_interval,
                WorkerWakeupReason::CoarseProgress,
                frame_timing,
            );
            return prefer_earlier_demux_retry(self, tick_config, now, existing_plan);
        }

        prefer_earlier_demux_retry(self, tick_config, now, PlayerWorkerWakeupPlan::idle())
    }
}

/// Выбирает runnable demux deadline только когда он строго раньше existing work.
///
/// Истёкший retry остаётся установленным, пока downstream capacity не разрешит
/// следующий demux read. Иначе zero-delay deadline крутил бы worker без работы.
pub(super) fn prefer_earlier_demux_retry(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
    existing_plan: PlayerWorkerWakeupPlan,
) -> PlayerWorkerWakeupPlan {
    let Some(retry_delay) = session.installed_demux_retry_delay(now) else {
        return prefer_earlier_staged_preflight(session, now, existing_plan);
    };
    if retry_delay.is_zero() && !demux_work_available(session, tick_config, now) {
        return prefer_earlier_staged_preflight(session, now, existing_plan);
    }
    if existing_plan
        .delay
        .is_some_and(|delay| delay <= retry_delay)
    {
        return prefer_earlier_staged_preflight(session, now, existing_plan);
    }

    let demux_plan = PlayerWorkerWakeupPlan::after(
        retry_delay,
        WorkerWakeupReason::DemuxRetryDeadline,
        existing_plan.frame_timing,
    );
    prefer_earlier_staged_preflight(session, now, demux_plan)
}

/// Добавляет staged retry/timeout, сохраняя existing-work-first при равенстве.
fn prefer_earlier_staged_preflight(
    session: &PlayerSession,
    now: Instant,
    existing_plan: PlayerWorkerWakeupPlan,
) -> PlayerWorkerWakeupPlan {
    let Some(staged_delay) = session.staged_preflight_wakeup_delay(now) else {
        return existing_plan;
    };
    if existing_plan
        .delay
        .is_some_and(|delay| delay <= staged_delay)
    {
        return existing_plan;
    }

    PlayerWorkerWakeupPlan::after(
        staged_delay,
        WorkerWakeupReason::StagedPreflightDeadline,
        existing_plan.frame_timing,
    )
}

/// Выбирает bounded decoder-readiness deadline без busy-spin.
///
/// Decoder activity может разбудить worker раньше timeout-а, но timeout остаётся
/// обязательным fallback-ом для unsupported/absent/fatal notifier и seek timeout policy.
fn decoder_readiness_wakeup_plan(
    audio_refill_delay: Option<Duration>,
    front_frame_delay: Option<Duration>,
    decoder_readiness_poll_interval: Duration,
    frame_timing: Option<WorkerFrameTimingSnapshot>,
    wait_for_decoder_activity: bool,
) -> PlayerWorkerWakeupPlan {
    let mut selected_delay = decoder_readiness_poll_interval;
    let mut selected_reason = WorkerWakeupReason::DecodeReadiness;

    if let Some(audio_refill_delay) = audio_refill_delay.filter(|delay| *delay < selected_delay) {
        selected_delay = audio_refill_delay;
        selected_reason = WorkerWakeupReason::PipelineWorkReady;
    }

    if let Some(front_frame_delay) = front_frame_delay.filter(|delay| *delay < selected_delay) {
        selected_delay = front_frame_delay;
        selected_reason = WorkerWakeupReason::FramePtsDeadline;
    }

    PlayerWorkerWakeupPlan::after_with_decoder_activity(
        selected_delay,
        selected_reason,
        frame_timing,
        wait_for_decoder_activity,
    )
}

/// Возвращает delay до момента, когда audio buffer снова можно пополнять.
pub(super) fn audio_buffer_refill_wakeup_delay(
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
pub(super) fn audio_refill_work_can_be_scheduled(session: &PlayerSession) -> bool {
    if !session.pipeline.has_selected_audio_track() {
        return false;
    }

    if session.is_demuxing_active() && session.pipeline.has_demuxer() {
        return true;
    }

    session.eof_drain_needs_progress() && !session.pipeline.pending_audio_packet_is_empty()
}

/// Проверяет, есть ли работа, которую tick может выполнить без ожидания media PTS.
pub(super) fn immediate_pipeline_work_available(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
) -> bool {
    if pending_audio_work_available(session, tick_config) {
        return true;
    }

    let demux_available = demux_work_available(session, tick_config, now);
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
pub(super) fn pending_audio_work_available(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    if session.pipeline.pending_audio_packet_is_empty() {
        return false;
    }

    session.audio_buffer_level_ms().unwrap_or(0.0)
        <= sanitize_audio_high_water_mark(tick_config.audio_buffer_high_water_mark_ms)
}

/// Проверяет, может ли worker прямо сейчас отправить или списать video packet.
pub(super) fn pending_video_work_available(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    let Some(packet) = session.pipeline.front_pending_video_packet() else {
        return false;
    };

    if !pending_video_packet_requires_decoder_send_capacity(session, packet) {
        return true;
    }

    if accurate_seek_pending_video_waits_for_decoder_readiness(session, tick_config) {
        return false;
    }

    if let Some(backpressure) = session
        .pipeline
        .video_decoder_send_backpressure(host_upload_ready_queue_capacity(tick_config))
    {
        return matches!(backpressure, VideoDecoderSendBackpressure::AbsentDecoder);
    }

    if normal_present_queue_blocks_video_admission(session, tick_config) {
        return false;
    }

    let decode_ahead_limit = if seek_fast_preroll_active(session, tick_config) {
        video_decode_ahead_limit(tick_config)
    } else {
        video_decode_ahead_target(tick_config)
    };

    can_send_video_packet_to_decoder(
        session,
        video_decoder_texture_limits(tick_config),
        video_decoder_decode_ahead_limits(tick_config, decode_ahead_limit),
        host_upload_ready_queue_capacity(tick_config),
        session.decoder_output_floor_applies_to_seek_preroll_packet(packet.pts, packet.generation),
        packet.pts,
    )
}

/// Проверяет, что Accurate seek уже упёрся в полный decoder send channel.
///
/// В этом состоянии очередной zero-delay tick не может отправить packet: он
/// снова получит bounded-channel backpressure. Поэтому planner должен перейти
/// к `DecodeReadiness`, где worker ждёт decoder activity или bounded fallback.
fn accurate_seek_pending_video_waits_for_decoder_readiness(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    seek_fast_preroll_active(session, tick_config)
        && decoder_send_queue_reached_tick_capacity(session, tick_config)
}

/// Сравнивает neutral decoder queue depth с tick capacity без доступа к backend storage.
///
/// `max_pending_video_packets` приходит из `decoder_packet_channel_frames` и
/// задаёт ту же границу, на которой `crossbeam-channel::bounded` создаёт
/// backpressure для send-side decoder queue.
fn decoder_send_queue_reached_tick_capacity(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    let Some(decoder_packet_queue_depth) = session.pipeline.video_decoder_packet_queue_depth()
    else {
        return false;
    };

    decoder_packet_queue_depth >= tick_config.max_pending_video_packets
}

/// Проверяет, может ли demuxer прочитать следующий packet без downstream overflow.
pub(super) fn demux_work_available(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
) -> bool {
    if !session.is_demuxing_active() || !session.pipeline.has_demuxer() {
        return false;
    }
    if session.installed_demux_read_is_blocked(now) {
        return false;
    }

    let seek_fast_preroll = seek_fast_preroll_active(session, tick_config);
    let prioritize_audio_catchup =
        !seek_fast_preroll && audio_demux_catchup_needed(session, tick_config);
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

/// Проверяет active accurate seek preroll через session boundary, без доступа к seek storage.
fn seek_fast_preroll_active(session: &PlayerSession, tick_config: &PlayerTickConfig) -> bool {
    session.active_accurate_seek_needs_fast_video_preroll(
        tick_config.effective_seek_resume_video_min_ready_frames(),
    )
}

/// Проверяет, нужен ли короткий poll decoded-frame readiness.
pub(super) fn decoder_readiness_poll_needed(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    let Some(decoder_packet_queue_depth) = session.pipeline.video_decoder_packet_queue_depth()
    else {
        return false;
    };

    decoder_readiness_poll_needed_for_state(DecoderReadinessPollState {
        video_track_selected: session.pipeline.has_selected_video_track(),
        video_frame_queue_len: session.pipeline.video_present_queue_len(),
        video_present_queue_target: video_present_queue_target(tick_config),
        worker_has_pending_video_packets: !session.pipeline.pending_video_packet_is_empty(),
        worker_has_demuxer: session.pipeline.has_demuxer(),
        decoder_has_queued_packets: decoder_packet_queue_depth > 0,
        decoder_has_in_flight_packets: session.pipeline.video_decode_in_flight_packets() > 0,
        seek_commit_active: session.has_active_seek_commit(),
    })
}

/// Read-only snapshot для чистой decoder readiness policy.
pub(super) struct DecoderReadinessPollState {
    /// В текущем media выбран video track.
    pub(super) video_track_selected: bool,

    /// Сколько decoded frames уже ждёт presentation scheduler.
    pub(super) video_frame_queue_len: usize,

    /// Целевой размер decoded-frame очереди до остановки короткого poll-а.
    pub(super) video_present_queue_target: usize,

    /// Worker уже держит packets, которые можно отправить decoder-у.
    pub(super) worker_has_pending_video_packets: bool,

    /// Demuxer ещё может дать новые decode inputs.
    pub(super) worker_has_demuxer: bool,

    /// Decoder boundary принял packets и ещё не подтвердил их завершение.
    pub(super) decoder_has_queued_packets: bool,

    /// Decoder уже выполняет packets текущего поколения.
    pub(super) decoder_has_in_flight_packets: bool,

    /// Активный seek ждёт landing/commit и не должен засыпать на in-flight decoder-е.
    pub(super) seek_commit_active: bool,
}

/// Чистая часть decoder readiness policy без реального GPU decoder thread.
pub(super) fn decoder_readiness_poll_needed_for_state(state: DecoderReadinessPollState) -> bool {
    let decoder_has_seek_in_flight_packets =
        state.seek_commit_active && state.decoder_has_in_flight_packets;
    let worker_has_decode_inputs = state.worker_has_pending_video_packets
        || state.worker_has_demuxer
        || state.decoder_has_queued_packets
        || decoder_has_seek_in_flight_packets;
    if !worker_has_decode_inputs {
        return false;
    }

    if decoder_has_seek_in_flight_packets {
        return state.video_track_selected;
    }

    if state.video_frame_queue_len < state.video_present_queue_target {
        return state.video_track_selected;
    }

    state.worker_has_pending_video_packets
}

/// Проверяет, должен ли активный seek получить bounded wakeup хотя бы для timeout/gates.
pub(super) fn seek_transition_needs_progress(session: &PlayerSession) -> bool {
    session.has_active_seek_commit()
}

/// Проверяет, нужен ли редкий progress wakeup без точного PTS deadline-а.
pub(super) fn active_pipeline_needs_coarse_progress(session: &PlayerSession) -> bool {
    matches!(
        session.playback_state(),
        PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Seeking
    )
}
