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
use symphonia::core::audio::{Channels, GenericAudioBufferRef, Position};
use symphonia::core::codecs::audio::{
    AudioCodecId, AudioCodecParameters, AudioDecoder as SymphoniaDecoder, AudioDecoderOptions,
    well_known as symphonia_audio_codec,
};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::packet::{PacketBuilder, PacketRef as SymphoniaPacketRef};
use symphonia::core::units::{Duration as SymphoniaDuration, Timestamp as SymphoniaTimestamp};
use tracing::{info, warn};

pub use audio_core::{
    AudioDecoder, AudioDecoderConfig, AudioDecoderError, AudioDecoderFactory, AudioDecoderHandle,
    AudioPacketTimeBase, AudioPacketTiming, EncodedAudioPacket,
};

/// Максимальное количество samples на packet для Opus fallback (120ms @ 48kHz stereo).
const MAX_OPUS_SAMPLES_PER_PACKET: usize = 48000 * 2 * 120 / 1000;

/// Production decoder factory, которая скрывает Symphonia registry и Opus fallback.
#[derive(Debug, Default)]
pub struct ProductionAudioDecoderFactory;

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

        Ok(Self {
            track_id: config.track_id(),
            codec_id: config.codec_id().to_string(),
            decoder,
            sample_rate: config.sample_rate().unwrap_or(0),
            channels: config.channels().unwrap_or(0),
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
                self.sample_rate = decoded_audio.spec().rate();
                self.channels = decoded_audio.spec().channels().count() as u32;
                Ok(decoded_audio_to_interleaved_f32(decoded_audio))
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
}

/// Проверяет базовые инварианты decoder config-а до обращения к backend registry.
fn validate_audio_decoder_config(config: &AudioDecoderConfig) -> Result<()> {
    config.validate_probe_metadata()
}

/// Возвращает обязательное значение для backend-ов, которые не умеют deferred audio spec.
fn required_audio_config_value(
    value: Option<u32>,
    value_name: &'static str,
    config: &AudioDecoderConfig,
) -> Result<u32> {
    value.ok_or_else(|| {
        AudioDecoderError::InvalidConfig {
            codec_id: config.codec_id().to_string(),
            reason: format!("{value_name} is required by this audio decoder backend"),
        }
        .into()
    })
}

/// Собирает Symphonia audio codec params из нейтрального config-а.
fn audio_codec_parameters(
    config: &AudioDecoderConfig,
    codec: AudioCodecId,
) -> Result<AudioCodecParameters> {
    let mut params = AudioCodecParameters::new();
    params.for_codec(codec);

    if let Some(sample_rate) = config.sample_rate() {
        params.with_sample_rate(sample_rate);
    }

    if let Some(channels) = config.channels() {
        params.with_channels(channels_from_count(channels, config.codec_id())?);
    }

    if let Some(codec_private) = config.codec_private() {
        params.with_extra_data(codec_private.to_vec().into_boxed_slice());
    }

    Ok(params)
}

/// Конвертирует channel count в Symphonia channel description без codec-specific policy в caller-е.
fn channels_from_count(channel_count: u32, codec_id: &str) -> Result<Channels> {
    match channel_count {
        1 => Ok(Channels::from(Position::FRONT_CENTER)),
        2 => Ok(Channels::from(Position::FRONT_LEFT | Position::FRONT_RIGHT)),
        count if count <= u32::from(u16::MAX) => Ok(Channels::Discrete(count as u16)),
        count => Err(AudioDecoderError::InvalidConfig {
            codec_id: codec_id.to_string(),
            reason: format!("channel count {count} exceeds Symphonia discrete channel limit"),
        }
        .into()),
    }
}

/// Преобразует neutral packet в zero-copy Symphonia `PacketRef`.
fn symphonia_packet_ref_from_encoded_packet<'a>(
    packet: &EncodedAudioPacket<'a>,
) -> SymphoniaPacketRef<'a> {
    let timing = packet.timing();
    let pts = SymphoniaTimestamp::new(timing.pts_units());
    let duration = timing
        .duration_units()
        .map(SymphoniaDuration::new)
        .unwrap_or(SymphoniaDuration::ZERO);
    let builder = PacketBuilder::new()
        .track_id(packet.track_id())
        .pts(pts)
        .dur(duration)
        .data_by_ref(packet.data());

    match timing.dts_units() {
        Some(dts_units) => builder
            .dts(SymphoniaTimestamp::new(dts_units))
            .build_packet_ref(),
        None => builder.build_packet_ref(),
    }
}

/// Копирует decoded audio в interleaved f32 без старого `SampleBuffer` API.
fn decoded_audio_to_interleaved_f32(decoded_audio: GenericAudioBufferRef<'_>) -> Vec<f32> {
    let mut interleaved_samples = Vec::with_capacity(decoded_audio.samples_interleaved());
    decoded_audio.copy_to_vec_interleaved(&mut interleaved_samples);
    interleaved_samples
}

