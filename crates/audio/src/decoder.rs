//! Audio decoder boundary и Symphonia-backed decoder factory.
//!
//! Архитектура:
//! - `AudioDecoderConfig` описывает выбранный audio track без Symphonia types.
//! - `EncodedAudioPacket` переносит packet metadata, нужную Symphonia decoder API.
//! - `AudioDecoder` остаётся codec-neutral boundary для player-core.
//! - `SymphoniaAudioDecoder` владеет Symphonia decoder registry object-ом.
//! - `OpusFallbackDecoder` остаётся приватным adapter-ом, потому что Symphonia 0.6
//!   распознаёт Opus codec id, но не предоставляет Opus audio decoder backend.
//! - Все decoder-ы возвращают interleaved `Vec<f32>`, как ожидает CPAL output path.

use anyhow::{Context, Result};
use symphonia::core::codecs::audio::{AudioDecoder as SymphoniaDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use tracing::{info, warn};

pub use audio_core::{
    AudioChannelLayout, AudioChannelPosition, AudioDecodeCapabilityProvider,
    AudioDecodeCapabilitySnapshot, AudioDecoder, AudioDecoderConfig, AudioDecoderError,
    AudioDecoderFactory, AudioDecoderHandle, AudioPacketTimeBase, AudioPacketTiming,
    EncodedAudioPacket,
};

/// Максимальное количество samples на packet для Opus fallback (120ms @ 48kHz stereo).
const MAX_OPUS_SAMPLES_PER_PACKET: usize = 48000 * 2 * 120 / 1000;

mod capability;
mod conversion;

use conversion::{
    audio_channel_layout_from_symphonia, audio_codec_parameters,
    audio_factory_error_from_symphonia, decoded_audio_to_interleaved_f32,
    required_audio_config_value, should_use_opus_fallback, symphonia_codec_id_from_container,
    symphonia_packet_ref_from_encoded_packet, validate_audio_decoder_config,
};
#[cfg(test)]
use symphonia::core::codecs::audio::well_known as symphonia_audio_codec;

use capability::production_audio_decode_capability_snapshot;

/// Production decoder factory, которая скрывает Symphonia registry и Opus fallback.
#[derive(Debug)]
pub struct ProductionAudioDecoderFactory {
    /// Immutable snapshot того же runtime decode path, которым владеет factory.
    decode_capabilities: AudioDecodeCapabilitySnapshot,
}

impl Default for ProductionAudioDecoderFactory {
    /// Снимает read-only capability snapshot без создания decoder-а.
    fn default() -> Self {
        Self {
            decode_capabilities: production_audio_decode_capability_snapshot(),
        }
    }
}

impl AudioDecodeCapabilityProvider for ProductionAudioDecoderFactory {
    /// Возвращает precomputed immutable snapshot без registry scan на каждом query.
    fn audio_decode_capability_snapshot(&self) -> AudioDecodeCapabilitySnapshot {
        self.decode_capabilities
    }
}

impl AudioDecoderFactory for ProductionAudioDecoderFactory {
    /// Создаёт concrete decoder через production helper без раскрытия backend-а caller-у.
    fn create_decoder(&self, config: AudioDecoderConfig) -> Result<AudioDecoderHandle> {
        create_audio_decoder(config)
    }
}

/// Создаёт codec-neutral decoder object для заданного audio track config.
pub fn create_audio_decoder(config: AudioDecoderConfig) -> Result<AudioDecoderHandle> {
    match SymphoniaAudioDecoder::new(&config) {
        Ok(decoder) => Ok(Box::new(decoder) as AudioDecoderHandle),
        Err(error) if should_use_opus_fallback(&config, &error) => {
            info!(
                codec_id = %config.codec_id(),
                "Symphonia не предоставила Opus decoder; используется Opus fallback adapter"
            );
            Ok(Box::new(OpusFallbackDecoder::new(&config)?) as AudioDecoderHandle)
        }
        Err(error) => Err(error),
    }
}

/// Symphonia-backed decoder implementation.
pub struct SymphoniaAudioDecoder {
    /// Track ID, для которого создан decoder.
    track_id: u32,

    /// Container codec id для diagnostics.
    codec_id: String,

    /// Symphonia decoder trait object из registry.
    decoder: Box<dyn SymphoniaDecoder>,

    /// Последний известный sample rate decoded PCM.
    sample_rate: u32,

    /// Последнее известное количество decoded PCM каналов.
    channels: u32,

    /// Neutral layout последнего decoded PCM buffer-а.
    channel_layout: Option<AudioChannelLayout>,
}

impl SymphoniaAudioDecoder {
    /// Создаёт Symphonia decoder через default registry.
    fn new(config: &AudioDecoderConfig) -> Result<Self> {
        validate_audio_decoder_config(config)?;

        let codec = symphonia_codec_id_from_container(config.codec_id()).ok_or_else(|| {
            AudioDecoderError::UnsupportedCodec {
                codec_id: config.codec_id().to_string(),
            }
        })?;
        let codec_params = audio_codec_parameters(config, codec)?;

        // Оставляем Symphonia default `gapless = true`, потому что это корректная
        // playback semantics для codec-ов с encoder delay/padding. Сейчас media-core
        // переносит duration, но не переносит trim_start/trim_end; когда demux boundary
        // будет расширен trim metadata, этот выбор начнёт применять gapless trimming
        // без изменения player-core или CPAL output contract-а.
        let decoder_options = AudioDecoderOptions::default().gapless(true).verify(false);

        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&codec_params, &decoder_options)
            .map_err(|error| audio_factory_error_from_symphonia(config.codec_id(), error))?;

        info!(
            track_id = config.track_id(),
            codec_id = %config.codec_id(),
            sample_rate = ?config.sample_rate(),
            channels = ?config.channels(),
            "Symphonia audio decoder создан"
        );

        let channel_layout = config
            .channels()
            .map(AudioChannelLayout::from_channel_count)
            .transpose()
            .map_err(|error| AudioDecoderError::InvalidConfig {
                codec_id: config.codec_id().to_string(),
                reason: error.to_string(),
            })?;

        Ok(Self {
            track_id: config.track_id(),
            codec_id: config.codec_id().to_string(),
            decoder,
            sample_rate: config.sample_rate().unwrap_or(0),
            channels: config.channels().unwrap_or(0),
            channel_layout,
        })
    }

    /// Проверяет, что packet относится к track-у decoder-а.
    fn ensure_packet_track_matches(&self, packet: &EncodedAudioPacket<'_>) -> Result<()> {
        if packet.track_id() == self.track_id {
            return Ok(());
        }

        anyhow::bail!(
            "Audio packet track mismatch: decoder track {}, packet track {}",
            self.track_id,
            packet.track_id()
        );
    }
}

