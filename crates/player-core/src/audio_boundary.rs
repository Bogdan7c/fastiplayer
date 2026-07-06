use std::sync::Arc;

use audio_core::{
    AudioDecoderConfig, AudioDecoderFactory, AudioDecoderHandle, AudioOutputFactory,
    AudioOutputSpec, PlayerAudioOutput,
};

/// Factory по умолчанию для tests/manual `PlayerSession::new` без concrete audio crate.
pub(crate) struct MissingAudioDecoderFactory;

impl AudioDecoderFactory for MissingAudioDecoderFactory {
    /// Явно сообщает, что production decoder adapter не был установлен composition layer-ом.
    fn create_decoder(&self, config: AudioDecoderConfig) -> anyhow::Result<AudioDecoderHandle> {
        anyhow::bail!(
            "audio decoder factory is not installed for codec {}",
            config.codec_id()
        )
    }
}

/// Создаёт shared missing decoder factory для default-конструкторов без backend deps.
pub(crate) fn missing_audio_decoder_factory() -> Arc<dyn AudioDecoderFactory> {
    Arc::new(MissingAudioDecoderFactory)
}

/// Factory по умолчанию для tests/manual `PlayerSession::new` без concrete CPAL wiring.
pub(crate) struct MissingAudioOutputFactory;

impl AudioOutputFactory for MissingAudioOutputFactory {
    /// Явно сообщает, что production output adapter не был установлен composition layer-ом.
    fn create_output(&self, _spec: AudioOutputSpec) -> anyhow::Result<Box<dyn PlayerAudioOutput>> {
        anyhow::bail!("audio output factory is not installed")
    }
}

/// Создаёт shared missing output factory для default-конструкторов без CPAL side effects.
pub(crate) fn missing_audio_output_factory() -> Arc<dyn AudioOutputFactory> {
    Arc::new(MissingAudioOutputFactory)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::Result;

    use crate::{
        AudioTempoChannelCount, AudioTempoDecodedMedia, AudioTempoFrameCount, AudioTempoPcmFormat,
        AudioTempoProcessReport, AudioTempoProcessor, AudioTempoProcessorConfig,
        AudioTempoProcessorFactory, AudioTempoProcessorHandle, AudioTempoRatio,
        AudioTempoReportFrameCounts, AudioTempoSampleRateHz, AudioTempoSegment,
        AudioTempoSegmentId, AudioTempoStretchedOutput,
    };

    struct CompileOnlyTempoProcessor {
        config: AudioTempoProcessorConfig,
    }

    impl CompileOnlyTempoProcessor {
        fn zero_report(&self) -> AudioTempoProcessReport {
            AudioTempoProcessReport::from_frame_counts(
                self.config.pcm_format(),
                self.config.initial_segment(),
                AudioTempoReportFrameCounts::ZERO,
            )
        }
    }

    impl AudioTempoProcessor for CompileOnlyTempoProcessor {
        fn process_decoded_media(
            &mut self,
            _decoded_media: AudioTempoDecodedMedia<'_>,
        ) -> Result<AudioTempoStretchedOutput> {
            AudioTempoStretchedOutput::new(Vec::new(), self.zero_report(), self.config.pcm_format())
        }

        fn flush(&mut self) -> Result<AudioTempoStretchedOutput> {
            AudioTempoStretchedOutput::new(Vec::new(), self.zero_report(), self.config.pcm_format())
        }

        fn reset(&mut self) -> Result<AudioTempoProcessReport> {
            Ok(self.zero_report())
        }
    }

    struct CompileOnlyTempoFactory;

    impl AudioTempoProcessorFactory for CompileOnlyTempoFactory {
        fn create_processor(
            &self,
            config: AudioTempoProcessorConfig,
        ) -> Result<AudioTempoProcessorHandle> {
            Ok(Box::new(CompileOnlyTempoProcessor { config }))
        }
    }

    fn compile_only_config() -> AudioTempoProcessorConfig {
        let pcm_format = AudioTempoPcmFormat::new(
            AudioTempoSampleRateHz::new(48_000).expect("sample rate should be valid"),
            AudioTempoChannelCount::new(2).expect("channel count should be valid"),
        );

        AudioTempoProcessorConfig::new(
            pcm_format,
            AudioTempoSegment::new(AudioTempoSegmentId::new(1), AudioTempoRatio::NORMAL),
        )
    }

    #[test]
    fn audio_tempo_boundary_is_visible_as_neutral_trait_objects() {
        let config = compile_only_config();
        let mut processor: AudioTempoProcessorHandle =
            Box::new(CompileOnlyTempoProcessor { config });
        let factory: Arc<dyn AudioTempoProcessorFactory> = Arc::new(CompileOnlyTempoFactory);

        let reset_report = processor
            .reset()
            .expect("compile-only processor reset should succeed");
        let _created_processor = factory
            .create_processor(config)
            .expect("compile-only factory should create neutral trait object");

        assert_eq!(
            reset_report.produced_stretched_output().frame_count(),
            AudioTempoFrameCount::ZERO
        );
    }
}
