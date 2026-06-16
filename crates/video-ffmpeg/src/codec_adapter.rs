//! Adapter helpers between neutral video contracts and future FFmpeg setup.

use crate::ffi::frame::FrameColorMetadata;
use crate::ffi::pixel_format::{SoftwarePixelFormat, SoftwarePixelFormatSet};
use codec_core::{
    Av1Profile, BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, H264Profile, H265Profile,
    MatrixCoefficients, TransferFunction, VideoCodec, VideoColorMetadata, VideoDecodeRequirement,
    VideoProfile, Vp8Profile, Vp9Profile,
};
use thiserror::Error;
use video_frame_contract::{
    FrameBitDepth, FrameChromaSubsampling, VideoFrameContract, VideoFrameContractValidationError,
    VideoFramePixelLayout,
};

/// План, который подтверждает: выбранный stream contract подходит software upload path-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoftwareDecodeContractPlan {
    /// Neutral contract, выбранный capability layer-ом.
    frame_contract: VideoFrameContract,
}

impl SoftwareDecodeContractPlan {
    /// Возвращает выбранный decoder->renderer contract.
    #[must_use]
    pub const fn frame_contract(&self) -> VideoFrameContract {
        self.frame_contract
    }
}

/// Decoder id, который adapter разрешает открыть через FFmpeg.
///
/// Raw `AVCodecID` остаётся в FFI module-е; этот enum нужен, чтобы codec/profile
/// policy была testable без обязательной линковки FFmpeg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FfmpegDecoderId {
    /// FFmpeg `AV_CODEC_ID_VP9`.
    Vp9,

    /// FFmpeg `AV_CODEC_ID_AV1`.
    Av1,

    /// FFmpeg `AV_CODEC_ID_H264`.
    H264,

    /// FFmpeg `AV_CODEC_ID_HEVC`.
    Hevc,

    /// FFmpeg `AV_CODEC_ID_VP8`.
    Vp8,
}

impl FfmpegDecoderId {
    /// Stable FFmpeg decoder name для diagnostics.
    #[must_use]
    pub const fn codec_name(self) -> &'static str {
        match self {
            Self::Vp9 => "vp9",
            Self::Av1 => "av1",
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Vp8 => "vp8",
        }
    }
}

/// Placeholder состояния чтения FFmpeg HDR side data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfmpegHdrSideDataStatus {
    /// Side data ещё не читается: это намеренный foundation marker.
    NotInspected,
}

/// План будущего чтения HDR side data из `AVFrame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegHdrSideDataPlan {
    /// FFmpeg `AV_FRAME_DATA_MASTERING_DISPLAY_METADATA`.
    pub mastering_display_metadata: FfmpegHdrSideDataStatus,

    /// FFmpeg `AV_FRAME_DATA_CONTENT_LIGHT_LEVEL`.
    pub content_light_metadata: FfmpegHdrSideDataStatus,
}

impl FfmpegHdrSideDataPlan {
    /// Создаёт placeholder без чтения side data.
    #[must_use]
    pub const fn placeholders() -> Self {
        Self {
            mastering_display_metadata: FfmpegHdrSideDataStatus::NotInspected,
            content_light_metadata: FfmpegHdrSideDataStatus::NotInspected,
        }
    }
}

/// Результат нормализации FFmpeg color fields в neutral codec-core модель.
#[derive(Debug, Clone, PartialEq)]
pub struct FfmpegColorMetadataPlan {
    /// Нормализованная color metadata, если FFmpeg сообщил хоть один известный field.
    metadata: Option<VideoColorMetadata>,

    /// Placeholder для будущих HDR side data.
    hdr_side_data: FfmpegHdrSideDataPlan,
}

impl FfmpegColorMetadataPlan {
    /// Возвращает normalized color metadata.
    #[must_use]
    pub const fn metadata(&self) -> Option<&VideoColorMetadata> {
        self.metadata.as_ref()
    }

    /// Возвращает состояние будущего HDR side-data чтения.
    #[must_use]
    pub const fn hdr_side_data(&self) -> FfmpegHdrSideDataPlan {
        self.hdr_side_data
    }
}

