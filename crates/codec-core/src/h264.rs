use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, H264Profile, MatrixCoefficients,
    TransferFunction, VideoColorMetadata,
};

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

    let write_result = (|| {
        if let H264ParameterSetInjection::BeforeAccessUnit {
            sequence_parameter_sets,
            picture_parameter_sets,
        } = parameter_set_injection
        {
            append_parameter_sets(output, sequence_parameter_sets);
            append_parameter_sets(output, picture_parameter_sets);
        }

        append_access_unit_nals_to_annex_b(packet_bytes, packetization, output)
    })();

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

/// Парсит первый SPS из `avcC` и возвращает v1 requirement metadata.
pub fn h264_sps_metadata_from_avc_decoder_configuration_record(
    record_bytes: &[u8],
) -> Result<H264SpsMetadata, H264RequirementError> {
    let record = parse_avc_decoder_configuration_record(record_bytes)
        .map_err(H264RequirementError::AvcDecoderConfigurationRecord)?;
    let sps = record
        .sequence_parameter_sets()
        .first()
        .ok_or(H264RequirementError::MissingSequenceParameterSet)?;

    parse_h264_sps_metadata(sps).map_err(H264RequirementError::SequenceParameterSet)
}

/// Ищет SPS внутри packet-а и возвращает v1 requirement metadata.
pub fn h264_sps_metadata_from_packet(
    packet_bytes: &[u8],
    packetization: H264Packetization,
) -> Result<H264SpsMetadata, H264RequirementError> {
    let nal_units =
        h264_nal_units(packet_bytes, packetization).map_err(H264RequirementError::ByteStream)?;

    for nal_unit in nal_units {
        if nal_unit.nal_unit_type() == H264_NAL_TYPE_SPS {
            return parse_h264_sps_metadata(nal_unit.bytes())
                .map_err(H264RequirementError::SequenceParameterSet);
        }
    }

    Err(H264RequirementError::MissingSequenceParameterSet)
}

/// Парсит SPS NAL unit и подтверждает v1 supported subset: 8-bit 4:2:0 CBP/Main/High.
pub fn parse_h264_sps_metadata(nal_unit_bytes: &[u8]) -> Result<H264SpsMetadata, H264SpsError> {
    let nal_header = parse_nal_header(nal_unit_bytes).map_err(|error| match error {
        H264ByteStreamError::EmptyNalUnit => H264SpsError::EmptyNalUnit,
        H264ByteStreamError::InvalidNalHeader { header } => {
            H264SpsError::InvalidNalHeader { header }
        }
        other_error => unreachable!("parse_nal_header returned unexpected error: {other_error:?}"),
    })?;
    if nal_header.nal_unit_type != H264_NAL_TYPE_SPS {
        return Err(H264SpsError::UnexpectedNalUnitType {
            nal_unit_type: nal_header.nal_unit_type,
        });
    }

    let rbsp_bytes = ebsp_to_rbsp(&nal_unit_bytes[1..]);
    let mut bit_reader = H264BitReader::new(&rbsp_bytes);

    let profile_idc = bit_reader.read_u8()?;
    let constraint_flags = bit_reader.read_u8()?;
    let level_idc = bit_reader.read_u8()?;
    let profile =
        validate_h264_profile(profile_idc, constraint_flags).map_err(|unsupported_profile| {
            H264SpsError::UnsupportedProfile {
                profile_idc,
                constraint_flags,
                reason: unsupported_profile.reason,
            }
        })?;
    let _seq_parameter_set_id = bit_reader.read_ue()?;

    let high_syntax = profile_uses_high_syntax(profile_idc);
    let mut chroma_format_idc = 1;
    let mut separate_colour_plane_flag = false;
    let mut bit_depth_luma = 8;
    let mut bit_depth_chroma = 8;

    if high_syntax {
        chroma_format_idc = bit_reader.read_ue()?;
        if chroma_format_idc == 3 {
            separate_colour_plane_flag = bit_reader.read_bit()?;
        }
        bit_depth_luma = read_bit_depth_from_sps(&mut bit_reader)?;
        bit_depth_chroma = read_bit_depth_from_sps(&mut bit_reader)?;
        let _qpprime_y_zero_transform_bypass_flag = bit_reader.read_bit()?;
        if bit_reader.read_bit()? {
            skip_scaling_matrices(&mut bit_reader, chroma_format_idc)?;
        }
    }

    if bit_depth_luma != 8 || bit_depth_chroma != 8 {
        return Err(H264SpsError::UnsupportedBitDepth {
            bit_depth: bit_depth_luma.max(bit_depth_chroma),
        });
    }
    let bit_depth = BitDepth::Eight;

    let chroma = match chroma_format_idc {
        1 => ChromaSubsampling::Yuv420,
        unsupported_chroma => {
            return Err(H264SpsError::UnsupportedChroma {
                chroma_format_idc: unsupported_chroma,
            });
        }
    };

    let _log2_max_frame_num_minus4 = bit_reader.read_ue()?;
    let pic_order_cnt_type = bit_reader.read_ue()?;
    skip_pic_order_count_fields(&mut bit_reader, pic_order_cnt_type)?;
    let _max_num_ref_frames = bit_reader.read_ue()?;
    let _gaps_in_frame_num_value_allowed_flag = bit_reader.read_bit()?;

    let pic_width_in_mbs_minus1 = bit_reader.read_ue()?;
    let pic_height_in_map_units_minus1 = bit_reader.read_ue()?;
    let frame_mbs_only_flag = bit_reader.read_bit()?;
    if !frame_mbs_only_flag {
        let _mb_adaptive_frame_field_flag = bit_reader.read_bit()?;
    }
    let _direct_8x8_inference_flag = bit_reader.read_bit()?;

    let crop = if bit_reader.read_bit()? {
        FrameCropOffsets {
            left: bit_reader.read_ue()?,
            right: bit_reader.read_ue()?,
            top: bit_reader.read_ue()?,
            bottom: bit_reader.read_ue()?,
        }
    } else {
        FrameCropOffsets::default()
    };

    let color = if bit_reader.has_more_bits() && bit_reader.read_bit()? {
        parse_vui_color_metadata(&mut bit_reader)?
    } else {
        None
    };
    let (width, height) = sps_display_dimensions(
        pic_width_in_mbs_minus1,
        pic_height_in_map_units_minus1,
        frame_mbs_only_flag,
        chroma_format_idc,
        separate_colour_plane_flag,
        crop,
    )?;

    Ok(H264SpsMetadata {
        profile,
        level_idc,
        bit_depth,
        chroma,
        width,
        height,
        color,
    })
}

