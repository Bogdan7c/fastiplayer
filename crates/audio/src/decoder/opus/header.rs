//! Structural parser и neutral channel mapping для `OpusHead` codec private.
//!
//! Модуль не создаёт decoder и не владеет runtime state. Его единственная
//! ответственность — проверить container bytes и выпустить готовый constructor
//! contract, в котором Vorbis channel order уже преобразован в audio-core order.

use thiserror::Error;

use super::super::{AudioChannelLayout, AudioChannelPosition};

/// Минимальная длина обязательной части `OpusHead` до optional mapping table.
const OPUS_HEAD_BASE_LENGTH: usize = 19;

/// Смещение первого channel mapping byte для family 1.
const OPUS_HEAD_MAPPING_OFFSET: usize = 21;

/// Позиции mapping family 1 для mono в codec-specific порядке.
const OPUS_FAMILY_ONE_MONO: [AudioChannelPosition; 1] = [AudioChannelPosition::FrontCenter];

/// Позиции mapping family 1 для stereo в codec-specific порядке.
const OPUS_FAMILY_ONE_STEREO: [AudioChannelPosition; 2] = [
    AudioChannelPosition::FrontLeft,
    AudioChannelPosition::FrontRight,
];

/// Позиции mapping family 1 для linear surround в Vorbis порядке.
const OPUS_FAMILY_ONE_3_0: [AudioChannelPosition; 3] = [
    AudioChannelPosition::FrontLeft,
    AudioChannelPosition::FrontCenter,
    AudioChannelPosition::FrontRight,
];

/// Позиции mapping family 1 для quad surround в Vorbis порядке.
const OPUS_FAMILY_ONE_QUAD: [AudioChannelPosition; 4] = [
    AudioChannelPosition::FrontLeft,
    AudioChannelPosition::FrontRight,
    AudioChannelPosition::RearLeft,
    AudioChannelPosition::RearRight,
];

/// Позиции mapping family 1 для 5.0 surround в Vorbis порядке.
const OPUS_FAMILY_ONE_5_0: [AudioChannelPosition; 5] = [
    AudioChannelPosition::FrontLeft,
    AudioChannelPosition::FrontCenter,
    AudioChannelPosition::FrontRight,
    AudioChannelPosition::RearLeft,
    AudioChannelPosition::RearRight,
];

/// Позиции mapping family 1 для 5.1 surround в Vorbis порядке.
const OPUS_FAMILY_ONE_5_1: [AudioChannelPosition; 6] = [
    AudioChannelPosition::FrontLeft,
    AudioChannelPosition::FrontCenter,
    AudioChannelPosition::FrontRight,
    AudioChannelPosition::RearLeft,
    AudioChannelPosition::RearRight,
    AudioChannelPosition::LowFrequencyEffects,
];

/// Позиции mapping family 1 для 6.1 surround в Vorbis порядке.
const OPUS_FAMILY_ONE_6_1: [AudioChannelPosition; 7] = [
    AudioChannelPosition::FrontLeft,
    AudioChannelPosition::FrontCenter,
    AudioChannelPosition::FrontRight,
    AudioChannelPosition::SideLeft,
    AudioChannelPosition::SideRight,
    AudioChannelPosition::RearCenter,
    AudioChannelPosition::LowFrequencyEffects,
];

/// Позиции mapping family 1 для 7.1 surround в Vorbis порядке.
const OPUS_FAMILY_ONE_7_1: [AudioChannelPosition; 8] = [
    AudioChannelPosition::FrontLeft,
    AudioChannelPosition::FrontCenter,
    AudioChannelPosition::FrontRight,
    AudioChannelPosition::SideLeft,
    AudioChannelPosition::SideRight,
    AudioChannelPosition::RearLeft,
    AudioChannelPosition::RearRight,
    AudioChannelPosition::LowFrequencyEffects,
];

/// Ошибки проверки `OpusHead`, которые parent adapter превращает в typed `InvalidConfig`.
#[derive(Debug, Error, PartialEq, Eq)]
pub(super) enum OpusHeadError {
    /// Header короче обязательных 19 bytes.
    #[error("OpusHead слишком короткий: ожидалось минимум {expected} bytes, получено {actual}")]
    TooShort { expected: usize, actual: usize },

    /// Codec private не содержит сигнатуру Opus.
    #[error("codec private не начинается с OpusHead magic")]
    InvalidMagic,

