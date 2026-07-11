use std::fmt;

use codec_core::{BitDepth, ChromaSubsampling, VideoDecodeRequirement};
use serde::{Deserialize, Serialize};
use video_frame_contract::{
    DmaBufImageLayout, FrameBitDepth, FrameChromaSubsampling, HardwareFrameHandle,
    VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath,
};

use crate::{
    HdrOutputMode, HdrToSdrSettings, HdrToneMappingOperator, RenderFrameContractRejection,
    RenderTextureDimension, RenderVideoOutputRejection, UiCompositionMode,
    color::pixel_layout_supports_phase10_hdr_to_sdr,
};

/// Стабильный идентификатор семейства renderer backend-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderBackendKind {
    /// WGPU backend: Vulkan-first на Linux, с будущими DX12/Metal target-ами.
    Wgpu,

    /// Future OpenGL ES fallback для старых X11/GLES2 систем.
    OpenGles,
}

impl RenderBackendKind {
    /// Возвращает короткое стабильное имя backend-а для diagnostics.
    #[must_use]
    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::Wgpu => "wgpu",
            Self::OpenGles => "opengles",
        }
    }

    /// Возвращает человекочитаемое имя backend-а для UI/report.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Wgpu => "WGPU",
            Self::OpenGles => "OpenGL ES",
        }
    }
}

impl fmt::Display for RenderBackendKind {
    /// Печатает стабильный id без дополнительного форматирования.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.stable_id())
    }
}

/// Состояние P010 на границе decoder/renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum P010RenderReadiness {
    /// P010 path недоступен даже как проверенная zero-copy граница.
    Unavailable,

    /// DMA-BUF/P010 boundary проверен диагностически, но production renderer ещё не рисует P010.
    ZeroCopyBoundaryVerified,

    /// Renderer умеет принять P010 и вывести его в production path.
    Renderable,
}

impl P010RenderReadiness {
    /// Возвращает `true`, только если P010 можно выбирать для production playback.
    #[must_use]
    pub const fn is_renderable(self) -> bool {
        matches!(self, Self::Renderable)
    }

    /// Возвращает короткое описание для capability report.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::Unavailable => "P010 unavailable",
            Self::ZeroCopyBoundaryVerified => {
                "P010 zero-copy boundary verified, render unavailable"
            }
            Self::Renderable => "P010 renderable",
        }
    }
}

impl fmt::Display for P010RenderReadiness {
    /// Печатает стабильное diagnostic-описание P010 readiness.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_label())
    }
}

impl Default for P010RenderReadiness {
    /// Старые reports без P010 readiness безопасно считаются production-неготовыми.
    fn default() -> Self {
        Self::Unavailable
    }
}

/// Возможности одного renderer backend-а.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderCapabilities {
    /// Семейство backend-а.
    pub backend: RenderBackendKind,

    /// Человекочитаемое имя backend-а.
    pub display_name: String,

    /// Полные frame contracts, которые backend может принять без скрытых fallback-ов.
    pub supported_frame_contracts: Vec<VideoFrameContract>,

    /// Состояние P010: diagnostic boundary отдельно от production renderability.
    #[serde(default)]
    pub p010_render_readiness: P010RenderReadiness,

    /// HDR-to-SDR operators, которые backend реально реализует в production shader path.
    #[serde(default)]
    pub supported_hdr_to_sdr_operators: Vec<HdrToneMappingOperator>,

    /// HDR output mode backend-а; Phase 10 разрешает только SDR BT.709.
    #[serde(default)]
    pub hdr_output_mode: HdrOutputMode,

    /// Raw claim backend-а: может ли он выполнить HDR-to-SDR tone mapping.
    ///
    /// Production selection должна использовать `supports_hdr_to_sdr_with`, потому что
    /// один этот flag не доказывает P010 renderer и конкретный operator.
    pub supports_hdr_to_sdr: bool,

    /// Может ли backend отдать native HDR output в swapchain/display.
    pub supports_native_hdr_output: bool,

    /// Максимальный размер 2D texture, если backend его сообщил.
    pub max_texture_size: Option<u32>,

    /// Поддерживает ли backend расширенный UI overlay.
    pub advanced_ui: bool,

    /// Как backend композитит UI.
    pub ui_composition_mode: UiCompositionMode,

    /// Есть ли у backend-а метрики present/frame pacing.
    pub present_timing_metrics: bool,
}

