use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, H264Profile, MatrixCoefficients,
    TransferFunction, VideoColorMetadata,
};

// Requirement policy живёт отдельно, а facade сохраняет прежние публичные пути H.264 API.
mod requirements;
pub use requirements::*;

#[cfg(test)]
mod tests;

const AVC_DECODER_CONFIGURATION_RECORD_VERSION: u8 = 1;
const AVC_LENGTH_SIZE_MINUS_ONE_MASK: u8 = 0b0000_0011;
const AVC_SPS_COUNT_MASK: u8 = 0b0001_1111;
const H264_START_CODE_3: &[u8] = &[0x00, 0x00, 0x01];
const H264_START_CODE_4: &[u8] = &[0x00, 0x00, 0x00, 0x01];
const H264_NAL_TYPE_MASK: u8 = 0b0001_1111;
const H264_NAL_REF_IDC_MASK: u8 = 0b0110_0000;
const H264_NAL_FORBIDDEN_ZERO_MASK: u8 = 0b1000_0000;
const H264_NAL_TYPE_IDR_SLICE: u8 = 5;
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

/// Один H.264 NAL unit без Annex B start code и без AVCC length-prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H264NalUnit<'a> {
    header: H264NalHeader,
    bytes: &'a [u8],
}

impl<'a> H264NalUnit<'a> {
    /// Возвращает raw NAL bytes вместе с однобайтовым header-ом.
    #[must_use]
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    /// Возвращает `nal_unit_type`.
    #[must_use]
    pub const fn nal_unit_type(self) -> u8 {
        self.header.nal_unit_type
    }

    /// Возвращает `nal_ref_idc`.
    #[must_use]
    pub const fn nal_ref_idc(self) -> u8 {
        self.header.nal_ref_idc
    }
}

/// Parsed NAL header fields, нужные codec-neutral adapter-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct H264NalHeader {
    nal_unit_type: u8,
    nal_ref_idc: u8,
}

/// Ошибка packet/access-unit framing parser-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H264ByteStreamError {
    /// Packet пустой или не содержит NAL units.
    NoNalUnits,

    /// Annex B packet не содержит start code.
    MissingStartCode,

    /// NAL unit пустой.
    EmptyNalUnit,

    /// Forbidden zero bit в NAL header-е установлен.
    InvalidNalHeader {
        /// Raw header byte.
        header: u8,
    },

    /// AVCC length-prefix оборван.
    TruncatedAvccNalLength {
        /// Размер length-prefix для этого packet-а.
        nal_length_size: H264NalLengthSize,

        /// Сколько байтов осталось в packet-е.
        remaining_bytes: usize,
    },

    /// AVCC NAL unit объявил нулевую длину.
    ZeroLengthAvccNalUnit,

    /// AVCC NAL unit выходит за границы packet-а.
    TruncatedAvccNalUnit {
        /// Объявленная длина NAL unit-а.
        declared_size: usize,

        /// Сколько байтов осталось в packet-е.
        remaining_bytes: usize,
    },

    /// Packetization не удалось доказать.
    Packetization(H264PacketizationError),
}

