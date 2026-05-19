//! Audio decoder boundary и Opus реализация поверх libopus (opus crate).
//!
//! Принимает raw Opus packet bytes (из demuxer) и возвращает
//! декодированные PCM f32 samples (interleaved).
//!
//! Архитектура:
//! - AudioDecoder описывает codec-neutral contract для player-core
//! - create_audio_decoder() выбирает concrete decoder по codec_core::AudioCodec
//! - OpusDecoder сохраняет прежний decode/reset path и формат Vec<f32>
//! - PCM остаётся interleaved, чтобы AudioOutput не знал о codec backend-е
//!
//! Зависимость: opus crate → требует libopus в системе.
//! На Linux: apt install libopus-dev / pacman -S opus

use anyhow::{Context, Result};
use codec_core::AudioCodec;
use thiserror::Error;
use tracing::info;

/// Максимальное количество samples на packet (120ms @ 48kHz stereo).
const MAX_SAMPLES_PER_PACKET: usize = 48000 * 2 * 120 / 1000;

/// Codec-neutral audio decoder contract для playback pipeline.
pub trait AudioDecoder: Send {
    /// Декодирует один encoded audio packet в interleaved PCM f32.
    fn decode(&mut self, packet_data: &[u8]) -> Result<Vec<f32>>;

    /// Сбрасывает codec state после seek или смены discontinuity.
    fn reset(&mut self) -> Result<()>;

    /// Возвращает sample rate decoded PCM.
    fn sample_rate(&self) -> u32;

    /// Возвращает количество interleaved channels decoded PCM.
    fn channels(&self) -> u32;
}

/// Владеющий handle codec-neutral decoder-а, который можно хранить в player pipeline.
pub type AudioDecoderHandle = Box<dyn AudioDecoder + Send>;

/// Ошибки audio decoder factory, которые caller может downcast-ить из anyhow.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AudioDecoderError {
    /// Codec известен общей модели, но production decoder для него ещё не добавлен.
    #[error("Audio codec {codec} is not supported by the audio decoder factory")]
    UnsupportedCodec {
        /// Codec, для которого нет concrete audio decoder implementation.
        codec: AudioCodec,
    },
}

/// Создаёт codec-neutral decoder object для заданного audio codec.
pub fn create_audio_decoder(
    codec: AudioCodec,
    sample_rate: u32,
    channels: u32,
) -> Result<AudioDecoderHandle> {
    match codec {
        AudioCodec::Opus => Ok(Box::new(OpusDecoder::new(sample_rate, channels)?)),
        AudioCodec::Aac | AudioCodec::Vorbis => {
            Err(AudioDecoderError::UnsupportedCodec { codec }.into())
        }
    }
}

/// Декодер для Opus audio.
///
/// Владеет opus::Decoder и reusable buffer.
pub struct OpusDecoder {
    /// Opus decoder — thread-safe (Send + Sync).
    decoder: opus::Decoder,

    /// Sample rate декодированного audio (Гц).
    /// Opus всегда декодирует в 48kHz internally.
    sample_rate: u32,

    /// Количество каналов (1 = mono, 2 = stereo).
    channels: u32,

    /// Reusable buffer для i16 samples из opus decoder.
    i16_buffer: Vec<i16>,
}

impl OpusDecoder {
    /// Создаёт Opus декодер.
    ///
    /// sample_rate — из track info (обычно 48000 для Opus).
    /// channels — количество каналов (1 или 2).
    pub fn new(sample_rate: u32, channels: u32) -> Result<Self> {
        let opus_channels = match channels {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            _ => anyhow::bail!(
                "Opus поддерживает только mono/stereo, получено: {}",
                channels
            ),
        };

        let decoder = opus::Decoder::new(sample_rate, opus_channels)
            .context("Не удалось создать Opus декодер. Убедитесь что libopus установлен")?;

        info!(sample_rate, channels, "Opus декодер создан (opus crate)");

        Ok(Self {
            decoder,
            sample_rate,
            channels,
            i16_buffer: vec![0i16; MAX_SAMPLES_PER_PACKET],
        })
    }

    /// Декодирует raw Opus packet в PCM f32 samples (interleaved).
    ///
    /// packet_data — raw Opus packet bytes.
    ///
    /// Возвращает Vec<f32> с interleaved samples.
    /// При corrupted packet возвращает пустой Vec.
    pub fn decode(&mut self, packet_data: &[u8]) -> Result<Vec<f32>> {
        // Декодируем в i16 buffer.
        match self
            .decoder
            .decode(packet_data, &mut self.i16_buffer, false)
        {
            Ok(sample_count) => {
                // opus::Decoder::decode возвращает samples per channel.
                // Для stereo total interleaved samples = sample_count * channels.
                let total_samples = sample_count * self.channels as usize;
                // Конвертируем i16 → f32: [-32768, 32767] → [-1.0, 1.0].
                let f32_samples: Vec<f32> = self.i16_buffer[..total_samples]
                    .iter()
                    .map(|&s| s as f32 / 32768.0)
                    .collect();

                tracing::trace!(
                    input_bytes = packet_data.len(),
                    output_samples = sample_count,
                    channels = self.channels,
                    "Opus packet decoded"
                );

                Ok(f32_samples)
            }
            Err(e) if e.code() == opus::ErrorCode::InvalidPacket => {
                // Corrupted packet — пропускаем.
                tracing::warn!("Corrupted Opus packet, skipping");
                Ok(Vec::new())
            }
            Err(e) => {
                anyhow::bail!("Opus decode error: {}", e)
            }
        }
    }

