use audio_core::AudioOutputWriteIntent;
use tracing::debug;

use crate::pipeline::AudioOutputRoutingStatus;
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
        output_spec: AudioOutputSpec,
    ) -> PlayerResult<()> {
        let needs_tempo_processor = self.snapshot.playback_rate != PlaybackRate::NORMAL
            || self.pipeline.has_audio_tempo_processor();
        if !needs_tempo_processor {
            self.pipeline
                .record_passthrough_audio_history(samples, output_spec);
            let write_status = self
                .pipeline
                .write_audio_output_samples(samples, AudioOutputWriteIntent::DirectDecodedPcm);
            return ensure_audio_output_written("direct decoded PCM", write_status);
        }

        let pcm_format = AudioTempoPcmFormat::from_audio_output_spec(output_spec)
            .map_err(player_error_from_audio_tempo_error)?;
        self.ensure_audio_tempo_processor_for_decoded_spec(output_spec)?;
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
        ensure_audio_output_written("tempo-processed PCM", routed_output.write_status)?;

        Ok(())
    }

    /// Создаёт или синхронизирует tempo processor под текущий decoded PCM format/rate.
    pub(super) fn ensure_audio_tempo_processor_for_decoded_spec(
        &mut self,
        output_spec: AudioOutputSpec,
    ) -> PlayerResult<()> {
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
        self.prime_audio_tempo_processor_from_passthrough_history(output_spec)?;
        self.pipeline.commit_audio_tempo_segment(segment);
        Ok(())
    }

    /// Праймит свежий tempo processor хвостом passthrough PCM без duplicate output.
    fn prime_audio_tempo_processor_from_passthrough_history(
        &mut self,
        output_spec: AudioOutputSpec,
    ) -> PlayerResult<()> {
        let history = self
            .pipeline
            .take_passthrough_audio_history_for_priming(output_spec);
        if history.is_empty() {
            return Ok(());
        }

        let pcm_format = AudioTempoPcmFormat::from_audio_output_spec(output_spec)
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
                    .record_passthrough_audio_history(&history, output_spec);
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
        ensure_audio_output_written("tempo EOF PCM", routed_output.write_status)?;

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

/// Запрещает молчаливую потерю PCM, не смешивая input/output frame domains.
fn ensure_audio_output_written(
    context: &'static str,
    status: AudioOutputRoutingStatus,
) -> PlayerResult<()> {
    match status {
        AudioOutputRoutingStatus::Written(report) if report.is_complete() => Ok(()),
        AudioOutputRoutingStatus::Written(report) => Err(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!(
                "Audio output queued only {} of {} output frames converted from {} input frames for {context}",
                report.queued_output_frames().get(),
                report.prepared_output_frames().get(),
                report.submitted_input_frames().get(),
            ),
        )),
        AudioOutputRoutingStatus::WriteFailed(error) => Err(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!("Audio output rejected {context}: {error}"),
        )),
        AudioOutputRoutingStatus::AudioOutputAbsent => Err(missing_audio_output_error(context)),
    }
}
