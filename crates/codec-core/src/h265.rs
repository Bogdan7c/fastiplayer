use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    BitDepth, ChromaSubsampling, H265Profile, VideoCodec, VideoDecodeRequirement,
    VideoFramePixelLayout, VideoProfile, video_frame_pixel_layout_from_codec_fields,
};

// Requirement policy живёт отдельно, а facade сохраняет прежние публичные пути H.265 API.
mod requirements;
pub use requirements::*;

#[cfg(test)]
mod tests;

const HEVC_DECODER_CONFIGURATION_RECORD_VERSION: u8 = 1;
const HEVC_DECODER_CONFIGURATION_RECORD_MIN_SIZE: usize = 23;
const HEVC_LENGTH_SIZE_MINUS_ONE_MASK: u8 = 0b0000_0011;
const HEVC_NAL_TYPE_MASK: u8 = 0b0111_1110;
const HEVC_NAL_FORBIDDEN_ZERO_MASK: u8 = 0b1000_0000;
const HEVC_START_CODE_3: &[u8] = &[0x00, 0x00, 0x01];
const HEVC_START_CODE_4: &[u8] = &[0x00, 0x00, 0x00, 0x01];
const HEVC_NAL_TYPE_BLA_W_LP: u8 = 16;
const HEVC_NAL_TYPE_RSV_IRAP_VCL23: u8 = 23;
const HEVC_NAL_TYPE_VPS: u8 = 32;
const HEVC_NAL_TYPE_SPS: u8 = 33;
const HEVC_NAL_TYPE_PPS: u8 = 34;

/// Размер length-prefix перед каждым HEVC NAL unit в hvcC packetization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct H265NalLengthSize(u8);

impl H265NalLengthSize {
    /// Один байт длины NAL unit.
    pub const ONE: Self = Self(1);

    /// Два байта длины NAL unit.
    pub const TWO: Self = Self(2);

    /// Четыре байта длины NAL unit.
    pub const FOUR: Self = Self(4);

    /// Создаёт typed размер только для значений, безопасных для hvcC packets.
    pub fn new(size_bytes: u8) -> Result<Self, H265PacketizationError> {
        match size_bytes {
            1 => Ok(Self::ONE),
            2 => Ok(Self::TWO),
            4 => Ok(Self::FOUR),
            unsupported_size => Err(H265PacketizationError::UnsupportedNalLengthSize {
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

/// Явный способ разбиения HEVC access unit-а на NAL units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H265Packetization {
    /// Annex B byte-stream с 3- или 4-байтовыми start codes.
    AnnexB,

    /// ISO BMFF/hvcC length-prefixed packetization.
    HvccLengthPrefixed {
        /// Размер big-endian length field перед каждым NAL unit.
        nal_length_size: H265NalLengthSize,
    },
}

impl H265Packetization {
    /// Строит packetization из уже проверенного `hvcC`.
    #[must_use]
    pub const fn from_hevc_decoder_configuration_record(
        record: &HevcDecoderConfigurationRecord,
    ) -> Self {
        Self::HvccLengthPrefixed {
            nal_length_size: record.nal_length_size,
        }
    }
}

/// Profile-tier-level часть HEVC stream metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H265ProfileTierLevel {
    /// `general_profile_space`.
    pub profile_space: u8,

    /// `general_tier_flag`.
    pub tier_flag: bool,

    /// `general_profile_idc`.
    pub profile_idc: u8,

    /// Compatibility flags, где bit `n` соответствует `general_profile_compatibility_flag[n]`.
    pub profile_compatibility_flags: u32,

    /// Raw 48-bit `general_constraint_indicator_flags` из hvcC header-а.
    pub constraint_indicator_flags: [u8; 6],

    /// `general_level_idc`.
    pub level_idc: u8,
}

impl H265ProfileTierLevel {
    /// Проверяет, объявлен ли stream совместимым с указанным HEVC profile idc.
    #[must_use]
    pub fn is_profile_compatible(self, profile_idc: u8) -> bool {
        self.profile_idc == profile_idc
            || (profile_idc < 32
                && (self.profile_compatibility_flags & (1_u32 << profile_idc)) != 0)
    }
}

/// Structural `hvcC` parser result без backend-specific state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HevcDecoderConfigurationRecord {
    /// Profile-tier-level metadata из record header-а.
    pub profile_tier_level: H265ProfileTierLevel,

    /// Raw `min_spatial_segmentation_idc`.
    pub min_spatial_segmentation_idc: u16,

    /// Raw `parallelismType`.
    pub parallelism_type: u8,

    /// Raw `chromaFormat`.
    pub chroma_format_idc: u8,

    /// `bitDepthLumaMinus8 + 8`.
    pub bit_depth_luma: u8,

    /// `bitDepthChromaMinus8 + 8`.
    pub bit_depth_chroma: u8,

    /// Raw `avgFrameRate`.
    pub avg_frame_rate: u16,

    /// Raw `constantFrameRate`.
    pub constant_frame_rate: u8,

    /// Raw `numTemporalLayers`.
    pub num_temporal_layers: u8,

    /// Raw `temporalIdNested`.
    pub temporal_id_nested: bool,

    /// Размер NAL length-prefix в length-prefixed HEVC packets.
    pub nal_length_size: H265NalLengthSize,

    video_parameter_sets: Vec<Vec<u8>>,
    sequence_parameter_sets: Vec<Vec<u8>>,
    picture_parameter_sets: Vec<Vec<u8>>,
}

impl HevcDecoderConfigurationRecord {
    /// VPS NAL units без length-prefix и без Annex B start code.
    #[must_use]
    pub fn video_parameter_sets(&self) -> &[Vec<u8>] {
        &self.video_parameter_sets
    }

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
    pub const fn packetization(&self) -> H265Packetization {
        H265Packetization::from_hevc_decoder_configuration_record(self)
    }
}

/// Ошибка packetization model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H265PacketizationError {
    /// HEVCDecoderConfigurationRecord разрешает только 1/2/4 байта length-prefix.
    UnsupportedNalLengthSize {
        /// Неподдержанный размер length-prefix.
        nal_length_size: u8,
    },

    /// Нельзя доказать packetization из codec-private и packet bytes.
    UnknownPacketization,
}

impl fmt::Display for H265PacketizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedNalLengthSize { nal_length_size } => write!(
                formatter,
                "unsupported H.265 NAL length size {nal_length_size}; expected 1, 2 or 4"
            ),
            Self::UnknownPacketization => {
                write!(formatter, "unable to determine H.265 packetization")
            }
        }
    }
}