    /// Несовместимая major version не должна интерпретироваться как текущий header.
    #[error("OpusHead version {version} несовместима с поддерживаемой major version 0")]
    UnsupportedVersion { version: u8 },

    /// Opus header никогда не может описывать ноль output channels.
    #[error("OpusHead содержит нулевое количество каналов")]
    ZeroChannels,

    /// Container metadata и codec private не должны расходиться.
    #[error(
        "channel count в track metadata ({metadata_channels}) не совпадает с OpusHead ({header_channels})"
    )]
    ChannelCountMismatch {
        metadata_channels: u32,
        header_channels: u8,
    },

    /// Family 0 нормативно допускает только один или два канала.
    #[error("Opus mapping family 0 допускает только mono/stereo, получено {channels} каналов")]
    InvalidFamilyZeroChannels { channels: u8 },

    /// Multichannel нельзя безопасно создать без codec-owned mapping table.
    #[error(
        "multichannel Opus с {channels} каналами требует валидный OpusHead с channel mapping table"
    )]
    MissingMultichannelMapping { channels: u32 },

    /// Family 1 имеет определённую speaker semantics только для 1..=8 каналов.
    #[error("Opus mapping family 1 допускает от 1 до 8 каналов, получено {channels}")]
    InvalidFamilyOneChannels { channels: u8 },

    /// Family 1 mapping table содержит stream count, coupled count и C mapping bytes.
    #[error(
        "OpusHead mapping table обрезан: ожидалось минимум {expected} bytes, получено {actual}"
    )]
    TruncatedMappingTable { expected: usize, actual: usize },

    /// Хотя бы один coded stream обязателен для multistream packet framing.
    #[error("OpusHead содержит нулевой stream count")]
    ZeroStreams,

    /// Coupled stream является подмножеством всех streams.
    #[error("OpusHead coupled stream count {coupled_streams} превышает stream count {streams}")]
    CoupledStreamsExceedStreams { streams: u8, coupled_streams: u8 },

    /// Opus mapping byte не способен адресовать больше 255 decoded channels.
    #[error(
        "OpusHead описывает {decoded_channels} decoded channels: максимум 255 (streams={streams}, coupled={coupled_streams})"
    )]
    TooManyDecodedChannels {
        streams: u8,
        coupled_streams: u8,
        decoded_channels: u16,
    },

    /// Mapping byte должен ссылаться на существующий decoded channel или special silence 255.
    #[error(
        "OpusHead mapping[{channel_index}]={mapping_index} выходит за decoded channel count {decoded_channels}"
    )]
    InvalidMappingIndex {
        channel_index: usize,
        mapping_index: u8,
        decoded_channels: u16,
    },

    /// Family 255 не даёт speaker positions, а reserved families нельзя угадывать.
    #[error(
        "Opus channel mapping family {family} не имеет поддерживаемой speaker semantics для playback"
    )]
    UnsupportedMappingFamily { family: u8 },

    /// Внутренний neutral layout должен принять нормативный family 1 набор позиций.
    #[error("не удалось построить neutral layout для Opus family 1: {reason}")]
    InvalidNeutralLayout { reason: String },
}

/// Concrete libopus constructor plan после проверки container metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OpusBackendConfig {
    /// Обычный single-stream decoder для mapping family 0.
    SingleStream { channels: opus::Channels },

    /// Multistream decoder с уже переставленной в neutral lane order mapping table.
    Multistream {
        streams: u8,
        coupled_streams: u8,
        canonical_mapping: Vec<u8>,
    },
}

/// Parsed `OpusHead` после structural validation.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ParsedOpusHead {
    /// Output channel count.
    pub(super) channels: u8,

    /// Required playback gain в Q7.8 dB.
    pub(super) output_gain_q8_db: i16,

    /// Neutral positional layout output buffer-а.
    pub(super) channel_layout: AudioChannelLayout,

    /// Backend constructor arguments.
    pub(super) backend: OpusBackendConfig,
}

