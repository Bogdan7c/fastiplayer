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