impl std::error::Error for H265PacketizationError {}

/// Ошибка `hvcC`/HEVCDecoderConfigurationRecord parser-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HevcDecoderConfigurationRecordError {
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

    /// Length-prefix имеет запрещённый размер.
    UnsupportedNalLengthSize {
        /// Неподдержанный размер length-prefix.
        nal_length_size: u8,
    },

    /// Record закончился до array header-а.
    TruncatedArrayHeader {
        /// Индекс array внутри `numOfArrays`.
        array_index: usize,

        /// Сколько байтов осталось.
        remaining_bytes: usize,
    },

    /// Record закончился до `numNalus`.
    TruncatedNalCount {
        /// Индекс array внутри `numOfArrays`.
        array_index: usize,

        /// Тип parameter set/NAL array.
        kind: H265ParameterSetKind,

        /// Сколько байтов осталось.
        remaining_bytes: usize,
    },

    /// NAL unit объявлен с нулевой длиной.
    EmptyNalUnit {
        /// Тип parameter set/NAL array.
        kind: H265ParameterSetKind,

        /// Индекс array внутри `numOfArrays`.
        array_index: usize,

        /// Индекс NAL unit-а внутри array.
        nal_index: usize,
    },

    /// Record закончился до поля длины NAL unit-а.
    TruncatedNalLength {
        /// Тип parameter set/NAL array.
        kind: H265ParameterSetKind,

        /// Индекс array внутри `numOfArrays`.
        array_index: usize,

        /// Индекс NAL unit-а внутри array.
        nal_index: usize,

        /// Сколько байтов осталось.
        remaining_bytes: usize,
    },

    /// Record закончился до полного NAL unit payload.
    TruncatedNalUnit {
        /// Тип parameter set/NAL array.
        kind: H265ParameterSetKind,

        /// Индекс array внутри `numOfArrays`.
        array_index: usize,

        /// Индекс NAL unit-а внутри array.
        nal_index: usize,

        /// Объявленная длина NAL unit-а.
        declared_size: usize,

        /// Сколько байтов осталось.
        remaining_bytes: usize,
    },
}

impl fmt::Display for HevcDecoderConfigurationRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort {
                actual_size,
                minimum_size,
            } => write!(
                formatter,
                "hvcC record too short: {actual_size} bytes, expected at least {minimum_size}"
            ),
            Self::UnsupportedVersion { version } => {
                write!(formatter, "unsupported hvcC version {version}")
            }
            Self::UnsupportedNalLengthSize { nal_length_size } => write!(
                formatter,
                "unsupported hvcC NAL length size {nal_length_size}; expected 1, 2 or 4"
            ),
            Self::TruncatedArrayHeader {
                array_index,
                remaining_bytes,
            } => write!(
                formatter,
                "truncated hvcC array header at index {array_index}: {remaining_bytes} bytes remaining"
            ),
            Self::TruncatedNalCount {
                array_index,
                kind,
                remaining_bytes,
            } => write!(
                formatter,
                "truncated hvcC {kind} NAL count in array {array_index}: {remaining_bytes} bytes remaining"
            ),
            Self::EmptyNalUnit {
                kind,
                array_index,
                nal_index,
            } => write!(
                formatter,
                "empty {kind} NAL unit at array {array_index}, index {nal_index}"
            ),
            Self::TruncatedNalLength {
                kind,
                array_index,
                nal_index,
                remaining_bytes,
            } => write!(
                formatter,
                "truncated {kind} NAL length at array {array_index}, index {nal_index}: {remaining_bytes} bytes remaining"
            ),
            Self::TruncatedNalUnit {
                kind,
                array_index,
                nal_index,
                declared_size,
                remaining_bytes,
            } => write!(
                formatter,
                "truncated {kind} NAL unit at array {array_index}, index {nal_index}: declared {declared_size} bytes, {remaining_bytes} bytes remaining"
            ),
        }
    }
}

impl std::error::Error for HevcDecoderConfigurationRecordError {}

/// Вид HEVC parameter set-а для typed diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H265ParameterSetKind {
    /// Video Parameter Set.
    Video,

    /// Sequence Parameter Set.
    Sequence,

    /// Picture Parameter Set.
    Picture,

    /// Другой NAL array, который parser валидирует, но не хранит для injection.
    Other {
        /// Raw HEVC `nal_unit_type`.
        nal_unit_type: u8,
    },
}

impl H265ParameterSetKind {
    fn from_nal_unit_type(nal_unit_type: u8) -> Self {
        match nal_unit_type {
            HEVC_NAL_TYPE_VPS => Self::Video,
            HEVC_NAL_TYPE_SPS => Self::Sequence,
            HEVC_NAL_TYPE_PPS => Self::Picture,
            other_type => Self::Other {
                nal_unit_type: other_type,
            },
        }
    }
}

