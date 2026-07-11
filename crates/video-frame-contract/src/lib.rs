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

    /// 12 бит на sample, обычно в 16-битном little-endian storage word-е.
    Twelve,
}

impl FrameBitDepth {
    /// Возвращает человекочитаемую diagnostic label.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::Eight => "8-bit",
            Self::Ten => "10-bit",
            Self::Twelve => "12-bit",
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

    /// YUV 4:2:2.
    Yuv422,

    /// YUV 4:4:4.
    Yuv444,
}

impl FrameChromaSubsampling {
    /// Возвращает человекочитаемую diagnostic label.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::Yuv420 => "YUV 4:2:0",
            Self::Yuv422 => "YUV 4:2:2",
            Self::Yuv444 => "YUV 4:4:4",
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

    /// 12-bit 4:2:0 planar host layout: little-endian 16-bit storage words.
    Yuv420Planar12Le,

    /// 8-bit 4:2:2 planar host layout: Y, U, V planes.
    Yuv422Planar8,

    /// 10-bit 4:2:2 planar host layout: little-endian 16-bit storage words.
    Yuv422Planar10Le,

    /// 12-bit 4:2:2 planar host layout: little-endian 16-bit storage words.
    Yuv422Planar12Le,

    /// 8-bit 4:4:4 planar host layout: Y, U, V planes.
    Yuv444Planar8,

    /// 10-bit 4:4:4 planar host layout: little-endian 16-bit storage words.
    Yuv444Planar10Le,

    /// Reserved packed RGBA layout for future producers, not current production.
    Rgba8,
}

impl VideoFramePixelLayout {
    /// Выводит current hardware baseline layout из frame-level bit depth/chroma.
    ///
    /// Этот helper намеренно не выбирает host-planar layout-ы: такой выбор уже
    /// является capability policy конкретного software producer/renderer pair.
    #[must_use]
    pub const fn hardware_baseline_from_frame_bit_depth_and_chroma(
        bit_depth: FrameBitDepth,
        chroma: FrameChromaSubsampling,
    ) -> Option<Self> {
        match (bit_depth, chroma) {
            (FrameBitDepth::Eight, FrameChromaSubsampling::Yuv420) => Some(Self::Nv12),
            (FrameBitDepth::Ten, FrameChromaSubsampling::Yuv420) => Some(Self::P010),
            _ => None,
        }
    }

    /// Выводит current hardware baseline layout из frame-level bit depth/chroma.
    ///
    /// Оставлено как compatibility name для существующих call site-ов:
    /// unsupported bit-depth/chroma пары возвращают `None`, а не guessed host layout.
    #[must_use]
    pub const fn from_frame_bit_depth_and_chroma(
        bit_depth: FrameBitDepth,
        chroma: FrameChromaSubsampling,
    ) -> Option<Self> {
        Self::hardware_baseline_from_frame_bit_depth_and_chroma(bit_depth, chroma)
    }

    /// Возвращает bit depth, если layout относится к known frame vocabulary.
    #[must_use]
    pub const fn bit_depth(self) -> Option<FrameBitDepth> {
        match self {
            Self::Nv12
            | Self::Yuv420Planar8
            | Self::Yuv422Planar8
            | Self::Yuv444Planar8
            | Self::Rgba8 => Some(FrameBitDepth::Eight),
            Self::P010
            | Self::Yuv420Planar10Le
            | Self::Yuv422Planar10Le
            | Self::Yuv444Planar10Le => Some(FrameBitDepth::Ten),
            Self::Yuv420Planar12Le | Self::Yuv422Planar12Le => Some(FrameBitDepth::Twelve),
        }
    }

    /// Возвращает chroma subsampling, если layout относится к YUV frame vocabulary.
    #[must_use]
    pub const fn chroma(self) -> Option<FrameChromaSubsampling> {
        match self {
            Self::Nv12
            | Self::P010
            | Self::Yuv420Planar8
            | Self::Yuv420Planar10Le
            | Self::Yuv420Planar12Le => Some(FrameChromaSubsampling::Yuv420),
            Self::Yuv422Planar8 | Self::Yuv422Planar10Le | Self::Yuv422Planar12Le => {
                Some(FrameChromaSubsampling::Yuv422)
            }
            Self::Yuv444Planar8 | Self::Yuv444Planar10Le => Some(FrameChromaSubsampling::Yuv444),
            Self::Rgba8 => None,
        }
    }

