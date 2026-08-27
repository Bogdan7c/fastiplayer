use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail, ensure};
use codec_core::VideoDisplayOrientation;
use render_core::{
    ColorPipelineSettings, HdrToSdrSettings, RenderCapabilities, RenderDiagnostics,
    RenderLiveApplyPhase, RenderLiveApplyReport, RenderLiveSettingId, RenderLiveSettings,
    RenderLiveSettingsAdapter, RenderLiveSettingsError, RenderLiveSettingsUpdate, RenderViewport,
    RenderableFrame, SwapchainTransferMode, ToneMappingMode,
};
#[cfg(test)]
use video_core::FrameResourceDescriptor;
use video_core::{
    DecodedFrame, DecodedPixelFormat, DmaBufDescriptorRejection, FrameResourceHandle,
};
use video_frame_contract::{HardwareFrameHandle, VideoFramePixelLayout, VideoFrameTransferPath};

use crate::capabilities::wgpu_capabilities_from_features;
use crate::dma_buf_import::ImportedDmaBufTexture;

pub use self::dma_buf_materializer::DmaBufWgpuFrameMaterializer;
#[cfg(test)]
use self::dma_buf_materializer::unsupported_lookup_for_non_dma_buf_descriptor;
pub use self::host_planar_upload::{
    HostPlanarWgpuFrameMaterializer, HostPlanarWgpuTextureViewLookup, HostPlanarWgpuTextureViews,
};
use self::host_yuv420_renderer::HostPlanarYuvVideoRenderer;
use self::nv12_renderer::Nv12VideoRenderer;
use self::p010_renderer::P010VideoRenderer;

mod dma_buf_materializer;
mod host_planar_upload;
mod host_yuv420_renderer;
mod nv12_renderer;
mod p010_renderer;

/// WGPU texture views, полученные renderer materializer-ом по decoded frame resource.
#[derive(Clone)]
pub struct WgpuFrameTextureViews {
    /// Concrete storage отличается для DMA-BUF и HostPlanar, но app видит один тип.
    storage: WgpuFrameTextureViewStorage,
}

#[derive(Clone)]
enum WgpuFrameTextureViewStorage {
    /// DMA-BUF import даёт Y + interleaved UV texture views.
    DmaBuf {
        /// Texture view с luma/Y plane.
        y_view: wgpu::TextureView,

        /// Texture view с chroma/UV plane.
        uv_view: wgpu::TextureView,

        /// Guard держит imported texture storage живым как минимум до drop-а views.
        _imported_texture_guard: Option<Arc<ImportedDmaBufTexture>>,
    },

    /// HostPlanar upload даёт отдельные Y/U/V texture views.
    HostPlanar(Box<HostPlanarWgpuTextureViews>),
}

impl WgpuFrameTextureViews {
    /// Создаёт views из renderer-owned imported texture.
    fn from_imported_texture(imported_texture: Arc<ImportedDmaBufTexture>) -> Self {
        let _owned_storage_textures = imported_texture.storage_texture_count();
        Self {
            storage: WgpuFrameTextureViewStorage::DmaBuf {
                y_view: imported_texture.y_view.clone(),
                uv_view: imported_texture.uv_view.clone(),
                _imported_texture_guard: Some(imported_texture),
            },
        }
    }

    /// Создаёт views из renderer-owned HostPlanar upload textures.
    fn from_host_planar_texture_views(views: Box<HostPlanarWgpuTextureViews>) -> Self {
        Self {
            storage: WgpuFrameTextureViewStorage::HostPlanar(views),
        }
    }

    /// Возвращает DMA-BUF Y/UV views, если materializer создал именно DMA-BUF storage.
    #[must_use]
    pub fn dma_buf_views(&self) -> Option<(&wgpu::TextureView, &wgpu::TextureView)> {
        match &self.storage {
            WgpuFrameTextureViewStorage::DmaBuf {
                y_view, uv_view, ..
            } => Some((y_view, uv_view)),
            WgpuFrameTextureViewStorage::HostPlanar(_) => None,
        }
    }

    /// Возвращает HostPlanar Y/U/V views, если materializer выполнил software upload.
    #[must_use]
    pub fn host_planar_views(
        &self,
    ) -> Option<(&wgpu::TextureView, &wgpu::TextureView, &wgpu::TextureView)> {
        match &self.storage {
            WgpuFrameTextureViewStorage::DmaBuf { .. } => None,
            WgpuFrameTextureViewStorage::HostPlanar(views) => {
                Some((&views.y_view, &views.u_view, &views.v_view))
            }
        }
    }
}

