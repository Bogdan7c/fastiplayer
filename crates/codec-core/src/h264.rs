use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, H264Profile, MatrixCoefficients,
    TransferFunction, VideoColorMetadata,
};

// Byte-stream и requirement policy живут в отдельных concerns, а facade сохраняет прежние
// публичные пути H.264 API.
mod bytestream;
use bytestream::parse_nal_header;
pub use bytestream::{
    H264ByteStreamError, H264NalUnit, H264ParameterSetInjection, h264_access_unit_to_annex_b,
    h264_access_unit_to_annex_b_into, h264_nal_units, infer_h264_packetization,
    probe_h264_packet_in_band_decode_start, probe_h264_packet_keyframe,
};

mod requirements;
pub use requirements::*;

#[cfg(test)]
mod tests;

const AVC_DECODER_CONFIGURATION_RECORD_VERSION: u8 = 1;
const AVC_LENGTH_SIZE_MINUS_ONE_MASK: u8 = 0b0000_0011;
const AVC_SPS_COUNT_MASK: u8 = 0b0001_1111;
const H264_NAL_TYPE_SPS: u8 = 7;

/// Размер length-prefix перед каждым NAL unit в AVCC packetization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct H264NalLengthSize(u8);

impl H264NalLengthSize {
    /// Один байт длины NAL unit.
    pub const ONE: Self = Self(1);

    /// Два байта длины NAL unit.
    pub const TWO: Self = Self(2);

    /// Четыре байта длины NAL unit.
    pub const FOUR: Self = Self(4);

    /// Создаёт typed размер только для значений, разрешённых AVCDecoderConfigurationRecord.
    pub fn new(size_bytes: u8) -> Result<Self, H264PacketizationError> {
        match size_bytes {
            1 => Ok(Self::ONE),
            2 => Ok(Self::TWO),
            4 => Ok(Self::FOUR),
            unsupported_size => Err(H264PacketizationError::UnsupportedNalLengthSize {
                nal_length_size: unsupported_size,
            }),
        }
    }

    /// Возвращает количество байтов в length-prefix.
    #[must_use]
    pub const fn bytes(self) -> usize {
        self.0 as usize
    }

    /// Возвращает raw значение для diagnostics и сериализации внешних контрактов.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Явный способ разбиения H.264 access unit-а на NAL units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H264Packetization {
    /// Annex B byte-stream с 3- или 4-байтовыми start codes.
    AnnexB,

    /// ISO BMFF/AVCC length-prefixed packetization.
    AvccLengthPrefixed {
        /// Размер big-endian length field перед каждым NAL unit.
        nal_length_size: H264NalLengthSize,
    },

    /// ISO BMFF/AVCC length-prefixed packetization с parameter sets внутри samples.
    AvccLengthPrefixedWithInBandParameterSets {
        /// Размер big-endian length field перед каждым NAL unit.
        nal_length_size: H264NalLengthSize,
    },
}

impl H264Packetization {
    /// Строит packetization из уже проверенного `avcC`.
    #[must_use]
    pub const fn from_avc_decoder_configuration_record(
        record: &AvcDecoderConfigurationRecord,
    ) -> Self {
        Self::AvccLengthPrefixed {
            nal_length_size: record.nal_length_size,
        }
    }

    /// Строит packetization для `avc3`, где SPS/PPS могут находиться только в samples.
    #[must_use]
    pub const fn from_avc3_decoder_configuration_record(
        record: &AvcDecoderConfigurationRecord,
    ) -> Self {
        Self::AvccLengthPrefixedWithInBandParameterSets {
            nal_length_size: record.nal_length_size,
        }
    }
}

/// Structural `avcC` parser result без backend-specific state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcDecoderConfigurationRecord {
    /// `AVCProfileIndication` из record header-а.
    pub profile_idc: u8,

    /// `profile_compatibility` с constraint flags.
    pub profile_compatibility: u8,

    /// `AVCLevelIndication`.
    pub level_idc: u8,

    /// Размер NAL length-prefix в AVCC packets.
    pub nal_length_size: H264NalLengthSize,

    sequence_parameter_sets: Vec<Vec<u8>>,
    picture_parameter_sets: Vec<Vec<u8>>,
}

impl AvcDecoderConfigurationRecord {
    /// SPS NAL units без length-prefix и без Annex B start code.
    #[must_use]
    pub fn sequence_parameter_sets(&self) -> &[Vec<u8>] {
        &self.sequence_parameter_sets
    }

