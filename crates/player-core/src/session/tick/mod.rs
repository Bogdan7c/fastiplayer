//! Playback tick и A/V scheduler.
//!
//! Этот модуль держит логику, которая раньше жила в `app-egui::main`:
//! чтение packets из demuxer, audio throttle, отправку video packets в decoder,
//! приём decoded frames, backpressure и выбор кадра для показа.

use std::time::{Duration, Instant};

use super::PlayerSession;
use crate::{PipelinePauseReason, PlaybackState};

mod audio_fallback_wakeup;
mod audio_gap_evidence;
mod audio_read_ahead;
mod audio_refill_cadence;
mod cut_short_wakeup;
mod demux_admission;
mod demux_read_failure;
mod held_audio_wakeup;
mod presentation_scheduler;
mod result_merge;
mod types;
mod video_backlog_recovery_admission;
mod video_decoder_io;
mod video_track_handoff;
mod wakeup;

pub(super) use demux_admission::sanitize_audio_demux_low_water_mark;

pub(crate) use presentation_scheduler::{
    SchedulerTimingDiagnosticsSnapshot, scheduler_timing_diagnostics,
};
pub use types::{
    PlayerPipelinePause, PlayerTickConfig, PlayerTickContext, PlayerTickPacket, PlayerTickResult,
    PlayerVideoDropReason, PlayerVideoFrameDrop,
};
pub(crate) use wakeup::PlayerWorkerWakeupPlan;

use audio_read_ahead::{
    StartupAudioBacklogRoom, audio_read_ahead_limit_ms, startup_audio_backlog_room,
};
use demux_admission::{
    demux_catch_up_deadline_for_tick, demux_packet_budget_for_tick, read_demux_packets,
};
use presentation_scheduler::{process_pending_video_packets, run_seek_fast_preroll_catch_up};

impl PlayerSession {
    /// Выполняет один playback tick.
    ///
    /// Shell вызывает этот метод один раз на redraw. Метод намеренно не рендерит
    /// и не знает про egui: он только продвигает media pipeline и возвращает
    /// компактный результат для телеметрии.
    #[must_use]
    pub fn tick(&mut self, tick_context: PlayerTickContext) -> PlayerTickResult {
        let mut tick_result = PlayerTickResult::default();

        self.service_pending_staged_preflight(tick_context.now);
        self.update_position_for_tick(tick_context.now);
        // Обрезанные данные: часы дошли до конца доступного — показ кончается здесь, а
        // прочитанное впрок дальше не играется (`eof_drain/cut_short_stop.rs`).
        self.stop_at_cut_short_playable_end_if_reached(tick_context.now);
        self.service_staged_position_preparation();
        self.service_prepared_demux_seek_receipts();

        let seek_fast_preroll_tick_handled =
            run_seek_fast_preroll_catch_up(self, tick_context, &mut tick_result);

        if !seek_fast_preroll_tick_handled
            && self.is_demuxing_active()
            && !self.prepared_demux_seek.receipt_pending()
            && self.pipeline.has_demuxer()
        {
            let demux_packet_budget = demux_packet_budget_for_tick(self, &tick_context.config);
            let demux_catch_up_deadline =
                demux_catch_up_deadline_for_tick(self, &tick_context.config, tick_context.now);
            read_demux_packets(
                self,
                &tick_context.config,
                &mut tick_result,
                demux_packet_budget,
                demux_catch_up_deadline,
            );
        }

        if self.is_demuxing_active() || self.is_eof_draining() {
            // Конец разрыва звука: звук забирает часы до записи нового PCM.
            self.resume_audio_after_gap_if_due(tick_context.now);
            self.process_pending_audio_packets_with_buffer_limit(audio_read_ahead_limit_ms(
                self,
                &tick_context.config,
            ));
            self.start_eof_audio_tail_if_needed();
        }
        // Звука впереди нет (доказано): видео дальше идёт по monotonic clock.
        let demux_audio_wait =
            audio_gap_evidence::demux_audio_priority_wait(self, &tick_context.config);
        self.hand_over_presentation_clock_for_audio_gap_if_proven(
            tick_context.now,
            demux_audio_wait,
        );

        self.diagnose_audio_output_starvation(tick_context.now);
        // Пропавшее устройство: поток мог сломаться между tick-ами — восстанавливаем звук
        // до video-шага, чтобы presentation clock не стоял на мёртвом output-е.
        self.recover_failed_audio_output_if_needed(tick_context.now);

        process_pending_video_packets(self, tick_context, &mut tick_result);
        self.observe_decoder_pace(&tick_context, &tick_result);
        // Demuxer мог создать deadline через `Instant::now()` уже после начала tick-а.
        // Runway decision обязан использовать один свежий owner timestamp, а не
        // более ранний `tick_context.now`, иначе remaining wait будет завышен.
        let demux_runway_decision_at = Instant::now();
        let audio_starvation_margin =
            super::demux_retry::DemuxAudioStarvationMargin::from_low_water_mark_ms(
                tick_context.config.audio_demux_low_water_mark_ms,
            );
        if let Err(error) = self.enter_buffering_for_demux_underrun_if_needed(
            demux_runway_decision_at,
            audio_starvation_margin,
        ) {
            self.mark_fatal_error(error);
        }
        self.finish_seek_commit_if_ready(tick_context.now, &tick_context.config);
        // Запас отложенного звука исчерпан, а кадра нет: ждать картинку дальше нельзя,
        // preroll стартует со звуком первым (`session/video_preroll_wait.rs`).
        if let StartupAudioBacklogRoom::Exhausted(audio_backlog) =
            startup_audio_backlog_room(self, &tick_context.config)
        {
            self.start_audio_first_after_audio_backlog_exhausted(audio_backlog);
        }
        // Короткий файл кончился раньше первого кадра, а видео больше нечего отдать:
        // картинки не будет, звук стартует один (иначе вечный `Draining`).
        self.start_audio_first_when_video_ended_in_eof_drain();
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
        // Tick публикует clock sample, но не владеет lifecycle re-anchor-ом.
        self.publish_clock_sample(playback_position);
        if let Some(progress_evidence) = self
            .seek_runtime
            .observe_post_commit_position_progress(playback_position, now)
        {
            tracing::info!(
                kind = "seek_acceptance",
                generation = progress_evidence.generation(),
                media_instance_id = self
                    .snapshot
                    .media_instance_id
                    .map(|identity| identity.get()),
                target_ms = progress_evidence.target_position().as_millis(),
                committed_ms = progress_evidence.committed_position().as_millis(),
                position_ms = progress_evidence.observed_position().as_millis(),
                progress_delta_ms = progress_evidence
                    .observed_position()
                    .saturating_sub(progress_evidence.committed_position())
                    .as_millis(),
                progress_delta_us = progress_evidence
                    .observed_position()
                    .saturating_sub(progress_evidence.committed_position())
                    .as_micros(),
                seek_elapsed_ms = progress_evidence.public_elapsed().as_millis(),
                public_to_progress_ms = progress_evidence.public_elapsed().as_millis(),
                receipt_to_progress_ms = progress_evidence.receipt_elapsed().as_millis(),
                commit_to_progress_ms = progress_evidence.commit_elapsed().as_millis(),
                "Post-seek position progress observed"
            );
        }
    }
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
mod tests;