/// Почему конкретный descriptor не подходит этому WGPU materializer-у.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WgpuFrameMaterializationUnsupportedReason {
    /// Descriptor содержит CPU-visible host planes, а этот materializer импортирует только DMA-BUF.
    HostPlanarRequiresUploadMaterializer,

    /// Descriptor содержит DMA-BUF, а этот materializer загружает только HostPlanar.
    DmaBufRequiresDmaBufMaterializer,

    /// HostPlanar frame пришёл не через software host-upload contract.
    HostPlanarRequiresSoftwareUploadContract,

    /// HostPlanar layout пока не входит в минимальный upload subset.
    HostPlanarLayoutNotSupportedByUploadMaterializer,

    /// DMA-BUF topology либо frame contract отклонены до Vulkan import-а.
    DmaBufDescriptorRejected(DmaBufDescriptorRejection),
}

impl WgpuFrameMaterializationUnsupportedReason {
    /// Stable diagnostic label без user-facing текста.
    #[must_use]
    pub const fn diagnostic_label(&self) -> &'static str {
        match self {
            Self::HostPlanarRequiresUploadMaterializer => {
                "host planar descriptor requires upload materializer"
            }
            Self::DmaBufRequiresDmaBufMaterializer => {
                "DMA-BUF descriptor requires DMA-BUF materializer"
            }
            Self::HostPlanarRequiresSoftwareUploadContract => {
                "host planar descriptor requires software host-upload contract"
            }
            Self::HostPlanarLayoutNotSupportedByUploadMaterializer => {
                "host planar layout is not supported by upload materializer"
            }
            Self::DmaBufDescriptorRejected(_) => {
                "DMA-BUF descriptor is incompatible with the decoded frame contract"
            }
        }
    }
}

/// Результат WGPU materialization без участия playback core.
pub enum WgpuFrameTextureViewLookup {
    /// Backend вернул валидные plane views для renderer-а.
    Ready {
        /// Views, которые можно передать WGPU render backend-у.
        views: WgpuFrameTextureViews,

        /// Сколько render thread ждал lock texture pool-а внутри backend provider-а.
        texture_pool_lock_wait: Duration,
    },

    /// Backend texture pool занят, render hot path должен выбрать fallback без ожидания.
    Busy {
        /// Сколько заняла non-blocking попытка получить lock.
        texture_pool_lock_wait: Duration,
    },

    /// Backend доступен, но views для handle отсутствуют.
    Missing {
        /// Сколько render thread ждал lock texture pool-а внутри backend provider-а.
        texture_pool_lock_wait: Duration,
    },

    /// Descriptor существует, но этот materializer не умеет такой resource kind.
    Unsupported {
        /// Техническая причина отказа materializer-а.
        reason: WgpuFrameMaterializationUnsupportedReason,

        /// Сколько render thread ждал lock texture pool-а внутри backend provider-а.
        texture_pool_lock_wait: Duration,
    },

    /// Backend обнаружил poisoned/fatal state при lookup-е.
    Error {
        /// Сколько render thread ждал lock texture pool-а внутри backend provider-а.
        texture_pool_lock_wait: Duration,
    },
}

impl WgpuFrameTextureViewLookup {
    /// Возвращает lock wait sample независимо от lookup outcome.
    #[must_use]
    pub const fn texture_pool_lock_wait(&self) -> Duration {
        match self {
            Self::Ready {
                texture_pool_lock_wait,
                ..
            }
            | Self::Busy {
                texture_pool_lock_wait,
            }
            | Self::Missing {
                texture_pool_lock_wait,
            }
            | Self::Unsupported {
                texture_pool_lock_wait,
                ..
            }
            | Self::Error {
                texture_pool_lock_wait,
            } => *texture_pool_lock_wait,
        }
    }

    /// Возвращает `true`, если materialization не стала ждать занятый backend lock.
    #[must_use]
    pub const fn lookup_was_busy(&self) -> bool {
        matches!(self, Self::Busy { .. })
    }
}

/// WGPU-side materializer plane views из backend-owned opaque frame resource-а.
pub trait WgpuFrameTextureViewMaterializer: Send + Sync {
    /// Пытается materialize frame resource без ожидания backend texture pool mutex-а.
    fn try_texture_view_lookup(&self, frame: &DecodedFrame) -> WgpuFrameTextureViewLookup;

    /// Готовит новый materializer для другого WGPU device, сохраняя neutral provider.
    ///
    /// Метод не меняет active materializer и поэтому безопасен до transactional commit-а.
    fn recreate_for_renderer(
        &self,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Arc<dyn WgpuFrameTextureViewMaterializer>;
}

/// GPU context одного video render pass-а.
///
/// Facade собирает эти ссылки один раз, а конкретные renderer paths получают уже
/// сгруппированную границу вместо длинного списка device/queue/target/encoder.
pub(crate) struct VideoRenderPassContext<'pass> {
    /// Swapchain texture view, в который renderer пишет video pass.
    pub(crate) target: &'pass wgpu::TextureView,

    /// Command encoder текущего кадра.
    pub(crate) encoder: &'pass mut wgpu::CommandEncoder,