/// Полный adapter plan для открытия FFmpeg software decoder-а под выбранный contract.
#[derive(Debug, Clone, PartialEq)]
pub struct FfmpegStreamAdapterPlan {
    /// FFmpeg decoder id без raw FFI enum.
    decoder_id: FfmpegDecoderId,

    /// Подтверждённый neutral software contract.
    contract_plan: SoftwareDecodeContractPlan,

    /// Единственный layout, который decoder может отдавать без CPU conversion.
    accepted_pixel_formats: SoftwarePixelFormatSet,

    /// Color metadata requirement-а, если она уже была известна до decode.
    input_color_metadata: FfmpegColorMetadataPlan,
}

impl FfmpegStreamAdapterPlan {
    /// Возвращает FFmpeg decoder id.
    #[must_use]
    pub const fn decoder_id(&self) -> FfmpegDecoderId {
        self.decoder_id
    }

    /// Возвращает выбранный decoder->renderer contract.
    #[must_use]
    pub const fn frame_contract(&self) -> VideoFrameContract {
        self.contract_plan.frame_contract()
    }

    /// Возвращает adapter-owned pixel format allowlist.
    #[must_use]
    pub const fn accepted_pixel_formats(&self) -> &SoftwarePixelFormatSet {
        &self.accepted_pixel_formats
    }

    /// Возвращает color metadata plan для входного stream-а.
    #[must_use]
    pub const fn input_color_metadata(&self) -> &FfmpegColorMetadataPlan {
        &self.input_color_metadata
    }
}

/// Ошибка адаптации neutral stream contract-а в FFmpeg software plan.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FfmpegCodecAdapterError {
    /// Contract сам по себе невалиден по правилам `video-frame-contract`.
    #[error("invalid software frame contract: {reason}")]
    InvalidFrameContract {
        /// Текст neutral validation error-а для diagnostics.
        reason: String,
    },

    /// FFmpeg software path не должен получать hardware zero-copy contract.
    #[error("FFmpeg software decode requires SoftwareHostUpload, got {transfer_path}")]
    NonSoftwareTransferPath {
        /// Diagnostic label фактического transfer path-а.
        transfer_path: String,
    },

    /// Profile относится к другому codec-у, чем сам stream requirement.
    #[error("profile {profile} does not belong to codec {codec}")]
    ProfileCodecMismatch {
        /// Codec из `VideoDecodeRequirement`.
        codec: VideoCodec,

        /// Diagnostic label profile-а.
        profile: VideoProfile,
    },

    /// Codec/profile известен, но не входит в v1 FFmpeg software policy.
    #[error("FFmpeg software decoder does not support profile {profile} for codec {codec}")]
    UnsupportedProfile {
        /// Codec из `VideoDecodeRequirement`.
        codec: VideoCodec,

        /// Неподдержанный profile.
        profile: VideoProfile,
    },

    /// Renderer-intersected contract выбрал layout, который FFmpeg adapter не принимает.
    #[error("FFmpeg software decoder cannot output pixel layout {pixel_layout}")]
    UnsupportedPixelLayout {
        /// Layout из выбранного frame contract-а.
        pixel_layout: VideoFramePixelLayout,
    },

    /// Requirement описывает bit-depth/chroma пару вне v1 software matrix.
    #[error("unsupported software YUV combination: {bit_depth} {chroma}")]
    UnsupportedBitDepthChroma {
        /// Bit depth из stream requirement-а.
        bit_depth: BitDepth,

        /// Chroma subsampling из stream requirement-а.
        chroma: ChromaSubsampling,
    },

    /// Contract layout не совпадает с bit-depth/chroma из stream requirement-а.
    #[error(
        "software pixel layout mismatch: requirement expects {expected_pixel_layout}, contract uses {actual_pixel_layout}"
    )]
    PixelLayoutMismatch {
        /// Layout, выведенный из stream requirement-а.
        expected_pixel_layout: VideoFramePixelLayout,

        /// Layout из выбранного frame contract-а.
        actual_pixel_layout: VideoFramePixelLayout,
    },

    /// Contract layout имеет другой bit depth, чем stream requirement.
    #[error(
        "software pixel layout {pixel_layout} has bit depth {actual}, requirement needs {expected}"
    )]
    BitDepthMismatch {
        /// Layout из выбранного frame contract-а.
        pixel_layout: VideoFramePixelLayout,

        /// Bit depth из stream requirement-а.
        expected: BitDepth,

        /// Bit depth layout-а.
        actual: BitDepth,
    },

    /// Contract layout имеет другую chroma subsampling, чем stream requirement.
    #[error(
        "software pixel layout {pixel_layout} has chroma {actual}, requirement needs {expected}"
    )]
    ChromaMismatch {
        /// Layout из выбранного frame contract-а.
        pixel_layout: VideoFramePixelLayout,

        /// Chroma subsampling из stream requirement-а.
        expected: ChromaSubsampling,

        /// Chroma subsampling layout-а.
        actual: ChromaSubsampling,
    },

    /// Codec profile не может легально выдавать выбранный layout.
    #[error("profile {profile} cannot produce software pixel layout {pixel_layout}")]
    ProfilePixelLayoutMismatch {
        /// Profile из stream requirement-а.
        profile: VideoProfile,

        /// Layout из выбранного frame contract-а.
        pixel_layout: VideoFramePixelLayout,
    },
}