impl fmt::Display for H265ParameterSetKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Video => formatter.write_str("VPS"),
            Self::Sequence => formatter.write_str("SPS"),
            Self::Picture => formatter.write_str("PPS"),
            Self::Other { nal_unit_type } => write!(formatter, "NAL type {nal_unit_type}"),
        }
    }
}

/// Один HEVC NAL unit без Annex B start code и без hvcC length-prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H265NalUnit<'a> {
    header: H265NalHeader,
    bytes: &'a [u8],
}

impl<'a> H265NalUnit<'a> {
    /// Возвращает raw NAL bytes вместе с двухбайтовым HEVC header-ом.
    #[must_use]
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    /// Возвращает `nal_unit_type`.
    #[must_use]
    pub const fn nal_unit_type(self) -> u8 {
        self.header.nal_unit_type
    }

    /// Возвращает `nuh_layer_id`.
    #[must_use]
    pub const fn nuh_layer_id(self) -> u8 {
        self.header.nuh_layer_id
    }

    /// Возвращает `nuh_temporal_id_plus1`.
    #[must_use]
    pub const fn temporal_id_plus1(self) -> u8 {
        self.header.temporal_id_plus1
    }
}

/// Parsed HEVC NAL header fields, нужные codec-neutral adapter-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct H265NalHeader {
    nal_unit_type: u8,
    nuh_layer_id: u8,
    temporal_id_plus1: u8,
}

/// Ошибка packet/access-unit framing parser-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H265ByteStreamError {
    /// Packet пустой или не содержит NAL units.
    NoNalUnits,

    /// Annex B packet не содержит start code.
    MissingStartCode,

    /// NAL unit короче двухбайтового HEVC header-а.
    TruncatedNalHeader {
        /// Сколько байтов было в NAL unit-е.
        nal_unit_size: usize,
    },

    /// Forbidden zero bit в NAL header-е установлен.
    InvalidNalHeader {
        /// Первый byte HEVC NAL header-а.
        first_header_byte: u8,

        /// Второй byte HEVC NAL header-а, если он был доступен.
        second_header_byte: Option<u8>,
    },

    /// `nuh_temporal_id_plus1` равен нулю.
    InvalidTemporalId,

    /// hvcC length-prefix оборван.
    TruncatedHvccNalLength {
        /// Размер length-prefix для этого packet-а.
        nal_length_size: H265NalLengthSize,

        /// Сколько байтов осталось в packet-е.
        remaining_bytes: usize,
    },

    /// hvcC NAL unit объявил нулевую длину.
    ZeroLengthHvccNalUnit,

    /// hvcC NAL unit выходит за границы packet-а.
    TruncatedHvccNalUnit {
        /// Объявленная длина NAL unit-а.
        declared_size: usize,

        /// Сколько байтов осталось в packet-е.
        remaining_bytes: usize,
    },

    /// Packetization не удалось доказать.
    Packetization(H265PacketizationError),
}

impl fmt::Display for H265ByteStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoNalUnits => write!(formatter, "H.265 packet contains no NAL units"),
            Self::MissingStartCode => write!(formatter, "Annex B H.265 packet has no start code"),
            Self::TruncatedNalHeader { nal_unit_size } => write!(
                formatter,
                "truncated H.265 NAL header: {nal_unit_size} bytes available"
            ),
            Self::InvalidNalHeader {
                first_header_byte,
                second_header_byte,
            } => write!(
                formatter,
                "invalid H.265 NAL header first=0x{first_header_byte:02x}, second={second_header_byte:?}"
            ),
            Self::InvalidTemporalId => {
                write!(formatter, "invalid H.265 NAL header temporal_id_plus1=0")
            }
            Self::TruncatedHvccNalLength {
                nal_length_size,
                remaining_bytes,
            } => write!(
                formatter,
                "truncated hvcC NAL length: expected {} bytes, remaining {remaining_bytes}",
                nal_length_size.bytes()
            ),
            Self::ZeroLengthHvccNalUnit => {
                write!(
                    formatter,
                    "hvcC H.265 packet contains a zero-length NAL unit"
                )
            }
            Self::TruncatedHvccNalUnit {
                declared_size,
                remaining_bytes,
            } => write!(
                formatter,
                "truncated hvcC NAL unit: declared {declared_size} bytes, remaining {remaining_bytes}"
            ),
            Self::Packetization(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for H265ByteStreamError {}

impl From<H265PacketizationError> for H265ByteStreamError {
    fn from(error: H265PacketizationError) -> Self {
        Self::Packetization(error)
    }
}

/// Политика добавления VPS/SPS/PPS при конвертации access unit-а в Annex B.
pub enum H265ParameterSetInjection<'a> {
    /// Не добавлять parameter sets, только перепаковать NAL units.
    None,

    /// Явно поставить доступные VPS/SPS/PPS перед access unit-ом.
    ///
    /// Caller решает, что текущий AU является decode-start. Неполный `hvcC`
    /// не делает injection ошибкой: отсутствующие parameter sets должны прийти
    /// in-band, а эта политика добавляет только те NAL units, которые доказанно есть.
    BeforeAccessUnit {
        /// VPS units без start code.
        video_parameter_sets: &'a [Vec<u8>],

        /// SPS units без start code.
        sequence_parameter_sets: &'a [Vec<u8>],

        /// PPS units без start code.
        picture_parameter_sets: &'a [Vec<u8>],
    },
}