    /// Проверяет, что layout описывает CPU-visible planar host storage.
    #[must_use]
    pub const fn is_host_planar(self) -> bool {
        matches!(
            self,
            Self::Yuv420Planar8
                | Self::Yuv420Planar10Le
                | Self::Yuv420Planar12Le
                | Self::Yuv422Planar8
                | Self::Yuv422Planar10Le
                | Self::Yuv422Planar12Le
                | Self::Yuv444Planar8
                | Self::Yuv444Planar10Le
        )
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
            Self::Yuv420Planar12Le => "YUV420 planar 12-bit little-endian",
            Self::Yuv422Planar8 => "YUV422 planar 8-bit",
            Self::Yuv422Planar10Le => "YUV422 planar 10-bit little-endian",
            Self::Yuv422Planar12Le => "YUV422 planar 12-bit little-endian",
            Self::Yuv444Planar8 => "YUV444 planar 8-bit",
            Self::Yuv444Planar10Le => "YUV444 planar 10-bit little-endian",
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
    /// Image приходит как composed multi-plane image в одном DMA-BUF object-е.
    ComposedLayers,

    /// Composed multi-plane image требует нескольких DMA-BUF objects/memory binds.
    ///
    /// Variant существует для точной capability rejection; текущий Vulkan importer
    /// намеренно его не поддерживает и renderer не рекламирует такой contract.
    ComposedMultiObject,

    /// Image приходит как отдельные importable layers/planes.
    SeparateLayers,
}

impl DmaBufImageLayout {
    /// Возвращает stable label layout-а для diagnostics.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::ComposedLayers => "composed DMA-BUF layers",
            Self::ComposedMultiObject => "composed multi-object DMA-BUF layers",
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

    /// Создаёт host-upload contract для 12-bit little-endian planar YUV420.
    #[must_use]
    pub const fn host_yuv420_planar12le() -> Self {
        Self {
            pixel_layout: VideoFramePixelLayout::Yuv420Planar12Le,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        }
    }

