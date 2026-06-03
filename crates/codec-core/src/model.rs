use std::fmt;

use serde::{Deserialize, Serialize};

use crate::VideoProfile;

/// Video codec, нормализованный из контейнера, сервиса или backend probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    /// VP9.
    Vp9,

    /// AV1.
    Av1,

    /// H.264/AVC.
    H264,

    /// H.265/HEVC.
    H265,

    /// VP8.
    Vp8,
}

impl VideoCodec {
    /// Нормализует container codec id в общий codec enum.
    #[must_use]
    pub fn from_container_codec_id(codec_id: &str) -> Option<Self> {
        let normalized_codec_id = codec_id.trim().to_ascii_uppercase();
        match normalized_codec_id.as_str() {
            "V_VP9" | "VP9" => Some(Self::Vp9),
            "V_AV1" | "AV1" | "AV01" => Some(Self::Av1),
            "V_MPEG4/ISO/AVC" | "AVC1" | "H264" | "H.264" => Some(Self::H264),
            "V_MPEGH/ISO/HEVC" | "HEV1" | "HVC1" | "H265" | "H.265" => Some(Self::H265),
            "V_VP8" | "VP8" => Some(Self::Vp8),
            _ => None,
        }
    }

    /// Возвращает стабильное имя codec для UI/report.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Vp9 => "VP9",
            Self::Av1 => "AV1",
            Self::H264 => "H.264",
            Self::H265 => "H.265",
            Self::Vp8 => "VP8",
        }
    }
}

impl fmt::Display for VideoCodec {
    /// Печатает человекочитаемое имя codec.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.display_name())
    }
}

/// Audio codec для будущей общей stream model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec {
    /// Opus.
    Opus,

    /// AAC.
    Aac,

    /// Vorbis.
    Vorbis,
}

impl AudioCodec {
    /// Нормализует container codec id в общий audio codec enum.
    #[must_use]
    pub fn from_container_codec_id(codec_id: &str) -> Option<Self> {
        let normalized_codec_id = codec_id.trim().to_ascii_uppercase();
        match normalized_codec_id.as_str() {
            "A_OPUS" | "OPUS" => Some(Self::Opus),
            "A_AAC" | "A_AAC/MPEG2/LC" | "A_AAC/MPEG4/LC" | "AAC" => Some(Self::Aac),
            "A_VORBIS" | "VORBIS" => Some(Self::Vorbis),
            _ => None,
        }
    }

    /// Возвращает стабильное имя codec для UI/report/errors.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Opus => "Opus",
            Self::Aac => "AAC",
            Self::Vorbis => "Vorbis",
        }
    }
}

impl fmt::Display for AudioCodec {
    /// Печатает человекочитаемое имя audio codec.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.display_name())
    }
}

/// Bit depth luma/chroma плоскостей.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BitDepth {
    /// 8 bit per component.
    Eight,

    /// 10 bit per component.
    Ten,

    /// 12 bit per component.
    Twelve,
}

impl BitDepth {
    /// Возвращает числовое значение bit depth.
    #[must_use]
    pub const fn bits(self) -> u8 {
        match self {
            Self::Eight => 8,
            Self::Ten => 10,
            Self::Twelve => 12,
        }
    }

    /// Нормализует числовой bit depth из container/bitstream metadata.
    #[must_use]
    pub const fn from_bits(bits_per_channel: u8) -> Option<Self> {
        match bits_per_channel {
            8 => Some(Self::Eight),
            10 => Some(Self::Ten),
            12 => Some(Self::Twelve),
            _ => None,
        }
    }
}

impl fmt::Display for BitDepth {
    /// Печатает bit depth как `8-bit`, `10-bit` или `12-bit`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}-bit", self.bits())
    }
}

/// Chroma subsampling входного video stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromaSubsampling {
    /// 4:2:0.
    Yuv420,

    /// 4:2:2.
    Yuv422,

    /// 4:4:4.
    Yuv444,
}

impl ChromaSubsampling {
    /// Нормализует Matroska ChromaSubsamplingHorz/Vert hints в общую chroma model.
    #[must_use]
    pub const fn from_matroska_subsampling(
        horizontal_subsampling: u64,
        vertical_subsampling: u64,
    ) -> Option<Self> {
        match (horizontal_subsampling, vertical_subsampling) {
            (1, 1) => Some(Self::Yuv420),
            (1, 0) => Some(Self::Yuv422),
            (0, 0) => Some(Self::Yuv444),
            _ => None,
        }
    }
}