/// Парсит `OpusHead` и сохраняет только runtime decoder invariants.
pub(super) fn parse_opus_head(codec_private: &[u8]) -> Result<ParsedOpusHead, OpusHeadError> {
    if codec_private.len() < OPUS_HEAD_BASE_LENGTH {
        return Err(OpusHeadError::TooShort {
            expected: OPUS_HEAD_BASE_LENGTH,
            actual: codec_private.len(),
        });
    }
    if &codec_private[0..8] != b"OpusHead" {
        return Err(OpusHeadError::InvalidMagic);
    }

    let version = codec_private[8];
    if version & 0xf0 != 0 {
        return Err(OpusHeadError::UnsupportedVersion { version });
    }

    let channels = codec_private[9];
    if channels == 0 {
        return Err(OpusHeadError::ZeroChannels);
    }

    let output_gain_q8_db = i16::from_le_bytes([codec_private[16], codec_private[17]]);
    let mapping_family = codec_private[18];
    match mapping_family {
        0 => parse_family_zero(channels, output_gain_q8_db),
        1 => parse_family_one(codec_private, channels, output_gain_q8_db),
        family => Err(OpusHeadError::UnsupportedMappingFamily { family }),
    }
}

/// Строит implicit single-stream plan для mapping family 0.
fn parse_family_zero(
    channels: u8,
    output_gain_q8_db: i16,
) -> Result<ParsedOpusHead, OpusHeadError> {
    let (opus_channels, channel_layout) = match channels {
        1 => (opus::Channels::Mono, AudioChannelLayout::mono()),
        2 => (opus::Channels::Stereo, AudioChannelLayout::stereo()),
        channel_count => {
            return Err(OpusHeadError::InvalidFamilyZeroChannels {
                channels: channel_count,
            });
        }
    };

    Ok(ParsedOpusHead {
        channels,
        output_gain_q8_db,
        channel_layout,
        backend: OpusBackendConfig::SingleStream {
            channels: opus_channels,
        },
    })
}

/// Проверяет mapping family 1 и переводит Vorbis lane order в neutral canonical order.
fn parse_family_one(
    codec_private: &[u8],
    channels: u8,
    output_gain_q8_db: i16,
) -> Result<ParsedOpusHead, OpusHeadError> {
    let codec_channel_positions = family_one_channel_positions(channels)?;
    let required_length = OPUS_HEAD_MAPPING_OFFSET + usize::from(channels);
    if codec_private.len() < required_length {
        return Err(OpusHeadError::TruncatedMappingTable {
            expected: required_length,
            actual: codec_private.len(),
        });
    }

    let streams = codec_private[19];
    let coupled_streams = codec_private[20];
    if streams == 0 {
        return Err(OpusHeadError::ZeroStreams);
    }
    if coupled_streams > streams {
        return Err(OpusHeadError::CoupledStreamsExceedStreams {
            streams,
            coupled_streams,
        });
    }

    let decoded_channels = u16::from(streams) + u16::from(coupled_streams);
    if decoded_channels > u16::from(u8::MAX) {
        return Err(OpusHeadError::TooManyDecodedChannels {
            streams,
            coupled_streams,
            decoded_channels,
        });
    }

    let codec_mapping = &codec_private[OPUS_HEAD_MAPPING_OFFSET..required_length];
    for (channel_index, mapping_index) in codec_mapping.iter().copied().enumerate() {
        if mapping_index != u8::MAX && u16::from(mapping_index) >= decoded_channels {
            return Err(OpusHeadError::InvalidMappingIndex {
                channel_index,
                mapping_index,
                decoded_channels,
            });
        }
    }

    let channel_layout =
        AudioChannelLayout::positioned(codec_channel_positions).map_err(|error| {
            OpusHeadError::InvalidNeutralLayout {
                reason: error.to_string(),
            }
        })?;
    let canonical_mapping = canonical_opus_mapping(codec_channel_positions, codec_mapping);

    Ok(ParsedOpusHead {
        channels,
        output_gain_q8_db,
        channel_layout,
        backend: OpusBackendConfig::Multistream {
            streams,
            coupled_streams,
            canonical_mapping,
        },
    })
}

/// Возвращает нормативный Vorbis speaker order для Opus mapping family 1.
fn family_one_channel_positions(
    channels: u8,
) -> Result<&'static [AudioChannelPosition], OpusHeadError> {
    match channels {
        1 => Ok(&OPUS_FAMILY_ONE_MONO),
        2 => Ok(&OPUS_FAMILY_ONE_STEREO),
        3 => Ok(&OPUS_FAMILY_ONE_3_0),
        4 => Ok(&OPUS_FAMILY_ONE_QUAD),
        5 => Ok(&OPUS_FAMILY_ONE_5_0),
        6 => Ok(&OPUS_FAMILY_ONE_5_1),
        7 => Ok(&OPUS_FAMILY_ONE_6_1),
        8 => Ok(&OPUS_FAMILY_ONE_7_1),
        channel_count => Err(OpusHeadError::InvalidFamilyOneChannels {
            channels: channel_count,
        }),
    }
}