    /// WGPU device для создания per-frame bind groups.
    pub(crate) device: &'pass wgpu::Device,

    /// WGPU queue для обновления uniform buffers.
    pub(crate) queue: &'pass wgpu::Queue,

    /// Уже зажатая область, по которой video shader сохраняет aspect ratio.
    pub(crate) viewport: RenderViewport,

    /// Видимые scissor-области, в которых video pass реально рисует кадр.
    pub(crate) draw_rects: Vec<RenderViewport>,

    /// LoadOp render target-а для main/overlay video pass-а.
    pub(crate) target_load: VideoRenderTargetLoad,
}

/// Явный load mode video pass-а, чтобы overlay preview не очищал main surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoRenderTargetLoad {
    /// Main video pass начинает кадр с black clear.
    ClearBlack,

    /// Overlay pass сохраняет уже нарисованные video/egui pixels.
    LoadExisting,
}

impl VideoRenderTargetLoad {
    /// Сколько независимых uniform slot-ов нужно на pass-роли одного кадра.
    pub(crate) const UNIFORM_SLOT_COUNT: usize = 2;

    /// Преобразует intent в WGPU attachment load operation.
    pub(crate) const fn as_wgpu_load_op(self) -> wgpu::LoadOp<wgpu::Color> {
        match self {
            Self::ClearBlack => wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            Self::LoadExisting => wgpu::LoadOp::Load,
        }
    }

    /// Индекс uniform buffer-а для main/overlay pass-а одного кадра.
    ///
    /// `queue.write_buffer` исполняется на submit ДО записанного command
    /// buffer-а, поэтому main video pass и дополнительный overlay pass в одном
    /// кадре НЕ могут делить один uniform buffer: последняя запись (letterbox
    /// overlay-я) применилась бы к обоим pass-ам и ломала пропорции основного
    /// видео. Каждая pass-роль пишет в собственный slot.
    pub(crate) const fn uniform_slot(self) -> usize {
        match self {
            Self::ClearBlack => 0,
            Self::LoadExisting => 1,
        }
    }
}

/// Создаёт по uniform buffer-у на каждую pass-роль (main/overlay).
///
/// См. `VideoRenderTargetLoad::uniform_slot`: общий buffer между pass-ами
/// одного submit-а недопустим из-за submission-time семантики `write_buffer`.
pub(crate) fn create_pass_uniform_buffers(
    device: &wgpu::Device,
    base_label: &str,
    size: u64,
) -> [wgpu::Buffer; VideoRenderTargetLoad::UNIFORM_SLOT_COUNT] {
    std::array::from_fn(|slot| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{base_label} (pass slot {slot})")),
            size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    })
}

/// Контракт одного video render pass-а между shell и renderer.
pub struct WgpuVideoRenderInput<'pass, 'frame> {
    /// Готовый renderable frame или `None`, если target надо очистить.
    pub frame: Option<&'pass WgpuRenderableFrame<'frame>>,

    /// Viewport video layer-а в physical pixels до renderer-side clamp-а.
    pub video_viewport: RenderViewport,

    /// Области UI, которые должны исключаться из video shading.
    pub video_exclusion_rects: &'pass [RenderViewport],

    /// Surface texture view текущего swapchain frame-а.
    pub target: &'pass wgpu::TextureView,

    /// Command encoder текущего frame-а.
    pub encoder: &'pass mut wgpu::CommandEncoder,

    /// WGPU device текущего renderer-а.
    pub device: &'pass wgpu::Device,

    /// WGPU queue текущего renderer-а.
    pub queue: &'pass wgpu::Queue,
}

/// Backend-specific texture resources для одного кадра.
pub enum WgpuFramePlanes<'frame> {
    /// NV12 frame: отдельная luma plane и interleaved chroma plane.
    Nv12 {
        /// Texture view с Y/luma plane.
        y_view: &'frame wgpu::TextureView,

        /// Texture view с interleaved UV/chroma plane.
        uv_view: &'frame wgpu::TextureView,
    },

    /// P010 frame: отдельная luma plane и interleaved chroma plane в 10-bit контракте.
    P010 {
        /// Texture view с Y/luma plane.
        y_view: &'frame wgpu::TextureView,

        /// Texture view с interleaved UV/chroma plane.
        uv_view: &'frame wgpu::TextureView,
    },

    /// HostPlanar YUV frame: отдельные Y, U и V plane textures.
    HostYuvPlanar {
        /// Texture view с Y/luma plane.
        y_view: &'frame wgpu::TextureView,

        /// Texture view с U/Cb chroma plane.
        u_view: &'frame wgpu::TextureView,

        /// Texture view с V/Cr chroma plane.
        v_view: &'frame wgpu::TextureView,
    },
}