impl fmt::Display for ChromaSubsampling {
    /// Печатает chroma в привычной записи.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Yuv420 => "4:2:0",
            Self::Yuv422 => "4:2:2",
            Self::Yuv444 => "4:4:4",
        };
        formatter.write_str(label)
    }
}

/// Диапазон кодовых значений YUV/RGB компоненты.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorRange {
    /// Limited/video range: luma 16..235 и chroma 16..240 для 8-bit SDR.
    Limited,

    /// Full/JPEG range: вся доступная шкала компоненты.
    Full,

    /// Диапазон не указан или не удалось надёжно определить.
    Unknown,
}

impl ColorRange {
    /// Нормализует Matroska `Range` value.
    #[must_use]
    pub const fn from_matroska_value(value: u64) -> Self {
        match value {
            1 => Self::Limited,
            2 => Self::Full,
            _ => Self::Unknown,
        }
    }
}

/// YUV->RGB matrix coefficients из stream metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatrixCoefficients {
    /// BT.601 matrix, чаще legacy SD-контент.
    Bt601,

    /// BT.709 matrix, основной SDR HD path.
    Bt709,

    /// BT.2020 non-constant-luminance matrix.
    Bt2020,

    /// Matrix coefficients не указаны или не поддержаны текущей typed model.
    Unknown,
}

impl MatrixCoefficients {
    /// Нормализует H.273/Matroska matrix coefficients в текущую typed model.
    #[must_use]
    pub const fn from_h273_value(value: u64) -> Self {
        match value {
            1 => Self::Bt709,
            5 | 6 => Self::Bt601,
            9 | 10 => Self::Bt2020,
            _ => Self::Unknown,
        }
    }
}

/// Цветовые primaries из bitstream/container metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorPrimaries {
    /// BT.709 / sRGB primaries.
    Bt709,

    /// BT.2020 primaries.
    Bt2020,

    /// SMPTE 170M primaries, типичные для NTSC SD.
    Smpte170m,

    /// BT.470 BG primaries, типичные для PAL/SECAM SD.
    Bt470Bg,

    /// Неизвестные или неуказанные primaries.
    Unknown,
}

impl ColorPrimaries {
    /// Нормализует H.273/Matroska colour primaries в текущую typed model.
    #[must_use]
    pub const fn from_h273_value(value: u64) -> Self {
        match value {
            1 => Self::Bt709,
            5 => Self::Bt470Bg,
            6 => Self::Smpte170m,
            9 => Self::Bt2020,
            _ => Self::Unknown,
        }
    }
}

/// Transfer function входного stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferFunction {
    /// BT.709 transfer.
    Bt709,

    /// sRGB transfer.
    Srgb,

    /// SMPTE ST 2084 / PQ.
    Pq,

    /// Hybrid Log-Gamma.
    Hlg,

    /// Неизвестная transfer function.
    Unknown,
}

impl TransferFunction {
    /// Нормализует H.273/Matroska transfer characteristics в текущую typed model.
    #[must_use]
    pub const fn from_h273_value(value: u64) -> Self {
        match value {
            1 | 14 | 15 => Self::Bt709,
            13 => Self::Srgb,
            16 => Self::Pq,
            18 => Self::Hlg,
            _ => Self::Unknown,
        }
    }

    /// Возвращает `true`, если transfer является HDR EOTF/OETF из Phase 9 strict core.
    #[must_use]
    pub const fn is_hdr(self) -> bool {
        matches!(self, Self::Pq | Self::Hlg)
    }
}

/// Display orientation, которую контейнер просит применить при показе кадра.
///
/// Значение описывает только поворот на четверть оборота. Decode backend по-прежнему
/// работает с coded surface, а renderer использует этот intent для выбора UV sampling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoDisplayOrientation {
    /// Кадр уже хранится в display orientation.
    #[default]
    Identity,

    /// Показать кадр с поворотом на 90 градусов по часовой стрелке.
    Rotate90Clockwise,

    /// Показать кадр с поворотом на 180 градусов.
    Rotate180,

    /// Показать кадр с поворотом на 270 градусов по часовой стрелке.
    Rotate270Clockwise,
}

impl VideoDisplayOrientation {
    /// Нормализует clockwise rotation в одну из поддержанных четвертей оборота.
    #[must_use]
    pub fn from_clockwise_degrees(clockwise_degrees: i32) -> Option<Self> {
        match clockwise_degrees.rem_euclid(360) {
            0 => Some(Self::Identity),
            90 => Some(Self::Rotate90Clockwise),
            180 => Some(Self::Rotate180),
            270 => Some(Self::Rotate270Clockwise),
            _ => None,
        }
    }

