use std::collections::VecDeque;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use anyhow::{Result, bail, ensure};
use codec_core::VideoDisplayOrientation;
use render_core::{
    ColorPipelineSettings, HdrToSdrSettings, RenderCapabilities, RenderDiagnostics,
    RenderLiveApplyPhase, RenderLiveApplyReport, RenderLiveSettingId, RenderLiveSettings,
    RenderLiveSettingsAdapter, RenderLiveSettingsError, RenderLiveSettingsUpdate, RenderViewport,
    RenderableFrame, SwapchainTransferMode, ToneMappingMode,
};
use video_backend_api::{PresentFrameResourceDescriptorLookup, PresentFrameResourceProviderHandle};
use video_core::{DecodedFrame, DecodedPixelFormat, FrameResourceDescriptor, FrameResourceHandle};
use video_frame_contract::{HardwareFrameHandle, VideoFramePixelLayout, VideoFrameTransferPath};

use crate::capabilities::wgpu_capabilities_from_features;
use crate::dma_buf_import::{DmaBufImporter, ImportedDmaBufTexture};

pub use self::host_planar_upload::{
    HostPlanarWgpuFrameMaterializer, HostPlanarWgpuTextureViewLookup, HostPlanarWgpuTextureViews,
};
use self::host_yuv420_renderer::HostYuv420VideoRenderer;
use self::nv12_renderer::Nv12VideoRenderer;
use self::p010_renderer::P010VideoRenderer;

mod host_planar_upload;
mod host_yuv420_renderer;
mod nv12_renderer;
mod p010_renderer;

/// WGPU texture views, полученные renderer materializer-ом по opaque frame handle.
#[derive(Clone)]
pub struct WgpuFrameTextureViews {
    /// Texture view с luma/Y plane.
    pub y_view: wgpu::TextureView,

    /// Texture view с chroma/UV plane.
    pub uv_view: wgpu::TextureView,

    /// Guard держит imported texture storage живым как минимум до drop-а views.
    _imported_texture_guard: Option<Arc<ImportedDmaBufTexture>>,
}

impl WgpuFrameTextureViews {
    /// Создаёт views из renderer-owned imported texture.
    fn from_imported_texture(imported_texture: Arc<ImportedDmaBufTexture>) -> Self {
        let _owned_storage_textures = imported_texture.storage_texture_count();
        Self {
            y_view: imported_texture.y_view.clone(),
            uv_view: imported_texture.uv_view.clone(),
            _imported_texture_guard: Some(imported_texture),
        }
    }
}

/// Почему конкретный descriptor не подходит этому WGPU materializer-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WgpuFrameMaterializationUnsupportedReason {
    /// Descriptor содержит CPU-visible host planes, а этот materializer импортирует только DMA-BUF.
    HostPlanarRequiresUploadMaterializer,

    /// Descriptor содержит DMA-BUF, а этот materializer загружает только HostPlanar.
    DmaBufRequiresDmaBufMaterializer,

    /// HostPlanar frame пришёл не через software host-upload contract.
    HostPlanarRequiresSoftwareUploadContract,

    /// HostPlanar layout пока не входит в минимальный upload subset.
    HostPlanarLayoutNotSupportedByUploadMaterializer,
}

impl WgpuFrameMaterializationUnsupportedReason {
    /// Stable diagnostic label без user-facing текста.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
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
    fn try_texture_view_lookup(&self, handle: FrameResourceHandle) -> WgpuFrameTextureViewLookup;
}

/// Renderer-side materializer, который импортирует neutral DMA-BUF descriptors в WGPU.
pub struct DmaBufWgpuFrameMaterializer {
    /// Backend provider возвращает duplicated descriptors и lock diagnostics.
    resource_provider: PresentFrameResourceProviderHandle,

    /// Renderer-owned cache/importer; VAAPI/cros types сюда не попадают.
    texture_cache: Mutex<DmaBufWgpuTextureCache>,
}