/// Кадр, готовый к рендерингу через WGPU backend.
pub struct WgpuRenderableFrame<'frame> {
    /// Renderer-neutral metadata, используемая capability layer и diagnostics.
    pub metadata: RenderableFrame,

    /// WGPU texture views, соответствующие `metadata.format`.
    pub planes: WgpuFramePlanes<'frame>,
}

impl<'frame> WgpuRenderableFrame<'frame> {
    /// Собирает WGPU frame wrapper из decoded NV12 frame и texture views.
    pub fn from_decoded_nv12(
        frame: &DecodedFrame,
        y_view: &'frame wgpu::TextureView,
        uv_view: &'frame wgpu::TextureView,
    ) -> Result<Self> {
        validate_decoded_nv12_frame(frame)?;

        Ok(Self {
            metadata: renderable_metadata_from_decoded(frame, VideoFramePixelLayout::Nv12),
            planes: WgpuFramePlanes::Nv12 { y_view, uv_view },
        })
    }

    /// Собирает WGPU frame wrapper из P010 boundary frame и texture views.
    pub fn from_decoded_p010(
        frame: &DecodedFrame,
        y_view: &'frame wgpu::TextureView,
        uv_view: &'frame wgpu::TextureView,
    ) -> Result<Self> {
        validate_decoded_p010_frame(frame)?;

        Ok(Self {
            metadata: renderable_metadata_from_decoded(frame, VideoFramePixelLayout::P010),
            planes: WgpuFramePlanes::P010 { y_view, uv_view },
        })
    }

    /// Собирает WGPU frame wrapper из HostPlanar YUV frame и upload texture views.
    pub fn from_decoded_host_yuv(
        frame: &DecodedFrame,
        y_view: &'frame wgpu::TextureView,
        u_view: &'frame wgpu::TextureView,
        v_view: &'frame wgpu::TextureView,
    ) -> Result<Self> {
        validate_decoded_host_yuv_frame(frame)?;

        Ok(Self {
            metadata: renderable_metadata_from_decoded(frame, frame.frame_contract.pixel_layout),
            planes: WgpuFramePlanes::HostYuvPlanar {
                y_view,
                u_view,
                v_view,
            },
        })
    }

    /// Совместимый wrapper для старого YUV420-only имени.
    pub fn from_decoded_host_yuv420(
        frame: &DecodedFrame,
        y_view: &'frame wgpu::TextureView,
        u_view: &'frame wgpu::TextureView,
        v_view: &'frame wgpu::TextureView,
    ) -> Result<Self> {
        Self::from_decoded_host_yuv(frame, y_view, u_view, v_view)
    }
}

/// Проверяет NV12 decoded frame до привязки backend-specific texture views.
fn validate_decoded_nv12_frame(frame: &DecodedFrame) -> Result<()> {
    validate_current_dma_buf_frame_contract(frame, DecodedPixelFormat::Nv12, "NV12")
}

/// Проверяет P010 decoded frame до привязки backend-specific texture views.
fn validate_decoded_p010_frame(frame: &DecodedFrame) -> Result<()> {
    validate_current_dma_buf_frame_contract(frame, DecodedPixelFormat::P010, "P010")
}

/// Проверяет HostPlanar YUV decoded frame до привязки renderer-owned upload views.
fn validate_decoded_host_yuv_frame(frame: &DecodedFrame) -> Result<()> {
    frame.validate_self_consistency()?;
    ensure!(
        frame.frame_contract.transfer_path == VideoFrameTransferPath::SoftwareHostUpload,
        "HostPlanar YUV WGPU boundary requires software host-upload frame contract, got {}",
        frame.frame_contract
    );
    ensure!(
        matches!(
            frame.frame_contract.pixel_layout,
            VideoFramePixelLayout::Yuv420Planar8
                | VideoFramePixelLayout::Yuv420Planar10Le
                | VideoFramePixelLayout::Yuv420Planar12Le
                | VideoFramePixelLayout::Yuv422Planar8
                | VideoFramePixelLayout::Yuv422Planar10Le
                | VideoFramePixelLayout::Yuv422Planar12Le
                | VideoFramePixelLayout::Yuv444Planar8
                | VideoFramePixelLayout::Yuv444Planar10Le
        ),
        "from_decoded_host_yuv received {} frame",
        frame.frame_contract.pixel_layout
    );

    Ok(())
}

/// Проверяет, что decoded frame соответствует текущему WGPU DMA-BUF path-у.
fn validate_current_dma_buf_frame_contract(
    frame: &DecodedFrame,
    expected_pixel_layout: VideoFramePixelLayout,
    boundary_label: &str,
) -> Result<()> {
    frame.validate_self_consistency()?;
    ensure!(
        matches!(
            frame.frame_contract.transfer_path,
            VideoFrameTransferPath::HardwareZeroCopy {
                handle: HardwareFrameHandle::DmaBuf { .. },
            }
        ),
        "{boundary_label} WGPU boundary requires DMA-BUF hardware zero-copy frame contract, got {}",
        frame.frame_contract
    );
    ensure!(
        frame.frame_contract.pixel_layout == expected_pixel_layout,
        "from_decoded_{boundary_label} received {} frame",
        frame.format()
    );

    Ok(())
}

