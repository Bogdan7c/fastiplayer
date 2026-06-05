use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    BitDepth, ChromaSubsampling, H265Profile, VideoCodec, VideoDecodeRequirement, VideoProfile,
    VideoSurfaceFormat,
};

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
    pub surface_format: VideoSurfaceFormat,
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
    pub surface_format: VideoSurfaceFormat,
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

/// Парсит первый SPS из `hvcC` и возвращает v1 requirement metadata.
pub fn h265_sps_metadata_from_hevc_decoder_configuration_record(
    record_bytes: &[u8],
) -> Result<H265SpsMetadata, H265RequirementError> {
    let record = parse_hevc_decoder_configuration_record(record_bytes)
        .map_err(H265RequirementError::HevcDecoderConfigurationRecord)?;
    let sps = record
        .sequence_parameter_sets()
        .first()
        .ok_or(H265RequirementError::MissingSequenceParameterSet)?;

    parse_h265_sps_metadata(sps).map_err(H265RequirementError::SequenceParameterSet)
}

/// Ищет SPS внутри packet-а и возвращает v1 requirement metadata.
pub fn h265_sps_metadata_from_packet(
    packet_bytes: &[u8],
    packetization: H265Packetization,
) -> Result<H265SpsMetadata, H265RequirementError> {
    let nal_units =
        h265_nal_units(packet_bytes, packetization).map_err(H265RequirementError::ByteStream)?;

    for nal_unit in nal_units {
        if nal_unit.nal_unit_type() == HEVC_NAL_TYPE_SPS {
            return parse_h265_sps_metadata(nal_unit.bytes())
                .map_err(H265RequirementError::SequenceParameterSet);
        }
    }

    Err(H265RequirementError::MissingSequenceParameterSet)
}

/// Строит requirement из `hvcC`; если SPS отсутствует, использует безопасные header-level поля.
pub fn h265_decode_requirement_from_hevc_decoder_configuration_record(
    record_bytes: &[u8],
) -> Result<VideoDecodeRequirement, H265RequirementError> {
    let record = parse_hevc_decoder_configuration_record(record_bytes)
        .map_err(H265RequirementError::HevcDecoderConfigurationRecord)?;

    if let Some(sps) = record.sequence_parameter_sets().first() {
        let metadata =
            parse_h265_sps_metadata(sps).map_err(H265RequirementError::SequenceParameterSet)?;
        return Ok(video_requirement_from_h265_sps_metadata(&metadata));
    }

    let metadata = h265_header_metadata_from_hevc_decoder_configuration_record(&record)?;
    Ok(video_requirement_from_h265_header_metadata(&metadata))
}

/// Строит requirement из in-band SPS внутри access unit-а.
pub fn h265_decode_requirement_from_packet(
    packet_bytes: &[u8],
    packetization: H265Packetization,
) -> Result<VideoDecodeRequirement, H265RequirementError> {
    let metadata = h265_sps_metadata_from_packet(packet_bytes, packetization)?;
    Ok(video_requirement_from_h265_sps_metadata(&metadata))
}

/// Парсит SPS NAL unit и подтверждает v1 supported subset.
pub fn parse_h265_sps_metadata(nal_unit_bytes: &[u8]) -> Result<H265SpsMetadata, H265SpsError> {
    let nal_header = parse_nal_header(nal_unit_bytes).map_err(|error| match error {
        H265ByteStreamError::TruncatedNalHeader { nal_unit_size } => {
            H265SpsError::TruncatedNalHeader { nal_unit_size }
        }
        H265ByteStreamError::InvalidNalHeader {
            first_header_byte,
            second_header_byte,
        } => H265SpsError::InvalidNalHeader {
            first_header_byte,
            second_header_byte,
        },
        H265ByteStreamError::InvalidTemporalId => H265SpsError::InvalidTemporalId,
        other_error => unreachable!("parse_nal_header returned unexpected error: {other_error:?}"),
    })?;
    if nal_header.nal_unit_type != HEVC_NAL_TYPE_SPS {
        return Err(H265SpsError::UnexpectedNalUnitType {
            nal_unit_type: nal_header.nal_unit_type,
        });
    }

    let rbsp_bytes = ebsp_to_rbsp(&nal_unit_bytes[2..]);
    let mut bit_reader = H265BitReader::new(&rbsp_bytes);

    let _sps_video_parameter_set_id = bit_reader.read_bits(4)? as u8;
    let max_sub_layers_minus1 = bit_reader.read_bits(3)? as u8;
    if max_sub_layers_minus1 > 6 {
        return Err(H265SpsError::InvalidSubLayerCount {
            max_sub_layers_minus1,
        });
    }
    let _sps_temporal_id_nesting_flag = bit_reader.read_bit()?;
    let profile_tier_level =
        parse_profile_tier_level(&mut bit_reader, true, max_sub_layers_minus1)?;
    let _sps_seq_parameter_set_id = bit_reader.read_ue()?;
    let chroma_format_idc = bit_reader.read_ue()?;
    let separate_colour_plane_flag = if chroma_format_idc == 3 {
        bit_reader.read_bit()?
    } else {
        false
    };
    let pic_width_in_luma_samples = bit_reader.read_ue()?;
    let pic_height_in_luma_samples = bit_reader.read_ue()?;
    let conformance_window = if bit_reader.read_bit()? {
        H265ConformanceWindow {
            left: bit_reader.read_ue()?,
            right: bit_reader.read_ue()?,
            top: bit_reader.read_ue()?,
            bottom: bit_reader.read_ue()?,
        }
    } else {
        H265ConformanceWindow::default()
    };
    let bit_depth_luma = read_bit_depth_from_sps(&mut bit_reader)?;
    let bit_depth_chroma = read_bit_depth_from_sps(&mut bit_reader)?;
    let decoded_format = refine_h265_decoded_format(
        profile_tier_level,
        bit_depth_luma,
        bit_depth_chroma,
        chroma_format_idc,
    )
    .map_err(H265SpsError::from)?;
    let (width, height) = sps_display_dimensions(
        pic_width_in_luma_samples,
        pic_height_in_luma_samples,
        chroma_format_idc,
        separate_colour_plane_flag,
        conformance_window,
    )?;

    Ok(H265SpsMetadata {
        profile: decoded_format.profile,
        level_idc: profile_tier_level.level_idc,
        bit_depth: decoded_format.bit_depth,
        chroma: decoded_format.chroma,
        width,
        height,
        surface_format: decoded_format.surface_format,
    })
}