impl AudioDecoder for SymphoniaAudioDecoder {
    /// Декодирует packet через Symphonia и копирует результат в interleaved `Vec<f32>`.
    fn decode(&mut self, packet: &EncodedAudioPacket<'_>) -> Result<Vec<f32>> {
        self.ensure_packet_track_matches(packet)?;

        let symphonia_packet = symphonia_packet_ref_from_encoded_packet(packet);

        match self.decoder.decode_ref(&symphonia_packet) {
            Ok(decoded_audio) => {
                let decoded_sample_rate = decoded_audio.spec().rate();
                let decoded_channels = decoded_audio.spec().channels().count() as u32;
                let decoded_channel_layout = audio_channel_layout_from_symphonia(
                    decoded_audio.spec().channels(),
                    &self.codec_id,
                )?;
                let interleaved_samples = decoded_audio_to_interleaved_f32(decoded_audio);

                // Публикуем spec только после успешного layout mapping и copy:
                // ошибка packet-а не должна оставить decoder с частично новым format.
                self.sample_rate = decoded_sample_rate;
                self.channels = decoded_channels;
                self.channel_layout = Some(decoded_channel_layout);
                Ok(interleaved_samples)
            }
            Err(SymphoniaError::DecodeError(message)) => {
                warn!(
                    codec_id = %self.codec_id,
                    error = message,
                    "Corrupted audio packet skipped by Symphonia decoder"
                );
                Ok(Vec::new())
            }
            Err(SymphoniaError::IoError(error)) => {
                warn!(
                    codec_id = %self.codec_id,
                    error = %error,
                    "Audio packet IO/decode error skipped by Symphonia decoder"
                );
                Ok(Vec::new())
            }
            Err(SymphoniaError::ResetRequired) => {
                self.decoder.reset();
                Ok(Vec::new())
            }
            Err(error) => Err(anyhow::anyhow!(
                "Symphonia audio decode error for {}: {}",
                self.codec_id,
                error
            )),
        }
    }