/// Превращает ошибку renderer-side DMA-BUF import-а в typed render lookup failure.
pub(super) fn texture_view_lookup_after_import_failure(
    handle: FrameResourceHandle,
    error: anyhow::Error,
    texture_pool_lock_wait: Duration,
) -> WgpuFrameTextureViewLookup {
    tracing::warn!(
        error = %error,
        handle_id = handle.0,
        "Renderer DMA-BUF import failed; CPU fallback is disabled"
    );
    WgpuFrameTextureViewLookup::Error {
        texture_pool_lock_wait,
    }
}

/// Копирует renderer-neutral metadata из decoded frame без backend-specific handles.
fn renderable_metadata_from_decoded(
    frame: &DecodedFrame,
    format: VideoFramePixelLayout,
) -> RenderableFrame {
    RenderableFrame {
        handle: frame.resource_handle.0,
        pts: frame.pts,
        format,
        bit_depth: frame
            .bit_depth()
            .expect("validated WGPU frame must expose YUV bit depth"),
        chroma: frame
            .chroma()
            .expect("validated WGPU frame must expose YUV chroma"),
        coded_width: frame.width,
        coded_height: frame.height,
        render_width: frame.render_width,
        render_height: frame.render_height,
        display_orientation: frame.display_orientation,
        color: frame.color.clone(),
    }
}

/// Высокоуровневый WGPU video renderer.
pub struct WgpuVideoRenderer {
    /// Приватный renderer текущего MVP NV12 path.
    nv12_renderer: Nv12VideoRenderer,

    /// Приватный renderer P010 path с отдельным shader module.
    p010_renderer: P010VideoRenderer,

    /// Приватный renderer для HostPlanar YUV software host-upload path-а.
    host_yuv_renderer: HostPlanarYuvVideoRenderer,

    /// Снимок возможностей backend-а для capability report и stream selection.
    capabilities: RenderCapabilities,

    /// Последняя renderer-neutral диагностика без GPU handles.
    diagnostics: RenderDiagnostics,

    /// Текущий размер surface target-а; нужен только для защитного clamp-а viewport-а.
    surface_size: (u32, u32),

    /// Текущий live settings snapshot, применённый к renderer state.
    live_settings: RenderLiveSettings,
}

/// Проверяет, какие live settings текущий WGPU renderer ещё не умеет применять.
fn unsupported_wgpu_live_settings_fields(
    settings: &RenderLiveSettings,
    changed_fields: &[RenderLiveSettingId],
) -> Vec<RenderLiveSettingId> {
    let mut unsupported_fields = Vec::new();

    for changed_field in changed_fields {
        if matches!(changed_field, RenderLiveSettingId::ShaderParameter(_)) {
            unsupported_fields.push(changed_field.clone());
        }
    }

    if changed_fields.contains(&RenderLiveSettingId::ColorPipelineToneMapping)
        && settings.color_pipeline.tone_mapping != ToneMappingMode::Off
    {
        unsupported_fields.push(RenderLiveSettingId::ColorPipelineToneMapping);
    }

    if changed_fields.contains(&RenderLiveSettingId::ColorPipelineSwapchainTransfer)
        && settings.color_pipeline.swapchain_transfer != SwapchainTransferMode::PreserveCurrentUnorm
    {
        unsupported_fields.push(RenderLiveSettingId::ColorPipelineSwapchainTransfer);
    }

    let hdr_to_sdr_changed = changed_fields.iter().any(|changed_field| {
        matches!(
            changed_field,
            RenderLiveSettingId::HdrToSdrEnabled
                | RenderLiveSettingId::HdrToSdrOperator
                | RenderLiveSettingId::HdrToSdrOutputMode
                | RenderLiveSettingId::HdrToSdrSdrReferenceWhiteNits
                | RenderLiveSettingId::HdrToSdrHdrReferencePeakNits
        )
    });

    if hdr_to_sdr_changed && !settings.hdr_to_sdr.is_phase10_bt2446_c_sdr_bt709() {
        unsupported_fields.extend(changed_fields.iter().filter_map(|changed_field| {
            match changed_field {
                RenderLiveSettingId::HdrToSdrEnabled
                | RenderLiveSettingId::HdrToSdrOperator
                | RenderLiveSettingId::HdrToSdrOutputMode
                | RenderLiveSettingId::HdrToSdrSdrReferenceWhiteNits
                | RenderLiveSettingId::HdrToSdrHdrReferencePeakNits => Some(changed_field.clone()),
                _ => None,
            }
        }));
    }

    unsupported_fields.sort();
    unsupported_fields.dedup();
    unsupported_fields
}

