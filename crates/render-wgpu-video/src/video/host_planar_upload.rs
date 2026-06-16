use std::collections::VecDeque;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, ensure};
use video_backend_api::{PresentFrameResourceDescriptorLookup, PresentFrameResourceProviderHandle};
use video_core::{
    DecodedFrame, FrameResourceDescriptor, FrameResourceHandle, HostPlanarFrameDescriptor,
    HostPlaneRole, validate_resource_descriptor_against_contract,
};
use video_frame_contract::{
    FrameChromaSubsampling, VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath,
};

use super::{
    WgpuFrameMaterializationUnsupportedReason, WgpuFrameTextureViewLookup,
    WgpuFrameTextureViewMaterializer, WgpuFrameTextureViews,
};

/// Renderer-side materializer, который загружает HostPlanar descriptors в WGPU textures.
pub struct HostPlanarWgpuFrameMaterializer {
    inner: HostPlanarFrameMaterializerCore<WgpuHostPlanarUploadBackend>,
}

impl HostPlanarWgpuFrameMaterializer {
    /// Создаёт materializer из WGPU handles renderer layer-а и neutral provider-а.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resource_provider: PresentFrameResourceProviderHandle,
    ) -> Self {
        Self {
            inner: HostPlanarFrameMaterializerCore::new(
                resource_provider,
                HostPlanarUploadTextureCache::new(WgpuHostPlanarUploadBackend::new(
                    device.clone(),
                    queue.clone(),
                )),
            ),
        }
    }

    /// Пытается получить renderer-owned HostPlanar texture views для decoded frame-а.
    ///
    /// Decoder/provider продолжает владеть host frame backing-ом до обычного
    /// release lease; cache ниже владеет только WGPU upload textures.
    pub fn try_host_planar_texture_view_lookup(
        &self,
        frame: &DecodedFrame,
    ) -> HostPlanarWgpuTextureViewLookup {
        HostPlanarWgpuTextureViewLookup::from_core_lookup(self.inner.try_upload_lookup(frame))
    }
}

impl WgpuFrameTextureViewMaterializer for HostPlanarWgpuFrameMaterializer {
    /// Подключает HostPlanar upload к общему app/render materializer trait-у.
    fn try_texture_view_lookup(&self, frame: &DecodedFrame) -> WgpuFrameTextureViewLookup {
        common_lookup_from_host_planar_lookup(self.try_host_planar_texture_view_lookup(frame))
    }
}

/// WGPU texture views, загруженные из HostPlanar Y/U/V planes.
#[derive(Clone)]
pub struct HostPlanarWgpuTextureViews {
    /// Texture view с luma/Y plane.
    pub y_view: wgpu::TextureView,

    /// Texture view с chroma U/Cb plane.
    pub u_view: wgpu::TextureView,

    /// Texture view с chroma V/Cr plane.
    pub v_view: wgpu::TextureView,

    /// Guard удерживает renderer-owned upload textures живыми до drop-а views.
    _uploaded_texture_guard: Arc<WgpuHostPlanarUploadedTextures>,
}

impl HostPlanarWgpuTextureViews {
    fn from_uploaded_textures(uploaded_textures: Arc<WgpuHostPlanarUploadedTextures>) -> Self {
        Self {
            y_view: uploaded_textures.y_view.clone(),
            u_view: uploaded_textures.u_view.clone(),
            v_view: uploaded_textures.v_view.clone(),
            _uploaded_texture_guard: uploaded_textures,
        }
    }
}

/// Результат HostPlanar upload materialization без участия playback core.
pub enum HostPlanarWgpuTextureViewLookup {
    /// Renderer получил texture views, которые принадлежат upload cache.
    Ready {
        /// Views трёх planar Y/U/V textures.
        views: Box<HostPlanarWgpuTextureViews>,

        /// Сколько render thread ждал lock texture/cache pool-а.
        texture_pool_lock_wait: Duration,
    },

    /// Backend resource pool или renderer upload cache занят.
    Busy {
        /// Сколько заняла non-blocking попытка получить lock.
        texture_pool_lock_wait: Duration,
    },

    /// Backend доступен, но resource для handle отсутствует.
    Missing {
        /// Сколько render thread ждал lock resource/cache pool-а.
        texture_pool_lock_wait: Duration,
    },

    /// Descriptor существует, но этот materializer не умеет такой resource/layout.
    Unsupported {
        /// Техническая причина отказа materializer-а.
        reason: WgpuFrameMaterializationUnsupportedReason,

        /// Сколько render thread ждал lock resource/cache pool-а.
        texture_pool_lock_wait: Duration,
    },

    /// Provider или renderer upload path обнаружил fatal/materialization error.
    Error {
        /// Сколько render thread ждал lock resource/cache pool-а.
        texture_pool_lock_wait: Duration,
    },
}