/// Валидирует полный stream requirement под FFmpeg software decoder.
pub fn plan_ffmpeg_software_decode(
    requirement: &VideoDecodeRequirement,
    frame_contract: VideoFrameContract,
) -> Result<FfmpegStreamAdapterPlan, FfmpegCodecAdapterError> {
    let contract_plan = validate_software_frame_contract(frame_contract)?;
    let decoder_id = decoder_id_from_requirement(requirement)?;
    let software_pixel_format = validate_layout_consistency(requirement, frame_contract)?;
    let accepted_pixel_formats =
        SoftwarePixelFormatSet::new([software_pixel_format]).map_err(|_| {
            FfmpegCodecAdapterError::UnsupportedPixelLayout {
                pixel_layout: frame_contract.pixel_layout,
            }
        })?;

    Ok(FfmpegStreamAdapterPlan {
        decoder_id,
        contract_plan,
        accepted_pixel_formats,
        input_color_metadata: color_metadata_plan_from_requirement(requirement),
    })
}

/// Валидирует только neutral contract; старые scaffold call site-ы используют эту boundary.
pub fn validate_software_frame_contract(
    frame_contract: VideoFrameContract,
) -> Result<SoftwareDecodeContractPlan, FfmpegCodecAdapterError> {
    frame_contract
        .validate()
        .map_err(map_contract_validation_error)?;

    if !frame_contract.transfer_path.is_software_host_upload() {
        return Err(FfmpegCodecAdapterError::NonSoftwareTransferPath {
            transfer_path: frame_contract.transfer_path.to_string(),
        });
    }

    Ok(SoftwareDecodeContractPlan { frame_contract })
}

/// Нормализует frame-level FFmpeg color fields в codec-core color model.
#[must_use]
pub fn color_metadata_plan_from_ffmpeg_frame(
    frame_color_metadata: FrameColorMetadata,
) -> FfmpegColorMetadataPlan {
    let range = color_range_from_ffmpeg_value(frame_color_metadata.color_range);
    let matrix = matrix_from_ffmpeg_value(frame_color_metadata.color_space);
    let primaries = color_primaries_from_ffmpeg_value(frame_color_metadata.color_primaries);
    let transfer = transfer_from_ffmpeg_value(frame_color_metadata.color_transfer);

    let metadata = (range != ColorRange::Unknown
        || matrix != MatrixCoefficients::Unknown
        || primaries != ColorPrimaries::Unknown
        || transfer != TransferFunction::Unknown)
        .then(|| VideoColorMetadata::bitstream(range, matrix, primaries, transfer));

    FfmpegColorMetadataPlan {
        metadata,
        hdr_side_data: FfmpegHdrSideDataPlan::placeholders(),
    }
}