/// Результат HEVC decode-start/keyframe probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H265PacketDecodeStartProbe {
    /// Access unit содержит IRAP NAL и может стартовать decode.
    DecodeStart,

    /// Access unit разобран, но IRAP NAL в нём нет.
    NotDecodeStart,

    /// Packet malformed/incomplete или framing не доказан; caller может пробовать дальше.
    Uncertain(H265ByteStreamError),
}

impl H265PacketDecodeStartProbe {
    /// Возвращает `true` только для доказанного decode-start AU.
    #[must_use]
    pub const fn is_decode_start(&self) -> bool {
        matches!(self, Self::DecodeStart)
    }
}

/// Подтверждённые HEVC SPS metadata, достаточные для v1 capability selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct H265SpsMetadata {
    /// Поддержанный production profile.
    pub profile: H265Profile,

    /// Raw `general_level_idc`.
    pub level_idc: u8,

    /// Поддержанная bit depth.
    pub bit_depth: BitDepth,

    /// Поддержанная chroma subsampling.
    pub chroma: ChromaSubsampling,

    /// Display width после conformance window.
    pub width: u32,

    /// Display height после conformance window.
    pub height: u32,

    /// Decoded surface format для zero-copy renderer boundary.
    pub surface_format: VideoFramePixelLayout,
}

/// Header-level HEVC metadata из `hvcC`, когда SPS отсутствует и должен прийти in-band.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct H265HeaderMetadata {
    /// Поддержанный production profile.
    pub profile: H265Profile,

    /// Raw `general_level_idc`.
    pub level_idc: u8,

    /// Поддержанная bit depth.
    pub bit_depth: BitDepth,

    /// Поддержанная chroma subsampling.
    pub chroma: ChromaSubsampling,

    /// Decoded surface format для zero-copy renderer boundary.
    pub surface_format: VideoFramePixelLayout,
}

/// Ошибка SPS parser-а: malformed отдельно от unsupported stream variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H265SpsError {
    /// NAL unit короче HEVC header-а.
    TruncatedNalHeader {
        /// Сколько байтов было в NAL unit-е.
        nal_unit_size: usize,
    },

    /// Переданный NAL не является SPS.
    UnexpectedNalUnitType {
        /// Фактический NAL unit type.
        nal_unit_type: u8,
    },

    /// NAL header структурно невалиден.
    InvalidNalHeader {
        /// Первый byte HEVC NAL header-а.
        first_header_byte: u8,

        /// Второй byte HEVC NAL header-а, если он был доступен.
        second_header_byte: Option<u8>,
    },

    /// `nuh_temporal_id_plus1` равен нулю.
    InvalidTemporalId,

    /// `sps_max_sub_layers_minus1` вне HEVC диапазона.
    InvalidSubLayerCount {
        /// Raw значение из SPS.
        max_sub_layers_minus1: u8,
    },

    /// Профиль вне v1 subset.
    UnsupportedProfile {
        /// Raw `general_profile_idc`.
        profile_idc: u8,

        /// Raw compatibility flags.
        profile_compatibility_flags: u32,

        /// Typed причина отказа.
        reason: &'static str,
    },

    /// Bit depth распознана, но v1 zero-copy path её не поддерживает.
    UnsupportedBitDepth {
        /// Luma bit depth.
        bit_depth_luma: u8,

        /// Chroma bit depth.
        bit_depth_chroma: u8,
    },

    /// Chroma layout распознан, но v1 zero-copy path его не поддерживает.
    UnsupportedChroma {
        /// Raw `chroma_format_idc`.
        chroma_format_idc: u32,
    },

    /// Profile совместим, но комбинация profile/bit-depth/chroma не входит в production subset.
    UnsupportedProfileFormat {
        /// HEVC profile.
        profile: H265Profile,

        /// Bit depth из SPS.
        bit_depth: BitDepth,

        /// Chroma из SPS.
        chroma: ChromaSubsampling,
    },

    /// SPS не содержит валидные coded/display dimensions.
    InvalidDimensions,

    /// Битовый reader не смог дочитать обязательное поле.
    MalformedBitstream(H265BitReaderError),
}

impl fmt::Display for H265SpsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedNalHeader { nal_unit_size } => write!(
                formatter,
                "truncated H.265 SPS NAL header: {nal_unit_size} bytes available"
            ),
            Self::UnexpectedNalUnitType { nal_unit_type } => write!(
                formatter,
                "expected H.265 SPS NAL unit type 33, got {nal_unit_type}"
            ),
            Self::InvalidNalHeader {
                first_header_byte,
                second_header_byte,
            } => write!(
                formatter,
                "invalid H.265 SPS NAL header first=0x{first_header_byte:02x}, second={second_header_byte:?}"
            ),
            Self::InvalidTemporalId => {
                write!(formatter, "invalid H.265 SPS temporal_id_plus1=0")
            }
            Self::InvalidSubLayerCount {
                max_sub_layers_minus1,
            } => write!(
                formatter,
                "invalid H.265 SPS max_sub_layers_minus1 {max_sub_layers_minus1}"
            ),
            Self::UnsupportedProfile {
                profile_idc,
                profile_compatibility_flags,
                reason,
            } => write!(
                formatter,
                "unsupported H.265 profile_idc={profile_idc}, compatibility=0x{profile_compatibility_flags:08x}: {reason}"
            ),
            Self::UnsupportedBitDepth {
                bit_depth_luma,
                bit_depth_chroma,
            } => write!(
                formatter,
                "unsupported H.265 bit depth luma={bit_depth_luma}, chroma={bit_depth_chroma}"
            ),
            Self::UnsupportedChroma { chroma_format_idc } => write!(
                formatter,
                "unsupported H.265 chroma_format_idc {chroma_format_idc}"
            ),
            Self::UnsupportedProfileFormat {
                profile,
                bit_depth,
                chroma,
            } => write!(
                formatter,
                "unsupported H.265 profile/format combination: {profile}, {bit_depth}, {chroma}"
            ),
            Self::InvalidDimensions => write!(formatter, "invalid H.265 SPS dimensions"),
            Self::MalformedBitstream(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for H265SpsError {}

impl From<H265BitReaderError> for H265SpsError {
    fn from(error: H265BitReaderError) -> Self {
        Self::MalformedBitstream(error)
    }
}

/// Ошибка bit-level SPS reader-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H265BitReaderError {
    /// Reader дошёл до конца RBSP до обязательного поля.
    UnexpectedEnd {
        /// Сколько бит запросили.
        requested_bits: usize,

        /// Сколько бит было доступно.
        remaining_bits: usize,
    },

    /// Exp-Golomb prefix слишком длинный для текущего typed parser-а.
    ExpGolombOverflow,
}

