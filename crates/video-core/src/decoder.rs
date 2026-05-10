use std::any::Any;
use std::fmt;
use std::time::Duration;

use anyhow::ensure;
use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};
use media_core::Packet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTextureHandle(pub u64);

/// Фактический pixel format, который decoder отдал на renderer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodedPixelFormat {
    /// 8-bit 4:2:0 NV12: Y plane и interleaved UV plane.
    Nv12,

    /// 10-bit 4:2:0 P010: Y plane и interleaved UV plane в 16-bit контейнере.
    P010,
}

impl fmt::Display for DecodedPixelFormat {
    /// Печатает формат в привычной video-терминологии.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Nv12 => "NV12",
            Self::P010 => "P010",
        };
        formatter.write_str(label)
    }
}

/// Путь памяти, по которому decoded frame дошёл до renderer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameMemoryPath {
    /// Decoder-owned DMA-BUF импортирован в renderer без CPU readback/upload.
    DmaBufZeroCopy,

    /// Frame скопирован в renderer texture через CPU-visible путь.
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
    pub pts: Duration,
    pub format: DecodedPixelFormat,
    pub bit_depth: BitDepth,
    pub chroma: ChromaSubsampling,
    pub memory_path: FrameMemoryPath,
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub color: VideoColorMetadata,
    pub texture_handle: FrameTextureHandle,
}

impl DecodedFrame {
    /// Проверяет инварианты decoded frame до передачи в scheduler или renderer boundary.
    pub fn validate_contract(&self) -> anyhow::Result<()> {
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

        match self.format {
            DecodedPixelFormat::Nv12 => {
                ensure!(
                    self.bit_depth == BitDepth::Eight,
                    "NV12 decoded frame must be 8-bit, got {}",
                    self.bit_depth
                );
                ensure!(
                    self.chroma == ChromaSubsampling::Yuv420,
                    "NV12 decoded frame must be 4:2:0, got {}",
                    self.chroma
                );
            }
            DecodedPixelFormat::P010 => {
                ensure!(
                    self.bit_depth == BitDepth::Ten,
                    "P010 decoded frame must be 10-bit, got {}",
                    self.bit_depth
                );
                ensure!(
                    self.chroma == ChromaSubsampling::Yuv420,
                    "P010 decoded frame must be 4:2:0, got {}",
                    self.chroma
                );
                ensure!(
                    self.memory_path == FrameMemoryPath::DmaBufZeroCopy,
                    "P010 decoded frame requires zero-copy memory path, got {}",
                    self.memory_path
                );
            }
        }

        Ok(())
    }
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

    /// Создаёт test frame без реальных GPU resources и без legacy `ColorSpace`.
    fn decoded_test_frame() -> DecodedFrame {
        DecodedFrame {
            pts: Duration::ZERO,
            format: DecodedPixelFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            memory_path: FrameMemoryPath::CpuUpload,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: VideoColorMetadata::sdr_bt709_limited(),
            texture_handle: FrameTextureHandle(1),
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
        assert_eq!(frame.format, DecodedPixelFormat::Nv12);
        assert_eq!(frame.bit_depth, BitDepth::Eight);
        assert_eq!(frame.chroma, ChromaSubsampling::Yuv420);
        assert_eq!(frame.memory_path, FrameMemoryPath::CpuUpload);
    }

    #[test]
    fn p010_boundary_frame_has_zero_copy_contract() {
        let frame = DecodedFrame {
            pts: Duration::ZERO,
            format: DecodedPixelFormat::P010,
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv420,
            memory_path: FrameMemoryPath::DmaBufZeroCopy,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: VideoColorMetadata::sdr_bt709_limited(),
            texture_handle: FrameTextureHandle(2),
        };

        frame.validate_contract().unwrap();
        assert_eq!(frame.format, DecodedPixelFormat::P010);
        assert_eq!(frame.bit_depth, BitDepth::Ten);
        assert_eq!(frame.memory_path, FrameMemoryPath::DmaBufZeroCopy);
    }

    #[test]
    fn p010_cpu_upload_is_rejected_by_contract_validation() {
        let frame = DecodedFrame {
            pts: Duration::ZERO,
            format: DecodedPixelFormat::P010,
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv420,
            memory_path: FrameMemoryPath::CpuUpload,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: VideoColorMetadata::sdr_bt709_limited(),
            texture_handle: FrameTextureHandle(3),
        };

        let error = frame.validate_contract().unwrap_err();

        assert!(
            error.to_string().contains("requires zero-copy"),
            "unexpected validation error: {error}"
        );
    }
}