/// Переставляет mapping entries из codec order в audio-core canonical position order.
fn canonical_opus_mapping(
    codec_channel_positions: &[AudioChannelPosition],
    codec_mapping: &[u8],
) -> Vec<u8> {
    let mut positioned_mapping: Vec<(AudioChannelPosition, u8)> = codec_channel_positions
        .iter()
        .copied()
        .zip(codec_mapping.iter().copied())
        .collect();
    positioned_mapping.sort_unstable_by_key(|(position, _)| *position as u8);
    positioned_mapping
        .into_iter()
        .map(|(_, mapping_index)| mapping_index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        AudioChannelLayout, OpusBackendConfig, OpusHeadError, canonical_opus_mapping,
        parse_opus_head,
    };

    /// Точный OpusHead acceptance asset-а `Big_buck_bunny_720p_5mb.webm`.
    const ACCEPTANCE_5_1_OPUS_HEAD: [u8; 27] = [
        b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 1, 6, 0x38, 0x01, 0x80, 0xbb, 0, 0, 0, 0,
        1, 4, 2, 0, 4, 1, 2, 3, 5,
    ];

    /// Raw mapping table того же acceptance asset-а в Vorbis output order.
    const ACCEPTANCE_5_1_CODEC_MAPPING: [u8; 6] = [0, 4, 1, 2, 3, 5];

    /// Exact acceptance header должен создать canonical 5.1 layout и mapping permutation.
    #[test]
    fn acceptance_5_1_header_maps_vorbis_order_to_neutral_canonical_order() {
        let parsed_header =
            parse_opus_head(&ACCEPTANCE_5_1_OPUS_HEAD).expect("acceptance OpusHead must parse");

        assert_eq!(parsed_header.channels, 6);
        assert_eq!(
            parsed_header.channel_layout,
            AudioChannelLayout::surround_5_1()
        );
        assert_eq!(
            parsed_header.backend,
            OpusBackendConfig::Multistream {
                streams: 4,
                coupled_streams: 2,
                canonical_mapping: vec![0, 1, 4, 5, 2, 3],
            }
        );
    }

    /// Header parser не должен принимать обрезанную family 1 mapping table.
    #[test]
    fn family_one_rejects_truncated_mapping_table() {
        let error = parse_opus_head(&ACCEPTANCE_5_1_OPUS_HEAD[..26])
            .expect_err("truncated mapping table must be rejected");

        assert_eq!(
            error,
            OpusHeadError::TruncatedMappingTable {
                expected: 27,
                actual: 26,
            }
        );
    }

    /// Mapping entry не имеет права ссылаться на отсутствующий decoded channel.
    #[test]
    fn family_one_rejects_out_of_range_mapping_index() {
        let mut invalid_header = ACCEPTANCE_5_1_OPUS_HEAD;
        invalid_header[26] = 6;

        let error = parse_opus_head(&invalid_header)
            .expect_err("out-of-range mapping index must be rejected");

        assert_eq!(
            error,
            OpusHeadError::InvalidMappingIndex {
                channel_index: 5,
                mapping_index: 6,
                decoded_channels: 6,
            }
        );
    }

    /// Суммарное число mono/stereo decoded lanes ограничено размером mapping byte.
    #[test]
    fn family_one_rejects_more_than_255_decoded_channels() {
        let mut invalid_header = ACCEPTANCE_5_1_OPUS_HEAD;
        invalid_header[19] = u8::MAX;
        invalid_header[20] = u8::MAX;

        let error = parse_opus_head(&invalid_header)
            .expect_err("decoded channel count above 255 must be rejected");

        assert_eq!(
            error,
            OpusHeadError::TooManyDecodedChannels {
                streams: u8::MAX,
                coupled_streams: u8::MAX,
                decoded_channels: 510,
            }
        );
    }

    /// Перестановка всегда сортирует codec lanes по neutral position discriminant-у.
    #[test]
    fn canonical_mapping_preserves_mapping_identity_with_position_permutation() {
        let canonical_mapping =
            canonical_opus_mapping(&super::OPUS_FAMILY_ONE_5_1, &ACCEPTANCE_5_1_CODEC_MAPPING);

        assert_eq!(canonical_mapping, [0, 1, 4, 5, 2, 3]);
    }
}