impl fmt::Display for H265BitReaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd {
                requested_bits,
                remaining_bits,
            } => write!(
                formatter,
                "unexpected end of H.265 RBSP: requested {requested_bits} bits, remaining {remaining_bits}"
            ),
            Self::ExpGolombOverflow => write!(formatter, "H.265 Exp-Golomb value overflow"),
        }
    }
}

impl std::error::Error for H265BitReaderError {}

/// Requirement-level ошибка HEVC adapter-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H265RequirementError {
    /// `hvcC` record не разобран.
    HevcDecoderConfigurationRecord(HevcDecoderConfigurationRecordError),

    /// Access unit framing не разобран.
    ByteStream(H265ByteStreamError),

    /// SPS отсутствует.
    MissingSequenceParameterSet,

    /// SPS найден, но не принят v1 parser-ом.
    SequenceParameterSet(H265SpsError),

    /// Профиль вне v1 subset.
    UnsupportedProfile {
        /// Raw `general_profile_idc`.
        profile_idc: u8,

        /// Raw compatibility flags.
        profile_compatibility_flags: u32,

        /// Typed причина отказа.
        reason: &'static str,
    },

    /// Bit depth распознана, но v1 zero-copy path её не поддерживает.
    UnsupportedBitDepth {
        /// Luma bit depth.
        bit_depth_luma: u8,

        /// Chroma bit depth.
        bit_depth_chroma: u8,
    },

    /// Chroma layout распознан, но v1 zero-copy path его не поддерживает.
    UnsupportedChroma {
        /// Raw `chroma_format_idc`.
        chroma_format_idc: u32,
    },

    /// Profile совместим, но комбинация profile/bit-depth/chroma не входит в production subset.
    UnsupportedProfileFormat {
        /// HEVC profile.
        profile: H265Profile,

        /// Bit depth из stream metadata.
        bit_depth: BitDepth,

        /// Chroma из stream metadata.
        chroma: ChromaSubsampling,
    },
}

impl fmt::Display for H265RequirementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HevcDecoderConfigurationRecord(error) => error.fmt(formatter),
            Self::ByteStream(error) => error.fmt(formatter),
            Self::MissingSequenceParameterSet => write!(formatter, "H.265 SPS is missing"),
            Self::SequenceParameterSet(error) => error.fmt(formatter),
            Self::UnsupportedProfile {
                profile_idc,
                profile_compatibility_flags,
                reason,
            } => write!(
                formatter,
                "unsupported H.265 profile_idc={profile_idc}, compatibility=0x{profile_compatibility_flags:08x}: {reason}"
            ),
            Self::UnsupportedBitDepth {
                bit_depth_luma,
                bit_depth_chroma,
            } => write!(
                formatter,
                "unsupported H.265 bit depth luma={bit_depth_luma}, chroma={bit_depth_chroma}"
            ),
            Self::UnsupportedChroma { chroma_format_idc } => write!(
                formatter,
                "unsupported H.265 chroma_format_idc {chroma_format_idc}"
            ),
            Self::UnsupportedProfileFormat {
                profile,
                bit_depth,
                chroma,
            } => write!(
                formatter,
                "unsupported H.265 profile/format combination: {profile}, {bit_depth}, {chroma}"
            ),
        }
    }
}

impl std::error::Error for H265RequirementError {}