    /// Возвращает clockwise rotation в градусах для logs и container adapters.
    #[must_use]
    pub const fn clockwise_degrees(self) -> u16 {
        match self {
            Self::Identity => 0,
            Self::Rotate90Clockwise => 90,
            Self::Rotate180 => 180,
            Self::Rotate270Clockwise => 270,
        }
    }

    /// Возвращает `true`, если display width/height должны поменяться местами.
    #[must_use]
    pub const fn swaps_axes(self) -> bool {
        matches!(self, Self::Rotate90Clockwise | Self::Rotate270Clockwise)
    }
}

impl fmt::Display for VideoDisplayOrientation {
    /// Печатает orientation компактно, без container-specific matrix деталей.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Identity => "identity",
            Self::Rotate90Clockwise => "rotate-90-clockwise",
            Self::Rotate180 => "rotate-180",
            Self::Rotate270Clockwise => "rotate-270-clockwise",
        };
        formatter.write_str(label)
    }
}

/// HDR metadata, нужная renderer-у для tone mapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HdrMetadata {
    /// Цветовые primaries HDR-контента.
    pub color_primaries: ColorPrimaries,

    /// Transfer function HDR-контента.
    pub transfer_function: TransferFunction,

    /// Максимальная яркость mastering display в нитах.
    pub max_luminance_nits: Option<f32>,

    /// Минимальная яркость mastering display в нитах.
    pub min_luminance_nits: Option<f32>,

    /// MaxCLL, если контейнер или bitstream его сообщил.
    pub max_content_light_level_nits: Option<u32>,

    /// MaxFALL, если контейнер или bitstream его сообщил.
    pub max_frame_average_light_level_nits: Option<u32>,
}

/// Источник, из которого получена color metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorMetadataOrigin {
    /// Явный project default, применённый только при отсутствии metadata.
    FallbackDefault,

    /// Service manifest сообщил ранний hint до demux/decode.
    Manifest,

    /// Container/track metadata сообщила colorimetry.
    Container,

    /// Codec bitstream parser подтвердил colorimetry.
    Bitstream,

    /// Decoder/backend подтвердил фактический decoded output.
    DecoderBackend,
}

/// Уровень доверия к color metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorMetadataConfidence {
    /// Fallback default, а не факт из media stream.
    Fallback,

    /// Неполная metadata, полезная для раннего выбора path.
    Hint,

    /// Metadata подтверждена parser-ом или backend-ом.
    Confirmed,
}

/// Полная typed color metadata одного video stream или decoded frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct VideoColorMetadata {
    /// Диапазон YUV/RGB значений.
    pub range: ColorRange,

    /// Matrix coefficients для YUV->RGB conversion.
    pub matrix: MatrixCoefficients,

    /// Color primaries исходного изображения.
    pub primaries: ColorPrimaries,

    /// Transfer function исходного изображения.
    pub transfer: TransferFunction,

    /// HDR mastering/content metadata, если stream её сообщил.
    pub hdr_metadata: Option<HdrMetadata>,

    /// Источник metadata.
    pub origin: ColorMetadataOrigin,

    /// Уровень доверия к metadata.
    pub confidence: ColorMetadataConfidence,
}

impl VideoColorMetadata {
    /// Возвращает текущий SDR fallback: NV12/8-bit BT.709 limited без HDR metadata.
    #[must_use]
    pub const fn sdr_bt709_limited() -> Self {
        Self {
            range: ColorRange::Limited,
            matrix: MatrixCoefficients::Bt709,
            primaries: ColorPrimaries::Bt709,
            transfer: TransferFunction::Bt709,
            hdr_metadata: None,
            origin: ColorMetadataOrigin::FallbackDefault,
            confidence: ColorMetadataConfidence::Fallback,
        }
    }

    /// Создаёт container color metadata из уже нормализованных Matroska Colour полей.
    #[must_use]
    pub const fn container(
        range: ColorRange,
        matrix: MatrixCoefficients,
        primaries: ColorPrimaries,
        transfer: TransferFunction,
        hdr_metadata: Option<HdrMetadata>,
    ) -> Self {
        Self {
            range,
            matrix,
            primaries,
            transfer,
            hdr_metadata,
            origin: ColorMetadataOrigin::Container,
            confidence: ColorMetadataConfidence::Hint,
        }
    }