impl DmaBufWgpuFrameMaterializer {
    /// Создаёт materializer из WGPU handles renderer layer-а и neutral provider-а.
    #[must_use]
    pub fn new(
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        resource_provider: PresentFrameResourceProviderHandle,
    ) -> Self {
        Self {
            resource_provider,
            texture_cache: Mutex::new(DmaBufWgpuTextureCache::new(DmaBufImporter::new(
                device.clone(),
                instance.clone(),
                adapter.clone(),
            ))),
        }
    }
}

impl WgpuFrameTextureViewMaterializer for DmaBufWgpuFrameMaterializer {
    fn try_texture_view_lookup(&self, handle: FrameResourceHandle) -> WgpuFrameTextureViewLookup {
        let provider_lookup = self
            .resource_provider
            .try_resource_descriptor_lookup(handle);
        let provider_wait = provider_lookup.resource_pool_lock_wait();

        let descriptor = match provider_lookup {
            PresentFrameResourceDescriptorLookup::Ready { descriptor, .. } => descriptor,
            PresentFrameResourceDescriptorLookup::Busy { .. } => {
                return WgpuFrameTextureViewLookup::Busy {
                    texture_pool_lock_wait: provider_wait,
                };
            }
            PresentFrameResourceDescriptorLookup::Missing { .. } => {
                return WgpuFrameTextureViewLookup::Missing {
                    texture_pool_lock_wait: provider_wait,
                };
            }
            PresentFrameResourceDescriptorLookup::Fatal { .. } => {
                return WgpuFrameTextureViewLookup::Error {
                    texture_pool_lock_wait: provider_wait,
                };
            }
        };

        if let Some(unsupported_lookup) =
            unsupported_lookup_for_non_dma_buf_descriptor(&descriptor, provider_wait)
        {
            return unsupported_lookup;
        }

        let cache_lock_started_at = Instant::now();
        let mut texture_cache = match self.texture_cache.try_lock() {
            Ok(texture_cache) => texture_cache,
            Err(TryLockError::WouldBlock) => {
                return WgpuFrameTextureViewLookup::Busy {
                    texture_pool_lock_wait: provider_wait
                        .saturating_add(cache_lock_started_at.elapsed()),
                };
            }
            Err(TryLockError::Poisoned(error)) => {
                tracing::warn!(error = %error, "WGPU DMA-BUF texture cache mutex poisoned");
                return WgpuFrameTextureViewLookup::Error {
                    texture_pool_lock_wait: provider_wait
                        .saturating_add(cache_lock_started_at.elapsed()),
                };
            }
        };
        let total_lock_wait = provider_wait.saturating_add(cache_lock_started_at.elapsed());

        match texture_cache.materialize(handle, descriptor) {
            Ok(views) => WgpuFrameTextureViewLookup::Ready {
                views,
                texture_pool_lock_wait: total_lock_wait,
            },
            Err(error) => texture_view_lookup_after_import_failure(handle, error, total_lock_wait),
        }
    }
}

fn unsupported_lookup_for_non_dma_buf_descriptor(
    descriptor: &FrameResourceDescriptor,
    texture_pool_lock_wait: Duration,
) -> Option<WgpuFrameTextureViewLookup> {
    match descriptor {
        FrameResourceDescriptor::DmaBuf(_) => None,
        FrameResourceDescriptor::HostPlanar(_) => Some(WgpuFrameTextureViewLookup::Unsupported {
            reason: WgpuFrameMaterializationUnsupportedReason::HostPlanarRequiresUploadMaterializer,
            texture_pool_lock_wait,
        }),
    }
}

/// Bounded renderer-side cache imported DMA-BUF textures.
struct DmaBufWgpuTextureCache {
    /// Vulkan/WGPU importer владеет unsafe platform import code.
    importer: DmaBufImporter,

    /// FIFO cache по frame resource handle; views держат storage через Arc guard.
    cached_textures: VecDeque<CachedDmaBufTexture>,

    /// Верхняя граница renderer-owned imported textures.
    capacity: usize,
}

