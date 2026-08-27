//! Чистая проверка decoded surface и DMA-BUF/frame contract границы.
//!
//! Здесь нет decoder lifecycle, пула, release/reclaim или очередей. Модуль
//! только преобразует adapter/VA форматы в typed NV12/P010 contract и
//! fail-closed отклоняет неподдерживаемые layouts, bit depth и chroma paths.

use anyhow::Result;
use codec_core::{BitDepth, ChromaSubsampling};
use cros_codecs::decoder::DecodedDmaBufExportLayout;
use cros_codecs::libva::{
    VA_RT_FORMAT_YUV420, VA_RT_FORMAT_YUV420_10, VA_RT_FORMAT_YUV420_12, VA_RT_FORMAT_YUV422,
    VA_RT_FORMAT_YUV422_10, VA_RT_FORMAT_YUV422_12, VA_RT_FORMAT_YUV444, VA_RT_FORMAT_YUV444_10,
    VA_RT_FORMAT_YUV444_12,
};
use video_core::DecodedPixelFormat;
use video_frame_contract::{
    DmaBufImageLayout, HardwareFrameHandle, VideoFrameContract, VideoFrameTransferPath,
};

use crate::codec_adapter::VaapiDecodedFormat;

/// Typed контракт decoded surface, который VA-API backend отдаёт renderer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DecodedSurfaceContract {
    /// Pixel format renderer boundary.
    pub(super) format: DecodedPixelFormat,

    /// Bit depth decoded samples.
    pub(super) bit_depth: BitDepth,

    /// Chroma subsampling decoded frame-а.
    pub(super) chroma: ChromaSubsampling,
}

/// Фатальное нарушение zero-copy video boundary contract.
///
/// Любой decoded video frame нельзя безопасно отправлять в CPU fallback: так
/// pipeline скрывает отсутствие production DMA-BUF export/materialization и ломает
/// диагностику плавности. Поэтому такие ошибки останавливают decoder thread.
#[derive(Debug)]
pub(super) struct ZeroCopyContractViolation {
    /// Человекочитаемое объяснение конкретной причины отказа.
    detail: String,
}

impl ZeroCopyContractViolation {
    /// Создаёт ошибку zero-copy boundary с понятной причиной для лога/UI.
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for ZeroCopyContractViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for ZeroCopyContractViolation {}

/// Создаёт typed anyhow error для фатальной zero-copy boundary ошибки.
pub(super) fn zero_copy_contract_violation(detail: impl Into<String>) -> anyhow::Error {
    ZeroCopyContractViolation::new(detail).into()
}

impl DecodedSurfaceContract {
    /// Создаёт контракт для текущего production NV12 path.
    const fn nv12() -> Self {
        Self {
            format: DecodedPixelFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
        }
    }