    /// Создаёт confirmed bitstream color metadata.
    #[must_use]
    pub const fn bitstream(
        range: ColorRange,
        matrix: MatrixCoefficients,
        primaries: ColorPrimaries,
        transfer: TransferFunction,
    ) -> Self {
        Self {
            range,
            matrix,
            primaries,
            transfer,
            hdr_metadata: None,
            origin: ColorMetadataOrigin::Bitstream,
            confidence: ColorMetadataConfidence::Confirmed,
        }
    }

    /// Проверяет, требует ли stream HDR processing, а не просто содержит side metadata.
    ///
    /// Matroska/WebM может хранить MaxCLL/MaxFALL рядом с обычным BT.709 SDR
    /// потоком. Поэтому сам факт `hdr_metadata` не переводит stream в HDR path:
    /// решающим сигналом остаётся HDR transfer PQ/HLG в основной colorimetry или
    /// в согласованной side metadata.
    #[must_use]
    pub fn requires_hdr_processing(&self) -> bool {
        self.transfer.is_hdr()
            || self
                .hdr_metadata
                .as_ref()
                .is_some_and(|hdr_metadata| hdr_metadata.transfer_function.is_hdr())
    }
}

/// Codec level без привязки к конкретному стандарту.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CodecLevel {
    /// Целая часть level.
    pub major: u8,

    /// Дробная часть level, если стандарт её использует.
    pub minor: Option<u8>,
}

/// Стабильный идентификатор decode backend.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DecodeBackendId(String);

impl DecodeBackendId {
    /// Создаёт backend id после минимальной validation.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        let valid = !value.trim().is_empty()
            && value.chars().all(|character| {
                character.is_ascii_lowercase() || character == '-' || character == '_'
            });

        valid.then_some(Self(value))
    }

    /// Возвращает backend id как строку.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Возвращает canonical VA-API backend id.
    #[must_use]
    pub fn vaapi() -> Self {
        Self("vaapi".to_string())
    }
}

impl fmt::Display for DecodeBackendId {
    /// Печатает backend id без дополнительного форматирования.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Codec-neutral decoded surface format на границе decoder -> renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoSurfaceFormat {
    /// 8-bit 4:2:0 semi-planar surface.
    Nv12,

    /// 10-bit 4:2:0 semi-planar surface in 16-bit storage words.
    P010,

    /// Packed 8-bit RGBA surface для будущих non-YUV producers, не для CPU fallback.
    Rgba8,
}

impl VideoSurfaceFormat {
    /// Выводит surface format из уже нормализованных bit depth/chroma полей.
    #[must_use]
    pub const fn from_bit_depth_and_chroma(
        bit_depth: BitDepth,
        chroma: ChromaSubsampling,
    ) -> Option<Self> {
        match (bit_depth, chroma) {
            (BitDepth::Eight, ChromaSubsampling::Yuv420) => Some(Self::Nv12),
            (BitDepth::Ten, ChromaSubsampling::Yuv420) => Some(Self::P010),
            _ => None,
        }
    }

    /// Выводит surface format, только если stream metadata уже достаточно точна.
    #[must_use]
    pub const fn from_optional_fields(
        bit_depth: Option<BitDepth>,
        chroma: Option<ChromaSubsampling>,
    ) -> Option<Self> {
        match (bit_depth, chroma) {
            (Some(bit_depth), Some(chroma)) => Self::from_bit_depth_and_chroma(bit_depth, chroma),
            _ => None,
        }
    }

    /// Выводит минимальный renderer input surface из stream requirement.
    ///
    /// Если codec adapter уже зафиксировал `surface_format`, именно это поле
    /// становится контрактом. Иначе используется текущая legacy эвристика:
    /// неизвестная bit depth остаётся SDR/NV12 до packet-level refinement.
    #[must_use]
    pub fn from_decode_requirement(requirement: &VideoDecodeRequirement) -> Option<Self> {
        if let Some(surface_format) = requirement.surface_format {
            return Some(surface_format);
        }

        if let Some(chroma) = requirement.chroma
            && chroma != ChromaSubsampling::Yuv420
        {
            return None;
        }

        match requirement.bit_depth {
            Some(BitDepth::Ten) => Some(Self::P010),
            Some(BitDepth::Twelve) => None,
            Some(BitDepth::Eight) | None => Some(Self::Nv12),
        }
    }

    /// Возвращает стабильное имя surface format для diagnostics.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Nv12 => "NV12",
            Self::P010 => "P010",
            Self::Rgba8 => "RGBA8",
        }
    }
}