/// Парсит `hvcC`/HEVCDecoderConfigurationRecord и извлекает VPS/SPS/PPS arrays.
pub fn parse_hevc_decoder_configuration_record(
    record_bytes: &[u8],
) -> Result<HevcDecoderConfigurationRecord, HevcDecoderConfigurationRecordError> {
    if record_bytes.len() < HEVC_DECODER_CONFIGURATION_RECORD_MIN_SIZE {
        return Err(HevcDecoderConfigurationRecordError::TooShort {
            actual_size: record_bytes.len(),
            minimum_size: HEVC_DECODER_CONFIGURATION_RECORD_MIN_SIZE,
        });
    }

    let version = record_bytes[0];
    if version != HEVC_DECODER_CONFIGURATION_RECORD_VERSION {
        return Err(HevcDecoderConfigurationRecordError::UnsupportedVersion { version });
    }

    let profile_tier_level = parse_hvcc_profile_tier_level(record_bytes);
    let min_spatial_segmentation_idc =
        u16::from_be_bytes([record_bytes[13], record_bytes[14]]) & 0x0fff;
    let parallelism_type = record_bytes[15] & 0b0000_0011;
    let chroma_format_idc = record_bytes[16] & 0b0000_0011;
    let bit_depth_luma = (record_bytes[17] & 0b0000_0111) + 8;
    let bit_depth_chroma = (record_bytes[18] & 0b0000_0111) + 8;
    let avg_frame_rate = u16::from_be_bytes([record_bytes[19], record_bytes[20]]);
    let packed_temporal_byte = record_bytes[21];
    let constant_frame_rate = (packed_temporal_byte >> 6) & 0b0000_0011;
    let num_temporal_layers = (packed_temporal_byte >> 3) & 0b0000_0111;
    let temporal_id_nested = packed_temporal_byte & 0b0000_0100 != 0;
    let nal_length_size_minus_one = packed_temporal_byte & HEVC_LENGTH_SIZE_MINUS_ONE_MASK;
    let nal_length_size =
        H265NalLengthSize::new(nal_length_size_minus_one + 1).map_err(|error| {
            let H265PacketizationError::UnsupportedNalLengthSize { nal_length_size } = error else {
                unreachable!("H265NalLengthSize::new возвращает только UnsupportedNalLengthSize");
            };
            HevcDecoderConfigurationRecordError::UnsupportedNalLengthSize { nal_length_size }
        })?;

    let array_count = usize::from(record_bytes[22]);
    let mut cursor = HEVC_DECODER_CONFIGURATION_RECORD_MIN_SIZE;
    let mut video_parameter_sets = Vec::new();
    let mut sequence_parameter_sets = Vec::new();
    let mut picture_parameter_sets = Vec::new();

    for array_index in 0..array_count {
        read_hvcc_array(
            record_bytes,
            &mut cursor,
            array_index,
            &mut video_parameter_sets,
            &mut sequence_parameter_sets,
            &mut picture_parameter_sets,
        )?;
    }

    Ok(HevcDecoderConfigurationRecord {
        profile_tier_level,
        min_spatial_segmentation_idc,
        parallelism_type,
        chroma_format_idc,
        bit_depth_luma,
        bit_depth_chroma,
        avg_frame_rate,
        constant_frame_rate,
        num_temporal_layers,
        temporal_id_nested,
        nal_length_size,
        video_parameter_sets,
        sequence_parameter_sets,
        picture_parameter_sets,
    })
}

/// Выводит HEVC packetization из codec private и/или packet bytes.
pub fn infer_h265_packetization(
    codec_private: Option<&[u8]>,
    packet_bytes: &[u8],
) -> Result<H265Packetization, H265ByteStreamError> {
    if let Some(codec_private) = codec_private.filter(|bytes| !bytes.is_empty())
        && let Ok(record) = parse_hevc_decoder_configuration_record(codec_private)
    {
        return Ok(record.packetization());
    }

    if contains_annex_b_start_code(packet_bytes) {
        return Ok(H265Packetization::AnnexB);
    }

    Err(H265PacketizationError::UnknownPacketization.into())
}

/// Возвращает NAL units access unit-а через явно указанную packetization model.
pub fn h265_nal_units<'a>(
    packet_bytes: &'a [u8],
    packetization: H265Packetization,
) -> Result<Vec<H265NalUnit<'a>>, H265ByteStreamError> {
    match packetization {
        H265Packetization::AnnexB => annex_b_nal_units(packet_bytes),
        H265Packetization::HvccLengthPrefixed { nal_length_size } => {
            hvcc_nal_units(packet_bytes, nal_length_size)
        }
    }
}

/// Конвертирует HEVC access unit в Annex B и применяет caller-owned VPS/SPS/PPS injection policy.
pub fn h265_access_unit_to_annex_b(
    packet_bytes: &[u8],
    packetization: H265Packetization,
    parameter_set_injection: H265ParameterSetInjection<'_>,
) -> Result<Vec<u8>, H265ByteStreamError> {
    let mut annex_b_bytes = Vec::new();
    h265_access_unit_to_annex_b_into(
        packet_bytes,
        packetization,
        parameter_set_injection,
        &mut annex_b_bytes,
    )?;
    Ok(annex_b_bytes)
}