fn wgpu_host_upload_frame_contracts() -> [VideoFrameContract; 8] {
    [
        VideoFrameContract::host_yuv420_planar8(),
        VideoFrameContract::host_yuv420_planar10le(),
        VideoFrameContract::host_yuv420_planar12le(),
        software_host_upload_contract(VideoFramePixelLayout::Yuv422Planar8),
        software_host_upload_contract(VideoFramePixelLayout::Yuv422Planar10Le),
        software_host_upload_contract(VideoFramePixelLayout::Yuv422Planar12Le),
        software_host_upload_contract(VideoFramePixelLayout::Yuv444Planar8),
        software_host_upload_contract(VideoFramePixelLayout::Yuv444Planar10Le),
    ]
}

pub(crate) const fn software_host_upload_contract(
    pixel_layout: VideoFramePixelLayout,
) -> VideoFrameContract {
    VideoFrameContract {
        pixel_layout,
        transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
    }
}

impl RenderCapabilities {
    /// Создаёт capabilities для текущего WGPU MVP backend-а.
    #[must_use]
    pub fn wgpu_nv12(max_texture_size: Option<u32>) -> Self {
        let mut supported_frame_contracts = vec![
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        ];
        supported_frame_contracts.extend(wgpu_host_upload_frame_contracts());

        Self {
            backend: RenderBackendKind::Wgpu,
            display_name: "WGPU NV12 + HostPlanar YUV renderer".to_string(),
            supported_frame_contracts,
            p010_render_readiness: P010RenderReadiness::Unavailable,
            supported_hdr_to_sdr_operators: Vec::new(),
            hdr_output_mode: HdrOutputMode::SdrBt709Only,
            supports_hdr_to_sdr: false,
            supports_native_hdr_output: false,
            max_texture_size,
            advanced_ui: true,
            ui_composition_mode: UiCompositionMode::Overlay,
            present_timing_metrics: true,
        }
    }

    /// Создаёт capabilities для WGPU renderer-а с production P010 BT.2446-C path.
    #[must_use]
    pub fn wgpu_p010_bt2446c(max_texture_size: Option<u32>) -> Self {
        Self::wgpu_p010_bt2446c_with_dma_buf_image_layouts(
            max_texture_size,
            vec![
                DmaBufImageLayout::SeparateLayers,
                DmaBufImageLayout::ComposedLayers,
            ],
        )
    }

    /// Создаёт capabilities для WGPU P010 renderer-а с явными import layout-ами.
    #[must_use]
    pub fn wgpu_p010_bt2446c_with_dma_buf_image_layouts(
        max_texture_size: Option<u32>,
        supported_p010_dma_buf_image_layouts: Vec<DmaBufImageLayout>,
    ) -> Self {
        let mut supported_frame_contracts = vec![
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        ];
        supported_frame_contracts.extend(
            supported_p010_dma_buf_image_layouts
                .into_iter()
                .map(VideoFrameContract::dma_buf_p010),
        );
        supported_frame_contracts.extend(wgpu_host_upload_frame_contracts());

        Self {
            backend: RenderBackendKind::Wgpu,
            display_name: "WGPU P010 BT.2446-C + HostPlanar YUV renderer".to_string(),
            supported_frame_contracts,
            p010_render_readiness: P010RenderReadiness::Renderable,
            supported_hdr_to_sdr_operators: vec![HdrToneMappingOperator::Bt2446C],
            hdr_output_mode: HdrOutputMode::SdrBt709Only,
            supports_hdr_to_sdr: true,
            supports_native_hdr_output: false,
            max_texture_size,
            advanced_ui: true,
            ui_composition_mode: UiCompositionMode::Overlay,
            present_timing_metrics: true,
        }
    }

    /// Проверяет прямую поддержку входного frame format.
    #[must_use]
    pub fn supports_frame_format(&self, format: VideoFramePixelLayout) -> bool {
        self.supported_frame_contracts
            .iter()
            .any(|contract| contract.pixel_layout == format)
    }

    /// Проверяет поддержку полного frame contract-а без stream-level HDR/size policy.
    #[must_use]
    pub fn supports_frame_contract(&self, frame_contract: VideoFrameContract) -> bool {
        self.check_frame_contract(frame_contract).is_ok()
    }