    /// Создаёт контракт для P010 zero-copy boundary.
    const fn p010() -> Self {
        Self {
            format: DecodedPixelFormat::P010,
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv420,
        }
    }
}

/// Преобразует adapter decoded format в внешний frame contract.
///
/// Важно: `cros-codecs::DecodedFormat::I010` здесь приходит из VA `P010`
/// image format mapping. Для renderer boundary это не planar I010 upload path,
/// а P010 DMA-BUF zero-copy contract.
pub(super) fn decoded_contract_for_stream_format(
    decoded_format: VaapiDecodedFormat,
) -> Result<DecodedSurfaceContract> {
    match decoded_format {
        VaapiDecodedFormat::Nv12 | VaapiDecodedFormat::I420 => Ok(DecodedSurfaceContract::nv12()),
        VaapiDecodedFormat::I010 => Ok(DecodedSurfaceContract::p010()),
        other => Err(anyhow::anyhow!(
            "Unsupported decoded stream format for VA-API renderer boundary: {:?}",
            other
        )),
    }
}

fn dma_buf_image_layout_from_export_layout(
    export_layout: DecodedDmaBufExportLayout,
) -> DmaBufImageLayout {
    match export_layout {
        DecodedDmaBufExportLayout::ComposedLayers => DmaBufImageLayout::ComposedLayers,
        DecodedDmaBufExportLayout::SeparateLayers => DmaBufImageLayout::SeparateLayers,
    }
}

fn dma_buf_export_layout_from_image_layout(
    image_layout: DmaBufImageLayout,
) -> Result<DecodedDmaBufExportLayout> {
    match image_layout {
        DmaBufImageLayout::ComposedLayers => Ok(DecodedDmaBufExportLayout::ComposedLayers),
        DmaBufImageLayout::SeparateLayers => Ok(DecodedDmaBufExportLayout::SeparateLayers),
        DmaBufImageLayout::ComposedMultiObject => Err(anyhow::anyhow!(
            "VA-API multi-object DMA-BUF export contract is unsupported by the Vulkan importer"
        )),
    }
}

pub(super) fn dma_buf_export_layout_from_frame_contract(
    frame_contract: VideoFrameContract,
) -> Result<DecodedDmaBufExportLayout> {
    match frame_contract.transfer_path {
        VideoFrameTransferPath::HardwareZeroCopy {
            handle: HardwareFrameHandle::DmaBuf { image_layout },
        } => dma_buf_export_layout_from_image_layout(image_layout),
        other_transfer_path => Err(anyhow::anyhow!(
            "VA-API decoder requires a DMA-BUF frame contract, got {}",
            other_transfer_path.diagnostic_label()
        )),
    }
}

pub(super) fn frame_contract_for_dma_buf_export(
    format: DecodedPixelFormat,
    export_layout: DecodedDmaBufExportLayout,
) -> Result<VideoFrameContract> {
    let image_layout = dma_buf_image_layout_from_export_layout(export_layout);

    match format {
        DecodedPixelFormat::Nv12 => Ok(VideoFrameContract::dma_buf_nv12(image_layout)),
        DecodedPixelFormat::P010 => Ok(VideoFrameContract::dma_buf_p010(image_layout)),
        other => Err(anyhow::anyhow!(
            "unsupported VA-API DMA-BUF decoded frame format for frame contract: {other}"
        )),
    }
}

/// Преобразует VA RT format в тот же внешний frame contract.
pub(super) fn decoded_contract_for_rt_format(rt_format: u32) -> Result<DecodedSurfaceContract> {
    match rt_format {
        VA_RT_FORMAT_YUV420 => Ok(DecodedSurfaceContract::nv12()),
        VA_RT_FORMAT_YUV420_10 => Ok(DecodedSurfaceContract::p010()),
        other => Err(anyhow::anyhow!(
            "Unsupported VA RT format for VA-API renderer boundary: {:#x}",
            other
        )),
    }
}

/// Преобразует decoded output format из `StreamInfo` в VA RT format для surface pool.
pub(super) fn rt_format_for_decoded_format(decoded_format: VaapiDecodedFormat) -> Result<u32> {
    match decoded_format {
        VaapiDecodedFormat::Nv12 | VaapiDecodedFormat::I420 => Ok(VA_RT_FORMAT_YUV420),
        VaapiDecodedFormat::I010 => Ok(VA_RT_FORMAT_YUV420_10),
        VaapiDecodedFormat::I012 => Ok(VA_RT_FORMAT_YUV420_12),
        VaapiDecodedFormat::I422 => Ok(VA_RT_FORMAT_YUV422),
        VaapiDecodedFormat::I210 => Ok(VA_RT_FORMAT_YUV422_10),
        VaapiDecodedFormat::I212 => Ok(VA_RT_FORMAT_YUV422_12),
        VaapiDecodedFormat::I444 => Ok(VA_RT_FORMAT_YUV444),
        VaapiDecodedFormat::I410 => Ok(VA_RT_FORMAT_YUV444_10),
        VaapiDecodedFormat::I412 => Ok(VA_RT_FORMAT_YUV444_12),
        other => Err(anyhow::anyhow!(
            "Unsupported VA decoded format for internal surface pool: {:?}",
            other
        )),
    }
}
