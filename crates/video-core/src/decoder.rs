use std::any::Any;
use std::fmt;
use std::time::Duration;

use anyhow::ensure;
use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata, VideoDisplayOrientation};
use media_core::Packet;
use video_frame_contract::{
    FrameBitDepth, FrameChromaSubsampling, VideoFrameContract, VideoFramePixelLayout,
    VideoFrameTransferPath,
};

use crate::FrameResourceHandle;
use crate::VideoFrameDiagnostics;

/// Compatibility alias: decoded frame использует общий surface contract.
pub type DecodedPixelFormat = VideoFramePixelLayout;

/// Путь памяти, по которому decoded frame дошёл до renderer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameMemoryPath {
    /// Decoder-owned DMA-BUF импортирован в renderer без CPU readback/upload.
    DmaBufZeroCopy,

    /// Test-only/legacy marker для явного negative coverage CPU-visible пути.
    CpuUpload,
}

impl fmt::Display for FrameMemoryPath {
    /// Печатает короткий id для logs/diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::DmaBufZeroCopy => "dma-buf-zero-copy",
            Self::CpuUpload => "cpu-upload",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub generation: u64,
    pub pts: Duration,
    pub frame_contract: VideoFrameContract,
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub display_orientation: VideoDisplayOrientation,
    pub color: VideoColorMetadata,
    pub resource_handle: FrameResourceHandle,
    pub diagnostics: VideoFrameDiagnostics,
}

impl DecodedFrame {
    /// Возвращает decoded pixel layout из единого runtime contract-а.
    #[must_use]
    pub const fn format(&self) -> DecodedPixelFormat {
        self.frame_contract.pixel_layout
    }

    /// Возвращает bit depth как compatibility metadata для diagnostics/render metadata.
    #[must_use]
    pub const fn bit_depth(&self) -> Option<BitDepth> {
        match self.frame_contract.pixel_layout.bit_depth() {
            Some(FrameBitDepth::Eight) => Some(BitDepth::Eight),
            Some(FrameBitDepth::Ten) => Some(BitDepth::Ten),
            None => None,
        }
    }

    /// Возвращает chroma как compatibility metadata для YUV layouts.
    #[must_use]
    pub const fn chroma(&self) -> Option<ChromaSubsampling> {
        match self.frame_contract.pixel_layout.chroma() {
            Some(FrameChromaSubsampling::Yuv420) => Some(ChromaSubsampling::Yuv420),
            None => None,
        }
    }

    /// Возвращает legacy memory-path marker, выведенный из transfer path-а.
    #[must_use]
    pub const fn memory_path(&self) -> FrameMemoryPath {
        match self.frame_contract.transfer_path {
            VideoFrameTransferPath::HardwareZeroCopy { .. } => FrameMemoryPath::DmaBufZeroCopy,
            VideoFrameTransferPath::SoftwareHostUpload => FrameMemoryPath::CpuUpload,
        }
    }

    /// Проверяет только локальные инварианты decoded frame-а без resource lookup-а.
    pub fn validate_self_consistency(&self) -> anyhow::Result<()> {
        ensure!(
            self.width > 0 && self.height > 0,
            "decoded frame has invalid coded size: {}x{}",
            self.width,
            self.height
        );
        ensure!(
            self.render_width > 0 && self.render_height > 0,
            "decoded frame has invalid render size: {}x{}",
            self.render_width,
            self.render_height
        );
        self.frame_contract
            .validate()
            .map_err(|error| anyhow::anyhow!("decoded frame contract is invalid: {error}"))?;

        Ok(())
    }

    /// Проверяет actual frame contract против expected stream contract-а.
    pub fn validate_against_expected_contract(
        &self,
        expected_contract: VideoFrameContract,
    ) -> anyhow::Result<()> {
        self.validate_self_consistency()?;
        ensure!(
            self.frame_contract == expected_contract,
            "decoded frame contract mismatch: expected {}, got {}",
            expected_contract,
            self.frame_contract
        );
        Ok(())
    }

