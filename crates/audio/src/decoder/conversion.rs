//! Conversion boundary между neutral audio-core и Symphonia.
//!
//! Модуль владеет codec/channel metadata mapping, zero-copy packet adaptation
//! и копированием decoded PCM в interleaved f32. Decoder lifecycle здесь отсутствует.

use anyhow::Result;
use audio_core::{
    AudioChannelLayout, AudioChannelPosition, AudioDecoderConfig, AudioDecoderError,
    EncodedAudioPacket,
};
use symphonia::core::audio::{Channels, GenericAudioBufferRef, Position};
use symphonia::core::codecs::audio::{
    AudioCodecId, AudioCodecParameters, well_known as symphonia_audio_codec,
};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::packet::{PacketBuilder, PacketRef as SymphoniaPacketRef};
use symphonia::core::units::{Duration as SymphoniaDuration, Timestamp as SymphoniaTimestamp};

/// Проверяет базовые инварианты decoder config-а до обращения к backend registry.
pub(super) fn validate_audio_decoder_config(config: &AudioDecoderConfig) -> Result<()> {
    config.validate_probe_metadata()
}

/// Возвращает обязательное значение для backend-ов, которые не умеют deferred audio spec.
pub(super) fn required_audio_config_value(
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
pub(super) fn audio_codec_parameters(
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
pub(super) fn channels_from_count(channel_count: u32, codec_id: &str) -> Result<Channels> {
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

/// Переводит Symphonia channel representation в neutral decoded PCM layout.
///
/// `Positioned` buffer-ы Symphonia всегда используют canonical order младших
/// position bits. `Discrete` сохраняется как unknown semantics; Ambisonic и
/// custom order нельзя безопасно свести к текущему neutral positional contract.
pub(super) fn audio_channel_layout_from_symphonia(
    channels: &Channels,
    codec_id: &str,
) -> Result<AudioChannelLayout> {
    let layout_result = match channels {
        Channels::Positioned(positions) => {
            let mut neutral_positions = [AudioChannelPosition::FrontLeft; 26];
            let mut position_count = 0_usize;

            for position in positions.iter() {
                let Some(neutral_position) = audio_channel_position_from_symphonia(position) else {
                    return Err(AudioDecoderError::UnsupportedDecodedChannelLayout {
                        codec_id: codec_id.to_string(),
                        layout: channels.to_string(),
                    }
                    .into());
                };
                neutral_positions[position_count] = neutral_position;
                position_count += 1;
            }

            AudioChannelLayout::positioned(&neutral_positions[..position_count])
        }
        Channels::Discrete(channel_count) => {
            AudioChannelLayout::discrete(u32::from(*channel_count))
        }
        _ => {
            return Err(AudioDecoderError::UnsupportedDecodedChannelLayout {
                codec_id: codec_id.to_string(),
                layout: channels.to_string(),
            }
            .into());
        }
    };

    layout_result.map_err(|_| {
        AudioDecoderError::UnsupportedDecodedChannelLayout {
            codec_id: codec_id.to_string(),
            layout: channels.to_string(),
        }
        .into()
    })
}

/// Переводит одну Symphonia position в нейтральную позицию audio-core.
pub(super) fn audio_channel_position_from_symphonia(
    position: Position,
) -> Option<AudioChannelPosition> {
    let neutral_position = if position == Position::FRONT_LEFT {
        AudioChannelPosition::FrontLeft
    } else if position == Position::FRONT_RIGHT {
        AudioChannelPosition::FrontRight
    } else if position == Position::FRONT_CENTER {
        AudioChannelPosition::FrontCenter
    } else if position == Position::LFE1 {
        AudioChannelPosition::LowFrequencyEffects
    } else if position == Position::REAR_LEFT {
        AudioChannelPosition::RearLeft
    } else if position == Position::REAR_RIGHT {
        AudioChannelPosition::RearRight
    } else if position == Position::FRONT_LEFT_CENTER {
        AudioChannelPosition::FrontLeftOfCenter
    } else if position == Position::FRONT_RIGHT_CENTER {
        AudioChannelPosition::FrontRightOfCenter
    } else if position == Position::REAR_CENTER {
        AudioChannelPosition::RearCenter
    } else if position == Position::SIDE_LEFT {
        AudioChannelPosition::SideLeft
    } else if position == Position::SIDE_RIGHT {
        AudioChannelPosition::SideRight
    } else if position == Position::TOP_CENTER {
        AudioChannelPosition::TopCenter
    } else if position == Position::TOP_FRONT_LEFT {
        AudioChannelPosition::TopFrontLeft
    } else if position == Position::TOP_FRONT_CENTER {
        AudioChannelPosition::TopFrontCenter
    } else if position == Position::TOP_FRONT_RIGHT {
        AudioChannelPosition::TopFrontRight
    } else if position == Position::TOP_REAR_LEFT {
        AudioChannelPosition::TopRearLeft
    } else if position == Position::TOP_REAR_CENTER {
        AudioChannelPosition::TopRearCenter
    } else if position == Position::TOP_REAR_RIGHT {
        AudioChannelPosition::TopRearRight
    } else if position == Position::LFE2 {
        AudioChannelPosition::LowFrequencyEffects2
    } else if position == Position::TOP_SIDE_LEFT {
        AudioChannelPosition::TopSideLeft
    } else if position == Position::TOP_SIDE_RIGHT {
        AudioChannelPosition::TopSideRight
    } else if position == Position::BOTTOM_FRONT_CENTER {
        AudioChannelPosition::BottomFrontCenter
    } else if position == Position::BOTTOM_FRONT_LEFT {
        AudioChannelPosition::BottomFrontLeft
    } else if position == Position::BOTTOM_FRONT_RIGHT {
        AudioChannelPosition::BottomFrontRight
    } else if position == Position::FRONT_LEFT_WIDE {
        AudioChannelPosition::FrontLeftWide
    } else if position == Position::FRONT_RIGHT_WIDE {
        AudioChannelPosition::FrontRightWide
    } else {
        return None;
    };

    Some(neutral_position)
}

/// Преобразует neutral packet в zero-copy Symphonia `PacketRef`.
pub(super) fn symphonia_packet_ref_from_encoded_packet<'a>(
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
pub(super) fn decoded_audio_to_interleaved_f32(
    decoded_audio: GenericAudioBufferRef<'_>,
) -> Vec<f32> {
    let mut interleaved_samples = Vec::with_capacity(decoded_audio.samples_interleaved());
    decoded_audio.copy_to_vec_interleaved(&mut interleaved_samples);
    interleaved_samples
}

/// Переводит Symphonia factory unsupported в typed factory error.
pub(super) fn audio_factory_error_from_symphonia(
    codec_id: &str,
    error: SymphoniaError,
) -> anyhow::Error {
    if matches!(error, SymphoniaError::Unsupported(_)) {
        return AudioDecoderError::UnsupportedCodec {
            codec_id: codec_id.to_string(),
        }
        .into();
    }

    anyhow::anyhow!("Symphonia audio decoder factory error for {codec_id}: {error}")
}

/// Проверяет, нужно ли включить legacy Opus adapter после Symphonia registry.
pub(super) fn should_use_opus_fallback(config: &AudioDecoderConfig, error: &anyhow::Error) -> bool {
    normalized_codec_id(config.codec_id()) == "A_OPUS"
        && matches!(
            error.downcast_ref::<AudioDecoderError>(),
            Some(AudioDecoderError::UnsupportedCodec { .. })
        )
}

/// Нормализует codec id без изменения ownership исходной строки.
pub(super) fn normalized_codec_id(codec_id: &str) -> String {
    codec_id.trim().to_ascii_uppercase()
}

/// Мапит container codec id в Symphonia 0.6 audio codec id.
pub(super) fn symphonia_codec_id_from_container(codec_id: &str) -> Option<AudioCodecId> {
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
