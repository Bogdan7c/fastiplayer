//! Нейтральные контракты decoded video frame между decoder и renderer.
//!
//! Crate намеренно не зависит от codec/render/backend crates: он описывает
//! только vocabulary кадра и пути передачи, а выбор backend-а остается выше.

#![forbid(unsafe_code)]

use std::fmt;

use serde::{Deserialize, Serialize};

/// Bit depth на уровне decoded frame layout-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameBitDepth {
    /// 8 бит на sample.
    Eight,

    /// 10 бит на sample, обычно в 16-битном storage word-е.
    Ten,
}

impl FrameBitDepth {
    /// Возвращает человекочитаемую diagnostic label.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::Eight => "8-bit",
            Self::Ten => "10-bit",
        }
    }
}

impl fmt::Display for FrameBitDepth {
    /// Печатает stable diagnostic label.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_label())
    }
}

/// Chroma subsampling на уровне decoded frame layout-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameChromaSubsampling {
    /// YUV 4:2:0.
    Yuv420,
}

impl FrameChromaSubsampling {
    /// Возвращает человекочитаемую diagnostic label.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::Yuv420 => "YUV 4:2:0",
        }
    }
}

impl fmt::Display for FrameChromaSubsampling {
    /// Печатает stable diagnostic label.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_label())
    }
}

/// Pixel layout decoded frame-а без привязки к backend transfer path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoFramePixelLayout {
    /// 8-bit 4:2:0 semi-planar layout: Y plane + interleaved UV plane.
    Nv12,

    /// 10-bit 4:2:0 semi-planar layout in 16-bit little-endian storage words.
    P010,

    /// 8-bit 4:2:0 planar host layout: Y, U, V planes.
    Yuv420Planar8,

    /// 10-bit 4:2:0 planar host layout: little-endian 16-bit storage words.
    Yuv420Planar10Le,

    /// Reserved packed RGBA layout for future producers, not current production.
    Rgba8,
}

impl VideoFramePixelLayout {
    /// Выводит hardware baseline pixel layout из frame-level bit depth/chroma.
    #[must_use]
    pub const fn from_frame_bit_depth_and_chroma(
        bit_depth: FrameBitDepth,
        chroma: FrameChromaSubsampling,
    ) -> Self {
        match (bit_depth, chroma) {
            (FrameBitDepth::Eight, FrameChromaSubsampling::Yuv420) => Self::Nv12,
            (FrameBitDepth::Ten, FrameChromaSubsampling::Yuv420) => Self::P010,
        }
    }

    /// Возвращает bit depth, если layout относится к YUV frame vocabulary.
    #[must_use]
    pub const fn bit_depth(self) -> Option<FrameBitDepth> {
        match self {
            Self::Nv12 | Self::Yuv420Planar8 | Self::Rgba8 => Some(FrameBitDepth::Eight),
            Self::P010 | Self::Yuv420Planar10Le => Some(FrameBitDepth::Ten),
        }
    }

    /// Возвращает chroma subsampling, если layout относится к YUV frame vocabulary.
    #[must_use]
    pub const fn chroma(self) -> Option<FrameChromaSubsampling> {
        match self {
            Self::Nv12 | Self::P010 | Self::Yuv420Planar8 | Self::Yuv420Planar10Le => {
                Some(FrameChromaSubsampling::Yuv420)
            }
            Self::Rgba8 => None,
        }
    }

    /// Проверяет, что layout описывает CPU-visible planar host storage.
    #[must_use]
    pub const fn is_host_planar(self) -> bool {
        matches!(self, Self::Yuv420Planar8 | Self::Yuv420Planar10Le)
    }

    /// Проверяет, что layout является current hardware zero-copy baseline.
    #[must_use]
    pub const fn is_current_dma_buf_baseline(self) -> bool {
        matches!(self, Self::Nv12 | Self::P010)
    }

    /// Возвращает stable label layout-а для diagnostics.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::Nv12 => "NV12",
            Self::P010 => "P010",
            Self::Yuv420Planar8 => "YUV420 planar 8-bit",
            Self::Yuv420Planar10Le => "YUV420 planar 10-bit little-endian",
            Self::Rgba8 => "RGBA8",
        }
    }

    /// Compatibility label для старых diagnostics, которые называли layout surface format-ом.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        self.diagnostic_label()
    }
}

