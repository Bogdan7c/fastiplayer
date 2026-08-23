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
    validate_dma_buf_descriptor_against_frame_contract,
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

    /// Создаёт materializer с test double-ом ровно на unsafe importer boundary.
    #[cfg(test)]
    fn new_with_importer(
        resource_provider: PresentFrameResourceProviderHandle,
        importer: impl DmaBufTextureImporter + 'static,
    ) -> Self {
        Self {
            resource_provider,
            texture_cache: Mutex::new(DmaBufWgpuTextureCache::new(importer)),
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
        if let Err(rejection) = validate_dma_buf_descriptor_against_frame_contract(
            frame.frame_contract,
            frame.width,
            frame.height,
            dma_buf_descriptor,
        ) {
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
    importer: Box<dyn DmaBufTextureImporter>,

    /// FIFO cache по frame resource handle; views держат storage через Arc guard.
    cached_textures: VecDeque<CachedDmaBufTexture>,

    /// Верхняя граница renderer-owned imported textures.
    capacity: usize,
}

impl DmaBufWgpuTextureCache {
    /// Default cache size совпадает с bounded decoder/resource pool порядком.
    const DEFAULT_CAPACITY: usize = 24;

    /// Создаёт cache вокруг renderer-owned importer-а.
    fn new(importer: impl DmaBufTextureImporter + 'static) -> Self {
        Self {
            importer: Box::new(importer),
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
        let imported_texture = Arc::new(self.importer.import(&dma_buf_descriptor)?);
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

/// Internal boundary позволяет тесту доказать, что contract rejection предшествует unsafe import-у.
trait DmaBufTextureImporter: Send {
    /// Импортирует один уже полностью проверенный neutral DMA-BUF descriptor.
    fn import(
        &self,
        descriptor: &video_core::DmaBufFrameDescriptor,
    ) -> anyhow::Result<ImportedDmaBufTexture>;
}

impl DmaBufTextureImporter for DmaBufImporter {
    fn import(
        &self,
        descriptor: &video_core::DmaBufFrameDescriptor,
    ) -> anyhow::Result<ImportedDmaBufTexture> {
        self.import_exported_dma_buf_image(descriptor)
    }
}

/// Один cached renderer import.
struct CachedDmaBufTexture {
    /// Frame resource handle, для которого выполнен import.
    handle: FrameResourceHandle,

    /// Imported storage и typed plane views.
    imported_texture: Arc<ImportedDmaBufTexture>,
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::OwnedFd;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
    use video_backend_api::{
        PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
        PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup,
    };
    use video_core::{
        DecodedFrame, DmaBufDescriptorRejection, DmaBufFrameDescriptor, DmaBufFrameExportLayout,
        DmaBufLayerDescriptor, DmaBufObjectDescriptor, DmaBufObjectIdentity,
        FrameResourceDescriptor, FrameResourceHandle, VideoFrameDiagnostics,
        validate_dma_buf_descriptor_import_topology,
    };
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
    use video_present_core::{
        SharedVideoFrameReleaseSink, VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease,
        VideoFrameReleaseOutcome, VideoFrameReleaseSink,
    };

    use super::{
        DmaBufTextureImporter, DmaBufWgpuFrameMaterializer, ImportedDmaBufTexture,
        WgpuFrameMaterializationUnsupportedReason, WgpuFrameTextureViewLookup,
        WgpuFrameTextureViewMaterializer,
    };

    /// Параметры descriptor-а, публикуемого fake provider-ом.
    #[derive(Clone, Copy)]
    struct DescriptorScenario {
        /// Фактический layout экспортированного DMA-BUF.
        export_layout: DmaBufFrameExportLayout,
        /// Фактическая coded ширина descriptor-а.
        width: u32,
        /// Фактическая coded высота descriptor-а.
        height: u32,
    }

    /// Ожидаемое поведение materializer-а на importer boundary.
    enum ExpectedImportBoundary {
        /// Typed contract rejection должен остановить путь до importer-а.
        RejectedBeforeImport(DmaBufDescriptorRejection),
        /// Полностью совместимый descriptor должен дойти до importer-а.
        ReachesImporter,
    }

    /// Recording importer доказывает достижение unsafe boundary без зависимости от GPU.
    struct RecordingFailingImporter {
        /// Число попыток передать descriptor importer-у.
        import_calls: Arc<AtomicUsize>,
    }

    impl DmaBufTextureImporter for RecordingFailingImporter {
        fn import(
            &self,
            _descriptor: &DmaBufFrameDescriptor,
        ) -> anyhow::Result<ImportedDmaBufTexture> {
            self.import_calls.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!(
                "recording importer stops before unsafe Vulkan/HAL import"
            ))
        }
    }

    /// Provider создаёт owned descriptor на каждый lookup и записывает release calls.
    struct RecordingDescriptorProvider {
        /// Единственный допустимый resource handle тестового кадра.
        expected_handle: FrameResourceHandle,
        /// Descriptor scenario для текущего теста.
        scenario: DescriptorScenario,
        /// Счётчик фактических provider release calls.
        release_calls: Arc<AtomicUsize>,
    }

    impl PresentFrameResourceProvider for RecordingDescriptorProvider {
        fn resource_lookup(
            &self,
            handle: FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            assert_eq!(handle, self.expected_handle);
            PresentFrameResourceProviderLookup::Ready {
                resource_pool_lock_wait: Duration::ZERO,
            }
        }

        fn resource_descriptor_lookup(
            &self,
            handle: FrameResourceHandle,
        ) -> PresentFrameResourceDescriptorLookup {
            assert_eq!(handle, self.expected_handle);
            PresentFrameResourceDescriptorLookup::Ready {
                descriptor: FrameResourceDescriptor::DmaBuf(descriptor_for_scenario(self.scenario)),
                resource_pool_lock_wait: Duration::ZERO,
            }
        }

        fn try_resource_descriptor_lookup(
            &self,
            handle: FrameResourceHandle,
        ) -> PresentFrameResourceDescriptorLookup {
            self.resource_descriptor_lookup(handle)
        }

        fn release_frame(&self, handle: FrameResourceHandle) {
            assert_eq!(handle, self.expected_handle);
            self.release_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Release sink сохраняет production ownership: lease освобождает provider resource.
    struct ForwardingReleaseSink;

    impl VideoFrameReleaseSink for ForwardingReleaseSink {
        fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
            let provider = release
                .resource_provider()
                .expect("DMA-BUF lease must retain its resource provider");
            provider.release_frame(release.resource_handle());
            VideoFrameReleaseOutcome::Accepted
        }
    }

    /// Открывает harmless owned fd, который recording importer не передаёт Vulkan-у.
    fn test_owned_fd() -> OwnedFd {
        File::open("/dev/null")
            .expect("/dev/null must be available for a DMA-BUF test fd")
            .into()
    }

    /// Создаёт topology-valid composed либо separate NV12 descriptor.
    fn descriptor_for_scenario(scenario: DescriptorScenario) -> DmaBufFrameDescriptor {
        let chroma_offset = scenario.width.saturating_mul(scenario.height);
        let layers = match scenario.export_layout {
            DmaBufFrameExportLayout::ComposedLayers => vec![DmaBufLayerDescriptor {
                drm_format: 0x3231_564e,
                num_planes: 2,
                object_index: [0, 0, 0, 0],
                offset: [0, chroma_offset, 0, 0],
                pitch: [scenario.width, scenario.width, 0, 0],
            }],
            DmaBufFrameExportLayout::SeparateLayers => vec![
                DmaBufLayerDescriptor {
                    drm_format: 0x2020_3852,
                    num_planes: 1,
                    object_index: [0, 0, 0, 0],
                    offset: [0, 0, 0, 0],
                    pitch: [scenario.width, 0, 0, 0],
                },
                DmaBufLayerDescriptor {
                    drm_format: 0x3838_5247,
                    num_planes: 1,
                    object_index: [0, 0, 0, 0],
                    offset: [chroma_offset, 0, 0, 0],
                    pitch: [scenario.width, 0, 0, 0],
                },
            ],
        };

        DmaBufFrameDescriptor {
            resource_id: 1600,
            fourcc: 0x3231_564e,
            export_layout: scenario.export_layout,
            width: scenario.width,
            height: scenario.height,
            objects: vec![DmaBufObjectDescriptor {
                fd: test_owned_fd(),
                size: 16 * 1024 * 1024,
                drm_format_modifier: 0,
                identity: DmaBufObjectIdentity {
                    device: 1,
                    inode: 2,
                    special_device: 3,
                },
            }],
            layers,
        }
    }

    /// Создаёт decoded frame с composed NV12 contract-ом и coded size 640x360.
    fn decoded_frame(resource_handle: FrameResourceHandle) -> DecodedFrame {
        DecodedFrame {
            generation: 16,
            pts: Duration::from_millis(16),
            frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle,
            diagnostics: VideoFrameDiagnostics::default(),
        }
    }

    /// Запускает production lookup boundary и проверяет importer/release accounting.
    fn run_materializer_scenario(
        scenario: DescriptorScenario,
        expected_boundary: ExpectedImportBoundary,
    ) {
        let resource_handle = FrameResourceHandle(1600);
        let descriptor = descriptor_for_scenario(scenario);
        assert_eq!(
            validate_dma_buf_descriptor_import_topology(&descriptor),
            Ok(()),
            "scenario must isolate frame-contract validation from topology validation"
        );

        let import_calls = Arc::new(AtomicUsize::new(0));
        let release_calls = Arc::new(AtomicUsize::new(0));
        let provider = PresentFrameResourceProviderHandle::new(RecordingDescriptorProvider {
            expected_handle: resource_handle,
            scenario,
            release_calls: release_calls.clone(),
        });
        let materializer = DmaBufWgpuFrameMaterializer::new_with_importer(
            provider.clone(),
            RecordingFailingImporter {
                import_calls: import_calls.clone(),
            },
        );
        let release_sink: SharedVideoFrameReleaseSink = Arc::new(ForwardingReleaseSink);
        let lease = VideoFrameLease::new(
            VideoFrameLeaseConfig::new(16, decoded_frame(resource_handle), release_sink)
                .with_resource_provider(provider),
        );

        let lookup = materializer.try_texture_view_lookup(lease.decoded_frame());
        match expected_boundary {
            ExpectedImportBoundary::RejectedBeforeImport(expected_rejection) => {
                assert!(matches!(
                    lookup,
                    WgpuFrameTextureViewLookup::Unsupported {
                        reason: WgpuFrameMaterializationUnsupportedReason::DmaBufDescriptorRejected(
                            actual_rejection
                        ),
                        ..
                    } if actual_rejection == expected_rejection
                ));
                assert_eq!(import_calls.load(Ordering::SeqCst), 0);
            }
            ExpectedImportBoundary::ReachesImporter => {
                assert!(matches!(lookup, WgpuFrameTextureViewLookup::Error { .. }));
                assert_eq!(import_calls.load(Ordering::SeqCst), 1);
            }
        }

        assert_eq!(release_calls.load(Ordering::SeqCst), 0);
        drop(lease);
        assert_eq!(release_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn wrong_coded_dimensions_are_rejected_before_dma_buf_import() {
        run_materializer_scenario(
            DescriptorScenario {
                export_layout: DmaBufFrameExportLayout::ComposedLayers,
                width: 800,
                height: 450,
            },
            ExpectedImportBoundary::RejectedBeforeImport(
                DmaBufDescriptorRejection::CodedSizeMismatch {
                    expected_width: 640,
                    expected_height: 360,
                    actual_width: 800,
                    actual_height: 450,
                },
            ),
        );
    }

    #[test]
    fn incompatible_layout_is_rejected_before_dma_buf_import() {
        run_materializer_scenario(
            DescriptorScenario {
                export_layout: DmaBufFrameExportLayout::SeparateLayers,
                width: 640,
                height: 360,
            },
            ExpectedImportBoundary::RejectedBeforeImport(
                DmaBufDescriptorRejection::ImageLayoutMismatch {
                    expected: DmaBufImageLayout::ComposedLayers,
                    actual: DmaBufImageLayout::SeparateLayers,
                },
            ),
        );
    }

    #[test]
    fn valid_descriptor_reaches_dma_buf_import_without_release_regression() {
        run_materializer_scenario(
            DescriptorScenario {
                export_layout: DmaBufFrameExportLayout::ComposedLayers,
                width: 640,
                height: 360,
            },
            ExpectedImportBoundary::ReachesImporter,
        );
    }
}