/// Выводит FFmpeg decoder id из neutral codec/profile requirement-а.
pub fn decoder_id_from_requirement(
    requirement: &VideoDecodeRequirement,
) -> Result<FfmpegDecoderId, FfmpegCodecAdapterError> {
    validate_profile_belongs_to_codec(requirement)?;
    validate_profile_supported(requirement)?;

    Ok(match requirement.codec {
        VideoCodec::Vp9 => FfmpegDecoderId::Vp9,
        VideoCodec::Av1 => FfmpegDecoderId::Av1,
        VideoCodec::H264 => FfmpegDecoderId::H264,
        VideoCodec::H265 => FfmpegDecoderId::Hevc,
        VideoCodec::Vp8 => FfmpegDecoderId::Vp8,
    })
}

/// Сохраняет typed boundary, но не заставляет `thiserror` зависеть от foreign type.
fn map_contract_validation_error(
    error: VideoFrameContractValidationError,
) -> FfmpegCodecAdapterError {
    FfmpegCodecAdapterError::InvalidFrameContract {
        reason: error.to_string(),
    }
}

/// Проверяет, что выбранный contract layout совпадает с codec/profile/format requirement-ом.
fn validate_layout_consistency(
    requirement: &VideoDecodeRequirement,
    frame_contract: VideoFrameContract,
) -> Result<SoftwarePixelFormat, FfmpegCodecAdapterError> {
    let pixel_layout = frame_contract.pixel_layout;
    let software_pixel_format = SoftwarePixelFormat::from_frame_pixel_layout(pixel_layout)
        .ok_or(FfmpegCodecAdapterError::UnsupportedPixelLayout { pixel_layout })?;

    validate_requirement_format_fields(requirement, pixel_layout)?;
    validate_profile_accepts_pixel_layout(requirement, pixel_layout)?;

    Ok(software_pixel_format)
}

/// Проверяет bit-depth/chroma fields requirement-а против explicit frame layout-а.
fn validate_requirement_format_fields(
    requirement: &VideoDecodeRequirement,
    pixel_layout: VideoFramePixelLayout,
) -> Result<(), FfmpegCodecAdapterError> {
    let layout_bit_depth = bit_depth_from_layout(pixel_layout)
        .ok_or(FfmpegCodecAdapterError::UnsupportedPixelLayout { pixel_layout })?;
    let layout_chroma = chroma_from_layout(pixel_layout)
        .ok_or(FfmpegCodecAdapterError::UnsupportedPixelLayout { pixel_layout })?;

    if let (Some(bit_depth), Some(chroma)) = (requirement.bit_depth, requirement.chroma) {
        let expected_layout = software_layout_from_codec_fields(bit_depth, chroma)
            .ok_or(FfmpegCodecAdapterError::UnsupportedBitDepthChroma { bit_depth, chroma })?;

        if expected_layout == pixel_layout {
            return Ok(());
        }
    }

    if let Some(expected_bit_depth) = requirement.bit_depth {
        if expected_bit_depth != layout_bit_depth {
            return Err(FfmpegCodecAdapterError::BitDepthMismatch {
                pixel_layout,
                expected: expected_bit_depth,
                actual: layout_bit_depth,
            });
        }
    }

    if let Some(expected_chroma) = requirement.chroma {
        if expected_chroma != layout_chroma {
            return Err(FfmpegCodecAdapterError::ChromaMismatch {
                pixel_layout,
                expected: expected_chroma,
                actual: layout_chroma,
            });
        }
    }

    if let (Some(bit_depth), Some(chroma)) = (requirement.bit_depth, requirement.chroma) {
        let expected_pixel_layout = software_layout_from_codec_fields(bit_depth, chroma)
            .expect("unsupported bit-depth/chroma pair was handled before field mismatch checks");

        return Err(FfmpegCodecAdapterError::PixelLayoutMismatch {
            expected_pixel_layout,
            actual_pixel_layout: pixel_layout,
        });
    }

    Ok(())
}

/// Проверяет, что profile принадлежит codec-у requirement-а.
fn validate_profile_belongs_to_codec(
    requirement: &VideoDecodeRequirement,
) -> Result<(), FfmpegCodecAdapterError> {
    let Some(profile) = requirement.profile else {
        return Ok(());
    };

    let profile_matches_codec = matches!(
        (requirement.codec, profile),
        (VideoCodec::Vp9, VideoProfile::Vp9(_))
            | (VideoCodec::Av1, VideoProfile::Av1(_))
            | (VideoCodec::H264, VideoProfile::H264(_))
            | (VideoCodec::H265, VideoProfile::H265(_))
            | (VideoCodec::Vp8, VideoProfile::Vp8(_))
    );

    if profile_matches_codec {
        Ok(())
    } else {
        Err(FfmpegCodecAdapterError::ProfileCodecMismatch {
            codec: requirement.codec,
            profile,
        })
    }
}

