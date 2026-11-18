//! Декодер HE-AAC (FFmpeg из crate `audio-ffmpeg`) под договор `AudioDecoder` плеера.
//!
//! Политика ошибок та же, что у Symphonia-декодера: битый пакет — `warn` и
//! пустой буфер (звук не останавливается, синхронизация не уезжает); любая
//! другая ошибка — ошибка декодирования, которую увидит player-core.

use anyhow::{Context, Result};
use audio_core::{AudioChannelLayout, AudioDecoder, AudioDecoderConfig, EncodedAudioPacket};
use audio_ffmpeg::{AacStreamConfig, AudioFfmpegError, FfmpegAacDecoder};
use tracing::{info, warn};

/// HE-AAC поверх FFmpeg с форматом последнего непустого буфера.
pub(super) struct HeAacDecoder {
    track_id: u32,
    codec_id: String,
    decoder: FfmpegAacDecoder,
    /// Частота последнего непустого PCM (0 — PCM ещё не было).
    sample_rate: u32,
    /// Раскладка последнего непустого PCM.
    channel_layout: Option<AudioChannelLayout>,
}

impl HeAacDecoder {
    /// Открывает FFmpeg под то, что известно о потоке.
    pub(super) fn open(
        config: &AudioDecoderConfig,
        stream_config: &AacStreamConfig,
    ) -> Result<Self> {
        let decoder = FfmpegAacDecoder::open(stream_config).with_context(|| {
            format!(
                "HE-AAC (SBR/Parametric Stereo) decoder for {} could not open",
                config.codec_id()
            )
        })?;
        info!(
            track_id = config.track_id(),
            codec_id = %config.codec_id(),
            "HE-AAC decoder created (FFmpeg)"
        );
        Ok(Self {
            track_id: config.track_id(),
            codec_id: config.codec_id().to_owned(),
            decoder,
            sample_rate: 0,
            channel_layout: None,
        })
    }
}

/// Что известно о потоке до первого пакета: паспорт или частота/каналы ядра.
pub(super) fn stream_config_for(config: &AudioDecoderConfig) -> Result<AacStreamConfig> {
    if let Some(audio_specific_config) = config.codec_private() {
        return Ok(AacStreamConfig::AudioSpecificConfig(
            audio_specific_config.to_vec(),
        ));
    }
    match (config.sample_rate(), config.channels()) {
        (Some(core_sample_rate), Some(core_channels)) => Ok(AacStreamConfig::BareFrames {
            core_sample_rate,
            core_channels,
        }),
        _ => anyhow::bail!(
            "HE-AAC stream {} has neither an AudioSpecificConfig nor a core sample rate and channel count",
            config.codec_id()
        ),
    }
}

impl AudioDecoder for HeAacDecoder {
    fn decode(&mut self, packet: &EncodedAudioPacket<'_>) -> Result<Vec<f32>> {
        if packet.track_id() != self.track_id {
            anyhow::bail!(
                "Audio packet track mismatch: decoder track {}, packet track {}",
                self.track_id,
                packet.track_id()
            );
        }
        match self.decoder.decode_packet(packet.data()) {
            Ok(Some(decoded)) => {
                // Формат публикуется вместе с буфером: getters описывают именно его.
                self.sample_rate = decoded.output_spec.sample_rate;
                self.channel_layout = Some(decoded.output_spec.channel_layout);
                Ok(decoded.interleaved_samples)
            }
            Ok(None) => Ok(Vec::new()),
            Err(AudioFfmpegError::CorruptPacket { message }) => {
                warn!(
                    codec_id = %self.codec_id,
                    error = %message,
                    "Corrupted audio packet skipped by HE-AAC decoder"
                );
                Ok(Vec::new())
            }
            Err(error) => Err(anyhow::Error::new(error)
                .context(format!("HE-AAC decode error for {}", self.codec_id))),
        }
    }

    fn reset(&mut self) -> Result<()> {
        self.decoder.flush();
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u32 {
        self.channel_layout
            .map_or(0, |channel_layout| channel_layout.channel_count())
    }

    fn channel_layout(&self) -> Option<AudioChannelLayout> {
        self.channel_layout
    }
}