impl WgpuVideoRenderer {
    /// Создаёт WGPU renderer для заданного swapchain format.
    #[must_use]
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let max_texture_size = Some(device.limits().max_texture_dimension_2d);
        let capabilities = wgpu_capabilities_from_features(max_texture_size, device.features());

        Self {
            nv12_renderer: Nv12VideoRenderer::new(device, surface_format),
            p010_renderer: P010VideoRenderer::new(device, surface_format),
            host_yuv_renderer: HostPlanarYuvVideoRenderer::new(device, surface_format),
            capabilities,
            diagnostics: RenderDiagnostics::default(),
            surface_size: (1, 1),
            live_settings: RenderLiveSettings::default(),
        }
    }

    /// Возвращает renderer capabilities без доступа к backend internals.
    #[must_use]
    pub const fn capabilities(&self) -> &RenderCapabilities {
        &self.capabilities
    }

    /// Возвращает текущий renderer-neutral live snapshot для нового renderer candidate-а.
    #[must_use]
    pub const fn live_settings(&self) -> &RenderLiveSettings {
        &self.live_settings
    }

    /// Возвращает последнюю диагностику renderer-а без backend-specific handles.
    #[must_use]
    pub const fn diagnostics(&self) -> &RenderDiagnostics {
        &self.diagnostics
    }

    /// Обновляет color pipeline settings для всех текущих video paths.
    pub fn set_color_pipeline_settings(&mut self, settings: ColorPipelineSettings) {
        self.nv12_renderer.set_color_pipeline_settings(settings);
        self.p010_renderer.set_color_pipeline_settings(settings);
        self.host_yuv_renderer.set_color_pipeline_settings(settings);
        self.live_settings.color_pipeline = settings;
    }

    /// Обновляет HDR-to-SDR settings для high-bit GPU HDR renderer-ов.
    pub fn set_hdr_to_sdr_settings(&mut self, settings: HdrToSdrSettings) {
        self.p010_renderer.set_hdr_to_sdr_settings(settings);
        self.host_yuv_renderer.set_hdr_to_sdr_settings(settings);
        self.live_settings.hdr_to_sdr = settings;
    }

    /// Применяет renderer-neutral live settings через существующие WGPU setters.
    fn apply_live_settings_snapshot(
        &mut self,
        phase: RenderLiveApplyPhase,
        settings: &RenderLiveSettings,
    ) -> std::result::Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        let changed_fields = settings.changed_fields_from(&self.live_settings);

        if changed_fields.is_empty() {
            return Ok(RenderLiveApplyReport::no_op(phase));
        }

        let unsupported_fields = unsupported_wgpu_live_settings_fields(settings, &changed_fields);
        if !unsupported_fields.is_empty() {
            return Err(RenderLiveSettingsError::unsupported(
                phase,
                unsupported_fields,
                "WGPU adapter supports color adjustment and current HDR-to-SDR settings; custom shader parameters and future color pipeline modes are not implemented yet",
            ));
        }

        if settings.color_pipeline != self.live_settings.color_pipeline {
            self.set_color_pipeline_settings(settings.color_pipeline);
        }

        if settings.hdr_to_sdr != self.live_settings.hdr_to_sdr {
            self.set_hdr_to_sdr_settings(settings.hdr_to_sdr);
        }

        self.live_settings.shader_parameters = settings.shader_parameters.clone();

        Ok(RenderLiveApplyReport::applied(phase, changed_fields))
    }

    /// Обновляет размер swapchain для защитного clamp-а video viewport-а.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.surface_size = (width.max(1), height.max(1));
    }

    /// Рендерит video frame или очищает target в чёрный цвет, если кадра нет.
    ///
    /// Возвращает `true`, если video pass реально нарисовал кадр.
    pub fn render_or_clear(&mut self, input: WgpuVideoRenderInput<'_, '_>) -> Result<bool> {
        if input.frame.is_none() {
            self.diagnostics = RenderDiagnostics::default();
            clear_to_black(input.target, input.encoder);
            return Ok(false);
        }
        self.render_video_frame(input, VideoRenderTargetLoad::ClearBlack, true)
    }

    /// Рендерит video overlay поверх уже нарисованного target-а без clear.
    pub fn render_overlay(&mut self, input: WgpuVideoRenderInput<'_, '_>) -> Result<bool> {
        if input.frame.is_none() {
            return Ok(false);
        }
        self.render_video_frame(input, VideoRenderTargetLoad::LoadExisting, false)
    }

    /// Общий path для main и overlay video pass-ов.
    fn render_video_frame(
        &mut self,
        input: WgpuVideoRenderInput<'_, '_>,
        target_load: VideoRenderTargetLoad,
        update_diagnostics: bool,
    ) -> Result<bool> {
        let WgpuVideoRenderInput {
            frame,
            video_viewport,
            video_exclusion_rects,
            target,
            encoder,
            device,
            queue,
        } = input;
        let Some(frame) = frame else {
            return Ok(false);
        };
        if update_diagnostics {
            self.diagnostics = RenderDiagnostics::default();
        }
        let video_viewport =
            video_viewport.clamp_to_surface(self.surface_size.0, self.surface_size.1);
        let draw_rects = visible_video_draw_rects(video_viewport, video_exclusion_rects);
        if update_diagnostics {
            self.diagnostics.video_draw_rect_count = draw_rects.len();
        }
        let mut pass_context = VideoRenderPassContext {
            target,
            encoder,
            device,
            queue,
            viewport: video_viewport,
            draw_rects,
            target_load,
        };

        if !frame.metadata.has_display_size() {
            bail!(
                "renderable frame has invalid display size: {}x{}",
                frame.metadata.render_width,
                frame.metadata.render_height
            );
        }

        match select_renderer_dispatch(frame.metadata.format, frame.planes.kind())? {
            RendererDispatch::Nv12 => {
                let WgpuFramePlanes::Nv12 { y_view, uv_view } = &frame.planes else {
                    unreachable!("renderer dispatch was selected from plane kind");
                };
                let active_color_path = self.nv12_renderer.render_frame(
                    &frame.metadata,
                    y_view,
                    uv_view,
                    &mut pass_context,
                )?;
                if update_diagnostics {
                    self.diagnostics.active_color_path = Some(active_color_path);
                }
                Ok(true)
            }
            RendererDispatch::P010 => {
                let WgpuFramePlanes::P010 { y_view, uv_view } = &frame.planes else {
                    unreachable!("renderer dispatch was selected from plane kind");
                };
                let p010_diagnostics = self.p010_renderer.render_frame(
                    &frame.metadata,
                    y_view,
                    uv_view,
                    &mut pass_context,
                )?;
                if update_diagnostics {
                    self.diagnostics.active_color_path = Some(p010_diagnostics.active_color_path);
                    self.diagnostics.hdr_reference_defaults =
                        p010_diagnostics.hdr_reference_defaults;
                }
                Ok(true)
            }
            RendererDispatch::HostYuvPlanar => {
                let WgpuFramePlanes::HostYuvPlanar {
                    y_view,
                    u_view,
                    v_view,
                } = &frame.planes
                else {
                    unreachable!("renderer dispatch was selected from plane kind");
                };
                let host_yuv_diagnostics = self.host_yuv_renderer.render_frame(
                    &frame.metadata,
                    y_view,
                    u_view,
                    v_view,
                    &mut pass_context,
                )?;
                if update_diagnostics {
                    self.diagnostics.active_color_path =
                        Some(host_yuv_diagnostics.active_color_path);
                    self.diagnostics.hdr_reference_defaults =
                        host_yuv_diagnostics.hdr_reference_defaults;
                }
                Ok(true)
            }
        }
    }
}