/// Проверяет profile policy, которая не зависит от выбранного renderer layout-а.
fn validate_profile_supported(
    requirement: &VideoDecodeRequirement,
) -> Result<(), FfmpegCodecAdapterError> {
    let Some(profile) = requirement.profile else {
        return Ok(());
    };

    let supported = match profile {
        VideoProfile::Vp9(profile) => matches!(
            profile,
            Vp9Profile::Profile0
                | Vp9Profile::Profile1
                | Vp9Profile::Profile2
                | Vp9Profile::Profile3
        ),
        VideoProfile::Av1(profile) => matches!(profile, Av1Profile::Main | Av1Profile::High),
        VideoProfile::H264(profile) => matches!(
            profile,
            H264Profile::ConstrainedBaseline | H264Profile::Main | H264Profile::High
        ),
        VideoProfile::H265(profile) => matches!(
            profile,
            H265Profile::Main
                | H265Profile::Main10
                | H265Profile::Main12
                | H265Profile::Main422_10
                | H265Profile::Main422_12
                | H265Profile::Main444
                | H265Profile::Main444_10
        ),
        VideoProfile::Vp8(profile) => matches!(profile, Vp8Profile::Version0To3),
    };

    if supported {
        Ok(())
    } else {
        Err(FfmpegCodecAdapterError::UnsupportedProfile {
            codec: requirement.codec,
            profile,
        })
    }
}

/// Проверяет, что выбранный layout совместим с codec-specific profile semantics.
fn validate_profile_accepts_pixel_layout(
    requirement: &VideoDecodeRequirement,
    pixel_layout: VideoFramePixelLayout,
) -> Result<(), FfmpegCodecAdapterError> {
    let Some(profile) = requirement.profile else {
        return Ok(());
    };

    let bit_depth = bit_depth_from_layout(pixel_layout)
        .ok_or(FfmpegCodecAdapterError::UnsupportedPixelLayout { pixel_layout })?;
    let chroma = chroma_from_layout(pixel_layout)
        .ok_or(FfmpegCodecAdapterError::UnsupportedPixelLayout { pixel_layout })?;

    if profile_accepts_format(profile, bit_depth, chroma) {
        Ok(())
    } else {
        Err(FfmpegCodecAdapterError::ProfilePixelLayoutMismatch {
            profile,
            pixel_layout,
        })
    }
}