    /// Возвращает техническую причину, если frame contract не входит в renderer boundary.
    pub fn check_frame_contract(
        &self,
        frame_contract: VideoFrameContract,
    ) -> Result<(), RenderFrameContractRejection> {
        frame_contract
            .validate()
            .map_err(|reason| RenderFrameContractRejection::InvalidContract { reason })?;

        if self.supported_frame_contracts.contains(&frame_contract) {
            return Ok(());
        }

        let transfer_family_supported = self.supported_frame_contracts.iter().any(|contract| {
            same_transfer_path_family(contract.transfer_path, frame_contract.transfer_path)
        });
        if !transfer_family_supported {
            return Err(RenderFrameContractRejection::UnsupportedTransferPath {
                transfer_path: frame_contract.transfer_path,
            });
        }

        if !self.supports_frame_format(frame_contract.pixel_layout) {
            return Err(RenderFrameContractRejection::UnsupportedPixelLayout {
                pixel_layout: frame_contract.pixel_layout,
            });
        }

        if let Some(image_layout) = dma_buf_image_layout(frame_contract.transfer_path) {
            let supports_same_pixel_dma_buf =
                self.supported_frame_contracts.iter().any(|contract| {
                    contract.pixel_layout == frame_contract.pixel_layout
                        && dma_buf_image_layout(contract.transfer_path).is_some()
                });
            if supports_same_pixel_dma_buf {
                return Err(RenderFrameContractRejection::UnsupportedDmaBufImageLayout {
                    pixel_layout: frame_contract.pixel_layout,
                    image_layout,
                });
            }
        }

        Err(RenderFrameContractRejection::UnsupportedContractCombination { frame_contract })
    }

    /// Проверяет, что P010 доступен именно как production-renderable path.
    #[must_use]
    pub fn supports_p010_rendering(&self) -> bool {
        self.p010_render_readiness.is_renderable()
            && self.supported_frame_contracts.iter().any(|contract| {
                contract.pixel_layout == VideoFramePixelLayout::P010
                    && dma_buf_image_layout(contract.transfer_path).is_some()
            })
    }

    /// Проверяет, что renderer умеет импортировать конкретный P010 storage layout.
    #[must_use]
    pub fn supports_p010_storage_layout(&self, layout: DmaBufImageLayout) -> bool {
        self.supports_p010_rendering()
            && self.supports_frame_contract(VideoFrameContract::dma_buf_p010(layout))
    }

    /// Проверяет production-ready HDR-to-SDR support для конкретных settings.
    #[must_use]
    pub fn supports_hdr_to_sdr_with(&self, settings: &HdrToSdrSettings) -> bool {
        self.supports_hdr_to_sdr
            && self.supports_p010_rendering()
            && settings.is_phase10_bt2446_c_sdr_bt709()
            && self.hdr_output_mode == settings.output_mode
            && self
                .supported_hdr_to_sdr_operators
                .contains(&settings.operator)
    }

    /// Проверяет stream-level renderability для уже выбранного frame contract-а.
    #[must_use]
    pub fn supports_video_output(
        &self,
        requirement: &VideoDecodeRequirement,
        frame_contract: VideoFrameContract,
    ) -> bool {
        self.check_video_output(requirement, frame_contract).is_ok()
    }

    /// Возвращает техническую причину stream-level renderer rejection-а.
    pub fn check_video_output(
        &self,
        requirement: &VideoDecodeRequirement,
        frame_contract: VideoFrameContract,
    ) -> Result<(), RenderVideoOutputRejection> {
        if let Err(reason) = self.check_frame_contract(frame_contract) {
            return Err(RenderVideoOutputRejection::FrameContract { reason });
        }

        check_frame_contract_matches_requirement(requirement, frame_contract)?;

        if frame_contract.pixel_layout == VideoFramePixelLayout::P010
            && !self.supports_p010_rendering()
        {
            return Err(RenderVideoOutputRejection::P010NotRenderable {
                readiness: self.p010_render_readiness,
            });
        }

        if requirement.hdr
            && !((self.supports_hdr_to_sdr_with(&HdrToSdrSettings::default())
                && frame_contract_supports_hdr_to_sdr(frame_contract))
                || self.supports_native_hdr_output)
        {
            return Err(RenderVideoOutputRejection::HdrUnsupported { frame_contract });
        }

        if let (Some(width), Some(max_texture_size)) = (requirement.width, self.max_texture_size)
            && width > max_texture_size
        {
            return Err(RenderVideoOutputRejection::MaxTextureSizeExceeded {
                dimension: RenderTextureDimension::Width,
                requested: width,
                max_texture_size,
            });
        }

        if let (Some(height), Some(max_texture_size)) = (requirement.height, self.max_texture_size)
            && height > max_texture_size
        {
            return Err(RenderVideoOutputRejection::MaxTextureSizeExceeded {
                dimension: RenderTextureDimension::Height,
                requested: height,
                max_texture_size,
            });
        }

        Ok(())
    }