    /// Сбрасывает Symphonia decoder state после seek.
    fn reset(&mut self) -> Result<()> {
        self.decoder.reset();
        Ok(())
    }

    /// Возвращает последний известный sample rate decoded PCM.
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Возвращает последнее известное количество decoded PCM каналов.
    fn channels(&self) -> u32 {
        self.channels
    }

    /// Возвращает layout последнего decoded PCM без Symphonia types в boundary.
    fn channel_layout(&self) -> Option<AudioChannelLayout> {
        self.channel_layout
    }
}

/// Приватный Opus fallback adapter поверх `opus` crate.
struct OpusFallbackDecoder {
    /// Track ID, для которого создан fallback decoder.
    track_id: u32,

    /// Opus decoder из `opus` crate.
    decoder: opus::Decoder,

    /// Sample rate decoded audio.
    sample_rate: u32,

    /// Количество каналов decoded audio.
    channels: u32,

    /// Neutral mono/stereo layout fallback decoder-а.
    channel_layout: AudioChannelLayout,

    /// Reusable buffer для i16 samples из opus decoder.
    i16_buffer: Vec<i16>,
}

impl OpusFallbackDecoder {
    /// Создаёт Opus fallback decoder для mono/stereo Opus tracks.
    fn new(config: &AudioDecoderConfig) -> Result<Self> {
        let sample_rate = required_audio_config_value(config.sample_rate(), "sample_rate", config)?;
        let channels = required_audio_config_value(config.channels(), "channels", config)?;

        let opus_channels = match channels {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            channel_count => {
                anyhow::bail!(
                    "Opus поддерживает только mono/stereo, получено: {}",
                    channel_count
                );
            }
        };

        let decoder = opus::Decoder::new(sample_rate, opus_channels).context(
            "Не удалось создать Opus fallback decoder. Убедитесь что libopus установлен",
        )?;

        Ok(Self {
            track_id: config.track_id(),
            decoder,
            sample_rate,
            channels,
            channel_layout: AudioChannelLayout::from_channel_count(channels).map_err(|error| {
                AudioDecoderError::InvalidConfig {
                    codec_id: config.codec_id().to_string(),
                    reason: error.to_string(),
                }
            })?,
            i16_buffer: vec![0i16; MAX_OPUS_SAMPLES_PER_PACKET],
        })
    }

    /// Проверяет, что packet относится к track-у fallback decoder-а.
    fn ensure_packet_track_matches(&self, packet: &EncodedAudioPacket<'_>) -> Result<()> {
        if packet.track_id() == self.track_id {
            return Ok(());
        }

        anyhow::bail!(
            "Audio packet track mismatch: decoder track {}, packet track {}",
            self.track_id,
            packet.track_id()
        );
    }
}

impl AudioDecoder for OpusFallbackDecoder {
    /// Декодирует Opus packet в interleaved PCM f32.
    fn decode(&mut self, packet: &EncodedAudioPacket<'_>) -> Result<Vec<f32>> {
        self.ensure_packet_track_matches(packet)?;

        match self
            .decoder
            .decode(packet.data(), &mut self.i16_buffer, false)
        {
            Ok(sample_count) => {
                let total_samples = sample_count * self.channels as usize;
                let f32_samples = self.i16_buffer[..total_samples]
                    .iter()
                    .map(|&sample| sample as f32 / 32768.0)
                    .collect();

                Ok(f32_samples)
            }
            Err(error) if error.code() == opus::ErrorCode::InvalidPacket => {
                warn!("Corrupted Opus packet skipped by fallback decoder");
                Ok(Vec::new())
            }
            Err(error) => Err(anyhow::anyhow!("Opus fallback decode error: {error}")),
        }
    }