/// Codec-specific profile constraints на decoded format без обращения к FFmpeg.
fn profile_accepts_format(
    profile: VideoProfile,
    bit_depth: BitDepth,
    chroma: ChromaSubsampling,
) -> bool {
    match profile {
        VideoProfile::Vp9(Vp9Profile::Profile0) => {
            bit_depth == BitDepth::Eight && chroma == ChromaSubsampling::Yuv420
        }
        VideoProfile::Vp9(Vp9Profile::Profile1) => {
            bit_depth == BitDepth::Eight
                && matches!(
                    chroma,
                    ChromaSubsampling::Yuv422 | ChromaSubsampling::Yuv444
                )
        }
        VideoProfile::Vp9(Vp9Profile::Profile2) => {
            matches!(bit_depth, BitDepth::Ten | BitDepth::Twelve)
                && chroma == ChromaSubsampling::Yuv420
        }
        VideoProfile::Vp9(Vp9Profile::Profile3) => {
            matches!(bit_depth, BitDepth::Ten | BitDepth::Twelve)
                && matches!(
                    chroma,
                    ChromaSubsampling::Yuv422 | ChromaSubsampling::Yuv444
                )
        }
        VideoProfile::Av1(Av1Profile::Main) => {
            matches!(bit_depth, BitDepth::Eight | BitDepth::Ten)
                && chroma == ChromaSubsampling::Yuv420
        }
        VideoProfile::Av1(Av1Profile::High) => {
            matches!(bit_depth, BitDepth::Eight | BitDepth::Ten)
                && chroma == ChromaSubsampling::Yuv444
        }
        VideoProfile::Av1(Av1Profile::Professional) => false,
        VideoProfile::H264(_) => {
            bit_depth == BitDepth::Eight && chroma == ChromaSubsampling::Yuv420
        }
        VideoProfile::H265(H265Profile::Main) => {
            bit_depth == BitDepth::Eight && chroma == ChromaSubsampling::Yuv420
        }
        VideoProfile::H265(H265Profile::Main10) => {
            bit_depth == BitDepth::Ten && chroma == ChromaSubsampling::Yuv420
        }
        VideoProfile::H265(H265Profile::Main12) => {
            bit_depth == BitDepth::Twelve && chroma == ChromaSubsampling::Yuv420
        }
        VideoProfile::H265(H265Profile::Main422_10) => {
            bit_depth == BitDepth::Ten && chroma == ChromaSubsampling::Yuv422
        }
        VideoProfile::H265(H265Profile::Main422_12) => {
            bit_depth == BitDepth::Twelve && chroma == ChromaSubsampling::Yuv422
        }
        VideoProfile::H265(H265Profile::Main444) => {
            bit_depth == BitDepth::Eight && chroma == ChromaSubsampling::Yuv444
        }
        VideoProfile::H265(H265Profile::Main444_10) => {
            bit_depth == BitDepth::Ten && chroma == ChromaSubsampling::Yuv444
        }
        VideoProfile::H265(
            H265Profile::Main444_12
            | H265Profile::SccMain
            | H265Profile::SccMain10
            | H265Profile::SccMain444
            | H265Profile::SccMain444_10,
        ) => false,
        VideoProfile::Vp8(Vp8Profile::Version0To3) => {
            bit_depth == BitDepth::Eight && chroma == ChromaSubsampling::Yuv420
        }
    }
}

/// Выводит exact software layout из bit-depth/chroma пары.
fn software_layout_from_codec_fields(
    bit_depth: BitDepth,
    chroma: ChromaSubsampling,
) -> Option<VideoFramePixelLayout> {
    match (bit_depth, chroma) {
        (BitDepth::Eight, ChromaSubsampling::Yuv420) => Some(VideoFramePixelLayout::Yuv420Planar8),
        (BitDepth::Ten, ChromaSubsampling::Yuv420) => Some(VideoFramePixelLayout::Yuv420Planar10Le),
        (BitDepth::Twelve, ChromaSubsampling::Yuv420) => {
            Some(VideoFramePixelLayout::Yuv420Planar12Le)
        }
        (BitDepth::Eight, ChromaSubsampling::Yuv422) => Some(VideoFramePixelLayout::Yuv422Planar8),
        (BitDepth::Ten, ChromaSubsampling::Yuv422) => Some(VideoFramePixelLayout::Yuv422Planar10Le),
        (BitDepth::Twelve, ChromaSubsampling::Yuv422) => {
            Some(VideoFramePixelLayout::Yuv422Planar12Le)
        }
        (BitDepth::Eight, ChromaSubsampling::Yuv444) => Some(VideoFramePixelLayout::Yuv444Planar8),
        (BitDepth::Ten, ChromaSubsampling::Yuv444) => Some(VideoFramePixelLayout::Yuv444Planar10Le),
        (BitDepth::Twelve, ChromaSubsampling::Yuv444) => None,
    }
}

/// Переводит frame-contract bit depth в codec-core bit depth.
fn bit_depth_from_layout(pixel_layout: VideoFramePixelLayout) -> Option<BitDepth> {
    match pixel_layout.bit_depth()? {
        FrameBitDepth::Eight => Some(BitDepth::Eight),
        FrameBitDepth::Ten => Some(BitDepth::Ten),
        FrameBitDepth::Twelve => Some(BitDepth::Twelve),
    }
}

/// Переводит frame-contract chroma в codec-core chroma.
fn chroma_from_layout(pixel_layout: VideoFramePixelLayout) -> Option<ChromaSubsampling> {
    match pixel_layout.chroma()? {
        FrameChromaSubsampling::Yuv420 => Some(ChromaSubsampling::Yuv420),
        FrameChromaSubsampling::Yuv422 => Some(ChromaSubsampling::Yuv422),
        FrameChromaSubsampling::Yuv444 => Some(ChromaSubsampling::Yuv444),
    }
}