impl RenderLiveSettingsAdapter for WgpuVideoRenderer {
    /// Применяет preview update без пересоздания WGPU pipeline.
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> std::result::Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.apply_live_settings_snapshot(RenderLiveApplyPhase::Preview, &update.settings)
    }

    /// Фиксирует live settings как committed runtime state.
    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> std::result::Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.apply_live_settings_snapshot(RenderLiveApplyPhase::Commit, settings)
    }

    /// Откатывает live settings к baseline preview transaction-а.
    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> std::result::Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.apply_live_settings_snapshot(RenderLiveApplyPhase::Rollback, baseline)
    }
}

/// Упрощённый kind plane set без lifetime/GPU handles для dispatch logic и unit tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WgpuFramePlaneKind {
    /// Renderer-facing NV12 Y/UV plane pair.
    Nv12,

    /// Renderer-facing P010 Y/UV plane pair.
    P010,

    /// Renderer-facing HostPlanar Y/U/V plane triplet.
    HostYuvPlanar,
}

impl WgpuFramePlanes<'_> {
    /// Возвращает kind plane set без раскрытия backend-specific handles.
    const fn kind(&self) -> WgpuFramePlaneKind {
        match self {
            Self::Nv12 { .. } => WgpuFramePlaneKind::Nv12,
            Self::P010 { .. } => WgpuFramePlaneKind::P010,
            Self::HostYuvPlanar { .. } => WgpuFramePlaneKind::HostYuvPlanar,
        }
    }
}

/// Конкретный renderer path, выбранный facade-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RendererDispatch {
    /// Текущий SDR/NV12 renderer.
    Nv12,

    /// Отдельный P010 renderer.
    P010,

    /// Отдельный HostPlanar YUV upload renderer.
    HostYuvPlanar,
}

