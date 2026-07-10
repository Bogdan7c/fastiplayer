//! Playback tick и A/V scheduler.
//!
//! Этот модуль держит логику, которая раньше жила в `app-egui::main`:
//! чтение packets из demuxer, audio throttle, отправку video packets в decoder,
//! приём decoded frames, backpressure и выбор кадра для показа.

use std::time::{Duration, Instant};

use super::PlayerSession;
use crate::{PipelinePauseReason, PlaybackState};

mod demux_admission;
mod presentation_scheduler;
mod types;
mod video_backlog_recovery_admission;
mod video_decoder_io;
mod wakeup;

pub(crate) use presentation_scheduler::{
    SchedulerTimingDiagnosticsSnapshot, scheduler_timing_diagnostics,
};
pub use types::{
    PlayerPipelinePause, PlayerTickConfig, PlayerTickContext, PlayerTickPacket, PlayerTickResult,
    PlayerVideoDropReason, PlayerVideoFrameDrop,
};
pub(crate) use wakeup::PlayerWorkerWakeupPlan;

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

        self.update_position_for_tick(tick_context.now);

        let seek_fast_preroll_tick_handled =
            run_seek_fast_preroll_catch_up(self, tick_context, &mut tick_result);

        if !seek_fast_preroll_tick_handled
            && self.is_demuxing_active()
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
            self.process_pending_audio_packets_with_buffer_limit(
                tick_context.config.audio_buffer_high_water_mark_ms,
            );
            self.start_eof_audio_tail_if_needed();
        }

        self.diagnose_audio_output_starvation(tick_context.now);

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
        // Tick публикует clock sample, но не владеет lifecycle re-anchor-ом.
        self.publish_clock_sample(playback_position);
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