/// Переводит Symphonia factory unsupported в typed factory error.
fn audio_factory_error_from_symphonia(codec_id: &str, error: SymphoniaError) -> anyhow::Error {
    if matches!(error, SymphoniaError::Unsupported(_)) {
        return AudioDecoderError::UnsupportedCodec {
            codec_id: codec_id.to_string(),
        }
        .into();
    }

    anyhow::anyhow!("Symphonia audio decoder factory error for {codec_id}: {error}")
}

/// Проверяет, нужно ли включить legacy Opus adapter после Symphonia registry.
fn should_use_opus_fallback(config: &AudioDecoderConfig, error: &anyhow::Error) -> bool {
    normalized_codec_id(config.codec_id()) == "A_OPUS"
        && matches!(
            error.downcast_ref::<AudioDecoderError>(),
            Some(AudioDecoderError::UnsupportedCodec { .. })
        )
}

/// Нормализует codec id без изменения ownership исходной строки.
fn normalized_codec_id(codec_id: &str) -> String {
    codec_id.trim().to_ascii_uppercase()
}

/// Мапит container codec id в Symphonia 0.6 audio codec id.
fn symphonia_codec_id_from_container(codec_id: &str) -> Option<AudioCodecId> {
    let normalized = normalized_codec_id(codec_id);
    let codec = match normalized.as_str() {
        "A_PCM_S32LE" => symphonia_audio_codec::CODEC_ID_PCM_S32LE,
        "A_PCM_S32LE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_S32LE_PLANAR,
        "A_PCM_S32BE" => symphonia_audio_codec::CODEC_ID_PCM_S32BE,
        "A_PCM_S32BE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_S32BE_PLANAR,
        "A_PCM_S24LE" => symphonia_audio_codec::CODEC_ID_PCM_S24LE,
        "A_PCM_S24LE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_S24LE_PLANAR,
        "A_PCM_S24BE" => symphonia_audio_codec::CODEC_ID_PCM_S24BE,
        "A_PCM_S24BE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_S24BE_PLANAR,
        "A_PCM_S16LE" => symphonia_audio_codec::CODEC_ID_PCM_S16LE,
        "A_PCM_S16LE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_S16LE_PLANAR,
        "A_PCM_S16BE" => symphonia_audio_codec::CODEC_ID_PCM_S16BE,
        "A_PCM_S16BE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_S16BE_PLANAR,
        "A_PCM_S8" => symphonia_audio_codec::CODEC_ID_PCM_S8,
        "A_PCM_S8_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_S8_PLANAR,
        "A_PCM_U32LE" => symphonia_audio_codec::CODEC_ID_PCM_U32LE,
        "A_PCM_U32LE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_U32LE_PLANAR,
        "A_PCM_U32BE" => symphonia_audio_codec::CODEC_ID_PCM_U32BE,
        "A_PCM_U32BE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_U32BE_PLANAR,
        "A_PCM_U24LE" => symphonia_audio_codec::CODEC_ID_PCM_U24LE,
        "A_PCM_U24LE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_U24LE_PLANAR,
        "A_PCM_U24BE" => symphonia_audio_codec::CODEC_ID_PCM_U24BE,
        "A_PCM_U24BE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_U24BE_PLANAR,
        "A_PCM_U16LE" => symphonia_audio_codec::CODEC_ID_PCM_U16LE,
        "A_PCM_U16LE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_U16LE_PLANAR,
        "A_PCM_U16BE" => symphonia_audio_codec::CODEC_ID_PCM_U16BE,
        "A_PCM_U16BE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_U16BE_PLANAR,
        "A_PCM_U8" => symphonia_audio_codec::CODEC_ID_PCM_U8,
        "A_PCM_U8_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_U8_PLANAR,
        "A_PCM_F32LE" => symphonia_audio_codec::CODEC_ID_PCM_F32LE,
        "A_PCM_F32LE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_F32LE_PLANAR,
        "A_PCM_F32BE" => symphonia_audio_codec::CODEC_ID_PCM_F32BE,
        "A_PCM_F32BE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_F32BE_PLANAR,
        "A_PCM_F64LE" => symphonia_audio_codec::CODEC_ID_PCM_F64LE,
        "A_PCM_F64LE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_F64LE_PLANAR,
        "A_PCM_F64BE" => symphonia_audio_codec::CODEC_ID_PCM_F64BE,
        "A_PCM_F64BE_PLANAR" => symphonia_audio_codec::CODEC_ID_PCM_F64BE_PLANAR,
        "A_PCM_ALAW" => symphonia_audio_codec::CODEC_ID_PCM_ALAW,
        "A_PCM_MULAW" => symphonia_audio_codec::CODEC_ID_PCM_MULAW,
        "A_ADPCM_G722" => symphonia_audio_codec::CODEC_ID_ADPCM_G722,
        "A_ADPCM_G726" => symphonia_audio_codec::CODEC_ID_ADPCM_G726,
        "A_ADPCM_G726LE" => symphonia_audio_codec::CODEC_ID_ADPCM_G726LE,
        "A_ADPCM_MS" => symphonia_audio_codec::CODEC_ID_ADPCM_MS,
        "A_ADPCM_IMA_WAV" => symphonia_audio_codec::CODEC_ID_ADPCM_IMA_WAV,
        "A_ADPCM_IMA_QT" => symphonia_audio_codec::CODEC_ID_ADPCM_IMA_QT,
        "A_OPUS" | "OPUS" => symphonia_audio_codec::CODEC_ID_OPUS,
        "A_VORBIS" | "VORBIS" => symphonia_audio_codec::CODEC_ID_VORBIS,
        "A_SPEEX" => symphonia_audio_codec::CODEC_ID_SPEEX,
        "A_MUSEPACK" => symphonia_audio_codec::CODEC_ID_MUSEPACK,
        "A_MP1" => symphonia_audio_codec::CODEC_ID_MP1,
        "A_MP2" => symphonia_audio_codec::CODEC_ID_MP2,
        "A_MP3" => symphonia_audio_codec::CODEC_ID_MP3,
        "A_AAC" | "A_AAC/MPEG2/LC" | "A_AAC/MPEG4/LC" | "AAC" => {
            symphonia_audio_codec::CODEC_ID_AAC
        }
        "A_AC3" => symphonia_audio_codec::CODEC_ID_AC3,
        "A_EAC3" => symphonia_audio_codec::CODEC_ID_EAC3,
        "A_AC4" => symphonia_audio_codec::CODEC_ID_AC4,
        "A_DCA" => symphonia_audio_codec::CODEC_ID_DCA,
        "A_ATRAC1" => symphonia_audio_codec::CODEC_ID_ATRAC1,
        "A_ATRAC3" => symphonia_audio_codec::CODEC_ID_ATRAC3,
        "A_ATRAC3PLUS" => symphonia_audio_codec::CODEC_ID_ATRAC3PLUS,
        "A_ATRAC9" => symphonia_audio_codec::CODEC_ID_ATRAC9,
        "A_WMA" => symphonia_audio_codec::CODEC_ID_WMA,
        "A_RA10" => symphonia_audio_codec::CODEC_ID_RA10,
        "A_RA20" => symphonia_audio_codec::CODEC_ID_RA20,
        "A_SIPR" => symphonia_audio_codec::CODEC_ID_SIPR,
        "A_COOK" => symphonia_audio_codec::CODEC_ID_COOK,
        "A_SBC" => symphonia_audio_codec::CODEC_ID_SBC,
        "A_APTX" => symphonia_audio_codec::CODEC_ID_APTX,
        "A_APTX_HD" => symphonia_audio_codec::CODEC_ID_APTX_HD,
        "A_LDAC" => symphonia_audio_codec::CODEC_ID_LDAC,
        "A_BINK_AUDIO" => symphonia_audio_codec::CODEC_ID_BINK_AUDIO,
        "A_SMACKER_AUDIO" => symphonia_audio_codec::CODEC_ID_SMACKER_AUDIO,
        "A_FLAC" | "FLAC" => symphonia_audio_codec::CODEC_ID_FLAC,
        "A_WAVPACK" => symphonia_audio_codec::CODEC_ID_WAVPACK,
        "A_MONKEYS_AUDIO" => symphonia_audio_codec::CODEC_ID_MONKEYS_AUDIO,
        "A_ALAC" => symphonia_audio_codec::CODEC_ID_ALAC,
        "A_TTA" => symphonia_audio_codec::CODEC_ID_TTA,
        "A_RALF" => symphonia_audio_codec::CODEC_ID_RALF,
        "A_TRUEHD" => symphonia_audio_codec::CODEC_ID_TRUEHD,
        _ => return None,
    };

    Some(codec)
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use symphonia::core::audio::{AudioBuffer, AudioSpec, GenericAudioBufferRef};

    use super::symphonia_audio_codec;
    use super::{
        AudioDecoder, AudioDecoderConfig, AudioDecoderError, AudioDecoderHandle,
        AudioPacketTimeBase, AudioPacketTiming, EncodedAudioPacket, audio_codec_parameters,
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
        assert_eq!(stereo_decoder.sample_rate(), 48_000);
        assert_eq!(stereo_decoder.channels(), 2);
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