impl fmt::Display for VideoSurfaceFormat {
    /// Печатает format в привычной video-терминологии.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.display_name())
    }
}

/// Обязательный zero-copy export/import механизм для production video path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZeroCopyExportRequirement {
    /// DMA-BUF fd export из decoder backend и import renderer-ом.
    DmaBuf,
}

impl fmt::Display for ZeroCopyExportRequirement {
    /// Печатает export requirement в стабильной diagnostic форме.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::DmaBuf => "DMA-BUF",
        };
        formatter.write_str(label)
    }
}

/// Контракт памяти decoded frame-а, общий для codec adapters, decoder backend-а и renderer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoMemoryContract {
    /// Production video кадры обязаны идти только через hardware zero-copy path.
    HardwareZeroCopy {
        /// Какой external-memory export должен подтвердить decode backend.
        export: ZeroCopyExportRequirement,
    },
}

impl VideoMemoryContract {
    /// Возвращает production baseline: hardware decode + DMA-BUF zero-copy.
    #[must_use]
    pub const fn dma_buf_zero_copy() -> Self {
        Self::HardwareZeroCopy {
            export: ZeroCopyExportRequirement::DmaBuf,
        }
    }

    /// Возвращает export requirement, нужный для удовлетворения контракта.
    #[must_use]
    pub const fn required_export(self) -> ZeroCopyExportRequirement {
        match self {
            Self::HardwareZeroCopy { export } => export,
        }
    }
}

impl Default for VideoMemoryContract {
    /// По умолчанию любое video requirement остаётся zero-copy-only.
    fn default() -> Self {
        Self::dma_buf_zero_copy()
    }
}

/// Требования к color path, отделённые от codec-specific metadata parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ColorPipelineRequirement {
    /// Требуется ли HDR processing/tone mapping для корректного вывода.
    pub requires_hdr_processing: bool,

    /// Источник color metadata, если он уже известен.
    pub metadata_origin: Option<ColorMetadataOrigin>,

    /// Уровень доверия к color metadata, если metadata уже известна.
    pub metadata_confidence: Option<ColorMetadataConfidence>,
}

impl ColorPipelineRequirement {
    /// Создаёт neutral SDR requirement без ложного metadata source.
    #[must_use]
    pub const fn unspecified_sdr() -> Self {
        Self {
            requires_hdr_processing: false,
            metadata_origin: None,
            metadata_confidence: None,
        }
    }

    /// Создаёт color pipeline requirement из resolved stream metadata.
    #[must_use]
    pub fn from_color_metadata(color: &VideoColorMetadata) -> Self {
        Self {
            requires_hdr_processing: color.requires_hdr_processing(),
            metadata_origin: Some(color.origin),
            metadata_confidence: Some(color.confidence),
        }
    }
}

impl Default for ColorPipelineRequirement {
    /// Без metadata renderer выбирает SDR-safe path и ждёт refinement.
    fn default() -> Self {
        Self::unspecified_sdr()
    }
}

/// Timing contract decoded frames для scheduler/capability matching без codec знаний.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FrameTimingContract {
    /// Номинальная frame rate, если container/service сообщил её до decode.
    pub nominal_frame_rate: Option<f64>,
}

impl FrameTimingContract {
    /// Создаёт timing contract с known nominal frame rate.
    #[must_use]
    pub const fn from_frame_rate(nominal_frame_rate: f64) -> Self {
        Self {
            nominal_frame_rate: Some(nominal_frame_rate),
        }
    }
}

/// Требования конкретного video stream к аппаратному decoder-у.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct VideoDecodeRequirement {
    /// Codec видеопотока.
    pub codec: VideoCodec,

    /// Profile, если он уже известен из manifest/container/bitstream.
    pub profile: Option<VideoProfile>,

    /// Bit depth, если он уже известен.
    pub bit_depth: Option<BitDepth>,

    /// Chroma subsampling, если он уже известен.
    pub chroma: Option<ChromaSubsampling>,

    /// Coded width, если он известен до decode.
    pub width: Option<u32>,

    /// Coded height, если он известен до decode.
    pub height: Option<u32>,

    /// FPS, если он известен.
    pub fps: Option<f64>,

    /// Codec-neutral surface format, если adapter уже вывел точный decoded boundary.
    #[serde(default)]
    pub surface_format: Option<VideoSurfaceFormat>,

    /// Общий memory contract; production default запрещает CPU fallback.
    #[serde(default)]
    pub memory_contract: VideoMemoryContract,

    /// Общий color pipeline requirement с origin/confidence metadata.
    #[serde(default)]
    pub color_pipeline: ColorPipelineRequirement,

    /// Общий timing contract для scheduler/capability layers.
    #[serde(default)]
    pub timing_contract: FrameTimingContract,

    /// Требуется ли корректная обработка HDR input.
    pub hdr: bool,

    /// Resolved color metadata, если она уже была собрана до capability selection.
    pub color: Option<VideoColorMetadata>,
}