impl HostPlanarWgpuTextureViewLookup {
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

    /// Возвращает `true`, если lookup не стал ждать занятый resource/cache lock.
    #[must_use]
    pub const fn lookup_was_busy(&self) -> bool {
        matches!(self, Self::Busy { .. })
    }

    fn from_core_lookup(
        lookup: HostPlanarUploadLookup<Arc<WgpuHostPlanarUploadedTextures>>,
    ) -> Self {
        match lookup {
            HostPlanarUploadLookup::Ready {
                uploaded_textures,
                texture_pool_lock_wait,
            } => Self::Ready {
                views: Box::new(HostPlanarWgpuTextureViews::from_uploaded_textures(
                    uploaded_textures,
                )),
                texture_pool_lock_wait,
            },
            HostPlanarUploadLookup::Busy {
                texture_pool_lock_wait,
            } => Self::Busy {
                texture_pool_lock_wait,
            },
            HostPlanarUploadLookup::Missing {
                texture_pool_lock_wait,
            } => Self::Missing {
                texture_pool_lock_wait,
            },
            HostPlanarUploadLookup::Unsupported {
                reason,
                texture_pool_lock_wait,
            } => Self::Unsupported {
                reason,
                texture_pool_lock_wait,
            },
            HostPlanarUploadLookup::Error {
                texture_pool_lock_wait,
            } => Self::Error {
                texture_pool_lock_wait,
            },
        }
    }
}

fn common_lookup_from_host_planar_lookup(
    lookup: HostPlanarWgpuTextureViewLookup,
) -> WgpuFrameTextureViewLookup {
    match lookup {
        HostPlanarWgpuTextureViewLookup::Ready {
            views,
            texture_pool_lock_wait,
        } => WgpuFrameTextureViewLookup::Ready {
            views: WgpuFrameTextureViews::from_host_planar_texture_views(views),
            texture_pool_lock_wait,
        },
        HostPlanarWgpuTextureViewLookup::Busy {
            texture_pool_lock_wait,
        } => WgpuFrameTextureViewLookup::Busy {
            texture_pool_lock_wait,
        },
        HostPlanarWgpuTextureViewLookup::Missing {
            texture_pool_lock_wait,
        } => WgpuFrameTextureViewLookup::Missing {
            texture_pool_lock_wait,
        },
        HostPlanarWgpuTextureViewLookup::Unsupported {
            reason,
            texture_pool_lock_wait,
        } => WgpuFrameTextureViewLookup::Unsupported {
            reason,
            texture_pool_lock_wait,
        },
        HostPlanarWgpuTextureViewLookup::Error {
            texture_pool_lock_wait,
        } => WgpuFrameTextureViewLookup::Error {
            texture_pool_lock_wait,
        },
    }
}

struct HostPlanarFrameMaterializerCore<B>
where
    B: HostPlanarUploadBackend,
{
    resource_provider: PresentFrameResourceProviderHandle,
    texture_cache: Mutex<HostPlanarUploadTextureCache<B>>,
}

impl<B> HostPlanarFrameMaterializerCore<B>
where
    B: HostPlanarUploadBackend,
{
    fn new(
        resource_provider: PresentFrameResourceProviderHandle,
        texture_cache: HostPlanarUploadTextureCache<B>,
    ) -> Self {
        Self {
            resource_provider,
            texture_cache: Mutex::new(texture_cache),
        }
    }

    fn try_upload_lookup(
        &self,
        frame: &DecodedFrame,
    ) -> HostPlanarUploadLookup<B::UploadedTextures> {
        let provider_lookup = self
            .resource_provider
            .try_resource_descriptor_lookup(frame.resource_handle);
        let provider_wait = provider_lookup.resource_pool_lock_wait();

        let descriptor = match provider_lookup {
            PresentFrameResourceDescriptorLookup::Ready { descriptor, .. } => descriptor,
            PresentFrameResourceDescriptorLookup::Busy { .. } => {
                return HostPlanarUploadLookup::Busy {
                    texture_pool_lock_wait: provider_wait,
                };
            }
            PresentFrameResourceDescriptorLookup::Missing { .. } => {
                return HostPlanarUploadLookup::Missing {
                    texture_pool_lock_wait: provider_wait,
                };
            }
            PresentFrameResourceDescriptorLookup::Fatal { .. } => {
                return HostPlanarUploadLookup::Error {
                    texture_pool_lock_wait: provider_wait,
                };
            }
        };

        let host_descriptor = match descriptor {
            FrameResourceDescriptor::HostPlanar(host_descriptor) => host_descriptor,
            FrameResourceDescriptor::DmaBuf(_) => {
                return HostPlanarUploadLookup::Unsupported {
                    reason:
                        WgpuFrameMaterializationUnsupportedReason::DmaBufRequiresDmaBufMaterializer,
                    texture_pool_lock_wait: provider_wait,
                };
            }
        };

        let cache_lock_started_at = Instant::now();
        let mut texture_cache = match self.texture_cache.try_lock() {
            Ok(texture_cache) => texture_cache,
            Err(TryLockError::WouldBlock) => {
                return HostPlanarUploadLookup::Busy {
                    texture_pool_lock_wait: provider_wait
                        .saturating_add(cache_lock_started_at.elapsed()),
                };
            }
            Err(TryLockError::Poisoned(error)) => {
                tracing::warn!(error = %error, "WGPU HostPlanar upload cache mutex poisoned");
                return HostPlanarUploadLookup::Error {
                    texture_pool_lock_wait: provider_wait
                        .saturating_add(cache_lock_started_at.elapsed()),
                };
            }
        };
        let total_lock_wait = provider_wait.saturating_add(cache_lock_started_at.elapsed());

        match texture_cache.materialize(frame, host_descriptor) {
            Ok(uploaded_textures) => HostPlanarUploadLookup::Ready {
                uploaded_textures,
                texture_pool_lock_wait: total_lock_wait,
            },
            Err(HostPlanarUploadFailure::Unsupported(reason)) => {
                HostPlanarUploadLookup::Unsupported {
                    reason,
                    texture_pool_lock_wait: total_lock_wait,
                }
            }
            Err(HostPlanarUploadFailure::Error(error)) => host_planar_lookup_after_upload_failure(
                frame.resource_handle,
                error,
                total_lock_wait,
            ),
        }
    }
}