    /// Формирует одну строку diagnostics для capability report.
    #[must_use]
    pub fn summary_text(&self) -> String {
        let frame_contracts = self
            .supported_frame_contracts
            .iter()
            .map(|contract| contract.diagnostic_label())
            .collect::<Vec<_>>()
            .join(", ");

        let hdr_support_label = if self.supports_hdr_to_sdr_with(&HdrToSdrSettings::default()) {
            "HDR available via HDR-to-SDR"
        } else if self.supports_native_hdr_output {
            "HDR available via native HDR output"
        } else {
            "SDR only, HDR unavailable"
        };
        let native_hdr_label = if self.supports_native_hdr_output {
            "native HDR supported"
        } else {
            "native HDR unsupported"
        };

        format!(
            "{}: {}, {}, {}, frame contracts: {}, max texture: {}",
            self.display_name,
            hdr_support_label,
            native_hdr_label,
            self.p010_render_readiness,
            frame_contracts,
            self.max_texture_size
                .map(|size| size.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        )
    }
}

/// Сравнивает family transfer path-а без смешивания concrete layout-ов.
fn same_transfer_path_family(left: VideoFrameTransferPath, right: VideoFrameTransferPath) -> bool {
    match (left, right) {
        (
            VideoFrameTransferPath::SoftwareHostUpload,
            VideoFrameTransferPath::SoftwareHostUpload,
        ) => true,
        (
            VideoFrameTransferPath::HardwareZeroCopy { handle: left },
            VideoFrameTransferPath::HardwareZeroCopy { handle: right },
        ) => same_hardware_handle_family(left, right),
        _ => false,
    }
}

/// Сравнивает hardware handle family, не считая layout details равными.
const fn same_hardware_handle_family(
    left: HardwareFrameHandle,
    right: HardwareFrameHandle,
) -> bool {
    matches!(
        (left, right),
        (
            HardwareFrameHandle::DmaBuf { .. },
            HardwareFrameHandle::DmaBuf { .. }
        )
    )
}

/// Достаёт DMA-BUF layout только из DMA-BUF transfer path-а.
const fn dma_buf_image_layout(transfer_path: VideoFrameTransferPath) -> Option<DmaBufImageLayout> {
    match transfer_path {
        VideoFrameTransferPath::HardwareZeroCopy {
            handle: HardwareFrameHandle::DmaBuf { image_layout },
        } => Some(image_layout),
        VideoFrameTransferPath::SoftwareHostUpload => None,
    }
}

/// Проверяет, что выбранный renderer contract совпадает с codec-level metadata stream-а.
fn check_frame_contract_matches_requirement(
    requirement: &VideoDecodeRequirement,
    frame_contract: VideoFrameContract,
) -> Result<(), RenderVideoOutputRejection> {
    let expected_bit_depth = required_frame_bit_depth(requirement);
    let expected_chroma = required_frame_chroma(requirement);

    if frame_contract.pixel_layout.bit_depth() != Some(expected_bit_depth)
        || frame_contract.pixel_layout.chroma() != Some(expected_chroma)
    {
        return Err(RenderVideoOutputRejection::FrameContract {
            reason: RenderFrameContractRejection::UnsupportedContractCombination { frame_contract },
        });
    }

    Ok(())
}

/// Проверяет, что конкретный frame contract имеет GPU HDR-to-SDR shader path.
fn frame_contract_supports_hdr_to_sdr(frame_contract: VideoFrameContract) -> bool {
    pixel_layout_supports_phase10_hdr_to_sdr(frame_contract.pixel_layout)
}

/// Переводит stream bit depth в neutral frame-contract vocabulary.
fn required_frame_bit_depth(requirement: &VideoDecodeRequirement) -> FrameBitDepth {
    match requirement.bit_depth.unwrap_or(BitDepth::Eight) {
        BitDepth::Eight => FrameBitDepth::Eight,
        BitDepth::Ten => FrameBitDepth::Ten,
        BitDepth::Twelve => FrameBitDepth::Twelve,
    }
}

/// Переводит stream chroma в neutral frame-contract vocabulary.
fn required_frame_chroma(requirement: &VideoDecodeRequirement) -> FrameChromaSubsampling {
    match requirement.chroma.unwrap_or(ChromaSubsampling::Yuv420) {
        ChromaSubsampling::Yuv420 => FrameChromaSubsampling::Yuv420,
        ChromaSubsampling::Yuv422 => FrameChromaSubsampling::Yuv422,
        ChromaSubsampling::Yuv444 => FrameChromaSubsampling::Yuv444,
    }
}
#[cfg(test)]
#[path = "tests/capabilities.rs"]
mod tests;