    /// PPS NAL units без length-prefix и без Annex B start code.
    #[must_use]
    pub fn picture_parameter_sets(&self) -> &[Vec<u8>] {
        &self.picture_parameter_sets
    }

    /// Возвращает packetization, которую должны использовать media/backend слои.
    #[must_use]
    pub const fn packetization(&self) -> H264Packetization {
        H264Packetization::from_avc_decoder_configuration_record(self)
    }
}

/// Ошибка packetization model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H264PacketizationError {
    /// AVCDecoderConfigurationRecord разрешает только 1/2/4 байта length-prefix.
    UnsupportedNalLengthSize {
        /// Неподдержанный размер length-prefix.
        nal_length_size: u8,
    },

    /// Нельзя доказать packetization из codec-private и packet bytes.
    UnknownPacketization,
}

impl fmt::Display for H264PacketizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedNalLengthSize { nal_length_size } => {
                write!(
                    formatter,
                    "unsupported H.264 NAL length size {nal_length_size}; expected 1, 2 or 4"
                )
            }
            Self::UnknownPacketization => {
                write!(formatter, "unable to determine H.264 packetization")
            }
        }
    }
}

impl std::error::Error for H264PacketizationError {}

/// Ошибка `avcC`/AVCDecoderConfigurationRecord parser-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvcDecoderConfigurationRecordError {
    /// Record короче минимально нужного header-а.
    TooShort {
        /// Фактический размер record-а.
        actual_size: usize,

        /// Минимальный ожидаемый размер.
        minimum_size: usize,
    },

    /// `configurationVersion` не равен 1.
    UnsupportedVersion {
        /// Значение из record-а.
        version: u8,
    },

    /// Reserved bits не соответствуют AVCDecoderConfigurationRecord.
    InvalidReservedBits {
        /// Имя поля record-а.
        field: &'static str,

        /// Значение поля с reserved bits.
        value: u8,

        /// Ожидаемое значение после mask-а.
        expected_masked_value: u8,
    },

    /// Профиль не входит в v1 production subset.
    UnsupportedProfile {
        /// Raw `profile_idc`.
        profile_idc: u8,

        /// Raw compatibility/constraint flags.
        profile_compatibility: u8,

        /// Typed причина отказа.
        reason: &'static str,
    },

    /// Level field структурно невалиден.
    InvalidLevel {
        /// Raw `level_idc`.
        level_idc: u8,
    },

    /// Length-prefix имеет запрещённый размер.
    UnsupportedNalLengthSize {
        /// Неподдержанный размер length-prefix.
        nal_length_size: u8,
    },

    /// Record не содержит ни одного SPS.
    MissingSequenceParameterSet,

    /// Record не содержит ни одного PPS.
    MissingPictureParameterSet,

    /// NAL unit объявлен с нулевой длиной.
    EmptyNalUnit {
        /// Какая parameter set таблица повреждена.
        kind: H264ParameterSetKind,

        /// Индекс NAL unit-а внутри таблицы.
        index: usize,
    },

    /// Record закончился до поля длины NAL unit-а.
    TruncatedNalLength {
        /// Какая parameter set таблица повреждена.
        kind: H264ParameterSetKind,

        /// Индекс NAL unit-а внутри таблицы.
        index: usize,

        /// Сколько байтов осталось.
        remaining_bytes: usize,
    },

    /// Record закончился до полного NAL unit payload.
    TruncatedNalUnit {
        /// Какая parameter set таблица повреждена.
        kind: H264ParameterSetKind,

        /// Индекс NAL unit-а внутри таблицы.
        index: usize,

        /// Объявленная длина NAL unit-а.
        declared_size: usize,

        /// Сколько байтов осталось.
        remaining_bytes: usize,
    },

    /// Record закончился до `numOfPictureParameterSets`.
    MissingPictureParameterSetCount,
}