/// Requirement-level ошибка H.264 adapter-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H264RequirementError {
    /// `avcC` record не разобран.
    AvcDecoderConfigurationRecord(AvcDecoderConfigurationRecordError),

    /// Access unit framing не разобран.
    ByteStream(H264ByteStreamError),

    /// SPS отсутствует.
    MissingSequenceParameterSet,

    /// SPS найден, но не принят v1 parser-ом.
    SequenceParameterSet(H264SpsError),
}

impl fmt::Display for H264RequirementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AvcDecoderConfigurationRecord(error) => error.fmt(formatter),
            Self::ByteStream(error) => error.fmt(formatter),
            Self::MissingSequenceParameterSet => write!(formatter, "H.264 SPS is missing"),
            Self::SequenceParameterSet(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for H264RequirementError {}

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

fn validate_h264_profile(
    profile_idc: u8,
    constraint_flags: u8,
) -> Result<H264Profile, UnsupportedH264Profile> {
    match profile_idc {
        66 if constraint_flags & 0b0100_0000 != 0 => Ok(H264Profile::ConstrainedBaseline),
        66 => Err(UnsupportedH264Profile {
            reason: "Baseline without constrained-baseline flag is not enabled in v1",
        }),
        77 => Ok(H264Profile::Main),
        100 => Ok(H264Profile::High),
        110 => Err(UnsupportedH264Profile {
            reason: "High 10 profile is reserved for a future zero-copy path",
        }),
        122 => Err(UnsupportedH264Profile {
            reason: "High 4:2:2 profile is reserved for a future zero-copy path",
        }),
        244 => Err(UnsupportedH264Profile {
            reason: "High 4:4:4 profile is reserved for a future zero-copy path",
        }),
        118 => Err(UnsupportedH264Profile {
            reason: "Multiview High profile is not part of H.264 v1",
        }),
        128 => Err(UnsupportedH264Profile {
            reason: "Stereo High profile is not part of H.264 v1",
        }),
        _ => Err(UnsupportedH264Profile {
            reason: "profile is not part of H.264 v1",
        }),
    }
}

fn profile_uses_high_syntax(profile_idc: u8) -> bool {
    matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnsupportedH264Profile {
    reason: &'static str,
}

fn read_bit_depth_from_sps(bit_reader: &mut H264BitReader<'_>) -> Result<u8, H264SpsError> {
    let bit_depth_minus8 = bit_reader.read_ue()?;
    let bit_depth = bit_depth_minus8
        .checked_add(8)
        .and_then(|value| u8::try_from(value).ok())
        .ok_or(H264SpsError::UnsupportedBitDepth { bit_depth: u8::MAX })?;
    Ok(bit_depth)
}

fn skip_scaling_matrices(
    bit_reader: &mut H264BitReader<'_>,
    chroma_format_idc: u32,
) -> Result<(), H264SpsError> {
    let scaling_list_count = if chroma_format_idc != 3 { 8 } else { 12 };
    for scaling_list_index in 0..scaling_list_count {
        if bit_reader.read_bit()? {
            let scaling_list_size = if scaling_list_index < 6 { 16 } else { 64 };
            skip_scaling_list(bit_reader, scaling_list_size)?;
        }
    }

    Ok(())
}

fn skip_scaling_list(
    bit_reader: &mut H264BitReader<'_>,
    scaling_list_size: usize,
) -> Result<(), H264SpsError> {
    let mut last_scale = 8_i32;
    let mut next_scale = 8_i32;

    for _ in 0..scaling_list_size {
        if next_scale != 0 {
            let delta_scale = bit_reader.read_se()?;
            next_scale = (last_scale + delta_scale + 256).rem_euclid(256);
        }
        if next_scale != 0 {
            last_scale = next_scale;
        }
    }

    Ok(())
}

fn skip_pic_order_count_fields(
    bit_reader: &mut H264BitReader<'_>,
    pic_order_cnt_type: u32,
) -> Result<(), H264SpsError> {
    match pic_order_cnt_type {
        0 => {
            let _log2_max_pic_order_cnt_lsb_minus4 = bit_reader.read_ue()?;
        }
        1 => {
            let _delta_pic_order_always_zero_flag = bit_reader.read_bit()?;
            let _offset_for_non_ref_pic = bit_reader.read_se()?;
            let _offset_for_top_to_bottom_field = bit_reader.read_se()?;
            let offset_cycle_count = bit_reader.read_ue()?;
            for _ in 0..offset_cycle_count {
                let _offset_for_ref_frame = bit_reader.read_se()?;
            }
        }
        _ => {}
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
struct FrameCropOffsets {
    left: u32,
    right: u32,
    top: u32,
    bottom: u32,
}

fn sps_display_dimensions(
    pic_width_in_mbs_minus1: u32,
    pic_height_in_map_units_minus1: u32,
    frame_mbs_only_flag: bool,
    chroma_format_idc: u32,
    separate_colour_plane_flag: bool,
    crop: FrameCropOffsets,
) -> Result<(u32, u32), H264SpsError> {
    let width_in_mbs = pic_width_in_mbs_minus1
        .checked_add(1)
        .ok_or(H264SpsError::InvalidDimensions)?;
    let height_in_map_units = pic_height_in_map_units_minus1
        .checked_add(1)
        .ok_or(H264SpsError::InvalidDimensions)?;
    let frame_height_multiplier = if frame_mbs_only_flag { 1 } else { 2 };
    let coded_width = width_in_mbs
        .checked_mul(16)
        .ok_or(H264SpsError::InvalidDimensions)?;
    let coded_height = height_in_map_units
        .checked_mul(16)
        .and_then(|height| height.checked_mul(frame_height_multiplier))
        .ok_or(H264SpsError::InvalidDimensions)?;

    let (crop_unit_x, crop_unit_y) = crop_units(
        chroma_format_idc,
        separate_colour_plane_flag,
        frame_mbs_only_flag,
    );
    let crop_width = crop
        .left
        .checked_add(crop.right)
        .and_then(|crop_sum| crop_sum.checked_mul(crop_unit_x))
        .ok_or(H264SpsError::InvalidDimensions)?;
    let crop_height = crop
        .top
        .checked_add(crop.bottom)
        .and_then(|crop_sum| crop_sum.checked_mul(crop_unit_y))
        .ok_or(H264SpsError::InvalidDimensions)?;
    let width = coded_width
        .checked_sub(crop_width)
        .ok_or(H264SpsError::InvalidDimensions)?;
    let height = coded_height
        .checked_sub(crop_height)
        .ok_or(H264SpsError::InvalidDimensions)?;

    if width == 0 || height == 0 {
        return Err(H264SpsError::InvalidDimensions);
    }

    Ok((width, height))
}

fn crop_units(
    chroma_format_idc: u32,
    separate_colour_plane_flag: bool,
    frame_mbs_only_flag: bool,
) -> (u32, u32) {
    if chroma_format_idc == 0 || separate_colour_plane_flag {
        return (1, if frame_mbs_only_flag { 1 } else { 2 });
    }

    let frame_height_multiplier = if frame_mbs_only_flag { 1 } else { 2 };
    match chroma_format_idc {
        1 => (2, 2 * frame_height_multiplier),
        2 => (2, frame_height_multiplier),
        3 => (1, frame_height_multiplier),
        _ => (1, frame_height_multiplier),
    }
}

fn parse_vui_color_metadata(
    bit_reader: &mut H264BitReader<'_>,
) -> Result<Option<VideoColorMetadata>, H264SpsError> {
    if bit_reader.read_bit()? {
        let aspect_ratio_idc = bit_reader.read_bits(8)?;
        if aspect_ratio_idc == 255 {
            let _sar_width = bit_reader.read_bits(16)?;
            let _sar_height = bit_reader.read_bits(16)?;
        }
    }

    if bit_reader.read_bit()? {
        let _overscan_appropriate_flag = bit_reader.read_bit()?;
    }

    if !bit_reader.read_bit()? {
        return Ok(None);
    }

    let _video_format = bit_reader.read_bits(3)?;
    let full_range = bit_reader.read_bit()?;
    if !bit_reader.read_bit()? {
        return Ok(None);
    }

    let primaries = ColorPrimaries::from_h273_value(u64::from(bit_reader.read_bits(8)?));
    let transfer = TransferFunction::from_h273_value(u64::from(bit_reader.read_bits(8)?));
    let matrix = MatrixCoefficients::from_h273_value(u64::from(bit_reader.read_bits(8)?));
    let range = if full_range {
        ColorRange::Full
    } else {
        ColorRange::Limited
    };

    Ok(Some(VideoColorMetadata::bitstream(
        range, matrix, primaries, transfer,
    )))
}

fn ebsp_to_rbsp(ebsp_bytes: &[u8]) -> Vec<u8> {
    let mut rbsp_bytes = Vec::with_capacity(ebsp_bytes.len());
    let mut consecutive_zero_count = 0_u8;

    for &byte in ebsp_bytes {
        if consecutive_zero_count >= 2 && byte == 0x03 {
            consecutive_zero_count = 0;
            continue;
        }

        rbsp_bytes.push(byte);
        if byte == 0 {
            consecutive_zero_count = consecutive_zero_count.saturating_add(1);
        } else {
            consecutive_zero_count = 0;
        }
    }

    rbsp_bytes
}

struct H264BitReader<'a> {
    rbsp_bytes: &'a [u8],
    bit_offset: usize,
}

impl<'a> H264BitReader<'a> {
    fn new(rbsp_bytes: &'a [u8]) -> Self {
        Self {
            rbsp_bytes,
            bit_offset: 0,
        }
    }

    fn has_more_bits(&self) -> bool {
        self.bit_offset < self.rbsp_bytes.len() * 8
    }

    fn read_bit(&mut self) -> Result<bool, H264BitReaderError> {
        Ok(self.read_bits(1)? != 0)
    }

    fn read_u8(&mut self) -> Result<u8, H264BitReaderError> {
        Ok(self.read_bits(8)? as u8)
    }

    fn read_bits(&mut self, bit_count: u8) -> Result<u32, H264BitReaderError> {
        let remaining_bits = self.remaining_bits();
        if remaining_bits < usize::from(bit_count) {
            return Err(H264BitReaderError::UnexpectedEnd {
                requested_bits: bit_count,
                remaining_bits,
            });
        }

        let mut value = 0_u32;
        for _ in 0..bit_count {
            let byte_offset = self.bit_offset / 8;
            let bit_in_byte = 7 - (self.bit_offset % 8);
            let bit = (self.rbsp_bytes[byte_offset] >> bit_in_byte) & 1;
            value = (value << 1) | u32::from(bit);
            self.bit_offset += 1;
        }

        Ok(value)
    }

    fn read_ue(&mut self) -> Result<u32, H264BitReaderError> {
        let mut leading_zero_bits = 0_u8;
        while !self.read_bit()? {
            leading_zero_bits = leading_zero_bits
                .checked_add(1)
                .ok_or(H264BitReaderError::ExpGolombOverflow)?;
            if leading_zero_bits >= 32 {
                return Err(H264BitReaderError::ExpGolombOverflow);
            }
        }

        if leading_zero_bits == 0 {
            return Ok(0);
        }

        let suffix = self.read_bits(leading_zero_bits)?;
        Ok((1_u32 << leading_zero_bits) - 1 + suffix)
    }

    fn read_se(&mut self) -> Result<i32, H264BitReaderError> {
        let unsigned_value = self.read_ue()?;
        let signed_magnitude = unsigned_value.div_ceil(2);
        let signed_magnitude = i32::try_from(signed_magnitude)
            .map_err(|_| H264BitReaderError::SignedExpGolombOverflow)?;

        if unsigned_value % 2 == 0 {
            signed_magnitude
                .checked_neg()
                .ok_or(H264BitReaderError::SignedExpGolombOverflow)
        } else {
            Ok(signed_magnitude)
        }
    }

    fn remaining_bits(&self) -> usize {
        self.rbsp_bytes.len() * 8 - self.bit_offset
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        H264Profile, VideoCodec, VideoRequirementProbe, VideoRequirementRejection,
        probe_video_packet_keyframe, probe_video_packet_requirement_with_codec_private,
    };

    use super::{
        AVC_LENGTH_SIZE_MINUS_ONE_MASK, AVC_SPS_COUNT_MASK, H264ByteStreamError, H264NalLengthSize,
        H264Packetization, H264ParameterSetInjection, H264SpsError, h264_access_unit_to_annex_b,
        h264_access_unit_to_annex_b_into, h264_nal_units,
        h264_sps_metadata_from_avc_decoder_configuration_record,
        parse_avc_decoder_configuration_record, parse_h264_sps_metadata,
        probe_h264_packet_keyframe,
    };

    fn constrained_baseline_sps() -> Vec<u8> {
        build_sps(BuildSpsOptions {
            profile_idc: 66,
            constraint_flags: 0b1100_0000,
            level_idc: 31,
            chroma_format_idc: 1,
            bit_depth_minus8: 0,
            width: 1_280,
            height: 720,
        })
    }

    fn high_sps() -> Vec<u8> {
        build_sps(BuildSpsOptions {
            profile_idc: 100,
            constraint_flags: 0,
            level_idc: 40,
            chroma_format_idc: 1,
            bit_depth_minus8: 0,
            width: 1_920,
            height: 1_088,
        })
    }

    fn pps() -> Vec<u8> {
        vec![0x68, 0xce, 0x3c, 0x80]
    }

    fn idr_slice() -> Vec<u8> {
        vec![0x65, 0x88]
    }

    fn non_idr_slice() -> Vec<u8> {
        vec![0x41, 0x9a]
    }

    fn aud() -> Vec<u8> {
        vec![0x09, 0xf0]
    }

    fn avcc(
        nal_length_size: u8,
        sequence_parameter_set: &[u8],
        picture_parameter_set: &[u8],
    ) -> Vec<u8> {
        let mut record_bytes = vec![
            1,
            sequence_parameter_set[1],
            sequence_parameter_set[2],
            sequence_parameter_set[3],
            0b1111_1100 | (nal_length_size - 1),
            0b1110_0001,
        ];
        push_u16(&mut record_bytes, sequence_parameter_set.len());
        record_bytes.extend_from_slice(sequence_parameter_set);
        record_bytes.push(1);
        push_u16(&mut record_bytes, picture_parameter_set.len());
        record_bytes.extend_from_slice(picture_parameter_set);
        record_bytes
    }

    fn annex_b_access_unit(nal_units: &[Vec<u8>]) -> Vec<u8> {
        let mut access_unit = Vec::new();
        for nal_unit in nal_units {
            access_unit.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            access_unit.extend_from_slice(nal_unit);
        }
        access_unit
    }

    fn avcc_access_unit(nal_length_size: H264NalLengthSize, nal_units: &[Vec<u8>]) -> Vec<u8> {
        let mut access_unit = Vec::new();
        for nal_unit in nal_units {
            push_nal_length(&mut access_unit, nal_length_size, nal_unit.len());
            access_unit.extend_from_slice(nal_unit);
        }
        access_unit
    }

    #[test]
    fn avcc_parser_accepts_valid_length_sizes_and_extracts_sps_pps() {
        let sequence_parameter_set = constrained_baseline_sps();
        let picture_parameter_set = pps();

        for (size_bytes, expected_length_size) in [
            (1, H264NalLengthSize::ONE),
            (2, H264NalLengthSize::TWO),
            (4, H264NalLengthSize::FOUR),
        ] {
            let record_bytes = avcc(size_bytes, &sequence_parameter_set, &picture_parameter_set);
            let record = parse_avc_decoder_configuration_record(&record_bytes)
                .expect("валидный avcC должен разбираться");

            assert_eq!(record.nal_length_size, expected_length_size);
            assert_eq!(
                record.sequence_parameter_sets(),
                std::slice::from_ref(&sequence_parameter_set)
            );
            assert_eq!(
                record.picture_parameter_sets(),
                std::slice::from_ref(&picture_parameter_set)
            );
        }
    }

    #[test]
    fn avcc_parser_accepts_zeroed_reserved_bits_from_noncanonical_muxers() {
        let sequence_parameter_set = constrained_baseline_sps();
        let picture_parameter_set = pps();
        let mut record_bytes = avcc(4, &sequence_parameter_set, &picture_parameter_set);

        record_bytes[4] &= AVC_LENGTH_SIZE_MINUS_ONE_MASK;
        record_bytes[5] &= AVC_SPS_COUNT_MASK;
        let record = parse_avc_decoder_configuration_record(&record_bytes)
            .expect("avcC с zeroed reserved bits должен разбираться по значимым битам");

        assert_eq!(record.nal_length_size, H264NalLengthSize::FOUR);
        assert_eq!(
            record.sequence_parameter_sets(),
            std::slice::from_ref(&sequence_parameter_set)
        );
        assert_eq!(
            record.picture_parameter_sets(),
            std::slice::from_ref(&picture_parameter_set)
        );
    }

    #[test]
    fn avcc_parser_rejects_malformed_lengths_and_empty_parameter_sets() {
        let sequence_parameter_set = constrained_baseline_sps();
        let picture_parameter_set = pps();
        let unsupported_length_size_record =
            avcc(3, &sequence_parameter_set, &picture_parameter_set);
        let mut empty_sps_record = avcc(4, &sequence_parameter_set, &picture_parameter_set);
        empty_sps_record[6] = 0;
        empty_sps_record[7] = 0;
        let mut truncated_sps_record = avcc(4, &sequence_parameter_set, &picture_parameter_set);
        truncated_sps_record.truncate(8);

        assert!(parse_avc_decoder_configuration_record(&unsupported_length_size_record).is_err());
        assert!(parse_avc_decoder_configuration_record(&empty_sps_record).is_err());
        assert!(parse_avc_decoder_configuration_record(&truncated_sps_record).is_err());
    }

    #[test]
    fn annex_b_parser_accepts_three_and_four_byte_start_codes() {
        let sequence_parameter_set = constrained_baseline_sps();
        let picture_parameter_set = pps();
        let mut access_unit = Vec::new();
        access_unit.extend_from_slice(&[0x00, 0x00, 0x01]);
        access_unit.extend_from_slice(&aud());
        access_unit.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        access_unit.extend_from_slice(&sequence_parameter_set);
        access_unit.extend_from_slice(&[0x00, 0x00, 0x01]);
        access_unit.extend_from_slice(&picture_parameter_set);
        access_unit.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        access_unit.extend_from_slice(&idr_slice());

        let nal_units = h264_nal_units(&access_unit, H264Packetization::AnnexB)
            .expect("Annex B packet должен разбираться по start codes");
        let nal_types = nal_units
            .iter()
            .map(|nal_unit| nal_unit.nal_unit_type())
            .collect::<Vec<_>>();

        assert_eq!(nal_types, vec![9, 7, 8, 5]);
    }

    #[test]
    fn avcc_to_annex_b_conversion_uses_explicit_parameter_set_injection() {
        let sequence_parameter_set = constrained_baseline_sps();
        let picture_parameter_set = pps();
        let access_unit = avcc_access_unit(H264NalLengthSize::FOUR, &[idr_slice()]);
        let annex_b = h264_access_unit_to_annex_b(
            &access_unit,
            H264Packetization::AvccLengthPrefixed {
                nal_length_size: H264NalLengthSize::FOUR,
            },
            H264ParameterSetInjection::BeforeAccessUnit {
                sequence_parameter_sets: std::slice::from_ref(&sequence_parameter_set),
                picture_parameter_sets: std::slice::from_ref(&picture_parameter_set),
            },
        )
        .expect("AVCC access unit должен конвертироваться в Annex B");

        let expected =
            annex_b_access_unit(&[sequence_parameter_set, picture_parameter_set, idr_slice()]);
        assert_eq!(annex_b, expected);
    }

    #[test]
    fn avcc_to_annex_b_into_matches_legacy_wrapper() {
        let sequence_parameter_set = constrained_baseline_sps();
        let picture_parameter_set = pps();
        let access_unit = avcc_access_unit(
            H264NalLengthSize::FOUR,
            &[aud(), idr_slice(), non_idr_slice()],
        );
        let packetization = H264Packetization::AvccLengthPrefixed {
            nal_length_size: H264NalLengthSize::FOUR,
        };
        let injection = H264ParameterSetInjection::BeforeAccessUnit {
            sequence_parameter_sets: std::slice::from_ref(&sequence_parameter_set),
            picture_parameter_sets: std::slice::from_ref(&picture_parameter_set),
        };
        let legacy_annex_b = h264_access_unit_to_annex_b(&access_unit, packetization, injection)
            .expect("legacy wrapper должен сохранить прежнюю конвертацию");
        let mut caller_owned_annex_b = Vec::with_capacity(legacy_annex_b.len() + 64);

        h264_access_unit_to_annex_b_into(
            &access_unit,
            packetization,
            H264ParameterSetInjection::BeforeAccessUnit {
                sequence_parameter_sets: std::slice::from_ref(&sequence_parameter_set),
                picture_parameter_sets: std::slice::from_ref(&picture_parameter_set),
            },
            &mut caller_owned_annex_b,
        )
        .expect("_into API должен писать те же Annex B bytes");

        assert_eq!(caller_owned_annex_b, legacy_annex_b);
    }

    #[test]
    fn avcc_to_annex_b_none_injection_does_not_add_parameter_sets() {
        let access_unit = avcc_access_unit(H264NalLengthSize::FOUR, &[idr_slice()]);
        let mut annex_b = annex_b_access_unit(&[aud()]);

        h264_access_unit_to_annex_b_into(
            &access_unit,
            H264Packetization::AvccLengthPrefixed {
                nal_length_size: H264NalLengthSize::FOUR,
            },
            H264ParameterSetInjection::None,
            &mut annex_b,
        )
        .expect("None injection должен только перепаковать NAL units");

        assert_eq!(annex_b, annex_b_access_unit(&[idr_slice()]));
    }

    #[test]
    fn h264_keyframe_probe_distinguishes_idr_non_idr_and_sps_only_packets() {
        let sequence_parameter_set = constrained_baseline_sps();
        let picture_parameter_set = pps();
        let idr_access_unit = annex_b_access_unit(&[
            sequence_parameter_set.clone(),
            picture_parameter_set.clone(),
            idr_slice(),
        ]);
        let non_idr_access_unit = annex_b_access_unit(&[non_idr_slice()]);
        let parameter_sets_only =
            annex_b_access_unit(&[sequence_parameter_set, picture_parameter_set]);

        assert!(
            probe_h264_packet_keyframe(&idr_access_unit, H264Packetization::AnnexB)
                .expect("IDR access unit должен разбираться")
        );
        assert!(
            !probe_h264_packet_keyframe(&non_idr_access_unit, H264Packetization::AnnexB)
                .expect("non-IDR access unit должен разбираться")
        );
        assert!(
            !probe_h264_packet_keyframe(&parameter_sets_only, H264Packetization::AnnexB)
                .expect("SPS/PPS-only packet должен быть валидным, но не presentable keyframe")
        );
    }

    #[test]
    fn generic_h264_keyframe_probe_keeps_unknown_recoverable_without_codec_private() {
        let avcc_packet = avcc_access_unit(H264NalLengthSize::FOUR, &[idr_slice()]);
        let probe = probe_video_packet_keyframe(VideoCodec::H264, &avcc_packet);

        assert!(matches!(
            probe,
            crate::VideoPacketKeyframeProbe::Uncertain(_)
        ));
    }

    #[test]
    fn h264_requirement_probe_uses_avcc_sps_and_rejects_unsupported_variants() {
        let sequence_parameter_set = high_sps();
        let picture_parameter_set = pps();
        let record_bytes = avcc(4, &sequence_parameter_set, &picture_parameter_set);
        let avcc_packet = avcc_access_unit(H264NalLengthSize::FOUR, &[idr_slice()]);
        let probe = probe_video_packet_requirement_with_codec_private(
            VideoCodec::H264,
            &avcc_packet,
            Some(&record_bytes),
        );

        let VideoRequirementProbe::Candidate(candidate) = probe else {
            panic!("High 8-bit 4:2:0 SPS должен стать candidate");
        };
        assert_eq!(
            candidate.requirement.profile,
            Some(crate::VideoProfile::H264(H264Profile::High))
        );
        assert_eq!(candidate.requirement.width, Some(1_920));
        assert_eq!(candidate.requirement.height, Some(1_088));
        assert_eq!(
            candidate.requirement.surface_format,
            Some(crate::VideoFramePixelLayout::Nv12)
        );

        let high_10_sps = build_sps(BuildSpsOptions {
            profile_idc: 110,
            constraint_flags: 0,
            level_idc: 40,
            chroma_format_idc: 1,
            bit_depth_minus8: 2,
            width: 1_280,
            height: 720,
        });
        let high_10_record = avcc(4, &high_10_sps, &picture_parameter_set);
        let rejected_probe = probe_video_packet_requirement_with_codec_private(
            VideoCodec::H264,
            &avcc_packet,
            Some(&high_10_record),
        );

        assert!(matches!(
            rejected_probe,
            VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedProfile { .. })
        ));
    }

    #[test]
    fn sps_parser_rejects_yuv422_and_yuv444_as_typed_unsupported_chroma() {
        for chroma_format_idc in [2, 3] {
            let sequence_parameter_set = build_sps(BuildSpsOptions {
                profile_idc: 100,
                constraint_flags: 0,
                level_idc: 40,
                chroma_format_idc,
                bit_depth_minus8: 0,
                width: 1_280,
                height: 720,
            });

            assert!(matches!(
                parse_h264_sps_metadata(&sequence_parameter_set),
                Err(H264SpsError::UnsupportedChroma { .. })
            ));
        }
    }

    #[test]
    fn malformed_avcc_packet_is_recoverable_uncertain_keyframe_result() {
        let malformed_packet = vec![0x00, 0x00, 0x00, 0x10, 0x65];
        let result = probe_h264_packet_keyframe(
            &malformed_packet,
            H264Packetization::AvccLengthPrefixed {
                nal_length_size: H264NalLengthSize::FOUR,
            },
        );

        assert!(matches!(
            result,
            Err(H264ByteStreamError::TruncatedAvccNalUnit { .. })
        ));
    }

    #[test]
    fn avcc_requirement_parser_extracts_supported_sps_metadata() {
        let sequence_parameter_set = constrained_baseline_sps();
        let picture_parameter_set = pps();
        let record_bytes = avcc(4, &sequence_parameter_set, &picture_parameter_set);
        let metadata = h264_sps_metadata_from_avc_decoder_configuration_record(&record_bytes)
            .expect("Constrained Baseline avcC должен дать SPS metadata");

        assert_eq!(metadata.profile, H264Profile::ConstrainedBaseline);
        assert_eq!(metadata.width, 1_280);
        assert_eq!(metadata.height, 720);
    }

    struct BuildSpsOptions {
        profile_idc: u8,
        constraint_flags: u8,
        level_idc: u8,
        chroma_format_idc: u32,
        bit_depth_minus8: u32,
        width: u32,
        height: u32,
    }

    fn build_sps(options: BuildSpsOptions) -> Vec<u8> {
        let mut bit_writer = BitWriter::new();
        bit_writer.u(8, u32::from(options.profile_idc));
        bit_writer.u(8, u32::from(options.constraint_flags));
        bit_writer.u(8, u32::from(options.level_idc));
        bit_writer.ue(0);

        if matches!(
            options.profile_idc,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        ) {
            bit_writer.ue(options.chroma_format_idc);
            if options.chroma_format_idc == 3 {
                bit_writer.u(1, 0);
            }
            bit_writer.ue(options.bit_depth_minus8);
            bit_writer.ue(options.bit_depth_minus8);
            bit_writer.u(1, 0);
            bit_writer.u(1, 0);
        }

        bit_writer.ue(0);
        bit_writer.ue(0);
        bit_writer.ue(0);
        bit_writer.ue(1);
        bit_writer.u(1, 0);
        bit_writer.ue(options.width / 16 - 1);
        bit_writer.ue(options.height / 16 - 1);
        bit_writer.u(1, 1);
        bit_writer.u(1, 1);
        bit_writer.u(1, 0);
        bit_writer.u(1, 0);
        bit_writer.rbsp_trailing_bits();

        let mut sps = vec![0x67];
        sps.extend(bit_writer.into_bytes());
        sps
    }

    fn push_u16(output_bytes: &mut Vec<u8>, value: usize) {
        output_bytes.extend_from_slice(&(value as u16).to_be_bytes());
    }

    fn push_nal_length(
        output_bytes: &mut Vec<u8>,
        nal_length_size: H264NalLengthSize,
        nal_size: usize,
    ) {
        match nal_length_size.get() {
            1 => output_bytes.push(nal_size as u8),
            2 => output_bytes.extend_from_slice(&(nal_size as u16).to_be_bytes()),
            4 => output_bytes.extend_from_slice(&(nal_size as u32).to_be_bytes()),
            _ => unreachable!("test uses validated H264NalLengthSize"),
        }
    }

    struct BitWriter {
        output_bytes: Vec<u8>,
        current_byte: u8,
        pending_bits: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                output_bytes: Vec::new(),
                current_byte: 0,
                pending_bits: 0,
            }
        }

        fn u(&mut self, bit_count: u8, value: u32) {
            for bit_index in (0..bit_count).rev() {
                self.push_bit(((value >> bit_index) & 1) != 0);
            }
        }

        fn ue(&mut self, value: u32) {
            let code_num = value + 1;
            let bit_count = 32 - code_num.leading_zeros();
            for _ in 0..bit_count - 1 {
                self.push_bit(false);
            }
            self.u(bit_count as u8, code_num);
        }

        fn rbsp_trailing_bits(&mut self) {
            self.push_bit(true);
            while self.pending_bits != 0 {
                self.push_bit(false);
            }
        }

        fn into_bytes(self) -> Vec<u8> {
            self.output_bytes
        }

        fn push_bit(&mut self, bit: bool) {
            self.current_byte <<= 1;
            self.current_byte |= u8::from(bit);
            self.pending_bits += 1;

            if self.pending_bits == 8 {
                self.output_bytes.push(self.current_byte);
                self.current_byte = 0;
                self.pending_bits = 0;
            }
        }
    }
}