    /// Compatibility wrapper для текущего zero-copy renderer/VAAPI path-а.
    pub fn validate_contract(&self) -> anyhow::Result<()> {
        self.validate_self_consistency()?;
        ensure!(
            self.memory_path() == FrameMemoryPath::DmaBufZeroCopy,
            "{} decoded frame requires zero-copy memory path, got {}",
            self.format(),
            self.memory_path()
        );

        match self.format() {
            DecodedPixelFormat::Nv12 => {
                ensure!(
                    self.bit_depth() == Some(BitDepth::Eight),
                    "NV12 decoded frame must be 8-bit, got {}",
                    optional_bit_depth_label(self.bit_depth())
                );
                ensure!(
                    self.chroma() == Some(ChromaSubsampling::Yuv420),
                    "NV12 decoded frame must be 4:2:0, got {}",
                    optional_chroma_label(self.chroma())
                );
            }
            DecodedPixelFormat::P010 => {
                ensure!(
                    self.bit_depth() == Some(BitDepth::Ten),
                    "P010 decoded frame must be 10-bit, got {}",
                    optional_bit_depth_label(self.bit_depth())
                );
                ensure!(
                    self.chroma() == Some(ChromaSubsampling::Yuv420),
                    "P010 decoded frame must be 4:2:0, got {}",
                    optional_chroma_label(self.chroma())
                );
            }
            DecodedPixelFormat::Rgba8 => {
                ensure!(
                    false,
                    "RGBA8 decoded frame is not a production zero-copy video surface"
                );
            }
            DecodedPixelFormat::Yuv420Planar8 | DecodedPixelFormat::Yuv420Planar10Le => {
                ensure!(
                    false,
                    "{} decoded frame is a host-planar layout and is not part of the current zero-copy runtime boundary",
                    self.format()
                );
            }
        }

        Ok(())
    }
}

fn optional_bit_depth_label(bit_depth: Option<BitDepth>) -> String {
    bit_depth.map_or_else(|| "none".to_string(), |bit_depth| bit_depth.to_string())
}

fn optional_chroma_label(chroma: Option<ChromaSubsampling>) -> String {
    chroma.map_or_else(|| "none".to_string(), |chroma| chroma.to_string())
}

pub trait VideoDecoder {
    fn decode(&mut self, packet: &Packet) -> anyhow::Result<Option<DecodedFrame>>;
    fn flush(&mut self) -> anyhow::Result<()>;
    fn backend_name(&self) -> &'static str;

    /// Downcast to concrete type for backend-specific operations.
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec_core::{ColorMetadataOrigin, ColorRange, MatrixCoefficients};
    use video_frame_contract::DmaBufImageLayout;

    /// Создаёт test frame без реальных GPU resources и без legacy `ColorSpace`.
    fn decoded_test_frame() -> DecodedFrame {
        DecodedFrame {
            generation: 0,
            pts: Duration::ZERO,
            frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: FrameResourceHandle(1),
            diagnostics: VideoFrameDiagnostics::default(),
        }
    }

    #[test]
    fn decoded_frame_test_helper_uses_explicit_color_metadata() {
        let frame = decoded_test_frame();

        assert_eq!(frame.color.range, ColorRange::Limited);
        assert_eq!(frame.color.matrix, MatrixCoefficients::Bt709);
        assert_eq!(frame.color.origin, ColorMetadataOrigin::FallbackDefault);
    }

    #[test]
    fn nv12_decoded_test_frame_has_explicit_frame_contract() {
        let frame = decoded_test_frame();

        frame.validate_contract().unwrap();
        assert_eq!(frame.format(), DecodedPixelFormat::Nv12);
        assert_eq!(frame.bit_depth(), Some(BitDepth::Eight));
        assert_eq!(frame.chroma(), Some(ChromaSubsampling::Yuv420));
        assert_eq!(frame.memory_path(), FrameMemoryPath::DmaBufZeroCopy);
    }

    #[test]
    fn p010_boundary_frame_has_zero_copy_contract() {
        let frame = DecodedFrame {
            generation: 0,
            pts: Duration::ZERO,
            frame_contract: VideoFrameContract::dma_buf_p010(DmaBufImageLayout::ComposedLayers),
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: FrameResourceHandle(2),
            diagnostics: VideoFrameDiagnostics::default(),
        };

        frame.validate_contract().unwrap();
        assert_eq!(frame.format(), DecodedPixelFormat::P010);
        assert_eq!(frame.bit_depth(), Some(BitDepth::Ten));
        assert_eq!(frame.memory_path(), FrameMemoryPath::DmaBufZeroCopy);
    }

    #[test]
    fn host_planar_upload_is_rejected_by_zero_copy_contract_validation() {
        let frame = DecodedFrame {
            generation: 0,
            pts: Duration::ZERO,
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: FrameResourceHandle(3),
            diagnostics: VideoFrameDiagnostics::default(),
        };

        let error = frame.validate_contract().unwrap_err();

        assert!(
            error.to_string().contains("requires zero-copy memory path"),
            "unexpected validation error: {error}"
        );
    }

    #[test]
    fn validate_against_expected_contract_reports_mismatch() {
        let frame = DecodedFrame {
            generation: 0,
            pts: Duration::ZERO,
            frame_contract: VideoFrameContract::host_yuv420_planar10le(),
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: FrameResourceHandle(4),
            diagnostics: VideoFrameDiagnostics::default(),
        };

        frame
            .validate_self_consistency()
            .expect("host-upload contract is locally valid");
        let error = frame
            .validate_against_expected_contract(VideoFrameContract::host_yuv420_planar8())
            .unwrap_err();

        assert!(
            error.to_string().contains("contract mismatch"),
            "unexpected validation error: {error}"
        );
    }
}