impl VideoDecodeRequirement {
    /// Создаёт минимальное требование по codec.
    #[must_use]
    pub const fn new(codec: VideoCodec) -> Self {
        Self {
            codec,
            profile: None,
            bit_depth: None,
            chroma: None,
            width: None,
            height: None,
            fps: None,
            surface_format: None,
            memory_contract: VideoMemoryContract::dma_buf_zero_copy(),
            color_pipeline: ColorPipelineRequirement::unspecified_sdr(),
            timing_contract: FrameTimingContract {
                nominal_frame_rate: None,
            },
            hdr: false,
            color: None,
        }
    }

    /// Сообщает, требует ли stream полноценной HDR-обработки.
    ///
    /// Объединяет явный `hdr`-флаг и resolved HDR color metadata, чтобы
    /// capability/orchestration слои спрашивали про HDR через намерение, а не
    /// читали отдельные поля `hdr`/`color` напрямую.
    #[must_use]
    pub fn requires_hdr_processing(&self) -> bool {
        self.hdr
            || self
                .color
                .as_ref()
                .is_some_and(VideoColorMetadata::requires_hdr_processing)
    }

    /// Возвращает копию requirement с уточнённым profile.
    #[must_use]
    pub const fn with_profile(mut self, profile: VideoProfile) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Возвращает копию requirement с уточнённым bit depth.
    #[must_use]
    pub const fn with_bit_depth(mut self, bit_depth: BitDepth) -> Self {
        self.bit_depth = Some(bit_depth);
        self.surface_format = VideoSurfaceFormat::from_optional_fields(self.bit_depth, self.chroma);
        self
    }

    /// Возвращает копию requirement с уточнённым chroma subsampling.
    #[must_use]
    pub const fn with_chroma(mut self, chroma: ChromaSubsampling) -> Self {
        self.chroma = Some(chroma);
        self.surface_format = VideoSurfaceFormat::from_optional_fields(self.bit_depth, self.chroma);
        self
    }

    /// Возвращает копию requirement с coded resolution.
    #[must_use]
    pub const fn with_resolution(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    /// Возвращает копию requirement с явно заданным decoded surface contract.
    #[must_use]
    pub const fn with_surface_format(mut self, surface_format: VideoSurfaceFormat) -> Self {
        self.surface_format = Some(surface_format);
        self
    }

    /// Возвращает копию requirement с nominal frame rate.
    #[must_use]
    pub fn with_frame_rate(mut self, nominal_frame_rate: f64) -> Self {
        self.fps = Some(nominal_frame_rate);
        self.timing_contract = FrameTimingContract::from_frame_rate(nominal_frame_rate);
        self
    }

    /// Возвращает копию requirement с resolved color metadata.
    #[must_use]
    pub fn with_color(mut self, color: VideoColorMetadata) -> Self {
        self.hdr = color.requires_hdr_processing();
        self.color_pipeline = ColorPipelineRequirement::from_color_metadata(&color);
        self.color = Some(color);
        self
    }

    /// Возвращает короткое описание stream requirement для ошибок.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts = vec![self.codec.to_string()];

        if let Some(profile) = self.profile {
            parts.push(profile.to_string());
        }

        if let Some(bit_depth) = self.bit_depth {
            parts.push(bit_depth.to_string());
        }

        if let Some(chroma) = self.chroma {
            parts.push(chroma.to_string());
        }

        if let (Some(width), Some(height)) = (self.width, self.height) {
            parts.push(format!("{width}x{height}"));
        }

        if let Some(fps) = self.timing_contract.nominal_frame_rate.or(self.fps) {
            parts.push(format!("{fps:.2} fps"));
        }

        if let Some(surface_format) = self.surface_format {
            parts.push(format!("surface {surface_format}"));
        }

        if self.hdr {
            parts.push("HDR".to_string());
        }

        parts.join(" ")
    }
}