/// Считает letterbox `uv_scale`/`uv_offset` для NV12 и P010 shader-ов.
///
/// Оба shader-а вычисляют display UV через `uv * uv_scale + uv_offset`, где `uv`
/// пробегает `[0, 1]` внутри active video viewport-а, а семплы за пределами `[0, 1]`
/// красятся чёрным до применения source orientation transform.
/// Чтобы кадр целиком уместился в viewport с сохранением пропорций (letterbox), масштаб
/// по "лишней" оси должен быть `> 1`: тогда края экрана отображаются в координаты
/// текстуры за пределами `[0, 1]` и превращаются в чёрные полосы, а смещение
/// `(1 - scale) * 0.5` центрирует видимую часть кадра.
///
/// Возвращает `(uv_scale, uv_offset)`; единый источник правды, чтобы NV12 и P010
/// не расходились в формуле.
pub(super) fn letterbox_scale_and_offset(
    frame: &RenderableFrame,
    viewport_size: (u32, u32),
) -> ([f32; 2], [f32; 2]) {
    // Соотношение сторон кадра после container display orientation.
    let video_aspect =
        frame.oriented_display_width() as f32 / frame.oriented_display_height().max(1) as f32;
    // Соотношение сторон области video draw, уже ужатой app layout-ом.
    let viewport_aspect = viewport_size.0 as f32 / viewport_size.1.max(1) as f32;

    if video_aspect > viewport_aspect {
        // Видео шире viewport-а: чёрные полосы сверху и снизу, масштабируем по вертикали.
        let scale_y = video_aspect / viewport_aspect;
        ([1.0, scale_y], [0.0, (1.0 - scale_y) * 0.5])
    } else {
        // Видео уже viewport-а (или равно): чёрные полосы слева и справа.
        let scale_x = viewport_aspect / video_aspect;
        ([scale_x, 1.0], [(1.0 - scale_x) * 0.5, 0.0])
    }
}

/// Возвращает affine rows для преобразования display UV в source texture UV.
pub(super) fn display_orientation_uv_transform(
    orientation: VideoDisplayOrientation,
) -> [[f32; 4]; 2] {
    match orientation {
        VideoDisplayOrientation::Identity => [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
        VideoDisplayOrientation::Rotate90Clockwise => [[0.0, 1.0, 0.0, 0.0], [-1.0, 0.0, 1.0, 0.0]],
        VideoDisplayOrientation::Rotate180 => [[-1.0, 0.0, 1.0, 0.0], [0.0, -1.0, 1.0, 0.0]],
        VideoDisplayOrientation::Rotate270Clockwise => {
            [[0.0, -1.0, 1.0, 0.0], [1.0, 0.0, 0.0, 0.0]]
        }
    }
}

/// Выбирает renderer path по renderer-neutral format и kind plane set.
fn select_renderer_dispatch(
    format: VideoFramePixelLayout,
    plane_kind: WgpuFramePlaneKind,
) -> Result<RendererDispatch> {
    match (format, plane_kind) {
        (VideoFramePixelLayout::Nv12, WgpuFramePlaneKind::Nv12) => Ok(RendererDispatch::Nv12),
        (VideoFramePixelLayout::P010, WgpuFramePlaneKind::P010) => Ok(RendererDispatch::P010),
        (
            VideoFramePixelLayout::Yuv420Planar8
            | VideoFramePixelLayout::Yuv420Planar10Le
            | VideoFramePixelLayout::Yuv420Planar12Le,
            WgpuFramePlaneKind::HostYuvPlanar,
        ) => Ok(RendererDispatch::HostYuvPlanar),
        (
            VideoFramePixelLayout::Yuv422Planar8
            | VideoFramePixelLayout::Yuv422Planar10Le
            | VideoFramePixelLayout::Yuv422Planar12Le
            | VideoFramePixelLayout::Yuv444Planar8
            | VideoFramePixelLayout::Yuv444Planar10Le,
            WgpuFramePlaneKind::HostYuvPlanar,
        ) => Ok(RendererDispatch::HostYuvPlanar),
        (format, _) => bail!("WGPU renderer frame metadata/plane mismatch for format: {format}"),
    }
}

/// Возвращает scissor-rects, в которых video pass должен рисовать кадр.
fn visible_video_draw_rects(
    video_viewport: RenderViewport,
    video_exclusion_rects: &[RenderViewport],
) -> Vec<RenderViewport> {
    let mut visible_rects = vec![video_viewport];

    for exclusion_rect in video_exclusion_rects {
        let Some(exclusion_rect) = exclusion_rect.intersection(video_viewport) else {
            continue;
        };

        visible_rects = visible_rects
            .into_iter()
            .flat_map(|visible_rect| visible_rect.subtract(exclusion_rect))
            .collect();

        if visible_rects.is_empty() {
            break;
        }
    }

    visible_rects
}

/// Очищает swapchain target в чёрный цвет, когда video frame ещё не готов.
pub fn clear_to_black(target: &wgpu::TextureView, encoder: &mut wgpu::CommandEncoder) {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("clear to black pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}

#[cfg(test)]
mod tests;