/// Сохраняет уже resolved stream color metadata в FFmpeg adapter plan.
fn color_metadata_plan_from_requirement(
    requirement: &VideoDecodeRequirement,
) -> FfmpegColorMetadataPlan {
    FfmpegColorMetadataPlan {
        metadata: requirement.color.clone(),
        hdr_side_data: FfmpegHdrSideDataPlan::placeholders(),
    }
}

/// FFmpeg `AVColorRange` numeric values follow H.273-like numbering.
const fn color_range_from_ffmpeg_value(value: i32) -> ColorRange {
    match value {
        1 => ColorRange::Limited,
        2 => ColorRange::Full,
        _ => ColorRange::Unknown,
    }
}

/// FFmpeg `AVColorSpace` numeric values line up with H.273 matrix coefficients for supported cases.
const fn matrix_from_ffmpeg_value(value: i32) -> MatrixCoefficients {
    match value {
        1 => MatrixCoefficients::Bt709,
        5 | 6 => MatrixCoefficients::Bt601,
        9 | 10 => MatrixCoefficients::Bt2020,
        _ => MatrixCoefficients::Unknown,
    }
}

/// FFmpeg `AVColorPrimaries` numeric values line up with H.273 for supported cases.
const fn color_primaries_from_ffmpeg_value(value: i32) -> ColorPrimaries {
    match value {
        1 => ColorPrimaries::Bt709,
        5 => ColorPrimaries::Bt470Bg,
        6 => ColorPrimaries::Smpte170m,
        9 => ColorPrimaries::Bt2020,
        _ => ColorPrimaries::Unknown,
    }
}