impl DmaBufWgpuTextureCache {
    /// Default cache size совпадает с bounded decoder/resource pool порядком.
    const DEFAULT_CAPACITY: usize = 24;

    /// Создаёт cache вокруг renderer-owned importer-а.
    fn new(importer: DmaBufImporter) -> Self {
        Self {
            importer,
            cached_textures: VecDeque::with_capacity(Self::DEFAULT_CAPACITY),
            capacity: Self::DEFAULT_CAPACITY,
        }
    }

    /// Возвращает cached views или импортирует descriptor как renderer error boundary.
    fn materialize(
        &mut self,
        handle: FrameResourceHandle,
        descriptor: FrameResourceDescriptor,
    ) -> anyhow::Result<WgpuFrameTextureViews> {
        if let Some(cached_texture) = self
            .cached_textures
            .iter()
            .find(|cached_texture| cached_texture.handle == handle)
        {
            return Ok(WgpuFrameTextureViews::from_imported_texture(
                cached_texture.imported_texture.clone(),
            ));
        }

        let FrameResourceDescriptor::DmaBuf(dma_buf_descriptor) = descriptor else {
            bail!("WGPU DMA-BUF texture cache received non-DMA-BUF descriptor");
        };
        let imported_texture = Arc::new(
            self.importer
                .import_exported_dma_buf_image(&dma_buf_descriptor)?,
        );
        let views = WgpuFrameTextureViews::from_imported_texture(imported_texture.clone());

        if self.cached_textures.len() >= self.capacity {
            self.cached_textures.pop_front();
        }
        self.cached_textures.push_back(CachedDmaBufTexture {
            handle,
            imported_texture,
        });

        Ok(views)
    }
}

/// Один cached renderer import.
struct CachedDmaBufTexture {
    /// Frame resource handle, для которого выполнен import.
    handle: FrameResourceHandle,

    /// Imported storage и typed plane views.
    imported_texture: Arc<ImportedDmaBufTexture>,
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