    /// Сбрасывает Opus state после seek.
    fn reset(&mut self) -> Result<()> {
        self.decoder
            .reset_state()
            .map_err(|error| anyhow::anyhow!("Opus fallback reset error: {error}"))
    }

    /// Возвращает sample rate fallback decoder-а.
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Возвращает channel count fallback decoder-а.
    fn channels(&self) -> u32 {
        self.channels
    }

    /// Возвращает однозначный mono/stereo layout fallback decoder-а.
    fn channel_layout(&self) -> Option<AudioChannelLayout> {
        Some(self.channel_layout)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use symphonia::core::audio::{
        AudioBuffer, AudioSpec, Channels, GenericAudioBufferRef, layouts,
    };

    use super::symphonia_audio_codec;
    use super::{
        AudioChannelLayout, AudioChannelPosition, AudioDecoder, AudioDecoderConfig,
        AudioDecoderError, AudioDecoderHandle, AudioPacketTimeBase, AudioPacketTiming,
        EncodedAudioPacket, audio_channel_layout_from_symphonia, audio_codec_parameters,
        create_audio_decoder, decoded_audio_to_interleaved_f32, symphonia_codec_id_from_container,
        symphonia_packet_ref_from_encoded_packet, validate_audio_decoder_config,
    };

    /// Fake decoder нужен только для проверки object-safe contract без codec backend-а.
    struct FakeAudioDecoder {
        /// Sample rate, который должен быть виден через trait object.
        sample_rate: u32,

        /// Количество каналов, которое должно быть видно через trait object.
        channels: u32,

        /// Shared counter reset-вызовов для проверки seek path-а.
        reset_count: Arc<AtomicUsize>,
    }

    impl AudioDecoder for FakeAudioDecoder {
        /// Fake decode возвращает входной размер как sample, чтобы путь был наблюдаемым.
        fn decode(&mut self, packet: &EncodedAudioPacket<'_>) -> Result<Vec<f32>> {
            Ok(vec![packet.data().len() as f32])
        }

        /// Fake reset только увеличивает счётчик.
        fn reset(&mut self) -> Result<()> {
            self.reset_count.fetch_add(1, Ordering::Relaxed);
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

        /// Возвращает однозначный fake layout из channel count-а.
        fn channel_layout(&self) -> Option<AudioChannelLayout> {
            AudioChannelLayout::from_channel_count(self.channels).ok()
        }
    }

    /// Создаёт минимальный Opus config для factory tests.
    fn opus_config(channels: u32) -> AudioDecoderConfig {
        AudioDecoderConfig::new(2, "A_OPUS", 48_000, channels)
    }

    /// Создаёт packet view для trait-object tests.
    fn packet(bytes: &'static [u8]) -> EncodedAudioPacket<'static> {
        EncodedAudioPacket::without_timing(2, bytes)
    }

    #[test]
    fn factory_creates_opus_fallback_after_symphonia_registry_rejects_opus() {
        let mono_decoder =
            create_audio_decoder(opus_config(1)).expect("mono Opus decoder should be created");
        let stereo_decoder =
            create_audio_decoder(opus_config(2)).expect("stereo Opus decoder should be created");

        assert_eq!(mono_decoder.sample_rate(), 48_000);
        assert_eq!(mono_decoder.channels(), 1);
        assert_eq!(
            mono_decoder.channel_layout(),
            Some(AudioChannelLayout::mono())
        );
        assert_eq!(stereo_decoder.sample_rate(), 48_000);
        assert_eq!(stereo_decoder.channels(), 2);
        assert_eq!(
            stereo_decoder.channel_layout(),
            Some(AudioChannelLayout::stereo())
        );
    }

    #[test]
    fn symphonia_positioned_5_1_maps_to_exact_neutral_canonical_layout() {
        let layout =
            audio_channel_layout_from_symphonia(&layouts::CHANNEL_LAYOUT_5P1, "A_AAC").unwrap();

        assert_eq!(layout.channel_count(), 6);
        assert_eq!(layout.position_at(0), Some(AudioChannelPosition::FrontLeft));
        assert_eq!(
            layout.position_at(1),
            Some(AudioChannelPosition::FrontRight)
        );
        assert_eq!(
            layout.position_at(2),
            Some(AudioChannelPosition::FrontCenter)
        );
        assert_eq!(
            layout.position_at(3),
            Some(AudioChannelPosition::LowFrequencyEffects)
        );
        assert_eq!(layout.position_at(4), Some(AudioChannelPosition::RearLeft));
        assert_eq!(layout.position_at(5), Some(AudioChannelPosition::RearRight));
    }

    #[test]
    fn symphonia_discrete_multichannel_never_becomes_guessed_5_1() {
        let layout =
            audio_channel_layout_from_symphonia(&Channels::Discrete(6), "A_UNKNOWN").unwrap();

        assert_eq!(layout, AudioChannelLayout::discrete(6).unwrap());
        assert!(!layout.is_positioned());
    }

    #[test]
    fn factory_keeps_opus_channel_validation_message() {
        let error = match create_audio_decoder(opus_config(6)) {
            Ok(_) => panic!("invalid Opus channel count should be rejected by fallback"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "Opus поддерживает только mono/stereo, получено: 6"
        );
    }

    #[test]
    fn symphonia_codec_params_keep_missing_probe_audio_spec_deferred() {
        let config = AudioDecoderConfig::from_track_metadata(2, "A_AAC", None, None);
        let codec_params = audio_codec_parameters(&config, symphonia_audio_codec::CODEC_ID_AAC)
            .expect("missing probe spec should still build Symphonia codec params");

        assert_eq!(codec_params.sample_rate, None);
        assert_eq!(codec_params.channels, None);
    }

    #[test]
    fn decoder_config_rejects_zero_values_but_allows_absent_probe_values() {
        let deferred_config = AudioDecoderConfig::from_track_metadata(2, "A_AAC", None, None);
        validate_audio_decoder_config(&deferred_config)
            .expect("absent probe values should be accepted for lazy decode");

        let invalid_config = AudioDecoderConfig::from_track_metadata(2, "A_AAC", Some(0), Some(2));
        let error = validate_audio_decoder_config(&invalid_config)
            .expect_err("zero sample rate should not be accepted as real metadata");

        assert_eq!(
            error
                .downcast_ref::<AudioDecoderError>()
                .expect("config validation should keep typed error"),
            &AudioDecoderError::InvalidConfig {
                codec_id: "A_AAC".to_string(),
                reason: "sample_rate must be greater than 0 when present".to_string(),
            }
        );
    }

    #[test]
    fn opus_fallback_reports_missing_deferred_spec_as_config_error() {
        let error = match create_audio_decoder(AudioDecoderConfig::from_track_metadata(
            2,
            "A_OPUS",
            Some(48_000),
            None,
        )) {
            Ok(_) => panic!("Opus fallback cannot start without channel count"),
            Err(error) => error,
        };

        assert_eq!(
            error
                .downcast_ref::<AudioDecoderError>()
                .expect("fallback config error should stay typed"),
            &AudioDecoderError::InvalidConfig {
                codec_id: "A_OPUS".to_string(),
                reason: "channels is required by this audio decoder backend".to_string(),
            }
        );
    }

    #[test]
    fn unsupported_codec_returns_stable_typed_error() {
        let error =
            match create_audio_decoder(AudioDecoderConfig::new(2, "A_NOT_A_REAL_CODEC", 48_000, 2))
            {
                Ok(_) => panic!("unknown codec id must be rejected"),
                Err(error) => error,
            };
        let typed_error = error
            .downcast_ref::<AudioDecoderError>()
            .expect("factory error should keep typed error");

        assert_eq!(
            typed_error,
            &AudioDecoderError::UnsupportedCodec {
                codec_id: "A_NOT_A_REAL_CODEC".to_string()
            }
        );
    }

    #[test]
    fn mapped_but_unregistered_codec_returns_typed_unsupported_error() {
        let error = match create_audio_decoder(AudioDecoderConfig::new(2, "A_SPEEX", 48_000, 2)) {
            Ok(_) => panic!("Speex is mapped but has no Symphonia decoder in all-codecs"),
            Err(error) => error,
        };
        let typed_error = error
            .downcast_ref::<AudioDecoderError>()
            .expect("factory error should keep typed error");

        assert_eq!(
            typed_error,
            &AudioDecoderError::UnsupportedCodec {
                codec_id: "A_SPEEX".to_string()
            }
        );
    }

    #[test]
    fn codec_id_mapping_covers_symphonia_supported_common_codecs() {
        assert_eq!(
            symphonia_codec_id_from_container("A_AAC"),
            Some(symphonia_audio_codec::CODEC_ID_AAC)
        );
        assert_eq!(
            symphonia_codec_id_from_container("A_VORBIS"),
            Some(symphonia_audio_codec::CODEC_ID_VORBIS)
        );
        assert_eq!(
            symphonia_codec_id_from_container("A_FLAC"),
            Some(symphonia_audio_codec::CODEC_ID_FLAC)
        );
        assert_eq!(
            symphonia_codec_id_from_container("A_MP3"),
            Some(symphonia_audio_codec::CODEC_ID_MP3)
        );
    }

    #[test]
    fn trait_object_path_preserves_decoder_properties_and_reset() {
        let reset_count = Arc::new(AtomicUsize::new(0));
        let mut decoder: AudioDecoderHandle = Box::new(FakeAudioDecoder {
            sample_rate: 44_100,
            channels: 2,
            reset_count: Arc::clone(&reset_count),
        });

        assert_eq!(decoder.sample_rate(), 44_100);
        assert_eq!(decoder.channels(), 2);
        assert_eq!(
            decoder.decode(&packet(b"abc")).expect("fake decode"),
            vec![3.0]
        );
        decoder.reset().expect("fake reset");
        assert_eq!(reset_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn conversion_helper_copies_planar_f32_as_interleaved_samples() {
        let spec = AudioSpec::new(
            48_000,
            symphonia::core::audio::Channels::from(
                symphonia::core::audio::Position::FRONT_LEFT
                    | symphonia::core::audio::Position::FRONT_RIGHT,
            ),
        );
        let mut audio_buffer = AudioBuffer::<f32>::new(spec, 2);

        audio_buffer
            .render_with(Some(2), |frame_index, planes| {
                planes[0][frame_index] = if frame_index == 0 { 0.25 } else { 0.5 };
                planes[1][frame_index] = if frame_index == 0 { -0.25 } else { -0.5 };
                Ok(())
            })
            .expect("test audio render should succeed");

        let samples = decoded_audio_to_interleaved_f32(GenericAudioBufferRef::F32(&audio_buffer));

        assert_eq!(samples, vec![0.25, -0.25, 0.5, -0.5]);
    }

    #[test]
    fn packet_ref_uses_container_timing_units_without_sample_rate_rebuild() {
        let time_base =
            AudioPacketTimeBase::new(1, 1_000).expect("container time base should be valid");
        let timing = AudioPacketTiming::from_track_units(time_base, 1_234, Some(1_200), Some(23));
        let packet = EncodedAudioPacket::new(2, timing, b"aac");

        let symphonia_packet = symphonia_packet_ref_from_encoded_packet(&packet);

        assert_eq!(symphonia_packet.pts.get(), 1_234);
        assert_eq!(symphonia_packet.dts.get(), 1_200);
        assert_eq!(symphonia_packet.dur.get(), 23);
        assert_eq!(
            packet
                .timing()
                .time_base()
                .expect("raw timing should keep time base")
                .denom(),
            1_000
        );
    }
}