/// Строит header-level metadata из уже разобранного `hvcC`.
pub fn h265_header_metadata_from_hevc_decoder_configuration_record(
    record: &HevcDecoderConfigurationRecord,
) -> Result<H265HeaderMetadata, H265RequirementError> {
    let decoded_format = refine_h265_decoded_format(
        record.profile_tier_level,
        record.bit_depth_luma,
        record.bit_depth_chroma,
        u32::from(record.chroma_format_idc),
    )
    .map_err(H265RequirementError::from)?;

    Ok(H265HeaderMetadata {
        profile: decoded_format.profile,
        level_idc: record.profile_tier_level.level_idc,
        bit_depth: decoded_format.bit_depth,
        chroma: decoded_format.chroma,
        surface_format: decoded_format.surface_format,
    })
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

fn parse_profile_tier_level(
    bit_reader: &mut H265BitReader<'_>,
    profile_present_flag: bool,
    max_sub_layers_minus1: u8,
) -> Result<H265ProfileTierLevel, H265BitReaderError> {
    let profile_tier_level = if profile_present_flag {
        parse_profile_tier_level_profile_fields(bit_reader)?
    } else {
        H265ProfileTierLevel {
            profile_space: 0,
            tier_flag: false,
            profile_idc: 0,
            profile_compatibility_flags: 0,
            constraint_indicator_flags: [0; 6],
            level_idc: 0,
        }
    };
    let level_idc = bit_reader.read_bits(8)? as u8;
    let mut sub_layer_profile_present_flags = [false; 6];
    let mut sub_layer_level_present_flags = [false; 6];

    for sub_layer_index in 0..usize::from(max_sub_layers_minus1) {
        sub_layer_profile_present_flags[sub_layer_index] = bit_reader.read_bit()?;
        sub_layer_level_present_flags[sub_layer_index] = bit_reader.read_bit()?;
    }

    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            bit_reader.skip_bits(2)?;
        }
    }

    for sub_layer_index in 0..usize::from(max_sub_layers_minus1) {
        if sub_layer_profile_present_flags[sub_layer_index] {
            bit_reader.skip_bits(88)?;
        }
        if sub_layer_level_present_flags[sub_layer_index] {
            bit_reader.skip_bits(8)?;
        }
    }

    Ok(H265ProfileTierLevel {
        level_idc,
        ..profile_tier_level
    })
}

fn parse_profile_tier_level_profile_fields(
    bit_reader: &mut H265BitReader<'_>,
) -> Result<H265ProfileTierLevel, H265BitReaderError> {
    let profile_space = bit_reader.read_bits(2)? as u8;
    let tier_flag = bit_reader.read_bit()?;
    let profile_idc = bit_reader.read_bits(5)? as u8;
    let mut profile_compatibility_flags = 0_u32;
    for profile_index in 0..32 {
        if bit_reader.read_bit()? {
            profile_compatibility_flags |= 1_u32 << profile_index;
        }
    }
    let mut constraint_indicator_flags = [0_u8; 6];
    for constraint_byte in &mut constraint_indicator_flags {
        *constraint_byte = bit_reader.read_bits(8)? as u8;
    }

    Ok(H265ProfileTierLevel {
        profile_space,
        tier_flag,
        profile_idc,
        profile_compatibility_flags,
        constraint_indicator_flags,
        level_idc: 0,
    })
}