impl fmt::Display for AvcDecoderConfigurationRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort {
                actual_size,
                minimum_size,
            } => write!(
                formatter,
                "avcC record too short: {actual_size} bytes, expected at least {minimum_size}"
            ),
            Self::UnsupportedVersion { version } => {
                write!(formatter, "unsupported avcC version {version}")
            }
            Self::InvalidReservedBits {
                field,
                value,
                expected_masked_value,
            } => write!(
                formatter,
                "invalid avcC reserved bits in {field}: value=0x{value:02x}, expected masked value=0x{expected_masked_value:02x}"
            ),
            Self::UnsupportedProfile {
                profile_idc,
                profile_compatibility,
                reason,
            } => write!(
                formatter,
                "unsupported H.264 profile in avcC: profile_idc={profile_idc}, compatibility=0x{profile_compatibility:02x}: {reason}"
            ),
            Self::InvalidLevel { level_idc } => {
                write!(formatter, "invalid H.264 level_idc {level_idc}")
            }
            Self::UnsupportedNalLengthSize { nal_length_size } => write!(
                formatter,
                "unsupported avcC NAL length size {nal_length_size}; expected 1, 2 or 4"
            ),
            Self::MissingSequenceParameterSet => {
                write!(formatter, "avcC record has no sequence parameter sets")
            }
            Self::MissingPictureParameterSet => {
                write!(formatter, "avcC record has no picture parameter sets")
            }
            Self::EmptyNalUnit { kind, index } => {
                write!(formatter, "empty {kind} NAL unit at index {index}")
            }
            Self::TruncatedNalLength {
                kind,
                index,
                remaining_bytes,
            } => write!(
                formatter,
                "truncated {kind} NAL length at index {index}: {remaining_bytes} bytes remaining"
            ),
            Self::TruncatedNalUnit {
                kind,
                index,
                declared_size,
                remaining_bytes,
            } => write!(
                formatter,
                "truncated {kind} NAL unit at index {index}: declared {declared_size} bytes, {remaining_bytes} bytes remaining"
            ),
            Self::MissingPictureParameterSetCount => {
                write!(formatter, "avcC record ended before PPS count")
            }
        }
    }
}

impl std::error::Error for AvcDecoderConfigurationRecordError {}

/// Вид H.264 parameter set-а для typed diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H264ParameterSetKind {
    /// Sequence Parameter Set.
    Sequence,

    /// Picture Parameter Set.
    Picture,
}

impl fmt::Display for H264ParameterSetKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sequence => formatter.write_str("SPS"),
            Self::Picture => formatter.write_str("PPS"),
        }
    }
}

/// Подтверждённые SPS metadata, достаточные для v1 capability selection.
#[derive(Debug, Clone, PartialEq)]
pub struct H264SpsMetadata {
    /// Поддержанный production profile.
    pub profile: H264Profile,

    /// Raw `level_idc`.
    pub level_idc: u8,

    /// Поддержанная bit depth.
    pub bit_depth: BitDepth,

    /// Поддержанная chroma subsampling.
    pub chroma: ChromaSubsampling,

    /// Coded display width после crop.
    pub width: u32,

    /// Coded display height после crop.
    pub height: u32,

    /// VUI color metadata, если SPS явно её сообщил.
    pub color: Option<VideoColorMetadata>,
}

/// Ошибка SPS parser-а: malformed отдельно от unsupported stream variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H264SpsError {
    /// NAL unit пустой.
    EmptyNalUnit,

    /// Переданный NAL не является SPS.
    UnexpectedNalUnitType {
        /// Фактический NAL unit type.
        nal_unit_type: u8,
    },

    /// NAL header структурно невалиден.
    InvalidNalHeader {
        /// Raw header byte.
        header: u8,
    },

    /// Профиль вне v1 subset.
    UnsupportedProfile {
        /// Raw `profile_idc`.
        profile_idc: u8,

        /// Constraint flags byte.
        constraint_flags: u8,

        /// Typed причина отказа.
        reason: &'static str,
    },

    /// Bit depth распознана, но v1 zero-copy path её не поддерживает.
    UnsupportedBitDepth {
        /// Raw bit depth из SPS.
        bit_depth: u8,
    },

    /// Chroma layout распознан, но v1 zero-copy path его не поддерживает.
    UnsupportedChroma {
        /// Raw `chroma_format_idc`.
        chroma_format_idc: u32,
    },

    /// SPS не содержит валидные coded dimensions.
    InvalidDimensions,

    /// Битовый reader не смог дочитать обязательное поле.
    MalformedBitstream(H264BitReaderError),
}

