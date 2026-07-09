use audio_core::AudioOutputWriteIntent;
use tracing::debug;

use crate::pipeline::AudioTempoOutputWriteStatus;
use crate::{
    AudioOutputSpec, AudioTempoPcmFormat, AudioTempoProcessorConfig, AudioTempoProcessorError,
    PlaybackRate, PlayerError, PlayerErrorKind, PlayerResult,
};

use super::PlayerSession;

impl PlayerSession {
    /// Пишет decoded PCM напрямую для чистого 1.0x или через уже активный tempo processor.
    ///
    /// После tempo→1x processor остаётся владельцем старого DSP tail-а до
    /// EOF/reset; такой PCM сохраняет `TempoProcessed` intent. Только clean 1x,
    /// где processor никогда не создавался, является bit-transparent direct path.
    pub(super) fn write_decoded_audio_samples_at_current_rate(
        &mut self,
        samples: &[f32],
        sample_rate: u32,
        channels: u32,
    ) -> PlayerResult<()> {
        let needs_tempo_processor = self.snapshot.playback_rate != PlaybackRate::NORMAL
            || self.pipeline.has_audio_tempo_processor();
        if !needs_tempo_processor {
            self.pipeline
                .record_passthrough_audio_history(samples, sample_rate, channels);
            let written_samples = self
                .pipeline
                .write_audio_output_samples(samples, AudioOutputWriteIntent::DirectDecodedPcm)
                .ok_or_else(|| missing_audio_output_error("direct decoded PCM"))?;
            return ensure_all_audio_samples_written(
                "direct decoded PCM",
                samples.len(),
                written_samples,
            );
        }

        let pcm_format = AudioTempoPcmFormat::from_audio_output_spec(AudioOutputSpec {
            sample_rate,
            channels,
        })
        .map_err(player_error_from_audio_tempo_error)?;
        self.ensure_audio_tempo_processor_for_decoded_spec(sample_rate, channels)?;
        let Some(process_result) = self
            .pipeline
            .process_audio_tempo_samples(samples, pcm_format)
        else {
            return Err(PlayerError::new(
                PlayerErrorKind::RuntimeError,
                "Audio tempo processor is missing for rate-aware playback".to_string(),
            ));
        };
        let routed_output = process_result.map_err(player_error_from_audio_tempo_error)?;
        ensure_routed_tempo_output_written(routed_output.write_status)?;

        Ok(())
    }

    /// Создаёт или синхронизирует tempo processor под текущий decoded PCM format/rate.
    pub(super) fn ensure_audio_tempo_processor_for_decoded_spec(
        &mut self,
        sample_rate: u32,
        channels: u32,
    ) -> PlayerResult<()> {
        let output_spec = AudioOutputSpec {
            sample_rate,
            channels,
        };
        let pcm_format = AudioTempoPcmFormat::from_audio_output_spec(output_spec)
            .map_err(player_error_from_audio_tempo_error)?;

        if let Some(processor_format) = self.pipeline.audio_tempo_pcm_format() {
            if processor_format == pcm_format {
                return Ok(());
            }

            // Mid-stream format change требует отдельной output reconfiguration;
            // старый tail нельзя ни отбросить, ни записать под новый layout.
            return Err(player_error_from_audio_tempo_error(
                AudioTempoProcessorError::PcmFormatMismatch {
                    expected: processor_format,
                    actual: pcm_format,
                }
                .into(),
            ));
        }

        let segment = self
            .pipeline
            .propose_audio_tempo_segment(self.snapshot.playback_rate)
            .map_err(player_error_from_audio_tempo_error)?;
        let config = AudioTempoProcessorConfig::new(pcm_format, segment);
        let processor = self
            .audio_tempo_processor_factory
            .create_processor(config)
            .map_err(player_error_from_audio_tempo_error)?;
        self.pipeline.install_audio_tempo_processor(processor);
        self.prime_audio_tempo_processor_from_passthrough_history(sample_rate, channels)?;
        self.pipeline.commit_audio_tempo_segment(segment);
        Ok(())
    }