    /// Сбрасывает внутреннее состояние Opus decoder после container seek.
    pub fn reset(&mut self) -> Result<()> {
        self.decoder
            .reset_state()
            .map_err(|error| anyhow::anyhow!("Opus reset error: {error}"))
    }

    /// Возвращает sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Возвращает количество каналов.
    pub fn channels(&self) -> u32 {
        self.channels
    }
}

impl AudioDecoder for OpusDecoder {
    /// Декодирует Opus packet через прежний concrete path.
    fn decode(&mut self, packet_data: &[u8]) -> Result<Vec<f32>> {
        OpusDecoder::decode(self, packet_data)
    }

    /// Сбрасывает Opus state через прежний concrete path.
    fn reset(&mut self) -> Result<()> {
        OpusDecoder::reset(self)
    }

    /// Возвращает sample rate, с которым создан Opus decoder.
    fn sample_rate(&self) -> u32 {
        OpusDecoder::sample_rate(self)
    }

    /// Возвращает количество каналов, с которым создан Opus decoder.
    fn channels(&self) -> u32 {
        OpusDecoder::channels(self)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use codec_core::AudioCodec;

    use super::{AudioDecoder, AudioDecoderError, AudioDecoderHandle, create_audio_decoder};

    /// Fake decoder нужен только для проверки object-safe contract без libopus path.
    struct FakeAudioDecoder {
        /// Sample rate, который должен быть виден через trait object.
        sample_rate: u32,

        /// Количество каналов, которое должно быть видно через trait object.
        channels: u32,
    }

    impl AudioDecoder for FakeAudioDecoder {
        /// Fake decode возвращает входной размер как sample, чтобы путь был наблюдаемым.
        fn decode(&mut self, packet_data: &[u8]) -> Result<Vec<f32>> {
            Ok(vec![packet_data.len() as f32])
        }

        /// Fake reset не владеет внешними ресурсами.
        fn reset(&mut self) -> Result<()> {
            Ok(())
        }

        /// Возвращает sample rate fake decoder-а.
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        /// Возвращает количество каналов fake decoder-а.
        fn channels(&self) -> u32 {
            self.channels
        }
    }

    #[test]
    fn factory_creates_opus_decoder_for_valid_mono_and_stereo_params() {
        let mono_decoder = create_audio_decoder(AudioCodec::Opus, 48_000, 1)
            .expect("mono Opus decoder should be created");
        let stereo_decoder = create_audio_decoder(AudioCodec::Opus, 48_000, 2)
            .expect("stereo Opus decoder should be created");

        assert_eq!(mono_decoder.sample_rate(), 48_000);
        assert_eq!(mono_decoder.channels(), 1);
        assert_eq!(stereo_decoder.sample_rate(), 48_000);
        assert_eq!(stereo_decoder.channels(), 2);
    }

    #[test]
    fn factory_keeps_opus_channel_validation_message() {
        let error = match create_audio_decoder(AudioCodec::Opus, 48_000, 6) {
            Ok(_) => panic!("invalid Opus channel count should be rejected"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "Opus поддерживает только mono/stereo, получено: 6"
        );
    }

    #[test]
    fn unsupported_codec_returns_stable_typed_error() {
        let error = match create_audio_decoder(AudioCodec::Aac, 48_000, 2) {
            Ok(_) => panic!("AAC must stay unsupported in this refactor"),
            Err(error) => error,
        };
        let typed_error = error
            .downcast_ref::<AudioDecoderError>()
            .expect("factory error should keep typed error");

        assert_eq!(
            typed_error,
            &AudioDecoderError::UnsupportedCodec {
                codec: AudioCodec::Aac
            }
        );
        assert_eq!(
            error.to_string(),
            "Audio codec AAC is not supported by the audio decoder factory"
        );
    }

    #[test]
    fn vorbis_codec_returns_same_typed_unsupported_error() {
        let error = match create_audio_decoder(AudioCodec::Vorbis, 48_000, 2) {
            Ok(_) => panic!("Vorbis must stay unsupported in this refactor"),
            Err(error) => error,
        };
        let typed_error = error
            .downcast_ref::<AudioDecoderError>()
            .expect("factory error should keep typed error");

        assert_eq!(
            typed_error,
            &AudioDecoderError::UnsupportedCodec {
                codec: AudioCodec::Vorbis
            }
        );
        assert_eq!(
            error.to_string(),
            "Audio codec Vorbis is not supported by the audio decoder factory"
        );
    }

    #[test]
    fn trait_object_path_preserves_decoder_properties() {
        let mut decoder: AudioDecoderHandle = Box::new(FakeAudioDecoder {
            sample_rate: 44_100,
            channels: 2,
        });

        assert_eq!(decoder.sample_rate(), 44_100);
        assert_eq!(decoder.channels(), 2);
        assert_eq!(decoder.decode(&[1, 2, 3]).expect("fake decode"), vec![3.0]);
        decoder.reset().expect("fake reset");
    }
}
