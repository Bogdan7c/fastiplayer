//! DMA-BUF materializer и bounded cache renderer-а.
//!
//! Модуль владеет преобразованием neutral resource descriptor в импортированные
//! WGPU texture views. Renderer facade не знает устройство cache/importer-а.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use anyhow::bail;
use video_backend_api::{PresentFrameResourceDescriptorLookup, PresentFrameResourceProviderHandle};
use video_core::{
    DecodedFrame, FrameResourceDescriptor, FrameResourceHandle,
    validate_dma_buf_descriptor_import_topology,
};

use crate::dma_buf_import::{DmaBufImporter, ImportedDmaBufTexture};

use super::{
    WgpuFrameMaterializationUnsupportedReason, WgpuFrameTextureViewLookup,
    WgpuFrameTextureViewMaterializer, WgpuFrameTextureViews,
    texture_view_lookup_after_import_failure,
};

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
    fn try_texture_view_lookup(&self, frame: &DecodedFrame) -> WgpuFrameTextureViewLookup {
        let handle = frame.resource_handle;
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

        let FrameResourceDescriptor::DmaBuf(dma_buf_descriptor) = &descriptor else {
            unreachable!("non-DMA-BUF descriptor was rejected above");
        };
        if let Err(rejection) = validate_dma_buf_descriptor_import_topology(dma_buf_descriptor) {
            return WgpuFrameTextureViewLookup::Unsupported {
                reason: WgpuFrameMaterializationUnsupportedReason::DmaBufDescriptorRejected(
                    rejection,
                ),
                texture_pool_lock_wait: provider_wait,
            };
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

    fn recreate_for_renderer(
        &self,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
    ) -> Arc<dyn WgpuFrameTextureViewMaterializer> {
        Arc::new(Self::new(
            instance,
            adapter,
            device,
            self.resource_provider.clone(),
        ))
    }
}

pub(super) fn unsupported_lookup_for_non_dma_buf_descriptor(
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