impl fmt::Display for VideoFramePixelLayout {
    /// Печатает stable diagnostic label.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_label())
    }
}

/// Нейтральное описание того, как image хранится внутри DMA-BUF handle-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DmaBufImageLayout {
    /// Image приходит как composed multi-plane image на стороне importer-а.
    ComposedLayers,

    /// Image приходит как отдельные importable layers/planes.
    SeparateLayers,
}

impl DmaBufImageLayout {
    /// Возвращает stable label layout-а для diagnostics.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::ComposedLayers => "composed DMA-BUF layers",
            Self::SeparateLayers => "separate DMA-BUF layers",
        }
    }
}

impl fmt::Display for DmaBufImageLayout {
    /// Печатает stable diagnostic label.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_label())
    }
}

/// Hardware handle family для zero-copy frame transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareFrameHandle {
    /// Linux DMA-BUF handle; image layout обязателен для любого pixel layout.
    DmaBuf {
        /// Как decoder экспортирует image внутри DMA-BUF path-а.
        image_layout: DmaBufImageLayout,
    },
}

impl HardwareFrameHandle {
    /// Создаёт DMA-BUF handle contract с явным image layout.
    #[must_use]
    pub const fn dma_buf(image_layout: DmaBufImageLayout) -> Self {
        Self::DmaBuf { image_layout }
    }

    /// Возвращает stable label handle-а для diagnostics.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::DmaBuf { .. } => "DMA-BUF",
        }
    }
}

impl fmt::Display for HardwareFrameHandle {
    /// Печатает stable diagnostic label.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DmaBuf { image_layout } => {
                write!(formatter, "{} ({image_layout})", self.diagnostic_label())
            }
        }
    }
}

/// Путь передачи decoded frame-а от decoder-а к renderer-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoFrameTransferPath {
    /// Hardware zero-copy path через backend-specific external handle.
    HardwareZeroCopy {
        /// Concrete hardware handle family и его обязательные details.
        handle: HardwareFrameHandle,
    },

    /// CPU-visible host upload path для будущего software decoder-а.
    SoftwareHostUpload,
}

impl VideoFrameTransferPath {
    /// Создаёт current hardware baseline transfer path через DMA-BUF.
    #[must_use]
    pub const fn dma_buf_zero_copy(image_layout: DmaBufImageLayout) -> Self {
        Self::HardwareZeroCopy {
            handle: HardwareFrameHandle::dma_buf(image_layout),
        }
    }

    /// Проверяет hardware zero-copy path.
    #[must_use]
    pub const fn is_hardware_zero_copy(self) -> bool {
        matches!(self, Self::HardwareZeroCopy { .. })
    }

    /// Проверяет software host upload path.
    #[must_use]
    pub const fn is_software_host_upload(self) -> bool {
        matches!(self, Self::SoftwareHostUpload)
    }

    /// Возвращает stable label path-а для diagnostics.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::HardwareZeroCopy { .. } => "hardware zero-copy",
            Self::SoftwareHostUpload => "software host upload",
        }
    }
}

impl fmt::Display for VideoFrameTransferPath {
    /// Печатает stable diagnostic label.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HardwareZeroCopy { handle } => {
                write!(formatter, "{} via {handle}", self.diagnostic_label())
            }
            Self::SoftwareHostUpload => formatter.write_str(self.diagnostic_label()),
        }
    }
}

/// Ошибка self-consistency validation для frame contract-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoFrameContractValidationError {
    /// Host planar layout нельзя передавать через hardware handle.
    HostPlanarRequiresSoftwareUpload {
        /// Pixel layout, который нарушил invariant.
        pixel_layout: VideoFramePixelLayout,
    },

    /// Semi-planar DMA-BUF baseline нельзя выдавать как host upload.
    HardwareLayoutRequiresZeroCopy {
        /// Pixel layout, который нарушил invariant.
        pixel_layout: VideoFramePixelLayout,
    },

    /// Reserved packed layout пока не имеет production transfer contract-а.
    ReservedLayoutWithoutTransferContract {
        /// Pixel layout, который нарушил invariant.
        pixel_layout: VideoFramePixelLayout,
    },
}

