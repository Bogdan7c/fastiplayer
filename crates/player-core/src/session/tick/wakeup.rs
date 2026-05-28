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
        normal_present_queue_blocks_video_admission, seek_admission_active,
        video_decode_ahead_target, video_decoder_decode_ahead_limits, video_decoder_texture_limits,
        video_present_queue_target,
    },
    video_decoder_io::can_send_video_packet_to_decoder,
};
use crate::{
    PlaybackState, WorkerFrameTimingSnapshot, WorkerWakeupDiagnosticsSnapshot, WorkerWakeupReason,
    session::PlayerSession, session::audio_runtime::sanitize_audio_high_water_mark,
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
pub(super) fn demux_work_available(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
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
pub(super) fn decoder_readiness_poll_needed(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
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
pub(super) fn decoder_readiness_poll_needed_for_state(
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