    /// HostPlanar YUV420 frame: отдельные Y, U и V plane textures.
    HostYuv420Planar {
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

    /// Собирает WGPU frame wrapper из HostPlanar YUV420 frame и upload texture views.
    pub fn from_decoded_host_yuv420(
        frame: &DecodedFrame,
        y_view: &'frame wgpu::TextureView,
        u_view: &'frame wgpu::TextureView,
        v_view: &'frame wgpu::TextureView,
    ) -> Result<Self> {
        validate_decoded_host_yuv420_frame(frame)?;

        Ok(Self {
            metadata: renderable_metadata_from_decoded(frame, frame.frame_contract.pixel_layout),
            planes: WgpuFramePlanes::HostYuv420Planar {
                y_view,
                u_view,
                v_view,
            },
        })
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

/// Проверяет HostPlanar YUV420 decoded frame до привязки renderer-owned upload views.
fn validate_decoded_host_yuv420_frame(frame: &DecodedFrame) -> Result<()> {
    frame.validate_self_consistency()?;
    ensure!(
        frame.frame_contract.transfer_path == VideoFrameTransferPath::SoftwareHostUpload,
        "HostPlanar YUV420 WGPU boundary requires software host-upload frame contract, got {}",
        frame.frame_contract
    );
    ensure!(
        matches!(
            frame.frame_contract.pixel_layout,
            VideoFramePixelLayout::Yuv420Planar8
                | VideoFramePixelLayout::Yuv420Planar10Le
                | VideoFramePixelLayout::Yuv420Planar12Le
        ),
        "from_decoded_host_yuv420 received {} frame",
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
fn texture_view_lookup_after_import_failure(
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

    /// Приватный renderer для HostPlanar YUV420 software host-upload path-а.
    host_yuv420_renderer: HostYuv420VideoRenderer,

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
            host_yuv420_renderer: HostYuv420VideoRenderer::new(device, surface_format),
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

    /// Возвращает последнюю диагностику renderer-а без backend-specific handles.
    #[must_use]
    pub const fn diagnostics(&self) -> &RenderDiagnostics {
        &self.diagnostics
    }

    /// Обновляет color pipeline settings для всех текущих video paths.
    pub fn set_color_pipeline_settings(&mut self, settings: ColorPipelineSettings) {
        self.nv12_renderer.set_color_pipeline_settings(settings);
        self.p010_renderer.set_color_pipeline_settings(settings);
        self.host_yuv420_renderer
            .set_color_pipeline_settings(settings);
        self.live_settings.color_pipeline = settings;
    }

    /// Обновляет HDR-to-SDR settings для high-bit GPU HDR renderer-ов.
    pub fn set_hdr_to_sdr_settings(&mut self, settings: HdrToSdrSettings) {
        self.p010_renderer.set_hdr_to_sdr_settings(settings);
        self.host_yuv420_renderer.set_hdr_to_sdr_settings(settings);
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
    pub fn render_or_clear(
        &mut self,
        frame: Option<&WgpuRenderableFrame<'_>>,
        video_viewport: RenderViewport,
        video_exclusion_rects: &[RenderViewport],
        target: &wgpu::TextureView,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<bool> {
        let Some(frame) = frame else {
            self.diagnostics = RenderDiagnostics::default();
            clear_to_black(target, encoder);
            return Ok(false);
        };
        self.diagnostics = RenderDiagnostics::default();
        let video_viewport =
            video_viewport.clamp_to_surface(self.surface_size.0, self.surface_size.1);
        let draw_rects = visible_video_draw_rects(video_viewport, video_exclusion_rects);
        self.diagnostics.video_draw_rect_count = draw_rects.len();
        let mut pass_context = VideoRenderPassContext {
            target,
            encoder,
            device,
            queue,
            viewport: video_viewport,
            draw_rects,
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
                self.diagnostics.active_color_path = Some(active_color_path);
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
                self.diagnostics.active_color_path = Some(p010_diagnostics.active_color_path);
                self.diagnostics.hdr_reference_defaults = p010_diagnostics.hdr_reference_defaults;
                Ok(true)
            }
            RendererDispatch::HostYuv420Planar => {
                let WgpuFramePlanes::HostYuv420Planar {
                    y_view,
                    u_view,
                    v_view,
                } = &frame.planes
                else {
                    unreachable!("renderer dispatch was selected from plane kind");
                };
                let host_yuv420_diagnostics = self.host_yuv420_renderer.render_frame(
                    &frame.metadata,
                    y_view,
                    u_view,
                    v_view,
                    &mut pass_context,
                )?;
                self.diagnostics.active_color_path =
                    Some(host_yuv420_diagnostics.active_color_path);
                self.diagnostics.hdr_reference_defaults =
                    host_yuv420_diagnostics.hdr_reference_defaults;
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
    HostYuv420Planar,
}

impl WgpuFramePlanes<'_> {
    /// Возвращает kind plane set без раскрытия backend-specific handles.
    const fn kind(&self) -> WgpuFramePlaneKind {
        match self {
            Self::Nv12 { .. } => WgpuFramePlaneKind::Nv12,
            Self::P010 { .. } => WgpuFramePlaneKind::P010,
            Self::HostYuv420Planar { .. } => WgpuFramePlaneKind::HostYuv420Planar,
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

    /// Отдельный HostPlanar YUV420 upload renderer.
    HostYuv420Planar,
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
            WgpuFramePlaneKind::HostYuv420Planar,
        ) => Ok(RendererDispatch::HostYuv420Planar),
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
mod tests {
    use std::time::Duration;

    use codec_core::VideoColorMetadata;
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

    use super::*;

    #[test]
    fn p010_frame_dispatches_to_p010_renderer_path() {
        let dispatch =
            select_renderer_dispatch(VideoFramePixelLayout::P010, WgpuFramePlaneKind::P010)
                .expect("P010 frame dispatches");

        assert_eq!(dispatch, RendererDispatch::P010);
    }

    #[test]
    fn nv12_frame_dispatches_to_nv12_renderer_path() {
        let dispatch =
            select_renderer_dispatch(VideoFramePixelLayout::Nv12, WgpuFramePlaneKind::Nv12)
                .expect("NV12 frame dispatches");

        assert_eq!(dispatch, RendererDispatch::Nv12);
    }

    #[test]
    fn host_yuv420_frame_dispatches_to_host_yuv420_renderer_path() {
        for format in [
            VideoFramePixelLayout::Yuv420Planar8,
            VideoFramePixelLayout::Yuv420Planar10Le,
            VideoFramePixelLayout::Yuv420Planar12Le,
        ] {
            let dispatch = select_renderer_dispatch(format, WgpuFramePlaneKind::HostYuv420Planar)
                .expect("HostPlanar YUV420 frame dispatches");

            assert_eq!(dispatch, RendererDispatch::HostYuv420Planar);
        }
    }

    #[test]
    fn live_settings_adapter_accepts_current_color_and_hdr_fields() {
        let baseline = RenderLiveSettings::default();
        let mut settings = baseline.clone();

        settings.color_pipeline.adjustment.contrast = 1.1;
        settings.hdr_to_sdr.hdr_reference_peak_nits = 900.0;

        let update = RenderLiveSettingsUpdate::from_baseline(&baseline, settings);
        let unsupported_fields =
            unsupported_wgpu_live_settings_fields(&update.settings, &update.changed_fields);

        assert!(unsupported_fields.is_empty());
    }

    #[test]
    fn live_settings_adapter_rejects_unimplemented_shader_parameters() {
        let baseline = RenderLiveSettings::default();
        let mut settings = baseline.clone();
        let shader_parameter_id = render_core::ShaderParameterId::new("render.shader.test_gain");

        settings
            .shader_parameters
            .parameters
            .push(render_core::ShaderParameter::new(
                shader_parameter_id.clone(),
                render_core::ShaderParameterValue::Float(0.5),
            ));

        let update = RenderLiveSettingsUpdate::from_baseline(&baseline, settings);
        let unsupported_fields =
            unsupported_wgpu_live_settings_fields(&update.settings, &update.changed_fields);

        assert_eq!(
            unsupported_fields,
            vec![RenderLiveSettingId::ShaderParameter(shader_parameter_id)]
        );
    }

    #[test]
    fn live_settings_adapter_rejects_future_color_pipeline_modes() {
        let baseline = RenderLiveSettings::default();
        let mut settings = baseline.clone();

        settings.color_pipeline.tone_mapping = ToneMappingMode::Reinhard;
        settings.color_pipeline.swapchain_transfer = SwapchainTransferMode::SrgbRenderTarget;

        let update = RenderLiveSettingsUpdate::from_baseline(&baseline, settings);
        let unsupported_fields =
            unsupported_wgpu_live_settings_fields(&update.settings, &update.changed_fields);

        assert_eq!(
            unsupported_fields,
            vec![
                RenderLiveSettingId::ColorPipelineToneMapping,
                RenderLiveSettingId::ColorPipelineSwapchainTransfer,
            ]
        );
    }

    #[test]
    fn live_settings_adapter_rejects_hdr_to_sdr_values_outside_phase10_contract() {
        let baseline = RenderLiveSettings::default();
        let mut settings = baseline.clone();

        settings.hdr_to_sdr.enabled = false;

        let update = RenderLiveSettingsUpdate::from_baseline(&baseline, settings);
        let unsupported_fields =
            unsupported_wgpu_live_settings_fields(&update.settings, &update.changed_fields);

        assert_eq!(
            unsupported_fields,
            vec![RenderLiveSettingId::HdrToSdrEnabled]
        );
    }

    #[test]
    fn metadata_plane_mismatch_is_rejected_before_renderer_call() {
        let error = select_renderer_dispatch(VideoFramePixelLayout::P010, WgpuFramePlaneKind::Nv12)
            .expect_err("P010 metadata must not use NV12 planes");

        assert!(
            error.to_string().contains("metadata/plane mismatch"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn p010_boundary_rejects_non_zero_copy_memory_path() {
        let frame = decoded_host_planar10_test_frame();

        let error =
            validate_decoded_p010_frame(&frame).expect_err("P010 host-upload path rejected");

        assert!(
            error.to_string().contains("zero-copy"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn host_yuv420_boundary_accepts_ready_host_upload_contracts() {
        for frame in [
            decoded_host_planar8_test_frame(),
            decoded_host_planar10_test_frame(),
            decoded_host_planar12_test_frame(),
        ] {
            validate_decoded_host_yuv420_frame(&frame)
                .expect("HostPlanar YUV420 software upload boundary accepts frame");
        }
    }

    #[test]
    fn host_yuv420_boundary_rejects_dma_buf_contract() {
        let frame = decoded_nv12_test_frame();

        let error = validate_decoded_host_yuv420_frame(&frame)
            .expect_err("HostPlanar YUV420 rejects DMA-BUF frame");

        assert!(
            error.to_string().contains("software host-upload"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn nv12_boundary_rejects_non_zero_copy_memory_path() {
        let frame = decoded_host_planar8_test_frame();

        let error =
            validate_decoded_nv12_frame(&frame).expect_err("NV12 host-upload path rejected");

        assert!(
            error.to_string().contains("zero-copy"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn dma_buf_import_failure_is_renderer_error_without_cpu_fallback() {
        let lookup = texture_view_lookup_after_import_failure(
            FrameResourceHandle(77),
            anyhow::anyhow!("synthetic import failure"),
            Duration::from_millis(9),
        );

        assert!(matches!(
            lookup,
            WgpuFrameTextureViewLookup::Error {
                texture_pool_lock_wait
            } if texture_pool_lock_wait == Duration::from_millis(9)
        ));
    }

    #[test]
    fn dma_buf_materializer_returns_unsupported_for_host_planar_descriptor() {
        let descriptor = FrameResourceDescriptor::HostPlanar(
            video_core::HostPlanarFrameDescriptor::from_owned_buffer(
                vec![0_u8; 6],
                vec![
                    video_core::HostPlaneDescriptor {
                        role: video_core::HostPlaneRole::Luma,
                        offset: 0,
                        stride: 2,
                        visible_width: 2,
                        visible_height: 2,
                        bytes_per_sample: 1,
                    },
                    video_core::HostPlaneDescriptor {
                        role: video_core::HostPlaneRole::ChromaU,
                        offset: 4,
                        stride: 1,
                        visible_width: 1,
                        visible_height: 1,
                        bytes_per_sample: 1,
                    },
                    video_core::HostPlaneDescriptor {
                        role: video_core::HostPlaneRole::ChromaV,
                        offset: 5,
                        stride: 1,
                        visible_width: 1,
                        visible_height: 1,
                        bytes_per_sample: 1,
                    },
                ],
            ),
        );

        let lookup =
            unsupported_lookup_for_non_dma_buf_descriptor(&descriptor, Duration::from_millis(4))
                .expect("host-planar descriptor must be classified before DMA-BUF import");

        assert!(matches!(
            lookup,
            WgpuFrameTextureViewLookup::Unsupported {
                reason:
                    WgpuFrameMaterializationUnsupportedReason::HostPlanarRequiresUploadMaterializer,
                texture_pool_lock_wait,
            } if texture_pool_lock_wait == Duration::from_millis(4)
        ));
    }

    #[test]
    fn p010_dma_buf_image_layouts_map_to_same_renderer_plane_kind() {
        let baseline_separate_layer_kind = p010_storage_layout_renderer_plane_kind(
            video_frame_contract::DmaBufImageLayout::SeparateLayers,
        );
        let compatibility_composed_kind = p010_storage_layout_renderer_plane_kind(
            video_frame_contract::DmaBufImageLayout::ComposedLayers,
        );

        assert_eq!(baseline_separate_layer_kind, WgpuFramePlaneKind::P010);
        assert_eq!(compatibility_composed_kind, WgpuFramePlaneKind::P010);
        assert_eq!(baseline_separate_layer_kind, compatibility_composed_kind);
    }

    /// Документирует, что renderer видит только P010 Y/UV pair, а не storage layout.
    const fn p010_storage_layout_renderer_plane_kind(
        _storage_layout: video_frame_contract::DmaBufImageLayout,
    ) -> WgpuFramePlaneKind {
        WgpuFramePlaneKind::P010
    }

    #[test]
    fn visible_video_draw_rects_exclude_sidebar_without_changing_viewport_size() {
        let video_viewport = RenderViewport::full_surface(1280, 720);
        let sidebar = RenderViewport::new(0, 64, 420, 576);

        let draw_rects = visible_video_draw_rects(video_viewport, &[sidebar]);

        assert_eq!(
            draw_rects,
            vec![
                RenderViewport::new(0, 0, 1280, 64),
                RenderViewport::new(0, 640, 1280, 80),
                RenderViewport::new(420, 64, 860, 576),
            ]
        );
        assert_eq!(video_viewport.size(), (1280, 720));
    }

    #[test]
    fn visible_video_draw_rects_keep_full_viewport_without_exclusions() {
        let video_viewport = RenderViewport::full_surface(1280, 720);

        assert_eq!(
            visible_video_draw_rects(video_viewport, &[]),
            vec![video_viewport]
        );
    }

    /// Строит renderer-neutral frame с заданным render-размером для letterbox тестов.
    fn renderable_test_frame(render_width: u32, render_height: u32) -> RenderableFrame {
        let mut decoded = decoded_nv12_test_frame();
        decoded.render_width = render_width;
        decoded.render_height = render_height;
        renderable_metadata_from_decoded(&decoded, VideoFramePixelLayout::Nv12)
    }

    /// Долю оси, занятую видео, восстанавливаем из uv_scale: видимая полоса = 1 / scale.
    fn visible_axis_fraction(scale: f32) -> f32 {
        1.0 / scale
    }

    #[test]
    fn letterbox_wide_video_uses_viewport_size_for_top_bottom_bars() {
        // Видео 16:9 (1.778) в почти квадратном viewport-е (1.0): кадр шире области.
        let frame = renderable_test_frame(1920, 1080);
        let (uv_scale, uv_offset) = letterbox_scale_and_offset(&frame, (1000, 1000));

        // По горизонтали кадр занимает весь viewport, по вертикали — сжимается с полосами.
        assert_eq!(uv_scale[0], 1.0);
        assert_eq!(uv_offset[0], 0.0);
        // Масштаб > 1 и отрицательное смещение => шейдер уводит края за [0, 1] и красит чёрным.
        assert!(
            uv_scale[1] > 1.0,
            "ожидали scale_y > 1, получили {}",
            uv_scale[1]
        );
        assert!(
            uv_offset[1] < 0.0,
            "ожидали offset_y < 0, получили {}",
            uv_offset[1]
        );
        // Видимая по вертикали доля = viewport_aspect / video_aspect = 1.0 / 1.778.
        let video_aspect = 1920.0_f32 / 1080.0;
        let viewport_aspect = 1.0_f32;
        assert!((visible_axis_fraction(uv_scale[1]) - viewport_aspect / video_aspect).abs() < 1e-4);
    }

    #[test]
    fn letterbox_portrait_video_uses_wide_viewport_size_for_left_right_bars() {
        // Портретное видео 9:16 (0.5625) в широком viewport-е 16:9 (1.778).
        let frame = renderable_test_frame(1080, 1920);
        let (uv_scale, uv_offset) = letterbox_scale_and_offset(&frame, (1920, 1080));

        // По вертикали кадр занимает весь viewport, по горизонтали — полосы слева и справа.
        assert_eq!(uv_scale[1], 1.0);
        assert_eq!(uv_offset[1], 0.0);
        assert!(
            uv_scale[0] > 1.0,
            "ожидали scale_x > 1, получили {}",
            uv_scale[0]
        );
        assert!(
            uv_offset[0] < 0.0,
            "ожидали offset_x < 0, получили {}",
            uv_offset[0]
        );
        let video_aspect = 1080.0_f32 / 1920.0;
        let viewport_aspect = 1920.0_f32 / 1080.0;
        assert!((visible_axis_fraction(uv_scale[0]) - video_aspect / viewport_aspect).abs() < 1e-4);
    }

    #[test]
    fn letterbox_rotated_phone_video_uses_oriented_display_size_inside_viewport() {
        let mut frame = renderable_test_frame(3840, 2160);
        frame.display_orientation = VideoDisplayOrientation::Rotate270Clockwise;

        let (uv_scale, uv_offset) = letterbox_scale_and_offset(&frame, (1920, 1080));

        assert_eq!(uv_scale[1], 1.0);
        assert_eq!(uv_offset[1], 0.0);
        assert!(
            uv_scale[0] > 1.0,
            "rotated portrait video should add left/right bars, got scale_x={}",
            uv_scale[0]
        );
    }

    #[test]
    fn rotate270_clockwise_orientation_maps_display_uv_to_source_uv() {
        let transform =
            display_orientation_uv_transform(VideoDisplayOrientation::Rotate270Clockwise);

        assert_eq!(transform[0], [0.0, -1.0, 1.0, 0.0]);
        assert_eq!(transform[1], [1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn letterbox_matching_aspect_is_identity() {
        // Кадр 16:9 во viewport-е 16:9: ни полос, ни масштабирования.
        let frame = renderable_test_frame(1920, 1080);
        let (uv_scale, uv_offset) = letterbox_scale_and_offset(&frame, (1280, 720));

        assert!((uv_scale[0] - 1.0).abs() < 1e-4);
        assert!((uv_scale[1] - 1.0).abs() < 1e-4);
        assert!(uv_offset[0].abs() < 1e-4);
        assert!(uv_offset[1].abs() < 1e-4);
    }

    #[test]
    fn letterbox_keeps_video_centered_in_clip_space() {
        // Центр экрана (uv = 0.5) должен отображаться в центр текстуры (0.5) на обеих осях.
        let frame = renderable_test_frame(1920, 1080);
        let (uv_scale, uv_offset) = letterbox_scale_and_offset(&frame, (800, 1200));

        let center_x = 0.5 * uv_scale[0] + uv_offset[0];
        let center_y = 0.5 * uv_scale[1] + uv_offset[1];
        assert!((center_x - 0.5).abs() < 1e-4, "center_x = {center_x}");
        assert!((center_y - 0.5).abs() < 1e-4, "center_y = {center_y}");
    }

    fn decoded_nv12_test_frame() -> DecodedFrame {
        DecodedFrame {
            generation: 0,
            pts: Duration::ZERO,
            frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: codec_core::VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: video_core::FrameResourceHandle(1),
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

    fn decoded_p010_test_frame() -> DecodedFrame {
        DecodedFrame {
            generation: 0,
            pts: Duration::ZERO,
            frame_contract: VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
            width: 1920,
            height: 1080,
            render_width: 1920,
            render_height: 1080,
            display_orientation: codec_core::VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: video_core::FrameResourceHandle(7),
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

    fn decoded_host_planar8_test_frame() -> DecodedFrame {
        DecodedFrame {
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
            ..decoded_nv12_test_frame()
        }
    }

    fn decoded_host_planar10_test_frame() -> DecodedFrame {
        DecodedFrame {
            frame_contract: VideoFrameContract::host_yuv420_planar10le(),
            ..decoded_p010_test_frame()
        }
    }

    fn decoded_host_planar12_test_frame() -> DecodedFrame {
        DecodedFrame {
            frame_contract: VideoFrameContract::host_yuv420_planar12le(),
            ..decoded_p010_test_frame()
        }
    }
}