impl fmt::Display for H264ByteStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoNalUnits => write!(formatter, "H.264 packet contains no NAL units"),
            Self::MissingStartCode => write!(formatter, "Annex B H.264 packet has no start code"),
            Self::EmptyNalUnit => write!(formatter, "H.264 packet contains an empty NAL unit"),
            Self::InvalidNalHeader { header } => {
                write!(formatter, "invalid H.264 NAL header 0x{header:02x}")
            }
            Self::TruncatedAvccNalLength {
                nal_length_size,
                remaining_bytes,
            } => write!(
                formatter,
                "truncated AVCC NAL length: expected {} bytes, remaining {remaining_bytes}",
                nal_length_size.bytes()
            ),
            Self::ZeroLengthAvccNalUnit => {
                write!(
                    formatter,
                    "AVCC H.264 packet contains a zero-length NAL unit"
                )
            }
            Self::TruncatedAvccNalUnit {
                declared_size,
                remaining_bytes,
            } => write!(
                formatter,
                "truncated AVCC NAL unit: declared {declared_size} bytes, remaining {remaining_bytes}"
            ),
            Self::Packetization(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for H264ByteStreamError {}

impl From<H264PacketizationError> for H264ByteStreamError {
    fn from(error: H264PacketizationError) -> Self {
        Self::Packetization(error)
    }
}

/// Политика добавления SPS/PPS при конвертации access unit-а в Annex B.
pub enum H264ParameterSetInjection<'a> {
    /// Не добавлять parameter sets, только перепаковать NAL units.
    None,

    /// Явно поставить SPS/PPS перед access unit-ом.
    ///
    /// Эта политика нужна для backend-ов, которые принимают Annex B stream и
    /// ожидают parameter sets рядом с IDR/decode-start packet-ом. Adapter не
    /// решает сам, когда инжектить SPS/PPS: caller выбирает lifecycle policy.
    BeforeAccessUnit {
        /// SPS units без start code.
        sequence_parameter_sets: &'a [Vec<u8>],

        /// PPS units без start code.
        picture_parameter_sets: &'a [Vec<u8>],
    },
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

/// Парсит `avcC`/AVCDecoderConfigurationRecord и извлекает SPS/PPS.
pub fn parse_avc_decoder_configuration_record(
    record_bytes: &[u8],
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
    validate_h264_profile(profile_idc, profile_compatibility).map_err(|unsupported_profile| {
        AvcDecoderConfigurationRecordError::UnsupportedProfile {
            profile_idc,
            profile_compatibility,
            reason: unsupported_profile.reason,
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
    if sps_count == 0 {
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
    if pps_count == 0 {
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

/// Выводит H.264 packetization из codec private и/или packet bytes.
pub fn infer_h264_packetization(
    codec_private: Option<&[u8]>,
    packet_bytes: &[u8],
) -> Result<H264Packetization, H264ByteStreamError> {
    if let Some(codec_private) = codec_private.filter(|bytes| !bytes.is_empty())
        && let Ok(record) = parse_avc_decoder_configuration_record(codec_private)
    {
        return Ok(record.packetization());
    }

    if contains_annex_b_start_code(packet_bytes) {
        return Ok(H264Packetization::AnnexB);
    }

    Err(H264PacketizationError::UnknownPacketization.into())
}

/// Возвращает NAL units access unit-а через явно указанную packetization model.
pub fn h264_nal_units<'a>(
    packet_bytes: &'a [u8],
    packetization: H264Packetization,
) -> Result<Vec<H264NalUnit<'a>>, H264ByteStreamError> {
    match packetization {
        H264Packetization::AnnexB => annex_b_nal_units(packet_bytes),
        H264Packetization::AvccLengthPrefixed { nal_length_size } => {
            avcc_nal_units(packet_bytes, nal_length_size)
        }
    }
}

/// Конвертирует H.264 access unit в Annex B и применяет caller-owned SPS/PPS injection policy.
pub fn h264_access_unit_to_annex_b(
    packet_bytes: &[u8],
    packetization: H264Packetization,
    parameter_set_injection: H264ParameterSetInjection<'_>,
) -> Result<Vec<u8>, H264ByteStreamError> {
    let mut annex_b_bytes = Vec::new();
    h264_access_unit_to_annex_b_into(
        packet_bytes,
        packetization,
        parameter_set_injection,
        &mut annex_b_bytes,
    )?;
    Ok(annex_b_bytes)
}

/// Конвертирует H.264 access unit в caller-owned Annex B buffer без новой output allocation.
///
/// Функция очищает `output`, сохраняет его capacity и оставляет его пустым при ошибке.
pub fn h264_access_unit_to_annex_b_into(
    packet_bytes: &[u8],
    packetization: H264Packetization,
    parameter_set_injection: H264ParameterSetInjection<'_>,
    output: &mut Vec<u8>,
) -> Result<(), H264ByteStreamError> {
    output.clear();

    let write_result = {
        if let H264ParameterSetInjection::BeforeAccessUnit {
            sequence_parameter_sets,
            picture_parameter_sets,
        } = parameter_set_injection
        {
            append_parameter_sets(output, sequence_parameter_sets);
            append_parameter_sets(output, picture_parameter_sets);
        }

        append_access_unit_nals_to_annex_b(packet_bytes, packetization, output)
    };

    if write_result.is_err() {
        output.clear();
    }

    write_result
}

/// Возвращает `true` только для access unit-а с IDR slice.
pub fn probe_h264_packet_keyframe(
    packet_bytes: &[u8],
    packetization: H264Packetization,
) -> Result<bool, H264ByteStreamError> {
    let nal_units = h264_nal_units(packet_bytes, packetization)?;

    for nal_unit in nal_units {
        if nal_unit.nal_unit_type() == H264_NAL_TYPE_IDR_SLICE {
            return Ok(true);
        }
    }

    Ok(false)
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

fn annex_b_nal_units(packet_bytes: &[u8]) -> Result<Vec<H264NalUnit<'_>>, H264ByteStreamError> {
    let Some((first_start_code_offset, first_start_code_size)) =
        find_annex_b_start_code(packet_bytes, 0)
    else {
        return Err(H264ByteStreamError::MissingStartCode);
    };

    let mut nal_units = Vec::new();
    let mut nal_start = first_start_code_offset + first_start_code_size;

    while nal_start < packet_bytes.len() {
        let next_start_code = find_annex_b_start_code(packet_bytes, nal_start);
        let nal_end = next_start_code
            .map(|(start_code_offset, _)| start_code_offset)
            .unwrap_or(packet_bytes.len());

        if nal_start < nal_end {
            nal_units.push(parse_nal_unit(&packet_bytes[nal_start..nal_end])?);
        }

        let Some((next_start_code_offset, next_start_code_size)) = next_start_code else {
            break;
        };
        nal_start = next_start_code_offset + next_start_code_size;
    }

    if nal_units.is_empty() {
        return Err(H264ByteStreamError::NoNalUnits);
    }

    Ok(nal_units)
}

fn avcc_nal_units(
    packet_bytes: &[u8],
    nal_length_size: H264NalLengthSize,
) -> Result<Vec<H264NalUnit<'_>>, H264ByteStreamError> {
    let mut cursor = 0;
    let mut nal_units = Vec::new();

    while cursor < packet_bytes.len() {
        let remaining_bytes = packet_bytes.len() - cursor;
        if remaining_bytes < nal_length_size.bytes() {
            return Err(H264ByteStreamError::TruncatedAvccNalLength {
                nal_length_size,
                remaining_bytes,
            });
        }

        let declared_size = read_avcc_nal_size(packet_bytes, cursor, nal_length_size);
        cursor += nal_length_size.bytes();
        if declared_size == 0 {
            return Err(H264ByteStreamError::ZeroLengthAvccNalUnit);
        }

        let remaining_bytes = packet_bytes.len() - cursor;
        if remaining_bytes < declared_size {
            return Err(H264ByteStreamError::TruncatedAvccNalUnit {
                declared_size,
                remaining_bytes,
            });
        }

        nal_units.push(parse_nal_unit(
            &packet_bytes[cursor..cursor + declared_size],
        )?);
        cursor += declared_size;
    }

    if nal_units.is_empty() {
        return Err(H264ByteStreamError::NoNalUnits);
    }

    Ok(nal_units)
}

fn append_access_unit_nals_to_annex_b(
    packet_bytes: &[u8],
    packetization: H264Packetization,
    output: &mut Vec<u8>,
) -> Result<(), H264ByteStreamError> {
    match packetization {
        H264Packetization::AnnexB => append_annex_b_nal_units_to_annex_b(packet_bytes, output),
        H264Packetization::AvccLengthPrefixed { nal_length_size } => {
            append_avcc_nal_units_to_annex_b(packet_bytes, nal_length_size, output)
        }
    }
}

fn append_annex_b_nal_units_to_annex_b(
    packet_bytes: &[u8],
    output: &mut Vec<u8>,
) -> Result<(), H264ByteStreamError> {
    let Some((first_start_code_offset, first_start_code_size)) =
        find_annex_b_start_code(packet_bytes, 0)
    else {
        return Err(H264ByteStreamError::MissingStartCode);
    };

    let mut nal_units_written = 0usize;
    let mut nal_start = first_start_code_offset + first_start_code_size;

    while nal_start < packet_bytes.len() {
        let next_start_code = find_annex_b_start_code(packet_bytes, nal_start);
        let nal_end = next_start_code
            .map(|(start_code_offset, _)| start_code_offset)
            .unwrap_or(packet_bytes.len());

        if nal_start < nal_end {
            append_nal_unit_to_annex_b(&packet_bytes[nal_start..nal_end], output)?;
            nal_units_written += 1;
        }

        let Some((next_start_code_offset, next_start_code_size)) = next_start_code else {
            break;
        };
        nal_start = next_start_code_offset + next_start_code_size;
    }

    if nal_units_written == 0 {
        return Err(H264ByteStreamError::NoNalUnits);
    }

    Ok(())
}

fn append_avcc_nal_units_to_annex_b(
    packet_bytes: &[u8],
    nal_length_size: H264NalLengthSize,
    output: &mut Vec<u8>,
) -> Result<(), H264ByteStreamError> {
    let mut cursor = 0;
    let mut nal_units_written = 0usize;

    while cursor < packet_bytes.len() {
        let remaining_bytes = packet_bytes.len() - cursor;
        if remaining_bytes < nal_length_size.bytes() {
            return Err(H264ByteStreamError::TruncatedAvccNalLength {
                nal_length_size,
                remaining_bytes,
            });
        }

        let declared_size = read_avcc_nal_size(packet_bytes, cursor, nal_length_size);
        cursor += nal_length_size.bytes();
        if declared_size == 0 {
            return Err(H264ByteStreamError::ZeroLengthAvccNalUnit);
        }

        let remaining_bytes = packet_bytes.len() - cursor;
        if remaining_bytes < declared_size {
            return Err(H264ByteStreamError::TruncatedAvccNalUnit {
                declared_size,
                remaining_bytes,
            });
        }

        append_nal_unit_to_annex_b(&packet_bytes[cursor..cursor + declared_size], output)?;
        cursor += declared_size;
        nal_units_written += 1;
    }

    if nal_units_written == 0 {
        return Err(H264ByteStreamError::NoNalUnits);
    }

    Ok(())
}

fn append_nal_unit_to_annex_b(
    nal_unit_bytes: &[u8],
    output: &mut Vec<u8>,
) -> Result<(), H264ByteStreamError> {
    let nal_unit = parse_nal_unit(nal_unit_bytes)?;
    output.extend_from_slice(H264_START_CODE_4);
    output.extend_from_slice(nal_unit.bytes());
    Ok(())
}

fn read_avcc_nal_size(
    packet_bytes: &[u8],
    cursor: usize,
    nal_length_size: H264NalLengthSize,
) -> usize {
    match nal_length_size.get() {
        1 => usize::from(packet_bytes[cursor]),
        2 => usize::from(u16::from_be_bytes([
            packet_bytes[cursor],
            packet_bytes[cursor + 1],
        ])),
        4 => u32::from_be_bytes([
            packet_bytes[cursor],
            packet_bytes[cursor + 1],
            packet_bytes[cursor + 2],
            packet_bytes[cursor + 3],
        ]) as usize,
        _ => unreachable!("H264NalLengthSize содержит только 1/2/4"),
    }
}

fn parse_nal_unit(nal_unit_bytes: &[u8]) -> Result<H264NalUnit<'_>, H264ByteStreamError> {
    Ok(H264NalUnit {
        header: parse_nal_header(nal_unit_bytes)?,
        bytes: nal_unit_bytes,
    })
}

fn parse_nal_header(nal_unit_bytes: &[u8]) -> Result<H264NalHeader, H264ByteStreamError> {
    let Some(&header) = nal_unit_bytes.first() else {
        return Err(H264ByteStreamError::EmptyNalUnit);
    };

    if header & H264_NAL_FORBIDDEN_ZERO_MASK != 0 {
        return Err(H264ByteStreamError::InvalidNalHeader { header });
    }

    Ok(H264NalHeader {
        nal_unit_type: header & H264_NAL_TYPE_MASK,
        nal_ref_idc: (header & H264_NAL_REF_IDC_MASK) >> 5,
    })
}

fn find_annex_b_start_code(packet_bytes: &[u8], from_offset: usize) -> Option<(usize, usize)> {
    let mut offset = from_offset;
    while offset + H264_START_CODE_3.len() <= packet_bytes.len() {
        if packet_bytes[offset..].starts_with(H264_START_CODE_4) {
            return Some((offset, H264_START_CODE_4.len()));
        }
        if packet_bytes[offset..].starts_with(H264_START_CODE_3) {
            return Some((offset, H264_START_CODE_3.len()));
        }
        offset += 1;
    }

    None
}

fn contains_annex_b_start_code(packet_bytes: &[u8]) -> bool {
    find_annex_b_start_code(packet_bytes, 0).is_some()
}

fn append_parameter_sets(output: &mut Vec<u8>, parameter_sets: &[Vec<u8>]) {
    for parameter_set in parameter_sets {
        output.extend_from_slice(H264_START_CODE_4);
        output.extend_from_slice(parameter_set);
    }
}
