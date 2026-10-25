//! Мост между tick-ом сессии и чистым [`DecoderPaceMonitor`](super::DecoderPaceMonitor).
//!
//! Здесь собирается наблюдение из intent-методов владельцев (pipeline, tick result) и
//! публикуется событие; сама логика оценки остаётся в мониторе.

use super::{
    AudioClockActivity, DecoderInputWork, DecoderPaceObservation, OutputPressure,
    PresentQueueStarvation, SeekActivity,
};
use crate::session::audio_stall::AudioClockStall;
use crate::session::tick::{PlayerTickContext, PlayerTickResult};
use crate::{PipelinePauseReason, PlayerEvent, PlayerSession};

/// Паузы, означающие, что декодер стоит из-за потребителя (выход заблокирован), а не из-за себя.
///
/// `DecoderPacketQueueFull` сюда НЕ входит: полный входной канал декодера значит,
/// что узкое место — сам декодер.
const fn is_output_backpressure(reason: PipelinePauseReason) -> bool {
    matches!(
        reason,
        PipelinePauseReason::WaitingForFreeSurface
            | PipelinePauseReason::WaitingForPresentQueue
            | PipelinePauseReason::WaitingForGpuRelease
            | PipelinePauseReason::DecoderControlQueueFull
            | PipelinePauseReason::HostUploadReadyQueueFull
            | PipelinePauseReason::HostUploadSlotsExhausted
    )
}

impl PlayerSession {
    /// Скармливает монитору наблюдение этого tick-а и публикует событие при отставании.
    pub(in crate::session) fn observe_decoder_pace(
        &mut self,
        tick_context: &PlayerTickContext,
        tick_result: &PlayerTickResult,
    ) {
        let observation = DecoderPaceObservation {
            now: tick_context.now,
            playback_state: self.playback_state(),
            seek_activity: if self.has_active_seek_commit() {
                SeekActivity::Active
            } else {
                SeekActivity::Idle
            },
            media_instance_id: self.snapshot.media_instance_id,
            render_generation: self.pipeline.render_generation(),
            playback_rate: self.snapshot.playback_rate,
            frame_interval: self.pipeline.video_frame_duration_estimate(),
            late_drops_this_tick: tick_result.late_video_drop_count(),
            starvation: if tick_result
                .has_pipeline_pause(|reason| reason == PipelinePauseReason::DecoderStarvation)
            {
                PresentQueueStarvation::Starved
            } else {
                PresentQueueStarvation::Fed
            },
            decoder_input: if self.pipeline.video_decoder_has_input_work() {
                DecoderInputWork::Pending
            } else {
                DecoderInputWork::Absent
            },
            output_pressure: if tick_result.has_pipeline_pause(is_output_backpressure) {
                OutputPressure::Blocked
            } else {
                OutputPressure::Clear
            },
            // Пауза планировщика при остановке звука тоже помечена DecoderStarvation
            // (на неё опираются счётчики диагностики), поэтому причину различаем здесь.
            audio_clock: match self.audio_clock_stall(tick_context.now, &tick_context.config) {
                AudioClockStall::Running => AudioClockActivity::Running,
                AudioClockStall::Stalled { .. } => AudioClockActivity::Stalled,
            },
        };

        if let Some(evidence) = self
            .decoder_pace
            .observe(observation, &tick_context.config.decoder_pace)
        {
            self.push_player_event(PlayerEvent::VideoDecoderFallingBehind(evidence));
        }
    }
}