impl fmt::Display for VideoFrameContractValidationError {
    /// Печатает ошибку validation без backend-specific details.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostPlanarRequiresSoftwareUpload { pixel_layout } => write!(
                formatter,
                "{pixel_layout} должен идти через software host upload, а не hardware zero-copy"
            ),
            Self::HardwareLayoutRequiresZeroCopy { pixel_layout } => write!(
                formatter,
                "{pixel_layout} должен идти через hardware zero-copy, а не software host upload"
            ),
            Self::ReservedLayoutWithoutTransferContract { pixel_layout } => write!(
                formatter,
                "{pixel_layout} зарезервирован и пока не имеет production transfer contract-а"
            ),
        }
    }
}

impl std::error::Error for VideoFrameContractValidationError {}

/// Единый decoded frame contract: pixel layout плюс путь передачи.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct VideoFrameContract {
    /// Pixel layout decoded frame-а.
    pub pixel_layout: VideoFramePixelLayout,

    /// Путь передачи этого layout-а от decoder-а к renderer-у.
    pub transfer_path: VideoFrameTransferPath,
}

impl VideoFrameContract {
    /// Создаёт NV12 DMA-BUF zero-copy contract.
    #[must_use]
    pub const fn dma_buf_nv12(image_layout: DmaBufImageLayout) -> Self {
        Self {
            pixel_layout: VideoFramePixelLayout::Nv12,
            transfer_path: VideoFrameTransferPath::dma_buf_zero_copy(image_layout),
        }
    }

    /// Создаёт P010 DMA-BUF zero-copy contract.
    #[must_use]
    pub const fn dma_buf_p010(image_layout: DmaBufImageLayout) -> Self {
        Self {
            pixel_layout: VideoFramePixelLayout::P010,
            transfer_path: VideoFrameTransferPath::dma_buf_zero_copy(image_layout),
        }
    }

    /// Создаёт host-upload contract для 8-bit planar YUV420.
    #[must_use]
    pub const fn host_yuv420_planar8() -> Self {
        Self {
            pixel_layout: VideoFramePixelLayout::Yuv420Planar8,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        }
    }

    /// Создаёт host-upload contract для 10-bit little-endian planar YUV420.
    #[must_use]
    pub const fn host_yuv420_planar10le() -> Self {
        Self {
            pixel_layout: VideoFramePixelLayout::Yuv420Planar10Le,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        }
    }

    /// Проверяет self-consistency между pixel layout и transfer path.
    pub const fn validate(self) -> Result<(), VideoFrameContractValidationError> {
        match (self.pixel_layout, self.transfer_path) {
            (
                VideoFramePixelLayout::Nv12 | VideoFramePixelLayout::P010,
                VideoFrameTransferPath::HardwareZeroCopy { .. },
            ) => Ok(()),
            (
                VideoFramePixelLayout::Yuv420Planar8 | VideoFramePixelLayout::Yuv420Planar10Le,
                VideoFrameTransferPath::SoftwareHostUpload,
            ) => Ok(()),
            (
                pixel_layout @ (VideoFramePixelLayout::Yuv420Planar8
                | VideoFramePixelLayout::Yuv420Planar10Le),
                VideoFrameTransferPath::HardwareZeroCopy { .. },
            ) => Err(
                VideoFrameContractValidationError::HostPlanarRequiresSoftwareUpload {
                    pixel_layout,
                },
            ),
            (
                pixel_layout @ (VideoFramePixelLayout::Nv12 | VideoFramePixelLayout::P010),
                VideoFrameTransferPath::SoftwareHostUpload,
            ) => Err(
                VideoFrameContractValidationError::HardwareLayoutRequiresZeroCopy { pixel_layout },
            ),
            (pixel_layout @ VideoFramePixelLayout::Rgba8, _) => Err(
                VideoFrameContractValidationError::ReservedLayoutWithoutTransferContract {
                    pixel_layout,
                },
            ),
        }
    }