enum HostPlanarUploadLookup<T> {
    Ready {
        uploaded_textures: T,
        texture_pool_lock_wait: Duration,
    },
    Busy {
        texture_pool_lock_wait: Duration,
    },
    Missing {
        texture_pool_lock_wait: Duration,
    },
    Unsupported {
        reason: WgpuFrameMaterializationUnsupportedReason,
        texture_pool_lock_wait: Duration,
    },
    Error {
        texture_pool_lock_wait: Duration,
    },
}

struct HostPlanarUploadTextureCache<B>
where
    B: HostPlanarUploadBackend,
{
    backend: B,
    cached_textures: VecDeque<CachedHostPlanarTexture<B::UploadedTextures>>,
    capacity: usize,
}

impl<B> HostPlanarUploadTextureCache<B>
where
    B: HostPlanarUploadBackend,
{
    const DEFAULT_CAPACITY: usize = 24;

    fn new(backend: B) -> Self {
        Self {
            backend,
            cached_textures: VecDeque::with_capacity(Self::DEFAULT_CAPACITY),
            capacity: Self::DEFAULT_CAPACITY,
        }
    }

    #[cfg(test)]
    fn with_capacity(backend: B, capacity: usize) -> Self {
        Self {
            backend,
            cached_textures: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn materialize(
        &mut self,
        frame: &DecodedFrame,
        host_descriptor: HostPlanarFrameDescriptor,
    ) -> std::result::Result<B::UploadedTextures, HostPlanarUploadFailure> {
        let layout = host_planar_upload_layout(frame.frame_contract, frame.width, frame.height)
            .map_err(HostPlanarUploadFailure::Unsupported)?;

        let descriptor_for_validation =
            FrameResourceDescriptor::HostPlanar(host_descriptor.clone());
        validate_resource_descriptor_against_contract(
            frame.frame_contract,
            frame.width,
            frame.height,
            &descriptor_for_validation,
        )
        .map_err(HostPlanarUploadFailure::Error)?;

        if let Some(cached_texture) = self
            .cached_textures
            .iter()
            .find(|cached_texture| cached_texture.handle == frame.resource_handle)
        {
            return Ok(cached_texture.uploaded_textures.clone());
        }

        let uploaded_textures = self
            .backend
            .allocate_textures(layout)
            .map_err(HostPlanarUploadFailure::Error)?;
        upload_host_planar_visible_rows(
            &host_descriptor,
            layout,
            &uploaded_textures,
            &mut self.backend,
        )
        .map_err(HostPlanarUploadFailure::Error)?;

        if self.capacity > 0 {
            if self.cached_textures.len() >= self.capacity {
                self.cached_textures.pop_front();
            }
            self.cached_textures.push_back(CachedHostPlanarTexture {
                handle: frame.resource_handle,
                uploaded_textures: uploaded_textures.clone(),
            });
        }

        Ok(uploaded_textures)
    }
}

struct CachedHostPlanarTexture<T> {
    handle: FrameResourceHandle,
    uploaded_textures: T,
}

#[derive(Debug)]
enum HostPlanarUploadFailure {
    Unsupported(WgpuFrameMaterializationUnsupportedReason),
    Error(anyhow::Error),
}

trait HostPlanarUploadBackend {
    type UploadedTextures: Clone;

    fn allocate_textures(
        &mut self,
        layout: HostPlanarUploadLayout,
    ) -> Result<Self::UploadedTextures>;

    fn upload_plane_row(
        &mut self,
        uploaded_textures: &Self::UploadedTextures,
        plane_index: usize,
        row_index: u32,
        row_bytes: &[u8],
    ) -> Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HostPlanarUploadLayout {
    planes: [HostPlanarUploadPlaneLayout; 3],
}

impl HostPlanarUploadLayout {
    fn plane(self, plane_index: usize) -> Result<HostPlanarUploadPlaneLayout> {
        self.planes.get(plane_index).copied().with_context(|| {
            format!("host-planar upload plane index {plane_index} is out of bounds")
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HostPlanarUploadPlaneLayout {
    role: HostPlaneRole,
    width: u32,
    height: u32,
    bytes_per_sample: usize,
    texture_format: HostPlanarUploadTextureFormat,
}

impl HostPlanarUploadPlaneLayout {
    fn visible_row_bytes(self) -> Result<usize> {
        let width = usize::try_from(self.width)
            .context("host-planar upload plane width does not fit usize")?;
        width
            .checked_mul(self.bytes_per_sample)
            .context("host-planar upload visible row byte count overflow")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostPlanarUploadTextureFormat {
    R8Unorm,
    R16Uint,
}

impl HostPlanarUploadTextureFormat {
    const fn wgpu_format(self) -> wgpu::TextureFormat {
        match self {
            Self::R8Unorm => wgpu::TextureFormat::R8Unorm,
            Self::R16Uint => wgpu::TextureFormat::R16Uint,
        }
    }
}

struct WgpuHostPlanarUploadBackend {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl WgpuHostPlanarUploadBackend {
    fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self { device, queue }
    }
}

impl HostPlanarUploadBackend for WgpuHostPlanarUploadBackend {
    type UploadedTextures = Arc<WgpuHostPlanarUploadedTextures>;

    fn allocate_textures(
        &mut self,
        layout: HostPlanarUploadLayout,
    ) -> Result<Self::UploadedTextures> {
        Ok(Arc::new(WgpuHostPlanarUploadedTextures::new(
            &self.device,
            layout,
        )))
    }

    fn upload_plane_row(
        &mut self,
        uploaded_textures: &Self::UploadedTextures,
        plane_index: usize,
        row_index: u32,
        row_bytes: &[u8],
    ) -> Result<()> {
        let plane_layout = uploaded_textures.layout.plane(plane_index)?;
        let texture = uploaded_textures.plane_texture(plane_index)?;
        let expected_row_bytes = plane_layout.visible_row_bytes()?;

        ensure!(
            row_bytes.len() == expected_row_bytes,
            "host-planar {:?} upload row has {} bytes, expected {}",
            plane_layout.role,
            row_bytes.len(),
            expected_row_bytes
        );
        ensure!(
            row_index < plane_layout.height,
            "host-planar {:?} upload row {} is outside texture height {}",
            plane_layout.role,
            row_index,
            plane_layout.height
        );

        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: row_index,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            row_bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: None,
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: plane_layout.width,
                height: 1,
                depth_or_array_layers: 1,
            },
        );

        Ok(())
    }
}

struct WgpuHostPlanarUploadedTextures {
    layout: HostPlanarUploadLayout,
    y_texture: wgpu::Texture,
    u_texture: wgpu::Texture,
    v_texture: wgpu::Texture,
    y_view: wgpu::TextureView,
    u_view: wgpu::TextureView,
    v_view: wgpu::TextureView,
}

impl WgpuHostPlanarUploadedTextures {
    fn new(device: &wgpu::Device, layout: HostPlanarUploadLayout) -> Self {
        let y_texture =
            create_upload_plane_texture(device, "host planar Y upload texture", layout.planes[0]);
        let u_texture =
            create_upload_plane_texture(device, "host planar U upload texture", layout.planes[1]);
        let v_texture =
            create_upload_plane_texture(device, "host planar V upload texture", layout.planes[2]);
        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("host planar Y upload texture view"),
            ..Default::default()
        });
        let u_view = u_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("host planar U upload texture view"),
            ..Default::default()
        });
        let v_view = v_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("host planar V upload texture view"),
            ..Default::default()
        });

        Self {
            layout,
            y_texture,
            u_texture,
            v_texture,
            y_view,
            u_view,
            v_view,
        }
    }

    fn plane_texture(&self, plane_index: usize) -> Result<&wgpu::Texture> {
        match plane_index {
            0 => Ok(&self.y_texture),
            1 => Ok(&self.u_texture),
            2 => Ok(&self.v_texture),
            _ => Err(anyhow!(
                "host-planar upload plane index {plane_index} is out of bounds"
            )),
        }
    }
}

fn create_upload_plane_texture(
    device: &wgpu::Device,
    label: &'static str,
    plane_layout: HostPlanarUploadPlaneLayout,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: plane_layout.width,
            height: plane_layout.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: plane_layout.texture_format.wgpu_format(),
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn upload_host_planar_visible_rows<B>(
    descriptor: &HostPlanarFrameDescriptor,
    layout: HostPlanarUploadLayout,
    uploaded_textures: &B::UploadedTextures,
    backend: &mut B,
) -> Result<()>
where
    B: HostPlanarUploadBackend,
{
    for (plane_index, plane_layout) in layout.planes.into_iter().enumerate() {
        upload_host_planar_plane_visible_rows(
            descriptor,
            plane_index,
            plane_layout,
            uploaded_textures,
            backend,
        )?;
    }

    Ok(())
}

fn upload_host_planar_plane_visible_rows<B>(
    descriptor: &HostPlanarFrameDescriptor,
    plane_index: usize,
    plane_layout: HostPlanarUploadPlaneLayout,
    uploaded_textures: &B::UploadedTextures,
    backend: &mut B,
) -> Result<()>
where
    B: HostPlanarUploadBackend,
{
    let descriptor_plane = descriptor
        .planes
        .get(plane_index)
        .with_context(|| format!("host-planar descriptor plane index {plane_index} is missing"))?;

    ensure!(
        descriptor_plane.role == plane_layout.role,
        "host-planar upload plane {plane_index} has role {:?}, expected {:?}",
        descriptor_plane.role,
        plane_layout.role
    );
    ensure!(
        descriptor_plane.visible_width == plane_layout.width
            && descriptor_plane.visible_height == plane_layout.height
            && descriptor_plane.bytes_per_sample == plane_layout.bytes_per_sample,
        "host-planar {:?} descriptor metadata does not match upload layout",
        descriptor_plane.role
    );

    let expected_row_bytes = plane_layout.visible_row_bytes()?;
    for row_index in 0..plane_layout.height {
        let row_bytes = descriptor
            .visible_plane_row_bytes(plane_index, row_index)
            .with_context(|| {
                format!(
                    "failed to read visible row {row_index} for host-planar {:?} upload",
                    plane_layout.role
                )
            })?;
        ensure!(
            row_bytes.len() == expected_row_bytes,
            "host-planar {:?} visible row has {} bytes, expected {}",
            plane_layout.role,
            row_bytes.len(),
            expected_row_bytes
        );

        backend.upload_plane_row(uploaded_textures, plane_index, row_index, row_bytes)?;
    }

    Ok(())
}

fn host_planar_upload_layout(
    frame_contract: VideoFrameContract,
    coded_width: u32,
    coded_height: u32,
) -> std::result::Result<HostPlanarUploadLayout, WgpuFrameMaterializationUnsupportedReason> {
    if frame_contract.transfer_path != VideoFrameTransferPath::SoftwareHostUpload {
        return Err(
            WgpuFrameMaterializationUnsupportedReason::HostPlanarRequiresSoftwareUploadContract,
        );
    }

    let Some(chroma_subsampling) = frame_contract.pixel_layout.chroma() else {
        return Err(
            WgpuFrameMaterializationUnsupportedReason::HostPlanarLayoutNotSupportedByUploadMaterializer,
        );
    };

    match frame_contract.pixel_layout {
        VideoFramePixelLayout::Yuv420Planar8
        | VideoFramePixelLayout::Yuv422Planar8
        | VideoFramePixelLayout::Yuv444Planar8 => Ok(planar_yuv_upload_layout(
            coded_width,
            coded_height,
            chroma_subsampling,
            1,
            HostPlanarUploadTextureFormat::R8Unorm,
        )),
        VideoFramePixelLayout::Yuv420Planar10Le
        | VideoFramePixelLayout::Yuv420Planar12Le
        | VideoFramePixelLayout::Yuv422Planar10Le
        | VideoFramePixelLayout::Yuv422Planar12Le
        | VideoFramePixelLayout::Yuv444Planar10Le => Ok(planar_yuv_upload_layout(
            coded_width,
            coded_height,
            chroma_subsampling,
            2,
            HostPlanarUploadTextureFormat::R16Uint,
        )),
        _ => Err(
            WgpuFrameMaterializationUnsupportedReason::HostPlanarLayoutNotSupportedByUploadMaterializer,
        ),
    }
}

fn planar_yuv_upload_layout(
    coded_width: u32,
    coded_height: u32,
    chroma_subsampling: FrameChromaSubsampling,
    bytes_per_sample: usize,
    texture_format: HostPlanarUploadTextureFormat,
) -> HostPlanarUploadLayout {
    let (chroma_width, chroma_height) =
        chroma_plane_dimensions(coded_width, coded_height, chroma_subsampling);

    HostPlanarUploadLayout {
        planes: [
            HostPlanarUploadPlaneLayout {
                role: HostPlaneRole::Luma,
                width: coded_width,
                height: coded_height,
                bytes_per_sample,
                texture_format,
            },
            HostPlanarUploadPlaneLayout {
                role: HostPlaneRole::ChromaU,
                width: chroma_width,
                height: chroma_height,
                bytes_per_sample,
                texture_format,
            },
            HostPlanarUploadPlaneLayout {
                role: HostPlaneRole::ChromaV,
                width: chroma_width,
                height: chroma_height,
                bytes_per_sample,
                texture_format,
            },
        ],
    }
}

fn chroma_plane_dimensions(
    coded_width: u32,
    coded_height: u32,
    chroma_subsampling: FrameChromaSubsampling,
) -> (u32, u32) {
    // В host-planar descriptor padding не является пикселями, поэтому размеры
    // chroma plane считаются только из видимой coded области.
    match chroma_subsampling {
        FrameChromaSubsampling::Yuv420 => {
            (half_rounded_up(coded_width), half_rounded_up(coded_height))
        }
        FrameChromaSubsampling::Yuv422 => (half_rounded_up(coded_width), coded_height),
        FrameChromaSubsampling::Yuv444 => (coded_width, coded_height),
    }
}

const fn half_rounded_up(value: u32) -> u32 {
    value.div_ceil(2)
}

fn host_planar_lookup_after_upload_failure<T>(
    handle: FrameResourceHandle,
    error: anyhow::Error,
    texture_pool_lock_wait: Duration,
) -> HostPlanarUploadLookup<T> {
    tracing::warn!(
        error = %error,
        handle_id = handle.0,
        "Renderer HostPlanar upload failed; CPU conversion fallback is disabled"
    );
    HostPlanarUploadLookup::Error {
        texture_pool_lock_wait,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use codec_core::VideoColorMetadata;
    use video_backend_api::{PresentFrameResourceProvider, PresentFrameResourceProviderLookup};
    use video_core::{
        DmaBufFrameDescriptor, DmaBufFrameExportLayout, HostPlanarFrameDescriptor,
        HostPlaneDescriptor,
    };
    use video_frame_contract::VideoFrameContract;

    use super::*;

    #[test]
    fn host_planar_materializer_rejects_dma_buf_descriptor() {
        let provider = RecordingDescriptorProvider::new_dma_buf();
        let materializer = HostPlanarFrameMaterializerCore::new(
            provider.handle(),
            HostPlanarUploadTextureCache::new(RecordingUploadBackend::default()),
        );

        let lookup = materializer
            .try_upload_lookup(&decoded_host_planar8_test_frame(FrameResourceHandle(11)));

        assert!(matches!(
            lookup,
            HostPlanarUploadLookup::Unsupported {
                reason: WgpuFrameMaterializationUnsupportedReason::DmaBufRequiresDmaBufMaterializer,
                ..
            }
        ));
        assert!(provider.released_handles().is_empty());
    }

    #[test]
    fn upload_path_copies_visible_bytes_only_with_padded_stride() {
        let descriptor = padded_host_planar_descriptor();
        let mut cache = HostPlanarUploadTextureCache::new(RecordingUploadBackend::default());
        let frame = decoded_host_planar8_test_frame(FrameResourceHandle(21));

        let uploaded_textures = cache
            .materialize(&frame, descriptor)
            .expect("host-planar upload should accept padded visible rows");

        assert_eq!(
            uploaded_textures.rows_for_plane(0),
            vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]]
        );
        assert_eq!(uploaded_textures.rows_for_plane(1), vec![vec![9, 10]]);
        assert_eq!(uploaded_textures.rows_for_plane(2), vec![vec![11, 12]]);
    }

    #[test]
    fn upload_cache_drops_gpu_textures_without_bypassing_provider_release() {
        let provider = RecordingDescriptorProvider::new_host_planar();
        let drop_count = Arc::new(AtomicUsize::new(0));
        let backend = RecordingUploadBackend::with_drop_counter(Arc::clone(&drop_count));
        let materializer = HostPlanarFrameMaterializerCore::new(
            provider.handle(),
            HostPlanarUploadTextureCache::with_capacity(backend, 1),
        );

        let first_handle = FrameResourceHandle(31);
        let second_handle = FrameResourceHandle(32);
        let first_frame = decoded_host_planar8_test_frame(first_handle);
        let second_frame = decoded_host_planar8_test_frame(second_handle);

        assert!(matches!(
            materializer.try_upload_lookup(&first_frame),
            HostPlanarUploadLookup::Ready { .. }
        ));
        assert!(matches!(
            materializer.try_upload_lookup(&second_frame),
            HostPlanarUploadLookup::Ready { .. }
        ));

        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
        assert!(provider.released_handles().is_empty());

        provider.handle().release_frame(first_handle);

        assert_eq!(provider.released_handles(), vec![first_handle]);
    }

    #[test]
    fn yuv420_upload_layout_uses_ceil_chroma_dimensions_for_all_ready_bit_depths() {
        let layout = host_planar_upload_layout(VideoFrameContract::host_yuv420_planar8(), 5, 3)
            .expect("YUV420 planar 8-bit is renderable");

        assert_upload_plane_layout(
            layout,
            [(5, 3), (3, 2), (3, 2)],
            1,
            HostPlanarUploadTextureFormat::R8Unorm,
        );

        for contract in [
            VideoFrameContract::host_yuv420_planar10le(),
            VideoFrameContract::host_yuv420_planar12le(),
        ] {
            let layout =
                host_planar_upload_layout(contract, 5, 3).expect("YUV420 16-bit words renderable");

            assert_upload_plane_layout(
                layout,
                [(5, 3), (3, 2), (3, 2)],
                2,
                HostPlanarUploadTextureFormat::R16Uint,
            );
        }
    }

    #[test]
    fn yuv422_upload_layout_uses_ceil_chroma_width_and_full_height() {
        for (coded_width, expected_chroma_width) in [(4, 2), (5, 3)] {
            let layout = host_planar_upload_layout(
                VideoFrameContract {
                    pixel_layout: VideoFramePixelLayout::Yuv422Planar8,
                    transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
                },
                coded_width,
                3,
            )
            .expect("YUV422 planar 8-bit is renderable");

            assert_upload_plane_layout(
                layout,
                [
                    (coded_width, 3),
                    (expected_chroma_width, 3),
                    (expected_chroma_width, 3),
                ],
                1,
                HostPlanarUploadTextureFormat::R8Unorm,
            );
        }

        for pixel_layout in [
            VideoFramePixelLayout::Yuv422Planar10Le,
            VideoFramePixelLayout::Yuv422Planar12Le,
        ] {
            let layout = host_planar_upload_layout(
                VideoFrameContract {
                    pixel_layout,
                    transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
                },
                5,
                3,
            )
            .expect("YUV422 16-bit words are renderable");

            assert_upload_plane_layout(
                layout,
                [(5, 3), (3, 3), (3, 3)],
                2,
                HostPlanarUploadTextureFormat::R16Uint,
            );
        }
    }

    #[test]
    fn yuv444_upload_layout_keeps_chroma_planes_full_size() {
        for (pixel_layout, bytes_per_sample, texture_format) in [
            (
                VideoFramePixelLayout::Yuv444Planar8,
                1,
                HostPlanarUploadTextureFormat::R8Unorm,
            ),
            (
                VideoFramePixelLayout::Yuv444Planar10Le,
                2,
                HostPlanarUploadTextureFormat::R16Uint,
            ),
        ] {
            let layout = host_planar_upload_layout(
                VideoFrameContract {
                    pixel_layout,
                    transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
                },
                5,
                3,
            )
            .expect("YUV444 v1 layout is renderable");

            assert_upload_plane_layout(
                layout,
                [(5, 3), (5, 3), (5, 3)],
                bytes_per_sample,
                texture_format,
            );
        }
    }

    fn assert_upload_plane_layout(
        layout: HostPlanarUploadLayout,
        expected_dimensions: [(u32, u32); 3],
        expected_bytes_per_sample: usize,
        expected_texture_format: HostPlanarUploadTextureFormat,
    ) {
        for (plane_layout, (expected_width, expected_height)) in
            layout.planes.into_iter().zip(expected_dimensions)
        {
            assert_eq!(plane_layout.width, expected_width);
            assert_eq!(plane_layout.height, expected_height);
            assert_eq!(plane_layout.bytes_per_sample, expected_bytes_per_sample);
            assert_eq!(plane_layout.texture_format, expected_texture_format);
        }
    }

    #[derive(Clone)]
    struct RecordingUploadedTextures {
        planes: Arc<Mutex<[Vec<Vec<u8>>; 3]>>,
        _drop_probe: Option<Arc<DropProbe>>,
    }

    impl RecordingUploadedTextures {
        fn rows_for_plane(&self, plane_index: usize) -> Vec<Vec<u8>> {
            self.planes.lock().expect("recorded rows lock poisoned")[plane_index].clone()
        }
    }

    #[derive(Default)]
    struct RecordingUploadBackend {
        drop_count: Option<Arc<AtomicUsize>>,
    }

    impl RecordingUploadBackend {
        fn with_drop_counter(drop_count: Arc<AtomicUsize>) -> Self {
            Self {
                drop_count: Some(drop_count),
            }
        }
    }

    impl HostPlanarUploadBackend for RecordingUploadBackend {
        type UploadedTextures = RecordingUploadedTextures;

        fn allocate_textures(
            &mut self,
            _layout: HostPlanarUploadLayout,
        ) -> Result<Self::UploadedTextures> {
            Ok(RecordingUploadedTextures {
                planes: Arc::new(Mutex::new([Vec::new(), Vec::new(), Vec::new()])),
                _drop_probe: self
                    .drop_count
                    .as_ref()
                    .map(|drop_count| Arc::new(DropProbe(Arc::clone(drop_count)))),
            })
        }

        fn upload_plane_row(
            &mut self,
            uploaded_textures: &Self::UploadedTextures,
            plane_index: usize,
            _row_index: u32,
            row_bytes: &[u8],
        ) -> Result<()> {
            uploaded_textures
                .planes
                .lock()
                .expect("recorded rows lock poisoned")[plane_index]
                .push(row_bytes.to_vec());
            Ok(())
        }
    }

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Clone)]
    struct RecordingDescriptorProvider {
        descriptor_kind: RecordingDescriptorKind,
        released_handles: Arc<Mutex<Vec<FrameResourceHandle>>>,
    }

    impl RecordingDescriptorProvider {
        fn new_host_planar() -> Self {
            Self {
                descriptor_kind: RecordingDescriptorKind::HostPlanar,
                released_handles: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn new_dma_buf() -> Self {
            Self {
                descriptor_kind: RecordingDescriptorKind::DmaBuf,
                released_handles: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn handle(&self) -> PresentFrameResourceProviderHandle {
            PresentFrameResourceProviderHandle::new(self.clone())
        }

        fn released_handles(&self) -> Vec<FrameResourceHandle> {
            self.released_handles
                .lock()
                .expect("released handles lock poisoned")
                .clone()
        }

        fn descriptor(&self) -> FrameResourceDescriptor {
            match self.descriptor_kind {
                RecordingDescriptorKind::HostPlanar => {
                    FrameResourceDescriptor::HostPlanar(padded_host_planar_descriptor())
                }
                RecordingDescriptorKind::DmaBuf => {
                    FrameResourceDescriptor::DmaBuf(DmaBufFrameDescriptor {
                        resource_id: 9,
                        fourcc: 0,
                        export_layout: DmaBufFrameExportLayout::SeparateLayers,
                        width: 4,
                        height: 2,
                        objects: Vec::new(),
                        layers: Vec::new(),
                    })
                }
            }
        }
    }

    #[derive(Clone, Copy)]
    enum RecordingDescriptorKind {
        HostPlanar,
        DmaBuf,
    }

    impl PresentFrameResourceProvider for RecordingDescriptorProvider {
        fn resource_lookup(
            &self,
            _handle: FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            PresentFrameResourceProviderLookup::Ready {
                resource_pool_lock_wait: Duration::from_millis(1),
            }
        }

        fn try_resource_descriptor_lookup(
            &self,
            _handle: FrameResourceHandle,
        ) -> PresentFrameResourceDescriptorLookup {
            PresentFrameResourceDescriptorLookup::Ready {
                descriptor: self.descriptor(),
                resource_pool_lock_wait: Duration::from_millis(1),
            }
        }

        fn release_frame(&self, handle: FrameResourceHandle) {
            self.released_handles
                .lock()
                .expect("released handles lock poisoned")
                .push(handle);
        }
    }

    fn padded_host_planar_descriptor() -> HostPlanarFrameDescriptor {
        HostPlanarFrameDescriptor::from_owned_buffer(
            vec![
                1, 2, 3, 4, 99, 98, 5, 6, 7, 8, 97, 96, 9, 10, 95, 11, 12, 94,
            ],
            vec![
                HostPlaneDescriptor {
                    role: HostPlaneRole::Luma,
                    offset: 0,
                    stride: 6,
                    visible_width: 4,
                    visible_height: 2,
                    bytes_per_sample: 1,
                },
                HostPlaneDescriptor {
                    role: HostPlaneRole::ChromaU,
                    offset: 12,
                    stride: 3,
                    visible_width: 2,
                    visible_height: 1,
                    bytes_per_sample: 1,
                },
                HostPlaneDescriptor {
                    role: HostPlaneRole::ChromaV,
                    offset: 15,
                    stride: 3,
                    visible_width: 2,
                    visible_height: 1,
                    bytes_per_sample: 1,
                },
            ],
        )
    }

    fn decoded_host_planar8_test_frame(resource_handle: FrameResourceHandle) -> DecodedFrame {
        DecodedFrame {
            generation: 0,
            pts: Duration::ZERO,
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
            width: 4,
            height: 2,
            render_width: 4,
            render_height: 2,
            display_orientation: codec_core::VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle,
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }
}
