use std::time::{Duration, Instant};

use tracing::warn;

use crate::PlaybackState;

use super::PlayerSession;

/// Честная классификация starvation telemetry без приписывания backend-у native xrun-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioOutputStarvationObservation {
    /// Output callback реально не получил весь блок PCM из ring и дополнил его тишиной.
    OutputRingUnderrunProven,

    /// Ring близок к нулю, но callback silence-padding ещё не наблюдался.
    LowBufferRiskOnly,
}

impl AudioOutputStarvationObservation {
    /// Возвращает стабильную machine-readable формулировку для structured log-а.
    const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::OutputRingUnderrunProven => "output_ring_underrun_proven_by_silence_padding",
            Self::LowBufferRiskOnly => "low_buffer_starvation_risk_only",
        }
    }
}

/// Отделяет доказанный output-ring underrun от одного только риска starvation.
pub(super) fn classify_audio_output_starvation(
    new_silence_padding_callbacks: u64,
    buffer_level_ms: f64,
) -> Option<AudioOutputStarvationObservation> {
    const STARVATION_RISK_LEVEL_MS: f64 = 1.0;

    if new_silence_padding_callbacks > 0 {
        return Some(AudioOutputStarvationObservation::OutputRingUnderrunProven);
    }

    (buffer_level_ms.is_finite() && buffer_level_ms <= STARVATION_RISK_LEVEL_MS)
        .then_some(AudioOutputStarvationObservation::LowBufferRiskOnly)
}

impl PlayerSession {
    /// Диагностирует доказанный output underrun либо риск starvation при playback.
    ///
    /// Проверка уровня буфера в конце tick-а слепа к голоданию, которое тот же
    /// tick уже залатал. Legacy clock counter доказывает только callback
    /// silence-padding из-за пустого output ring-а; это не native CPAL xrun.
    /// Низкий текущий buffer без такой дельты остаётся лишь starvation risk.
    pub(super) fn diagnose_audio_output_starvation(&mut self, now: Instant) {
        const WARN_INTERVAL: Duration = Duration::from_secs(2);

        let previous_tick_at = self.last_tick_observed_at.replace(now);

        if self.snapshot.playback_state != PlaybackState::Playing
            || !self.pipeline.has_audio_clock()
            || self.eof_drain_needs_progress()
        {
            self.last_seen_audio_underrun_callbacks =
                self.pipeline.audio_clock_underrun_callbacks();
            return;
        }

        let silence_padding_callbacks = self.pipeline.audio_clock_underrun_callbacks();
        let new_silence_padding_callbacks =
            silence_padding_callbacks.saturating_sub(self.last_seen_audio_underrun_callbacks);
        self.last_seen_audio_underrun_callbacks = silence_padding_callbacks;

        let buffer_level_ms = self.audio_buffer_level_ms().unwrap_or(0.0);
        let Some(starvation_observation) =
            classify_audio_output_starvation(new_silence_padding_callbacks, buffer_level_ms)
        else {
            return;
        };

        let warn_is_due = self
            .last_audio_starvation_warn_at
            .is_none_or(|last| now.saturating_duration_since(last) >= WARN_INTERVAL);
        if !warn_is_due {
            return;
        }
        self.last_audio_starvation_warn_at = Some(now);

        let tick_gap_ms = previous_tick_at
            .map(|at| now.saturating_duration_since(at).as_secs_f64() * 1000.0)
            .unwrap_or(0.0);

        warn!(
            starvation_observation = starvation_observation.diagnostic_label(),
            new_silence_padding_callbacks,
            buffer_level_ms,
            tick_gap_ms,
            pending_audio_packets = self.pipeline.pending_audio_packet_len(),
            pending_video_packets = self.pipeline.pending_video_packet_len(),
            video_present_queue = self.pipeline.video_present_queue_len(),
            playback_rate = %self.snapshot.playback_rate,
            "Audio output starvation observation"
        );
    }
}
