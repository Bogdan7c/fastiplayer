//! Session-часть передачи времени хвосту видео после конца звука.
//!
//! Решение «пора передавать» принимает EOF drain: только он знает, что demux уже на EOF,
//! seek не владеет lifecycle-ом, а drain ждёт именно хвост видео. Сами часы и проверка
//! «звук действительно доигран» принадлежат `PlaybackPipeline`
//! (`hand_over_presentation_clock_after_audio_end`). Если звук и видео кончаются вместе,
//! drain не ждёт видео, и этот путь не вызывается — поведение таких файлов не меняется.

use std::time::Instant;

use tracing::debug;

use crate::pipeline::AudioEndClockHandoverOutcome;

use super::PlayerSession;

impl PlayerSession {
    /// Передаёт presentation clock monotonic-часам, если звук доигран раньше видео.
    ///
    /// Вызывается EOF drain-ом, когда его blocker — ещё не показанный хвост видео.
    /// Повторные вызовы безопасны: pipeline отвечает `AlreadyHandedOver` без изменений.
    pub(super) fn hand_over_presentation_clock_for_video_tail(&mut self, now: Instant) {
        // Пока seek commit активен или drain стоит на паузе, время двигать нельзя.
        if !self.eof_drain_needs_progress() {
            return;
        }

        let outcome = self
            .pipeline
            .hand_over_presentation_clock_after_audio_end(now, self.snapshot.playback_rate);
        match outcome {
            AudioEndClockHandoverOutcome::HandedOver { audio_end_position } => {
                debug!(
                    audio_end_position_ms = audio_end_position.as_secs_f64() * 1000.0,
                    playback_rate = self.snapshot.playback_rate.as_f32(),
                    queued_video_frames = self.pipeline.video_present_queue_len(),
                    pending_video_packets = self.pipeline.pending_video_packet_len(),
                    "Audio ended before video; video tail continues on monotonic clock"
                );
            }
            // Нечего передавать (нет audio clock), уже передано или звук ещё играет:
            // текущий источник времени остаётся прежним.
            AudioEndClockHandoverOutcome::AlreadyHandedOver
            | AudioEndClockHandoverOutcome::NoAudioClock
            | AudioEndClockHandoverOutcome::AudioTailNotDrained { .. }
            // Эти исходы относятся к передаче на время разрыва звука, EOF их не вызывает.
            | AudioEndClockHandoverOutcome::AudioOutputStillPlaying { .. }
            | AudioEndClockHandoverOutcome::NoAudioOutput => {}
        }
    }
}
