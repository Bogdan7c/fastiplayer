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

    /// Требуется ли корректная обработка HDR input.
    pub hdr: bool,
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
            hdr: false,
        }
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
        self
    }

    /// Возвращает копию requirement с уточнённым chroma subsampling.
    #[must_use]
    pub const fn with_chroma(mut self, chroma: ChromaSubsampling) -> Self {
        self.chroma = Some(chroma);
        self
    }

    /// Возвращает копию requirement с coded resolution.
    #[must_use]
    pub const fn with_resolution(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
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

        if let Some(fps) = self.fps {
            parts.push(format!("{fps:.2} fps"));
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

        if let (Some(fps), Some(max_fps)) = (requirement.fps, self.max_fps)
            && fps > max_fps
        {
            return false;
        }

        !requirement.hdr || self.hdr_input
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