impl fmt::Display for H264SpsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyNalUnit => write!(formatter, "empty H.264 SPS NAL unit"),
            Self::UnexpectedNalUnitType { nal_unit_type } => {
                write!(
                    formatter,
                    "expected H.264 SPS NAL unit type 7, got {nal_unit_type}"
                )
            }
            Self::InvalidNalHeader { header } => {
                write!(formatter, "invalid H.264 SPS NAL header 0x{header:02x}")
            }
            Self::UnsupportedProfile {
                profile_idc,
                constraint_flags,
                reason,
            } => write!(
                formatter,
                "unsupported H.264 SPS profile_idc={profile_idc}, constraints=0x{constraint_flags:02x}: {reason}"
            ),
            Self::UnsupportedBitDepth { bit_depth } => {
                write!(formatter, "unsupported H.264 bit depth {bit_depth}")
            }
            Self::UnsupportedChroma { chroma_format_idc } => write!(
                formatter,
                "unsupported H.264 chroma_format_idc {chroma_format_idc}"
            ),
            Self::InvalidDimensions => write!(formatter, "invalid H.264 SPS dimensions"),
            Self::MalformedBitstream(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for H264SpsError {}

impl From<H264BitReaderError> for H264SpsError {
    fn from(error: H264BitReaderError) -> Self {
        Self::MalformedBitstream(error)
    }
}

/// Ошибка bit-level SPS reader-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H264BitReaderError {
    /// Reader дошёл до конца RBSP до обязательного поля.
    UnexpectedEnd {
        /// Сколько бит запросили.
        requested_bits: u8,

        /// Сколько бит было доступно.
        remaining_bits: usize,
    },

    /// Exp-Golomb prefix слишком длинный для текущего typed parser-а.
    ExpGolombOverflow,

    /// Signed Exp-Golomb value не помещается в `i32`.
    SignedExpGolombOverflow,
}

impl fmt::Display for H264BitReaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd {
                requested_bits,
                remaining_bits,
            } => write!(
                formatter,
                "unexpected end of H.264 RBSP: requested {requested_bits} bits, remaining {remaining_bits}"
            ),
            Self::ExpGolombOverflow => write!(formatter, "H.264 Exp-Golomb value overflow"),
            Self::SignedExpGolombOverflow => {
                write!(formatter, "H.264 signed Exp-Golomb value overflow")
            }
        }
    }
}

impl std::error::Error for H264BitReaderError {}

/// Определяет, обязан ли configuration record сам нести parameter sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AvcParameterSetContract {
    /// `avc1`: SPS/PPS обязаны находиться в `avcC`.
    ConfigurationRequired,
    /// `avc3`: SPS/PPS могут находиться только внутри media samples.
    InBandAllowed,
}

/// Парсит строгий `avc1` `avcC`/AVCDecoderConfigurationRecord и извлекает SPS/PPS.
pub fn parse_avc_decoder_configuration_record(
    record_bytes: &[u8],
) -> Result<AvcDecoderConfigurationRecord, AvcDecoderConfigurationRecordError> {
    parse_avc_decoder_configuration_record_with_contract(
        record_bytes,
        AvcParameterSetContract::ConfigurationRequired,
    )
}

/// Парсит `avc3` `avcC`, где SPS/PPS разрешено передавать внутри media samples.
pub fn parse_avc3_decoder_configuration_record(
    record_bytes: &[u8],
) -> Result<AvcDecoderConfigurationRecord, AvcDecoderConfigurationRecordError> {
    parse_avc_decoder_configuration_record_with_contract(
        record_bytes,
        AvcParameterSetContract::InBandAllowed,
    )
}