fn read_bit_depth_from_sps(bit_reader: &mut H265BitReader<'_>) -> Result<u8, H265SpsError> {
    let bit_depth_minus8 = bit_reader.read_ue()?;
    bit_depth_minus8
        .checked_add(8)
        .and_then(|value| u8::try_from(value).ok())
        .ok_or(H265SpsError::UnsupportedBitDepth {
            bit_depth_luma: u8::MAX,
            bit_depth_chroma: u8::MAX,
        })
}

#[derive(Debug, Clone, Copy, Default)]
struct H265ConformanceWindow {
    left: u32,
    right: u32,
    top: u32,
    bottom: u32,
}

fn sps_display_dimensions(
    pic_width_in_luma_samples: u32,
    pic_height_in_luma_samples: u32,
    chroma_format_idc: u32,
    separate_colour_plane_flag: bool,
    conformance_window: H265ConformanceWindow,
) -> Result<(u32, u32), H265SpsError> {
    if pic_width_in_luma_samples == 0 || pic_height_in_luma_samples == 0 {
        return Err(H265SpsError::InvalidDimensions);
    }

    let (sub_width_c, sub_height_c) =
        chroma_subsampling_units(chroma_format_idc, separate_colour_plane_flag);
    let crop_width = conformance_window
        .left
        .checked_add(conformance_window.right)
        .and_then(|crop_sum| crop_sum.checked_mul(sub_width_c))
        .ok_or(H265SpsError::InvalidDimensions)?;
    let crop_height = conformance_window
        .top
        .checked_add(conformance_window.bottom)
        .and_then(|crop_sum| crop_sum.checked_mul(sub_height_c))
        .ok_or(H265SpsError::InvalidDimensions)?;
    let width = pic_width_in_luma_samples
        .checked_sub(crop_width)
        .ok_or(H265SpsError::InvalidDimensions)?;
    let height = pic_height_in_luma_samples
        .checked_sub(crop_height)
        .ok_or(H265SpsError::InvalidDimensions)?;

    if width == 0 || height == 0 {
        return Err(H265SpsError::InvalidDimensions);
    }

    Ok((width, height))
}