/// Конвертирует HEVC access unit в caller-owned Annex B buffer без новой output allocation.
///
/// Функция очищает `output`, сохраняет его capacity и оставляет его пустым при ошибке.
pub fn h265_access_unit_to_annex_b_into(
    packet_bytes: &[u8],
    packetization: H265Packetization,
    parameter_set_injection: H265ParameterSetInjection<'_>,
    output: &mut Vec<u8>,
) -> Result<(), H265ByteStreamError> {
    output.clear();

    let write_result = {
        if let H265ParameterSetInjection::BeforeAccessUnit {
            video_parameter_sets,
            sequence_parameter_sets,
            picture_parameter_sets,
        } = parameter_set_injection
        {
            append_parameter_sets(output, video_parameter_sets);
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

/// Возвращает decode-start classification: любой IRAP NAL type 16..=23 считается стартовым.
pub fn probe_h265_packet_decode_start(
    packet_bytes: &[u8],
    packetization: H265Packetization,
) -> H265PacketDecodeStartProbe {
    let nal_units = match h265_nal_units(packet_bytes, packetization) {
        Ok(nal_units) => nal_units,
        Err(error) => return H265PacketDecodeStartProbe::Uncertain(error),
    };

    for nal_unit in nal_units {
        if is_irap_nal_type(nal_unit.nal_unit_type()) {
            return H265PacketDecodeStartProbe::DecodeStart;
        }
    }

    H265PacketDecodeStartProbe::NotDecodeStart
}

fn parse_hvcc_profile_tier_level(record_bytes: &[u8]) -> H265ProfileTierLevel {
    let profile_space_tier_idc = record_bytes[1];
    let profile_space = profile_space_tier_idc >> 6;
    let tier_flag = profile_space_tier_idc & 0b0010_0000 != 0;
    let profile_idc = profile_space_tier_idc & 0b0001_1111;
    let profile_compatibility_flags = parse_profile_compatibility_flags(&record_bytes[2..6]);
    let constraint_indicator_flags = [
        record_bytes[6],
        record_bytes[7],
        record_bytes[8],
        record_bytes[9],
        record_bytes[10],
        record_bytes[11],
    ];
    let level_idc = record_bytes[12];

    H265ProfileTierLevel {
        profile_space,
        tier_flag,
        profile_idc,
        profile_compatibility_flags,
        constraint_indicator_flags,
        level_idc,
    }
}

fn parse_profile_compatibility_flags(flag_bytes: &[u8]) -> u32 {
    let mut flags = 0_u32;
    for profile_index in 0..32 {
        let byte = flag_bytes[profile_index / 8];
        let mask = 1 << (7 - (profile_index % 8));
        if byte & mask != 0 {
            flags |= 1_u32 << profile_index;
        }
    }
    flags
}

fn read_hvcc_array(
    record_bytes: &[u8],
    cursor: &mut usize,
    array_index: usize,
    video_parameter_sets: &mut Vec<Vec<u8>>,
    sequence_parameter_sets: &mut Vec<Vec<u8>>,
    picture_parameter_sets: &mut Vec<Vec<u8>>,
) -> Result<(), HevcDecoderConfigurationRecordError> {
    let remaining_bytes = record_bytes.len().saturating_sub(*cursor);
    if remaining_bytes < 1 {
        return Err(HevcDecoderConfigurationRecordError::TruncatedArrayHeader {
            array_index,
            remaining_bytes,
        });
    }

    let array_header = record_bytes[*cursor];
    *cursor += 1;
    let kind = H265ParameterSetKind::from_nal_unit_type(array_header & 0b0011_1111);
    let remaining_bytes = record_bytes.len().saturating_sub(*cursor);
    if remaining_bytes < 2 {
        return Err(HevcDecoderConfigurationRecordError::TruncatedNalCount {
            array_index,
            kind,
            remaining_bytes,
        });
    }

    let nal_unit_count =
        u16::from_be_bytes([record_bytes[*cursor], record_bytes[*cursor + 1]]) as usize;
    *cursor += 2;
    for nal_index in 0..nal_unit_count {
        let nal_unit =
            read_hvcc_array_nal_unit(record_bytes, cursor, kind, array_index, nal_index)?;
        match kind {
            H265ParameterSetKind::Video => video_parameter_sets.push(nal_unit),
            H265ParameterSetKind::Sequence => sequence_parameter_sets.push(nal_unit),
            H265ParameterSetKind::Picture => picture_parameter_sets.push(nal_unit),
            H265ParameterSetKind::Other { .. } => {}
        }
    }

    Ok(())
}

fn read_hvcc_array_nal_unit(
    record_bytes: &[u8],
    cursor: &mut usize,
    kind: H265ParameterSetKind,
    array_index: usize,
    nal_index: usize,
) -> Result<Vec<u8>, HevcDecoderConfigurationRecordError> {
    let remaining_bytes = record_bytes.len().saturating_sub(*cursor);
    if remaining_bytes < 2 {
        return Err(HevcDecoderConfigurationRecordError::TruncatedNalLength {
            kind,
            array_index,
            nal_index,
            remaining_bytes,
        });
    }

    let declared_size =
        u16::from_be_bytes([record_bytes[*cursor], record_bytes[*cursor + 1]]) as usize;
    *cursor += 2;
    if declared_size == 0 {
        return Err(HevcDecoderConfigurationRecordError::EmptyNalUnit {
            kind,
            array_index,
            nal_index,
        });
    }

    let remaining_bytes = record_bytes.len().saturating_sub(*cursor);
    if remaining_bytes < declared_size {
        return Err(HevcDecoderConfigurationRecordError::TruncatedNalUnit {
            kind,
            array_index,
            nal_index,
            declared_size,
            remaining_bytes,
        });
    }

    let nal_unit = record_bytes[*cursor..*cursor + declared_size].to_vec();
    *cursor += declared_size;
    Ok(nal_unit)
}

fn annex_b_nal_units(packet_bytes: &[u8]) -> Result<Vec<H265NalUnit<'_>>, H265ByteStreamError> {
    let Some((first_start_code_offset, first_start_code_size)) =
        find_annex_b_start_code(packet_bytes, 0)
    else {
        return Err(H265ByteStreamError::MissingStartCode);
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
        return Err(H265ByteStreamError::NoNalUnits);
    }

    Ok(nal_units)
}

fn hvcc_nal_units(
    packet_bytes: &[u8],
    nal_length_size: H265NalLengthSize,
) -> Result<Vec<H265NalUnit<'_>>, H265ByteStreamError> {
    let mut cursor = 0;
    let mut nal_units = Vec::new();

    while cursor < packet_bytes.len() {
        let remaining_bytes = packet_bytes.len() - cursor;
        if remaining_bytes < nal_length_size.bytes() {
            return Err(H265ByteStreamError::TruncatedHvccNalLength {
                nal_length_size,
                remaining_bytes,
            });
        }

        let declared_size = read_hvcc_nal_size(packet_bytes, cursor, nal_length_size);
        cursor += nal_length_size.bytes();
        if declared_size == 0 {
            return Err(H265ByteStreamError::ZeroLengthHvccNalUnit);
        }

        let remaining_bytes = packet_bytes.len() - cursor;
        if remaining_bytes < declared_size {
            return Err(H265ByteStreamError::TruncatedHvccNalUnit {
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
        return Err(H265ByteStreamError::NoNalUnits);
    }

    Ok(nal_units)
}

fn append_access_unit_nals_to_annex_b(
    packet_bytes: &[u8],
    packetization: H265Packetization,
    output: &mut Vec<u8>,
) -> Result<(), H265ByteStreamError> {
    match packetization {
        H265Packetization::AnnexB => append_annex_b_access_unit_to_annex_b(packet_bytes, output),
        H265Packetization::HvccLengthPrefixed { nal_length_size } => {
            append_hvcc_nal_units_to_annex_b(packet_bytes, nal_length_size, output)
        }
    }
}

fn append_annex_b_access_unit_to_annex_b(
    packet_bytes: &[u8],
    output: &mut Vec<u8>,
) -> Result<(), H265ByteStreamError> {
    annex_b_nal_units(packet_bytes)?;
    output.extend_from_slice(packet_bytes);
    Ok(())
}

fn append_hvcc_nal_units_to_annex_b(
    packet_bytes: &[u8],
    nal_length_size: H265NalLengthSize,
    output: &mut Vec<u8>,
) -> Result<(), H265ByteStreamError> {
    let mut cursor = 0;
    let mut nal_units_written = 0usize;

    while cursor < packet_bytes.len() {
        let remaining_bytes = packet_bytes.len() - cursor;
        if remaining_bytes < nal_length_size.bytes() {
            return Err(H265ByteStreamError::TruncatedHvccNalLength {
                nal_length_size,
                remaining_bytes,
            });
        }

        let declared_size = read_hvcc_nal_size(packet_bytes, cursor, nal_length_size);
        cursor += nal_length_size.bytes();
        if declared_size == 0 {
            return Err(H265ByteStreamError::ZeroLengthHvccNalUnit);
        }

        let remaining_bytes = packet_bytes.len() - cursor;
        if remaining_bytes < declared_size {
            return Err(H265ByteStreamError::TruncatedHvccNalUnit {
                declared_size,
                remaining_bytes,
            });
        }

        append_nal_unit_to_annex_b(&packet_bytes[cursor..cursor + declared_size], output)?;
        cursor += declared_size;
        nal_units_written += 1;
    }

    if nal_units_written == 0 {
        return Err(H265ByteStreamError::NoNalUnits);
    }

    Ok(())
}

fn append_nal_unit_to_annex_b(
    nal_unit_bytes: &[u8],
    output: &mut Vec<u8>,
) -> Result<(), H265ByteStreamError> {
    let nal_unit = parse_nal_unit(nal_unit_bytes)?;
    output.extend_from_slice(HEVC_START_CODE_4);
    output.extend_from_slice(nal_unit.bytes());
    Ok(())
}

fn read_hvcc_nal_size(
    packet_bytes: &[u8],
    cursor: usize,
    nal_length_size: H265NalLengthSize,
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
        _ => unreachable!("H265NalLengthSize содержит только 1/2/4"),
    }
}

fn parse_nal_unit(nal_unit_bytes: &[u8]) -> Result<H265NalUnit<'_>, H265ByteStreamError> {
    Ok(H265NalUnit {
        header: parse_nal_header(nal_unit_bytes)?,
        bytes: nal_unit_bytes,
    })
}

fn parse_nal_header(nal_unit_bytes: &[u8]) -> Result<H265NalHeader, H265ByteStreamError> {
    if nal_unit_bytes.len() < 2 {
        return Err(H265ByteStreamError::TruncatedNalHeader {
            nal_unit_size: nal_unit_bytes.len(),
        });
    }

    let first_header_byte = nal_unit_bytes[0];
    let second_header_byte = nal_unit_bytes[1];
    if first_header_byte & HEVC_NAL_FORBIDDEN_ZERO_MASK != 0 {
        return Err(H265ByteStreamError::InvalidNalHeader {
            first_header_byte,
            second_header_byte: Some(second_header_byte),
        });
    }

    let temporal_id_plus1 = second_header_byte & 0b0000_0111;
    if temporal_id_plus1 == 0 {
        return Err(H265ByteStreamError::InvalidTemporalId);
    }

    Ok(H265NalHeader {
        nal_unit_type: (first_header_byte & HEVC_NAL_TYPE_MASK) >> 1,
        nuh_layer_id: ((first_header_byte & 0b0000_0001) << 5) | (second_header_byte >> 3),
        temporal_id_plus1,
    })
}

fn find_annex_b_start_code(packet_bytes: &[u8], from_offset: usize) -> Option<(usize, usize)> {
    let mut offset = from_offset;
    while offset + HEVC_START_CODE_3.len() <= packet_bytes.len() {
        if packet_bytes[offset..].starts_with(HEVC_START_CODE_4) {
            return Some((offset, HEVC_START_CODE_4.len()));
        }
        if packet_bytes[offset..].starts_with(HEVC_START_CODE_3) {
            return Some((offset, HEVC_START_CODE_3.len()));
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
        output.extend_from_slice(HEVC_START_CODE_4);
        output.extend_from_slice(parameter_set);
    }
}

fn is_irap_nal_type(nal_unit_type: u8) -> bool {
    (HEVC_NAL_TYPE_BLA_W_LP..=HEVC_NAL_TYPE_RSV_IRAP_VCL23).contains(&nal_unit_type)
}