/// Один формат, который backend заявил как аппаратно декодируемый.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SupportedVideoDecodeFormat {
    /// Codec поддерживаемого формата.
    pub codec: VideoCodec,

    /// Profile поддерживаемого формата.
    pub profile: VideoProfile,

    /// Bit depth поддерживаемого формата.
    pub bit_depth: BitDepth,

    /// Chroma subsampling поддерживаемого формата.
    pub chroma: ChromaSubsampling,

    /// Максимальная coded width, если backend её сообщил.
    pub max_width: Option<u32>,

    /// Максимальная coded height, если backend её сообщил.
    pub max_height: Option<u32>,

    /// Максимальный FPS, если backend его сообщил.
    pub max_fps: Option<f64>,

    /// Может ли backend принять HDR input для этого формата.
    pub hdr_input: bool,

    /// Backend, который предоставил формат.
    pub backend: DecodeBackendId,
}

impl SupportedVideoDecodeFormat {
    /// Проверяет, закрывает ли backend format требования stream-а.
    #[must_use]
    pub fn satisfies(&self, requirement: &VideoDecodeRequirement) -> bool {
        if self.codec != requirement.codec {
            return false;
        }

        if let Some(profile) = requirement.profile
            && self.profile != profile
        {
            return false;
        }

        if let Some(bit_depth) = requirement.bit_depth
            && self.bit_depth != bit_depth
        {
            return false;
        }

        if let Some(chroma) = requirement.chroma
            && self.chroma != chroma
        {
            return false;
        }

        if let (Some(width), Some(max_width)) = (requirement.width, self.max_width)
            && width > max_width
        {
            return false;
        }

        if let (Some(height), Some(max_height)) = (requirement.height, self.max_height)
            && height > max_height
        {
            return false;
        }

        let required_frame_rate = requirement
            .timing_contract
            .nominal_frame_rate
            .or(requirement.fps);

        if let (Some(fps), Some(max_fps)) = (required_frame_rate, self.max_fps)
            && fps > max_fps
        {
            return false;
        }

        if let Some(required_surface_format) = requirement.surface_format
            && self.surface_format() != Some(required_surface_format)
        {
            return false;
        }

        !requirement.hdr || self.hdr_input
    }

    /// Возвращает decoded surface format, который backend format может произвести.
    #[must_use]
    pub const fn surface_format(&self) -> Option<VideoSurfaceFormat> {
        VideoSurfaceFormat::from_bit_depth_and_chroma(self.bit_depth, self.chroma)
    }

    /// Формирует компактное описание формата для report/UI.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut description = format!("{} {} {}", self.profile, self.bit_depth, self.chroma);

        if let (Some(max_width), Some(max_height)) = (self.max_width, self.max_height) {
            description.push_str(&format!(" до {max_width}x{max_height}"));
        }

        if let Some(max_fps) = self.max_fps {
            description.push_str(&format!(" {max_fps:.2} fps"));
        }

        if self.hdr_input {
            description.push_str(" HDR");
        }

        description
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{VideoProfile, Vp9Profile};

    #[test]
    fn container_codec_id_normalizes_common_names() {
        assert_eq!(
            VideoCodec::from_container_codec_id("V_VP9"),
            Some(VideoCodec::Vp9)
        );
        assert_eq!(
            VideoCodec::from_container_codec_id("v_mpeg4/iso/avc"),
            Some(VideoCodec::H264)
        );
        assert_eq!(VideoCodec::from_container_codec_id("unknown"), None);
    }

    #[test]
    fn audio_container_codec_id_normalizes_common_names() {
        assert_eq!(
            AudioCodec::from_container_codec_id("A_OPUS"),
            Some(AudioCodec::Opus)
        );
        assert_eq!(
            AudioCodec::from_container_codec_id("a_aac/mpeg4/lc"),
            Some(AudioCodec::Aac)
        );
        assert_eq!(
            AudioCodec::from_container_codec_id("A_VORBIS"),
            Some(AudioCodec::Vorbis)
        );
        assert_eq!(AudioCodec::from_container_codec_id("unknown"), None);
    }

    #[test]
    fn display_orientation_normalizes_quarter_turn_degrees() {
        assert_eq!(
            VideoDisplayOrientation::from_clockwise_degrees(-90),
            Some(VideoDisplayOrientation::Rotate270Clockwise)
        );
        assert_eq!(
            VideoDisplayOrientation::from_clockwise_degrees(450),
            Some(VideoDisplayOrientation::Rotate90Clockwise)
        );
        assert_eq!(VideoDisplayOrientation::from_clockwise_degrees(45), None);
        assert!(VideoDisplayOrientation::Rotate90Clockwise.swaps_axes());
        assert!(!VideoDisplayOrientation::Rotate180.swaps_axes());
    }