fn chroma_subsampling_units(
    chroma_format_idc: u32,
    separate_colour_plane_flag: bool,
) -> (u32, u32) {
    if chroma_format_idc == 0 || separate_colour_plane_flag {
        return (1, 1);
    }

    match chroma_format_idc {
        1 => (2, 2),
        2 => (2, 1),
        3 => (1, 1),
        _ => (1, 1),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct H265DecodedFormat {
    profile: H265Profile,
    bit_depth: BitDepth,
    chroma: ChromaSubsampling,
    surface_format: VideoSurfaceFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum H265FormatError {
    Profile {
        profile_idc: u8,
        profile_compatibility_flags: u32,
        reason: &'static str,
    },
    BitDepth {
        bit_depth_luma: u8,
        bit_depth_chroma: u8,
    },
    Chroma {
        chroma_format_idc: u32,
    },
    ProfileFormat {
        profile: H265Profile,
        bit_depth: BitDepth,
        chroma: ChromaSubsampling,
    },
}

impl From<H265FormatError> for H265SpsError {
    fn from(error: H265FormatError) -> Self {
        match error {
            H265FormatError::Profile {
                profile_idc,
                profile_compatibility_flags,
                reason,
            } => Self::UnsupportedProfile {
                profile_idc,
                profile_compatibility_flags,
                reason,
            },
            H265FormatError::BitDepth {
                bit_depth_luma,
                bit_depth_chroma,
            } => Self::UnsupportedBitDepth {
                bit_depth_luma,
                bit_depth_chroma,
            },
            H265FormatError::Chroma { chroma_format_idc } => {
                Self::UnsupportedChroma { chroma_format_idc }
            }
            H265FormatError::ProfileFormat {
                profile,
                bit_depth,
                chroma,
            } => Self::UnsupportedProfileFormat {
                profile,
                bit_depth,
                chroma,
            },
        }
    }
}

impl From<H265FormatError> for H265RequirementError {
    fn from(error: H265FormatError) -> Self {
        match error {
            H265FormatError::Profile {
                profile_idc,
                profile_compatibility_flags,
                reason,
            } => Self::UnsupportedProfile {
                profile_idc,
                profile_compatibility_flags,
                reason,
            },
            H265FormatError::BitDepth {
                bit_depth_luma,
                bit_depth_chroma,
            } => Self::UnsupportedBitDepth {
                bit_depth_luma,
                bit_depth_chroma,
            },
            H265FormatError::Chroma { chroma_format_idc } => {
                Self::UnsupportedChroma { chroma_format_idc }
            }
            H265FormatError::ProfileFormat {
                profile,
                bit_depth,
                chroma,
            } => Self::UnsupportedProfileFormat {
                profile,
                bit_depth,
                chroma,
            },
        }
    }
}

fn refine_h265_decoded_format(
    profile_tier_level: H265ProfileTierLevel,
    bit_depth_luma: u8,
    bit_depth_chroma: u8,
    chroma_format_idc: u32,
) -> Result<H265DecodedFormat, H265FormatError> {
    let bit_depth = normalize_h265_bit_depth(bit_depth_luma, bit_depth_chroma)?;
    let chroma = normalize_h265_chroma(chroma_format_idc)?;

    if profile_tier_level.is_profile_compatible(9) {
        return Err(H265FormatError::Profile {
            profile_idc: profile_tier_level.profile_idc,
            profile_compatibility_flags: profile_tier_level.profile_compatibility_flags,
            reason: "HEVC Screen Content Coding profiles are reserved for a future zero-copy path",
        });
    }

    let profile = if bit_depth == BitDepth::Eight && profile_tier_level.is_profile_compatible(1) {
        H265Profile::Main
    } else if bit_depth == BitDepth::Ten && profile_tier_level.is_profile_compatible(2) {
        H265Profile::Main10
    } else if profile_tier_level.is_profile_compatible(1) {
        return Err(H265FormatError::ProfileFormat {
            profile: H265Profile::Main,
            bit_depth,
            chroma,
        });
    } else if profile_tier_level.is_profile_compatible(2) {
        return Err(H265FormatError::ProfileFormat {
            profile: H265Profile::Main10,
            bit_depth,
            chroma,
        });
    } else {
        return Err(H265FormatError::Profile {
            profile_idc: profile_tier_level.profile_idc,
            profile_compatibility_flags: profile_tier_level.profile_compatibility_flags,
            reason: "profile is not part of the H.265 v1 zero-copy subset",
        });
    };

    let surface_format = VideoSurfaceFormat::from_bit_depth_and_chroma(bit_depth, chroma).ok_or(
        H265FormatError::ProfileFormat {
            profile,
            bit_depth,
            chroma,
        },
    )?;

    Ok(H265DecodedFormat {
        profile,
        bit_depth,
        chroma,
        surface_format,
    })
}

fn normalize_h265_bit_depth(
    bit_depth_luma: u8,
    bit_depth_chroma: u8,
) -> Result<BitDepth, H265FormatError> {
    if bit_depth_luma != bit_depth_chroma {
        return Err(H265FormatError::BitDepth {
            bit_depth_luma,
            bit_depth_chroma,
        });
    }

    match BitDepth::from_bits(bit_depth_luma) {
        Some(BitDepth::Eight) => Ok(BitDepth::Eight),
        Some(BitDepth::Ten) => Ok(BitDepth::Ten),
        Some(BitDepth::Twelve) | None => Err(H265FormatError::BitDepth {
            bit_depth_luma,
            bit_depth_chroma,
        }),
    }
}

fn normalize_h265_chroma(chroma_format_idc: u32) -> Result<ChromaSubsampling, H265FormatError> {
    match chroma_format_idc {
        1 => Ok(ChromaSubsampling::Yuv420),
        0 | 2 | 3 => Err(H265FormatError::Chroma { chroma_format_idc }),
        _ => Err(H265FormatError::Chroma { chroma_format_idc }),
    }
}

fn video_requirement_from_h265_sps_metadata(metadata: &H265SpsMetadata) -> VideoDecodeRequirement {
    VideoDecodeRequirement::new(VideoCodec::H265)
        .with_profile(VideoProfile::H265(metadata.profile))
        .with_bit_depth(metadata.bit_depth)
        .with_chroma(metadata.chroma)
        .with_resolution(metadata.width, metadata.height)
        .with_surface_format(metadata.surface_format)
}

fn video_requirement_from_h265_header_metadata(
    metadata: &H265HeaderMetadata,
) -> VideoDecodeRequirement {
    VideoDecodeRequirement::new(VideoCodec::H265)
        .with_profile(VideoProfile::H265(metadata.profile))
        .with_bit_depth(metadata.bit_depth)
        .with_chroma(metadata.chroma)
        .with_surface_format(metadata.surface_format)
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

struct H265BitReader<'a> {
    rbsp_bytes: &'a [u8],
    bit_offset: usize,
}

impl<'a> H265BitReader<'a> {
    fn new(rbsp_bytes: &'a [u8]) -> Self {
        Self {
            rbsp_bytes,
            bit_offset: 0,
        }
    }

    fn read_bit(&mut self) -> Result<bool, H265BitReaderError> {
        Ok(self.read_bits(1)? != 0)
    }

    fn read_bits(&mut self, bit_count: usize) -> Result<u32, H265BitReaderError> {
        let remaining_bits = self.remaining_bits();
        if remaining_bits < bit_count {
            return Err(H265BitReaderError::UnexpectedEnd {
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

    fn skip_bits(&mut self, bit_count: usize) -> Result<(), H265BitReaderError> {
        let remaining_bits = self.remaining_bits();
        if remaining_bits < bit_count {
            return Err(H265BitReaderError::UnexpectedEnd {
                requested_bits: bit_count,
                remaining_bits,
            });
        }
        self.bit_offset += bit_count;
        Ok(())
    }

    fn read_ue(&mut self) -> Result<u32, H265BitReaderError> {
        let mut leading_zero_bits = 0_u8;
        while !self.read_bit()? {
            leading_zero_bits = leading_zero_bits
                .checked_add(1)
                .ok_or(H265BitReaderError::ExpGolombOverflow)?;
            if leading_zero_bits >= 32 {
                return Err(H265BitReaderError::ExpGolombOverflow);
            }
        }

        if leading_zero_bits == 0 {
            return Ok(0);
        }

        let suffix = self.read_bits(usize::from(leading_zero_bits))?;
        Ok((1_u32 << leading_zero_bits) - 1 + suffix)
    }

    fn remaining_bits(&self) -> usize {
        self.rbsp_bytes.len() * 8 - self.bit_offset
    }
}

#[cfg(test)]
mod tests {
    use super::{
        H265ByteStreamError, H265NalLengthSize, H265PacketDecodeStartProbe, H265Packetization,
        H265ParameterSetInjection, H265RequirementError, H265SpsError, h265_access_unit_to_annex_b,
        h265_access_unit_to_annex_b_into,
        h265_decode_requirement_from_hevc_decoder_configuration_record,
        h265_decode_requirement_from_packet,
        h265_header_metadata_from_hevc_decoder_configuration_record, h265_nal_units,
        h265_sps_metadata_from_hevc_decoder_configuration_record, infer_h265_packetization,
        parse_h265_sps_metadata, parse_hevc_decoder_configuration_record,
        probe_h265_packet_decode_start,
    };
    use crate::{BitDepth, ChromaSubsampling, H265Profile, VideoProfile, VideoSurfaceFormat};

    fn vps() -> Vec<u8> {
        nal_unit(32, &[0x01, 0x60])
    }

    fn pps() -> Vec<u8> {
        nal_unit(34, &[0xc0])
    }

    fn aud() -> Vec<u8> {
        nal_unit(35, &[0x50])
    }

    fn cra_slice() -> Vec<u8> {
        nal_unit(21, &[0x01])
    }

    fn trail_slice() -> Vec<u8> {
        nal_unit(1, &[0x01])
    }

    fn main_sps() -> Vec<u8> {
        build_sps(BuildSpsOptions {
            profile_idc: 1,
            chroma_format_idc: 1,
            bit_depth: 8,
            width: 1_920,
            height: 1_080,
        })
    }

    fn main10_sps() -> Vec<u8> {
        build_sps(BuildSpsOptions {
            profile_idc: 2,
            chroma_format_idc: 1,
            bit_depth: 10,
            width: 3_840,
            height: 2_160,
        })
    }

    fn nal_unit(nal_unit_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut nal_unit = vec![(nal_unit_type & 0x3f) << 1, 0x01];
        nal_unit.extend_from_slice(payload);
        nal_unit
    }

    fn annex_b_access_unit(nal_units: &[Vec<u8>]) -> Vec<u8> {
        let mut access_unit = Vec::new();
        for nal_unit in nal_units {
            access_unit.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            access_unit.extend_from_slice(nal_unit);
        }
        access_unit
    }

    fn hvcc_access_unit(nal_length_size: H265NalLengthSize, nal_units: &[Vec<u8>]) -> Vec<u8> {
        let mut access_unit = Vec::new();
        for nal_unit in nal_units {
            push_nal_length(&mut access_unit, nal_length_size, nal_unit.len());
            access_unit.extend_from_slice(nal_unit);
        }
        access_unit
    }

    fn hvcc(
        nal_length_size: u8,
        profile_idc: u8,
        chroma_format_idc: u8,
        bit_depth: u8,
        arrays: &[HvccArray],
    ) -> Vec<u8> {
        let mut record_bytes = vec![
            1,
            profile_idc & 0x1f,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            120,
            0xf0,
            0x00,
            0xfc,
            0xfc | (chroma_format_idc & 0x03),
            0xf8 | ((bit_depth - 8) & 0x07),
            0xf8 | ((bit_depth - 8) & 0x07),
            0,
            0,
            0b0000_1100 | ((nal_length_size - 1) & 0x03),
            arrays.len() as u8,
        ];

        set_profile_compatibility_flag(&mut record_bytes, profile_idc);
        for array in arrays {
            record_bytes.push(array.nal_unit_type & 0x3f);
            record_bytes.extend_from_slice(&(array.nal_units.len() as u16).to_be_bytes());
            for nal_unit in array.nal_units {
                record_bytes.extend_from_slice(&(nal_unit.len() as u16).to_be_bytes());
                record_bytes.extend_from_slice(nal_unit);
            }
        }

        record_bytes
    }

    fn set_profile_compatibility_flag(record_bytes: &mut [u8], profile_idc: u8) {
        let flag_index = usize::from(profile_idc);
        let byte_index = 2 + flag_index / 8;
        let bit_index = 7 - (flag_index % 8);
        record_bytes[byte_index] |= 1 << bit_index;
    }

    struct HvccArray<'a> {
        nal_unit_type: u8,
        nal_units: &'a [Vec<u8>],
    }

    #[test]
    fn h265_hvcc_parser_accepts_length_sizes_and_extracts_vps_sps_pps() {
        let video_parameter_set = vps();
        let sequence_parameter_set = main_sps();
        let picture_parameter_set = pps();

        for (size_bytes, expected_length_size) in [
            (1, H265NalLengthSize::ONE),
            (2, H265NalLengthSize::TWO),
            (4, H265NalLengthSize::FOUR),
        ] {
            let record_bytes = hvcc(
                size_bytes,
                1,
                1,
                8,
                &[
                    HvccArray {
                        nal_unit_type: 32,
                        nal_units: std::slice::from_ref(&video_parameter_set),
                    },
                    HvccArray {
                        nal_unit_type: 33,
                        nal_units: std::slice::from_ref(&sequence_parameter_set),
                    },
                    HvccArray {
                        nal_unit_type: 34,
                        nal_units: std::slice::from_ref(&picture_parameter_set),
                    },
                ],
            );
            let record = parse_hevc_decoder_configuration_record(&record_bytes)
                .expect("валидный hvcC должен разбираться");

            assert_eq!(record.nal_length_size, expected_length_size);
            assert_eq!(
                record.video_parameter_sets(),
                std::slice::from_ref(&video_parameter_set)
            );
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
    fn h265_hvcc_parser_accepts_incomplete_records_when_header_is_safe() {
        let record_bytes = hvcc(4, 1, 1, 8, &[]);
        let record = parse_hevc_decoder_configuration_record(&record_bytes)
            .expect("hvcC без parameter sets всё ещё сообщает packetization/header metadata");
        let metadata = h265_header_metadata_from_hevc_decoder_configuration_record(&record)
            .expect("Main 8-bit 4:2:0 header должен быть production-safe");

        assert!(record.video_parameter_sets().is_empty());
        assert!(record.sequence_parameter_sets().is_empty());
        assert!(record.picture_parameter_sets().is_empty());
        assert_eq!(
            record.packetization(),
            H265Packetization::HvccLengthPrefixed {
                nal_length_size: H265NalLengthSize::FOUR,
            }
        );
        assert_eq!(metadata.profile, H265Profile::Main);
        assert_eq!(metadata.surface_format, VideoSurfaceFormat::Nv12);
    }

    #[test]
    fn h265_hvcc_parser_rejects_bad_version_lengths_and_truncated_arrays() {
        let sequence_parameter_set = main_sps();
        let mut bad_version = hvcc(4, 1, 1, 8, &[]);
        bad_version[0] = 2;
        let unsupported_length_size = hvcc(3, 1, 1, 8, &[]);
        let mut truncated_array = hvcc(
            4,
            1,
            1,
            8,
            &[HvccArray {
                nal_unit_type: 33,
                nal_units: std::slice::from_ref(&sequence_parameter_set),
            }],
        );
        truncated_array.truncate(24);

        assert!(parse_hevc_decoder_configuration_record(&bad_version).is_err());
        assert!(parse_hevc_decoder_configuration_record(&unsupported_length_size).is_err());
        assert!(parse_hevc_decoder_configuration_record(&truncated_array).is_err());
    }

    #[test]
    fn h265_hvcc_to_annex_b_conversion_handles_length_sizes_and_injection() {
        let video_parameter_set = vps();
        let sequence_parameter_set = main_sps();
        let picture_parameter_set = pps();

        for nal_length_size in [
            H265NalLengthSize::ONE,
            H265NalLengthSize::TWO,
            H265NalLengthSize::FOUR,
        ] {
            let access_unit = hvcc_access_unit(nal_length_size, &[cra_slice()]);
            let annex_b = h265_access_unit_to_annex_b(
                &access_unit,
                H265Packetization::HvccLengthPrefixed { nal_length_size },
                H265ParameterSetInjection::BeforeAccessUnit {
                    video_parameter_sets: std::slice::from_ref(&video_parameter_set),
                    sequence_parameter_sets: std::slice::from_ref(&sequence_parameter_set),
                    picture_parameter_sets: std::slice::from_ref(&picture_parameter_set),
                },
            )
            .expect("length-prefixed HEVC AU должен конвертироваться");

            assert_eq!(
                annex_b,
                annex_b_access_unit(&[
                    video_parameter_set.clone(),
                    sequence_parameter_set.clone(),
                    picture_parameter_set.clone(),
                    cra_slice(),
                ])
            );
        }
    }

    #[test]
    fn h265_annex_b_conversion_preserves_annex_b_input_and_scratch_contract() {
        let annex_b_input = {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&[0x00, 0x00, 0x01]);
            bytes.extend_from_slice(&aud());
            bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            bytes.extend_from_slice(&cra_slice());
            bytes
        };
        let mut output = Vec::with_capacity(128);
        let capacity_before = output.capacity();

        h265_access_unit_to_annex_b_into(
            &annex_b_input,
            H265Packetization::AnnexB,
            H265ParameterSetInjection::None,
            &mut output,
        )
        .expect("Annex B input должен оставаться Annex B");

        assert_eq!(output, annex_b_input);
        assert_eq!(output.capacity(), capacity_before);

        let malformed_packet = vec![0x00, 0x00, 0x00, 0x10, 0x2a, 0x01];
        let result = h265_access_unit_to_annex_b_into(
            &malformed_packet,
            H265Packetization::HvccLengthPrefixed {
                nal_length_size: H265NalLengthSize::FOUR,
            },
            H265ParameterSetInjection::None,
            &mut output,
        );

        assert!(matches!(
            result,
            Err(H265ByteStreamError::TruncatedHvccNalUnit { .. })
        ));
        assert!(output.is_empty());
        assert_eq!(output.capacity(), capacity_before);
    }

    #[test]
    fn h265_injection_adds_only_available_parameter_sets() {
        let video_parameter_set = vps();
        let access_unit = hvcc_access_unit(H265NalLengthSize::FOUR, &[cra_slice()]);
        let mut output = Vec::new();

        h265_access_unit_to_annex_b_into(
            &access_unit,
            H265Packetization::HvccLengthPrefixed {
                nal_length_size: H265NalLengthSize::FOUR,
            },
            H265ParameterSetInjection::BeforeAccessUnit {
                video_parameter_sets: std::slice::from_ref(&video_parameter_set),
                sequence_parameter_sets: &[],
                picture_parameter_sets: &[],
            },
            &mut output,
        )
        .expect("неполный injection не должен падать из-за отсутствующих SPS/PPS");

        assert_eq!(
            output,
            annex_b_access_unit(&[video_parameter_set, cra_slice()])
        );
    }

    #[test]
    fn h265_decode_start_probe_treats_all_irap_types_as_start() {
        for nal_unit_type in 16..=23 {
            let access_unit = annex_b_access_unit(&[nal_unit(nal_unit_type, &[0x01])]);
            let probe = probe_h265_packet_decode_start(&access_unit, H265Packetization::AnnexB);

            assert_eq!(probe, H265PacketDecodeStartProbe::DecodeStart);
        }
    }

    #[test]
    fn h265_decode_start_probe_distinguishes_parameter_sets_and_uncertainty() {
        let parameter_sets_only = annex_b_access_unit(&[vps(), main_sps(), pps()]);
        let non_irap = annex_b_access_unit(&[trail_slice()]);
        let malformed_packet = vec![0x00, 0x00, 0x00, 0x10, 0x2a, 0x01];

        assert_eq!(
            probe_h265_packet_decode_start(&parameter_sets_only, H265Packetization::AnnexB),
            H265PacketDecodeStartProbe::NotDecodeStart
        );
        assert_eq!(
            probe_h265_packet_decode_start(&non_irap, H265Packetization::AnnexB),
            H265PacketDecodeStartProbe::NotDecodeStart
        );
        assert!(matches!(
            probe_h265_packet_decode_start(
                &malformed_packet,
                H265Packetization::HvccLengthPrefixed {
                    nal_length_size: H265NalLengthSize::FOUR,
                },
            ),
            H265PacketDecodeStartProbe::Uncertain(H265ByteStreamError::TruncatedHvccNalUnit { .. })
        ));
    }

    #[test]
    fn h265_hev1_style_in_band_parameter_sets_refine_requirement() {
        let record_bytes = hvcc(4, 2, 1, 10, &[]);
        let access_unit = hvcc_access_unit(
            H265NalLengthSize::FOUR,
            &[vps(), main10_sps(), pps(), cra_slice()],
        );

        let packetization = infer_h265_packetization(Some(&record_bytes), &access_unit)
            .expect("hvcC должен доказать length-prefixed packetization");
        let requirement = h265_decode_requirement_from_packet(&access_unit, packetization)
            .expect("in-band SPS должен уточнить Main10 requirement");
        let nal_types = h265_nal_units(&access_unit, packetization)
            .expect("hev1-style AU должен разбираться")
            .iter()
            .map(|nal_unit| nal_unit.nal_unit_type())
            .collect::<Vec<_>>();

        assert_eq!(
            requirement.profile,
            Some(VideoProfile::H265(H265Profile::Main10))
        );
        assert_eq!(requirement.bit_depth, Some(BitDepth::Ten));
        assert_eq!(requirement.chroma, Some(ChromaSubsampling::Yuv420));
        assert_eq!(requirement.width, Some(3_840));
        assert_eq!(requirement.height, Some(2_160));
        assert_eq!(requirement.surface_format, Some(VideoSurfaceFormat::P010));
        assert_eq!(nal_types, vec![32, 33, 34, 21]);
    }

    #[test]
    fn h265_requirement_from_hvcc_uses_sps_when_present_and_header_when_incomplete() {
        let sequence_parameter_set = main_sps();
        let record_with_sps = hvcc(
            4,
            1,
            1,
            8,
            &[HvccArray {
                nal_unit_type: 33,
                nal_units: std::slice::from_ref(&sequence_parameter_set),
            }],
        );
        let record_without_sps = hvcc(4, 2, 1, 10, &[]);

        let sps_metadata =
            h265_sps_metadata_from_hevc_decoder_configuration_record(&record_with_sps)
                .expect("hvcC SPS должен давать metadata");
        let requirement_with_dimensions =
            h265_decode_requirement_from_hevc_decoder_configuration_record(&record_with_sps)
                .expect("hvcC с SPS должен дать requirement с dimensions");
        let requirement_without_dimensions =
            h265_decode_requirement_from_hevc_decoder_configuration_record(&record_without_sps)
                .expect("неполный hvcC должен дать header-level requirement");

        assert_eq!(sps_metadata.profile, H265Profile::Main);
        assert_eq!(requirement_with_dimensions.width, Some(1_920));
        assert_eq!(requirement_with_dimensions.height, Some(1_080));
        assert_eq!(
            requirement_without_dimensions.profile,
            Some(VideoProfile::H265(H265Profile::Main10))
        );
        assert_eq!(requirement_without_dimensions.width, None);
        assert_eq!(
            requirement_without_dimensions.surface_format,
            Some(VideoSurfaceFormat::P010)
        );
    }

    #[test]
    fn h265_requirement_rejects_unsupported_chroma_bit_depth_and_scc() {
        let twelve_bit_sps = build_sps(BuildSpsOptions {
            profile_idc: 2,
            chroma_format_idc: 1,
            bit_depth: 12,
            width: 1_280,
            height: 720,
        });
        let scc_record = hvcc(4, 9, 1, 8, &[]);

        for chroma_format_idc in [2, 3] {
            let unsupported_chroma_sps = build_sps(BuildSpsOptions {
                profile_idc: 1,
                chroma_format_idc,
                bit_depth: 8,
                width: 1_280,
                height: 720,
            });

            assert!(matches!(
                parse_h265_sps_metadata(&unsupported_chroma_sps),
                Err(H265SpsError::UnsupportedChroma { .. })
            ));
        }
        assert!(matches!(
            parse_h265_sps_metadata(&twelve_bit_sps),
            Err(H265SpsError::UnsupportedBitDepth { .. })
        ));
        assert!(matches!(
            h265_decode_requirement_from_hevc_decoder_configuration_record(&scc_record),
            Err(H265RequirementError::UnsupportedProfile { .. })
        ));
    }

    struct BuildSpsOptions {
        profile_idc: u8,
        chroma_format_idc: u32,
        bit_depth: u8,
        width: u32,
        height: u32,
    }

    fn build_sps(options: BuildSpsOptions) -> Vec<u8> {
        let mut bit_writer = BitWriter::new();
        bit_writer.u(4, 0);
        bit_writer.u(3, 0);
        bit_writer.u(1, 1);
        bit_writer.u(2, 0);
        bit_writer.u(1, 0);
        bit_writer.u(5, u64::from(options.profile_idc));
        for profile_index in 0..32 {
            bit_writer.u(
                1,
                u64::from(profile_index == u32::from(options.profile_idc)),
            );
        }
        bit_writer.u(48, 0);
        bit_writer.u(8, 120);
        bit_writer.ue(0);
        bit_writer.ue(options.chroma_format_idc);
        if options.chroma_format_idc == 3 {
            bit_writer.u(1, 0);
        }
        bit_writer.ue(options.width);
        bit_writer.ue(options.height);
        bit_writer.u(1, 0);
        bit_writer.ue(u32::from(options.bit_depth - 8));
        bit_writer.ue(u32::from(options.bit_depth - 8));
        bit_writer.rbsp_trailing_bits();

        let mut sps = nal_unit(33, &[]);
        sps.extend(bit_writer.into_bytes());
        sps
    }

    fn push_nal_length(
        output_bytes: &mut Vec<u8>,
        nal_length_size: H265NalLengthSize,
        nal_size: usize,
    ) {
        match nal_length_size.get() {
            1 => output_bytes.push(nal_size as u8),
            2 => output_bytes.extend_from_slice(&(nal_size as u16).to_be_bytes()),
            4 => output_bytes.extend_from_slice(&(nal_size as u32).to_be_bytes()),
            _ => unreachable!("test uses validated H265NalLengthSize"),
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

        fn u(&mut self, bit_count: u8, value: u64) {
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
            self.u(bit_count as u8, u64::from(code_num));
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