    /// Праймит свежий tempo processor хвостом passthrough PCM без duplicate output.
    fn prime_audio_tempo_processor_from_passthrough_history(
        &mut self,
        sample_rate: u32,
        channels: u32,
    ) -> PlayerResult<()> {
        let history = self
            .pipeline
            .take_passthrough_audio_history_for_priming(sample_rate, channels);
        if history.is_empty() {
            return Ok(());
        }

        let pcm_format = AudioTempoPcmFormat::from_audio_output_spec(AudioOutputSpec {
            sample_rate,
            channels,
        })
        .map_err(player_error_from_audio_tempo_error)?;
        let prime_result = self
            .pipeline
            .prime_audio_tempo_history(&history, pcm_format)
            .ok_or_else(|| {
                PlayerError::new(
                    PlayerErrorKind::RuntimeError,
                    "Audio tempo processor disappeared during warmup".to_string(),
                )
            })
            .and_then(|result| result.map_err(player_error_from_audio_tempo_error));
        let prime_report = match prime_result {
            Ok(report) => report,
            Err(error) => {
                self.pipeline.clear_audio_tempo_processor();
                self.pipeline
                    .record_passthrough_audio_history(&history, sample_rate, channels);
                return Err(error);
            }
        };

        debug!(
            primed_samples = history.len(),
            produced_warmup_frames = prime_report.produced_stretched_output().frame_count().get(),
            pending_warmup_frames = prime_report.pending_processor_output().frame_count().get(),
            "Tempo processor праймлен passthrough историей"
        );
        Ok(())
    }

    /// Дренирует tempo processor tail при EOF, чтобы не потерять buffered stretched output.
    pub(super) fn flush_audio_tempo_processor_for_eof(&mut self) -> PlayerResult<()> {
        let Some(finish_result) = self.pipeline.finish_and_clear_audio_tempo_processor() else {
            return Ok(());
        };
        let routed_output = finish_result.map_err(player_error_from_audio_tempo_error)?;
        ensure_routed_tempo_output_written(routed_output.write_status)?;

        let pending_output_frames = routed_output
            .report
            .pending_processor_output()
            .frame_count()
            .get();
        if pending_output_frames != 0 {
            return Err(PlayerError::new(
                PlayerErrorKind::RuntimeError,
                format!(
                    "Audio tempo finish (EOF) left {pending_output_frames} pending output frames"
                ),
            ));
        }

        debug!(
            produced_output_frames = routed_output
                .report
                .produced_stretched_output()
                .frame_count()
                .get(),
            pending_output_frames, "Audio tempo finish routed complete tail"
        );
        Ok(())
    }
}

/// Преобразует neutral tempo/backend failures в runtime audio error без direct PCM fallback.
fn player_error_from_audio_tempo_error(error: anyhow::Error) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::RuntimeError,
        format!("Audio tempo error: {error}"),
    )
}

/// Строит typed runtime error для нарушенного output-slot инварианта.
fn missing_audio_output_error(context: &'static str) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::RuntimeError,
        format!("Audio output is missing while writing {context}"),
    )
}

/// Запрещает молчаливую потерю PCM при частичной записи в ring buffer.
fn ensure_all_audio_samples_written(
    context: &'static str,
    requested_samples: usize,
    written_samples: u64,
) -> PlayerResult<()> {
    if written_samples == requested_samples as u64 {
        return Ok(());
    }

    Err(PlayerError::new(
        PlayerErrorKind::RuntimeError,
        format!(
            "Audio output accepted only {written_samples} of {requested_samples} samples for {context}"
        ),
    ))
}

/// Проверяет typed routing result tempo PCM без слияния absent/partial/discard paths.
fn ensure_routed_tempo_output_written(status: AudioTempoOutputWriteStatus) -> PlayerResult<()> {
    match status {
        AudioTempoOutputWriteStatus::Written {
            requested_samples,
            written_samples,
        } => ensure_all_audio_samples_written(
            "tempo-processed PCM",
            requested_samples,
            written_samples,
        ),
        AudioTempoOutputWriteStatus::AudioOutputAbsent => {
            Err(missing_audio_output_error("tempo-processed PCM"))
        }
    }
}