    #[test]
    fn supported_format_rejects_wrong_profile_before_decode() {
        let supported_format = SupportedVideoDecodeFormat {
            codec: VideoCodec::Vp9,
            profile: VideoProfile::Vp9(Vp9Profile::Profile0),
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(1920),
            max_height: Some(1080),
            max_fps: None,
            hdr_input: false,
            backend: DecodeBackendId::vaapi(),
        };
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2));

        assert!(!supported_format.satisfies(&requirement));
    }

    #[test]
    fn supported_format_accepts_unknown_profile_when_codec_matches() {
        let supported_format = SupportedVideoDecodeFormat {
            codec: VideoCodec::Vp9,
            profile: VideoProfile::Vp9(Vp9Profile::Profile0),
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(1920),
            max_height: Some(1080),
            max_fps: None,
            hdr_input: false,
            backend: DecodeBackendId::vaapi(),
        };
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);

        assert!(supported_format.satisfies(&requirement));
    }

    #[test]
    fn sdr_bt709_limited_uses_explicit_fallback_metadata() {
        let metadata = VideoColorMetadata::sdr_bt709_limited();

        assert_eq!(metadata.range, ColorRange::Limited);
        assert_eq!(metadata.matrix, MatrixCoefficients::Bt709);
        assert_eq!(metadata.primaries, ColorPrimaries::Bt709);
        assert_eq!(metadata.transfer, TransferFunction::Bt709);
        assert_eq!(metadata.origin, ColorMetadataOrigin::FallbackDefault);
        assert_eq!(metadata.confidence, ColorMetadataConfidence::Fallback);
        assert!(metadata.hdr_metadata.is_none());
    }

    #[test]
    fn bt709_content_light_side_metadata_does_not_require_hdr_processing() {
        let color = VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt709,
            ColorPrimaries::Bt709,
            TransferFunction::Bt709,
            Some(HdrMetadata {
                color_primaries: ColorPrimaries::Bt709,
                transfer_function: TransferFunction::Bt709,
                max_luminance_nits: None,
                min_luminance_nits: None,
                max_content_light_level_nits: Some(1_100),
                max_frame_average_light_level_nits: Some(180),
            }),
        );
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9).with_color(color.clone());

        assert!(!color.requires_hdr_processing());
        assert!(!requirement.hdr);
    }

    #[test]
    fn pq_side_metadata_requires_hdr_processing_when_primary_transfer_is_missing() {
        let color = VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            TransferFunction::Unknown,
            Some(HdrMetadata {
                color_primaries: ColorPrimaries::Bt2020,
                transfer_function: TransferFunction::Pq,
                max_luminance_nits: Some(1_000.0),
                min_luminance_nits: Some(0.01),
                max_content_light_level_nits: Some(1_000),
                max_frame_average_light_level_nits: Some(400),
            }),
        );
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9).with_color(color.clone());

        assert!(color.requires_hdr_processing());
        assert!(requirement.hdr);
    }

    #[test]
    fn color_enums_serialize_as_expected_snake_case_values() {
        assert_json_string(&ColorRange::Limited, "limited");
        assert_json_string(&ColorRange::Full, "full");
        assert_json_string(&ColorRange::Unknown, "unknown");
        assert_json_string(&MatrixCoefficients::Bt601, "bt601");
        assert_json_string(&MatrixCoefficients::Bt709, "bt709");
        assert_json_string(&MatrixCoefficients::Bt2020, "bt2020");
        assert_json_string(&MatrixCoefficients::Unknown, "unknown");
        assert_json_string(&ColorPrimaries::Bt709, "bt709");
        assert_json_string(&ColorPrimaries::Bt2020, "bt2020");
        assert_json_string(&ColorPrimaries::Smpte170m, "smpte170m");
        assert_json_string(&ColorPrimaries::Bt470Bg, "bt470_bg");
        assert_json_string(&ColorPrimaries::Unknown, "unknown");
        assert_json_string(&TransferFunction::Bt709, "bt709");
        assert_json_string(&TransferFunction::Srgb, "srgb");
        assert_json_string(&TransferFunction::Pq, "pq");
        assert_json_string(&TransferFunction::Hlg, "hlg");
        assert_json_string(&TransferFunction::Unknown, "unknown");
    }

    fn assert_json_string(value: &impl Serialize, expected_value: &str) {
        assert_eq!(
            serde_json::to_string(value).unwrap(),
            format!("\"{expected_value}\"")
        );
    }
}