/// Реализует общую структурную проверку `avcC` с явным контрактом parameter sets.
fn parse_avc_decoder_configuration_record_with_contract(
    record_bytes: &[u8],
    parameter_set_contract: AvcParameterSetContract,
) -> Result<AvcDecoderConfigurationRecord, AvcDecoderConfigurationRecordError> {
    if record_bytes.len() < 6 {
        return Err(AvcDecoderConfigurationRecordError::TooShort {
            actual_size: record_bytes.len(),
            minimum_size: 6,
        });
    }

    let version = record_bytes[0];
    if version != AVC_DECODER_CONFIGURATION_RECORD_VERSION {
        return Err(AvcDecoderConfigurationRecordError::UnsupportedVersion { version });
    }

    let profile_idc = record_bytes[1];
    let profile_compatibility = record_bytes[2];
    let profile_indication = H264ProfileIndication::new(profile_idc, profile_compatibility);
    h264_profile_from_indication(profile_indication).map_err(|unsupported_profile| {
        AvcDecoderConfigurationRecordError::UnsupportedProfile {
            profile_idc: unsupported_profile.indication().profile_idc(),
            profile_compatibility: unsupported_profile.indication().constraint_flags(),
            reason: unsupported_profile.reason(),
        }
    })?;

    let level_idc = record_bytes[3];
    if level_idc == 0 {
        return Err(AvcDecoderConfigurationRecordError::InvalidLevel { level_idc });
    }

    let length_size_byte = record_bytes[4];
    let nal_length_size_minus_one = length_size_byte & AVC_LENGTH_SIZE_MINUS_ONE_MASK;
    // Некоторые MP4 muxer-ы зануляют reserved bits в `avcC`; полезную семантику
    // несут только младшие два бита, поэтому не отвергаем воспроизводимый stream.
    let nal_length_size =
        H264NalLengthSize::new(nal_length_size_minus_one + 1).map_err(|error| {
            let H264PacketizationError::UnsupportedNalLengthSize { nal_length_size } = error else {
                unreachable!("H264NalLengthSize::new возвращает только UnsupportedNalLengthSize");
            };
            AvcDecoderConfigurationRecordError::UnsupportedNalLengthSize { nal_length_size }
        })?;

    let sps_count_byte = record_bytes[5];
    let sps_count = usize::from(sps_count_byte & AVC_SPS_COUNT_MASK);
    if sps_count == 0 && parameter_set_contract == AvcParameterSetContract::ConfigurationRequired {
        return Err(AvcDecoderConfigurationRecordError::MissingSequenceParameterSet);
    }

    let mut cursor = 6;
    let sequence_parameter_sets = read_parameter_sets(
        record_bytes,
        &mut cursor,
        sps_count,
        H264ParameterSetKind::Sequence,
    )?;

    let Some(&pps_count_byte) = record_bytes.get(cursor) else {
        return Err(AvcDecoderConfigurationRecordError::MissingPictureParameterSetCount);
    };
    cursor += 1;
    let pps_count = usize::from(pps_count_byte);
    if pps_count == 0 && parameter_set_contract == AvcParameterSetContract::ConfigurationRequired {
        return Err(AvcDecoderConfigurationRecordError::MissingPictureParameterSet);
    }

    let picture_parameter_sets = read_parameter_sets(
        record_bytes,
        &mut cursor,
        pps_count,
        H264ParameterSetKind::Picture,
    )?;

    Ok(AvcDecoderConfigurationRecord {
        profile_idc,
        profile_compatibility,
        level_idc,
        nal_length_size,
        sequence_parameter_sets,
        picture_parameter_sets,
    })
}

fn read_parameter_sets(
    record_bytes: &[u8],
    cursor: &mut usize,
    parameter_set_count: usize,
    parameter_set_kind: H264ParameterSetKind,
) -> Result<Vec<Vec<u8>>, AvcDecoderConfigurationRecordError> {
    let mut parameter_sets = Vec::with_capacity(parameter_set_count);

    for index in 0..parameter_set_count {
        let declared_size =
            read_parameter_set_size(record_bytes, cursor, parameter_set_kind, index)?;
        if declared_size == 0 {
            return Err(AvcDecoderConfigurationRecordError::EmptyNalUnit {
                kind: parameter_set_kind,
                index,
            });
        }

        let remaining_bytes = record_bytes.len().saturating_sub(*cursor);
        if remaining_bytes < declared_size {
            return Err(AvcDecoderConfigurationRecordError::TruncatedNalUnit {
                kind: parameter_set_kind,
                index,
                declared_size,
                remaining_bytes,
            });
        }

        let parameter_set = record_bytes[*cursor..*cursor + declared_size].to_vec();
        *cursor += declared_size;
        parameter_sets.push(parameter_set);
    }

    Ok(parameter_sets)
}

fn read_parameter_set_size(
    record_bytes: &[u8],
    cursor: &mut usize,
    parameter_set_kind: H264ParameterSetKind,
    index: usize,
) -> Result<usize, AvcDecoderConfigurationRecordError> {
    let remaining_bytes = record_bytes.len().saturating_sub(*cursor);
    if remaining_bytes < 2 {
        return Err(AvcDecoderConfigurationRecordError::TruncatedNalLength {
            kind: parameter_set_kind,
            index,
            remaining_bytes,
        });
    }

    let size = u16::from_be_bytes([record_bytes[*cursor], record_bytes[*cursor + 1]]) as usize;
    *cursor += 2;
    Ok(size)
}