    /// Проверяет self-consistency между pixel layout и transfer path.
    pub const fn validate(self) -> Result<(), VideoFrameContractValidationError> {
        match self.transfer_path {
            VideoFrameTransferPath::HardwareZeroCopy { .. } => {
                if self.pixel_layout.is_current_dma_buf_baseline() {
                    Ok(())
                } else if self.pixel_layout.is_host_planar() {
                    Err(
                        VideoFrameContractValidationError::HostPlanarRequiresSoftwareUpload {
                            pixel_layout: self.pixel_layout,
                        },
                    )
                } else {
                    Err(
                        VideoFrameContractValidationError::ReservedLayoutWithoutTransferContract {
                            pixel_layout: self.pixel_layout,
                        },
                    )
                }
            }
            VideoFrameTransferPath::SoftwareHostUpload => {
                if self.pixel_layout.is_host_planar() {
                    Ok(())
                } else if self.pixel_layout.is_current_dma_buf_baseline() {
                    Err(
                        VideoFrameContractValidationError::HardwareLayoutRequiresZeroCopy {
                            pixel_layout: self.pixel_layout,
                        },
                    )
                } else {
                    Err(
                        VideoFrameContractValidationError::ReservedLayoutWithoutTransferContract {
                            pixel_layout: self.pixel_layout,
                        },
                    )
                }
            }
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

    const EXPLICIT_HOST_PLANAR_LAYOUTS: [VideoFramePixelLayout; 8] = [
        VideoFramePixelLayout::Yuv420Planar8,
        VideoFramePixelLayout::Yuv420Planar10Le,
        VideoFramePixelLayout::Yuv420Planar12Le,
        VideoFramePixelLayout::Yuv422Planar8,
        VideoFramePixelLayout::Yuv422Planar10Le,
        VideoFramePixelLayout::Yuv422Planar12Le,
        VideoFramePixelLayout::Yuv444Planar8,
        VideoFramePixelLayout::Yuv444Planar10Le,
    ];

    fn software_host_upload_contract(pixel_layout: VideoFramePixelLayout) -> VideoFrameContract {
        VideoFrameContract {
            pixel_layout,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        }
    }

    fn dma_buf_zero_copy_contract(pixel_layout: VideoFramePixelLayout) -> VideoFrameContract {
        VideoFrameContract {
            pixel_layout,
            transfer_path: VideoFrameTransferPath::dma_buf_zero_copy(
                DmaBufImageLayout::SeparateLayers,
            ),
        }
    }

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
    fn host_yuv420_contract_helpers_bind_exact_software_layouts() {
        let contracts = [
            (
                VideoFrameContract::host_yuv420_planar8(),
                VideoFramePixelLayout::Yuv420Planar8,
            ),
            (
                VideoFrameContract::host_yuv420_planar10le(),
                VideoFramePixelLayout::Yuv420Planar10Le,
            ),
            (
                VideoFrameContract::host_yuv420_planar12le(),
                VideoFramePixelLayout::Yuv420Planar12Le,
            ),
        ];

        for (contract, expected_layout) in contracts {
            assert_eq!(contract.pixel_layout, expected_layout);
            assert_eq!(
                contract.transfer_path,
                VideoFrameTransferPath::SoftwareHostUpload
            );
            assert_eq!(contract.validate(), Ok(()));
        }
    }

    #[test]
    fn hardware_baseline_helper_does_not_guess_software_layouts() {
        let unsupported_hardware_baseline_pairs = [
            (FrameBitDepth::Twelve, FrameChromaSubsampling::Yuv420),
            (FrameBitDepth::Eight, FrameChromaSubsampling::Yuv422),
            (FrameBitDepth::Ten, FrameChromaSubsampling::Yuv422),
            (FrameBitDepth::Twelve, FrameChromaSubsampling::Yuv422),
            (FrameBitDepth::Eight, FrameChromaSubsampling::Yuv444),
            (FrameBitDepth::Ten, FrameChromaSubsampling::Yuv444),
            (FrameBitDepth::Twelve, FrameChromaSubsampling::Yuv444),
        ];

        assert_eq!(
            VideoFramePixelLayout::hardware_baseline_from_frame_bit_depth_and_chroma(
                FrameBitDepth::Eight,
                FrameChromaSubsampling::Yuv420,
            ),
            Some(VideoFramePixelLayout::Nv12)
        );
        assert_eq!(
            VideoFramePixelLayout::hardware_baseline_from_frame_bit_depth_and_chroma(
                FrameBitDepth::Ten,
                FrameChromaSubsampling::Yuv420,
            ),
            Some(VideoFramePixelLayout::P010)
        );
        for (bit_depth, chroma) in unsupported_hardware_baseline_pairs {
            assert_eq!(
                VideoFramePixelLayout::hardware_baseline_from_frame_bit_depth_and_chroma(
                    bit_depth, chroma,
                ),
                None
            );
            assert_eq!(
                VideoFramePixelLayout::from_frame_bit_depth_and_chroma(bit_depth, chroma),
                None
            );
        }
    }

    #[test]
    fn new_planar_layouts_expose_expected_bit_depth_and_chroma() {
        let expected_layout_metadata = [
            (
                VideoFramePixelLayout::Yuv420Planar12Le,
                FrameBitDepth::Twelve,
                FrameChromaSubsampling::Yuv420,
            ),
            (
                VideoFramePixelLayout::Yuv422Planar8,
                FrameBitDepth::Eight,
                FrameChromaSubsampling::Yuv422,
            ),
            (
                VideoFramePixelLayout::Yuv422Planar10Le,
                FrameBitDepth::Ten,
                FrameChromaSubsampling::Yuv422,
            ),
            (
                VideoFramePixelLayout::Yuv422Planar12Le,
                FrameBitDepth::Twelve,
                FrameChromaSubsampling::Yuv422,
            ),
            (
                VideoFramePixelLayout::Yuv444Planar8,
                FrameBitDepth::Eight,
                FrameChromaSubsampling::Yuv444,
            ),
            (
                VideoFramePixelLayout::Yuv444Planar10Le,
                FrameBitDepth::Ten,
                FrameChromaSubsampling::Yuv444,
            ),
        ];

        for (pixel_layout, bit_depth, chroma) in expected_layout_metadata {
            assert_eq!(pixel_layout.bit_depth(), Some(bit_depth));
            assert_eq!(pixel_layout.chroma(), Some(chroma));
            assert!(pixel_layout.is_host_planar());
            assert!(!pixel_layout.diagnostic_label().is_empty());
            assert_eq!(pixel_layout.display_name(), pixel_layout.diagnostic_label());
        }
    }

    #[test]
    fn software_host_upload_accepts_all_explicit_planar_yuv_layouts() {
        for pixel_layout in EXPLICIT_HOST_PLANAR_LAYOUTS {
            let contract = software_host_upload_contract(pixel_layout);

            assert_eq!(contract.validate(), Ok(()));
        }
    }

    #[test]
    fn host_planar_layouts_do_not_validate_as_dma_buf_zero_copy() {
        for pixel_layout in EXPLICIT_HOST_PLANAR_LAYOUTS {
            let contract = dma_buf_zero_copy_contract(pixel_layout);

            assert!(matches!(
                contract.validate(),
                Err(
                    VideoFrameContractValidationError::HostPlanarRequiresSoftwareUpload {
                        pixel_layout: rejected_pixel_layout
                    }
                ) if rejected_pixel_layout == pixel_layout
            ));
        }
    }

    #[test]
    fn software_host_upload_rejects_hardware_and_reserved_layouts() {
        let invalid_nv12 = software_host_upload_contract(VideoFramePixelLayout::Nv12);
        let invalid_p010 = software_host_upload_contract(VideoFramePixelLayout::P010);
        let invalid_rgba8 = software_host_upload_contract(VideoFramePixelLayout::Rgba8);

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
        assert!(matches!(
            invalid_rgba8.validate(),
            Err(
                VideoFrameContractValidationError::ReservedLayoutWithoutTransferContract {
                    pixel_layout: VideoFramePixelLayout::Rgba8
                }
            )
        ));
    }

    #[test]
    fn rgba8_is_reserved_and_not_current_production_contract() {
        let rgba8_contract = dma_buf_zero_copy_contract(VideoFramePixelLayout::Rgba8);

        assert!(!VideoFramePixelLayout::Rgba8.is_host_planar());
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