/// FFmpeg `AVColorTransferCharacteristic` numeric values line up with H.273 for supported cases.
const fn transfer_from_ffmpeg_value(value: i32) -> TransferFunction {
    match value {
        1 | 14 | 15 => TransferFunction::Bt709,
        13 => TransferFunction::Srgb,
        16 => TransferFunction::Pq,
        18 => TransferFunction::Hlg,
        _ => TransferFunction::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec_core::{H265Profile, VideoProfile};
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

    #[test]
    fn software_contract_plan_accepts_host_upload_contract() {
        let contract = VideoFrameContract::host_yuv420_planar8();
        let plan = validate_software_frame_contract(contract).unwrap();

        assert_eq!(plan.frame_contract(), contract);
    }

    #[test]
    fn software_contract_plan_rejects_dma_buf_contract() {
        let hardware_contract = VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers);
        let error = validate_software_frame_contract(hardware_contract)
            .expect_err("hardware contract must not enter FFmpeg software adapter");

        assert!(matches!(
            error,
            FfmpegCodecAdapterError::NonSoftwareTransferPath { .. }
        ));
    }

    #[test]
    fn stream_plan_maps_supported_codec_profile_to_decoder_and_exact_pixel_format() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::H265)
            .with_profile(VideoProfile::H265(H265Profile::Main10))
            .with_bit_depth(BitDepth::Ten)
            .with_chroma(ChromaSubsampling::Yuv420);
        let contract = VideoFrameContract::host_yuv420_planar10le();
        let plan = plan_ffmpeg_software_decode(&requirement, contract).unwrap();

        assert_eq!(plan.decoder_id(), FfmpegDecoderId::Hevc);
        assert_eq!(plan.decoder_id().codec_name(), "hevc");
        assert_eq!(plan.frame_contract(), contract);
        assert_eq!(
            plan.accepted_pixel_formats().iter().collect::<Vec<_>>(),
            vec![SoftwarePixelFormat::Yuv420Planar10Le]
        );
    }

    #[test]
    fn unsupported_codec_profile_gets_typed_rejection() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::H265)
            .with_profile(VideoProfile::H265(H265Profile::SccMain));
        let error =
            plan_ffmpeg_software_decode(&requirement, VideoFrameContract::host_yuv420_planar8())
                .expect_err("SCC profiles are outside the v1 software adapter policy");

        assert!(matches!(
            error,
            FfmpegCodecAdapterError::UnsupportedProfile {
                codec: VideoCodec::H265,
                profile: VideoProfile::H265(H265Profile::SccMain),
            }
        ));
    }

    #[test]
    fn profile_from_another_codec_gets_typed_rejection() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_profile(VideoProfile::H265(H265Profile::Main));
        let error = decoder_id_from_requirement(&requirement)
            .expect_err("profile codec must match stream codec");

        assert!(matches!(
            error,
            FfmpegCodecAdapterError::ProfileCodecMismatch {
                codec: VideoCodec::Vp9,
                profile: VideoProfile::H265(H265Profile::Main),
            }
        ));
    }

    #[test]
    fn bit_depth_mismatch_gets_typed_rejection() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::H265)
            .with_profile(VideoProfile::H265(H265Profile::Main10))
            .with_bit_depth(BitDepth::Ten)
            .with_chroma(ChromaSubsampling::Yuv420);
        let error =
            plan_ffmpeg_software_decode(&requirement, VideoFrameContract::host_yuv420_planar8())
                .expect_err("10-bit requirement must not accept an 8-bit layout");

        assert!(matches!(
            error,
            FfmpegCodecAdapterError::BitDepthMismatch {
                pixel_layout: VideoFramePixelLayout::Yuv420Planar8,
                expected: BitDepth::Ten,
                actual: BitDepth::Eight,
            }
        ));
    }

    #[test]
    fn unsupported_v1_layout_combination_gets_typed_rejection() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_bit_depth(BitDepth::Twelve)
            .with_chroma(ChromaSubsampling::Yuv444);
        let error =
            plan_ffmpeg_software_decode(&requirement, VideoFrameContract::host_yuv420_planar12le())
                .expect_err("4:4:4 12-bit is outside the v1 software layout matrix");

        assert!(matches!(
            error,
            FfmpegCodecAdapterError::UnsupportedBitDepthChroma {
                bit_depth: BitDepth::Twelve,
                chroma: ChromaSubsampling::Yuv444,
            }
        ));
    }

    #[test]
    fn profile_layout_mismatch_gets_typed_rejection() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0));
        let contract = VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Yuv422Planar8,
            transfer_path: video_frame_contract::VideoFrameTransferPath::SoftwareHostUpload,
        };
        let error = plan_ffmpeg_software_decode(&requirement, contract)
            .expect_err("VP9 profile 0 is only 8-bit 4:2:0");

        assert!(matches!(
            error,
            FfmpegCodecAdapterError::ProfilePixelLayoutMismatch {
                profile: VideoProfile::Vp9(Vp9Profile::Profile0),
                pixel_layout: VideoFramePixelLayout::Yuv422Planar8,
            }
        ));
    }

    #[test]
    fn frame_color_metadata_maps_ffmpeg_values_and_leaves_hdr_side_data_placeholder() {
        let plan = color_metadata_plan_from_ffmpeg_frame(FrameColorMetadata {
            color_range: 2,
            color_primaries: 9,
            color_transfer: 16,
            color_space: 9,
            chroma_location: 0,
        });
        let metadata = plan.metadata().expect("known FFmpeg fields should map");

        assert_eq!(metadata.range, ColorRange::Full);
        assert_eq!(metadata.matrix, MatrixCoefficients::Bt2020);
        assert_eq!(metadata.primaries, ColorPrimaries::Bt2020);
        assert_eq!(metadata.transfer, TransferFunction::Pq);
        assert_eq!(
            plan.hdr_side_data(),
            FfmpegHdrSideDataPlan {
                mastering_display_metadata: FfmpegHdrSideDataStatus::NotInspected,
                content_light_metadata: FfmpegHdrSideDataStatus::NotInspected,
            }
        );
    }

    #[test]
    fn unknown_frame_color_metadata_does_not_create_fake_metadata() {
        let plan = color_metadata_plan_from_ffmpeg_frame(FrameColorMetadata {
            color_range: 0,
            color_primaries: 2,
            color_transfer: 2,
            color_space: 2,
            chroma_location: 0,
        });

        assert!(plan.metadata().is_none());
    }
}