    /// Возвращает stable label всего contract-а для diagnostics.
    #[must_use]
    pub fn diagnostic_label(self) -> String {
        format!("{} via {}", self.pixel_layout, self.transfer_path)
    }
}

impl fmt::Display for VideoFrameContract {
    /// Печатает stable diagnostic label.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.diagnostic_label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dma_buf_contracts_bind_nv12_and_p010_to_hardware_zero_copy() {
        let nv12_contract = VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers);
        let p010_contract = VideoFrameContract::dma_buf_p010(DmaBufImageLayout::ComposedLayers);

        assert_eq!(nv12_contract.pixel_layout, VideoFramePixelLayout::Nv12);
        assert!(matches!(
            nv12_contract.transfer_path,
            VideoFrameTransferPath::HardwareZeroCopy {
                handle: HardwareFrameHandle::DmaBuf {
                    image_layout: DmaBufImageLayout::SeparateLayers
                }
            }
        ));
        assert_eq!(p010_contract.pixel_layout, VideoFramePixelLayout::P010);
        assert!(matches!(
            p010_contract.transfer_path,
            VideoFrameTransferPath::HardwareZeroCopy {
                handle: HardwareFrameHandle::DmaBuf {
                    image_layout: DmaBufImageLayout::ComposedLayers
                }
            }
        ));
        assert_eq!(nv12_contract.validate(), Ok(()));
        assert_eq!(p010_contract.validate(), Ok(()));
    }

    #[test]
    fn software_host_upload_accepts_only_host_planar_yuv420_layouts() {
        let yuv420_planar8 = VideoFrameContract::host_yuv420_planar8();
        let yuv420_planar10le = VideoFrameContract::host_yuv420_planar10le();
        let invalid_nv12 = VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Nv12,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        };
        let invalid_p010 = VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::P010,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        };

        assert_eq!(yuv420_planar8.validate(), Ok(()));
        assert_eq!(yuv420_planar10le.validate(), Ok(()));
        assert!(matches!(
            invalid_nv12.validate(),
            Err(
                VideoFrameContractValidationError::HardwareLayoutRequiresZeroCopy {
                    pixel_layout: VideoFramePixelLayout::Nv12
                }
            )
        ));
        assert!(matches!(
            invalid_p010.validate(),
            Err(
                VideoFrameContractValidationError::HardwareLayoutRequiresZeroCopy {
                    pixel_layout: VideoFramePixelLayout::P010
                }
            )
        ));
    }

    #[test]
    fn host_planar_layouts_do_not_validate_as_dma_buf_zero_copy() {
        let invalid_planar8 = VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Yuv420Planar8,
            transfer_path: VideoFrameTransferPath::dma_buf_zero_copy(
                DmaBufImageLayout::SeparateLayers,
            ),
        };
        let invalid_planar10le = VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Yuv420Planar10Le,
            transfer_path: VideoFrameTransferPath::dma_buf_zero_copy(
                DmaBufImageLayout::SeparateLayers,
            ),
        };

        assert!(matches!(
            invalid_planar8.validate(),
            Err(
                VideoFrameContractValidationError::HostPlanarRequiresSoftwareUpload {
                    pixel_layout: VideoFramePixelLayout::Yuv420Planar8
                }
            )
        ));
        assert!(matches!(
            invalid_planar10le.validate(),
            Err(
                VideoFrameContractValidationError::HostPlanarRequiresSoftwareUpload {
                    pixel_layout: VideoFramePixelLayout::Yuv420Planar10Le
                }
            )
        ));
    }

    #[test]
    fn rgba8_is_reserved_and_not_current_production_contract() {
        let rgba8_contract = VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Rgba8,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        };

        assert!(!VideoFramePixelLayout::Rgba8.is_current_dma_buf_baseline());
        assert!(matches!(
            rgba8_contract.validate(),
            Err(
                VideoFrameContractValidationError::ReservedLayoutWithoutTransferContract {
                    pixel_layout: VideoFramePixelLayout::Rgba8
                }
            )
        ));
    }
}
