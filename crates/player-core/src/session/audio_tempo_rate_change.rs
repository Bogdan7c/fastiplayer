use tracing::debug;

use crate::{
    AudioTempoPcmFormat, AudioTempoProcessReport, AudioTempoProcessorConfig, PlaybackRate,
    PlaybackRateAudioTempoRejectReason, PlayerCommandReject, PlayerError, PlayerErrorKind,
};

use super::PlayerSession;

/// Подготовленный результат audio-части атомарной смены playback rate.
pub(super) struct PreparedAudioTempoRateChange {
    /// Neutral report присутствует только после подтверждённой DSP mutation.
    tempo_report: Option<AudioTempoProcessReport>,
}

impl PreparedAudioTempoRateChange {
    /// Video-only либо чистый direct-PCM `1.0x` не требуют DSP mutation.
    fn no_processor_change() -> Self {
        Self { tempo_report: None }
    }

    /// Сохраняет точный pending mapping подтверждённого tempo segment-а.
    fn tempo_segment_applied(report: AudioTempoProcessReport) -> Self {
        Self {
            tempo_report: Some(report),
        }
    }

    /// Возвращает report только для перехода, реально затронувшего tempo backend.
    pub(super) fn tempo_report(&self) -> Option<&AudioTempoProcessReport> {
        self.tempo_report.as_ref()
    }
}

impl PlayerSession {
    /// Подготавливает audio backend до изменения snapshot и clock mapping.
    ///
    /// Ошибка реализует согласованную политику 1A: новый rate отклоняется,
    /// старый processor/audio path остаются активными, session не становится
    /// video-only и не переходит в `Failed`.
    pub(super) fn prepare_audio_tempo_rate_change(
        &mut self,
        playback_rate: PlaybackRate,
    ) -> Result<PreparedAudioTempoRateChange, PlayerCommandReject> {
        if !self.pipeline.has_selected_audio_track() {
            return Ok(PreparedAudioTempoRateChange::no_processor_change());
        }

        if !self.pipeline.has_audio_output() || !self.pipeline.has_audio_clock() {
            return Err(audio_output_unavailable_reject());
        }

        if self.pipeline.has_audio_tempo_processor() {
            return self.prepare_existing_audio_tempo_processor(playback_rate);
        }

        if playback_rate == PlaybackRate::NORMAL {
            return Ok(PreparedAudioTempoRateChange::no_processor_change());
        }

        self.prepare_new_audio_tempo_processor(playback_rate)
    }

    /// Атомарно переключает segment уже установленного processor-а.
    fn prepare_existing_audio_tempo_processor(
        &mut self,
        playback_rate: PlaybackRate,
    ) -> Result<PreparedAudioTempoRateChange, PlayerCommandReject> {
        let segment = self
            .pipeline
            .propose_audio_tempo_segment(playback_rate)
            .map_err(|error| self.reject_audio_tempo_rate_change(error))?;

        let report = match self.pipeline.set_audio_tempo_segment(segment) {
            Some(Ok(report)) => report,
            Some(Err(error)) => return Err(self.reject_audio_tempo_rate_change(error)),
            None => {
                return Err(self.reject_audio_tempo_rate_change(anyhow::anyhow!(
                    "audio tempo processor исчез во время атомарной смены segment-а"
                )));
            }
        };

        self.pipeline.commit_audio_tempo_segment(segment);
        log_prepared_tempo_segment(&report);

        Ok(PreparedAudioTempoRateChange::tempo_segment_applied(report))
    }

    /// Создаёт и праймит первый processor до публикации нового playback rate.
    fn prepare_new_audio_tempo_processor(
        &mut self,
        playback_rate: PlaybackRate,
    ) -> Result<PreparedAudioTempoRateChange, PlayerCommandReject> {
        let pcm_format = self
            .pipeline
            .passthrough_audio_history_pcm_format()
            .ok_or_else(pcm_format_not_ready_reject)?;
        let segment = self
            .pipeline
            .propose_audio_tempo_segment(playback_rate)
            .map_err(|error| self.reject_audio_tempo_rate_change(error))?;
        let processor = self
            .audio_tempo_processor_factory
            .create_processor(AudioTempoProcessorConfig::new(pcm_format, segment))
            .map_err(|error| self.reject_audio_tempo_rate_change(error))?;

        let sample_rate = pcm_format.sample_rate_hz().get();
        let channels = pcm_format.channel_count().get();
        let warmup_history = self
            .pipeline
            .take_passthrough_audio_history_for_priming(sample_rate, channels);
        self.pipeline.install_audio_tempo_processor(processor);

        let prime_result = self.prime_new_audio_tempo_processor(&warmup_history, pcm_format);
        let prime_report = match prime_result {
            Ok(report) => report,
            Err(error) => {
                // Новый processor ещё не опубликован clock/snapshot слоям: его можно
                // безопасно отбросить и вернуть direct-PCM history без потери данных.
                self.pipeline.clear_audio_tempo_processor();
                self.pipeline.record_passthrough_audio_history(
                    &warmup_history,
                    sample_rate,
                    channels,
                );
                return Err(self.reject_audio_tempo_rate_change(error));
            }
        };

        self.pipeline.commit_audio_tempo_segment(segment);
        log_prepared_tempo_segment(&prime_report);

        Ok(PreparedAudioTempoRateChange::tempo_segment_applied(
            prime_report,
        ))
    }

    /// Праймит свежий processor position-free историей direct-PCM пути.
    fn prime_new_audio_tempo_processor(
        &mut self,
        warmup_history: &[f32],
        pcm_format: AudioTempoPcmFormat,
    ) -> anyhow::Result<AudioTempoProcessReport> {
        self.pipeline
            .prime_audio_tempo_history(warmup_history, pcm_format)
            .ok_or_else(|| anyhow::anyhow!("audio tempo processor исчез до завершения warmup"))?
    }

    /// Записывает recoverable diagnostic и строит typed non-fatal reject.
    fn reject_audio_tempo_rate_change(&mut self, error: anyhow::Error) -> PlayerCommandReject {
        self.record_recoverable_error(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!("Audio tempo rate change rejected: {error}"),
        ));
        PlayerCommandReject::PlaybackRateAudioTempoUnavailable {
            reason: PlaybackRateAudioTempoRejectReason::BackendRejected,
        }
    }
}

/// Отдельный typed reject сообщает, что backend ещё нельзя корректно сконфигурировать.
fn pcm_format_not_ready_reject() -> PlayerCommandReject {
    PlayerCommandReject::PlaybackRateAudioTempoUnavailable {
        reason: PlaybackRateAudioTempoRejectReason::PcmFormatNotReady,
    }
}

/// Потерянный selected-audio output не превращается в молчаливый video-only fallback.
pub(super) fn audio_output_unavailable_reject() -> PlayerCommandReject {
    PlayerCommandReject::PlaybackRateAudioTempoUnavailable {
        reason: PlaybackRateAudioTempoRejectReason::AudioOutputUnavailable,
    }
}

/// Единый marker подтверждает backend prepare до session-level commit-а.
fn log_prepared_tempo_segment(report: &AudioTempoProcessReport) {
    debug!(
        segment_id = report.segment_id().get(),
        effective_ratio = %report.effective_ratio(),
        pending_output_frames = report.pending_processor_output().frame_count().get(),
        pending_output_ms = report.pending_processor_output().duration().as_millis(),
        input_latency_media_ms = report.input_latency().duration().as_millis(),
        output_latency_ms = report.output_latency().duration().as_millis(),
        "Audio tempo segment подготовлен для атомарного commit"
    );
}
