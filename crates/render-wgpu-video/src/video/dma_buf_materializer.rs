//! DMA-BUF materializer и bounded cache renderer-а.
//!
//! Модуль владеет преобразованием neutral resource descriptor в импортированные
//! WGPU texture views. Renderer facade не знает устройство cache/importer-а.
//!
//! Порядок на горячем пути (cpu-parity сессия 03):
//! 1. дешёвый identity lookup у provider-а (без dup fd) → ключ кэша;
//! 2. попадание в кэш → готовые views, Vulkan не трогаем вовсе;
//! 3. только на промахе — descriptor lookup с dup fd, проверка контракта и импорт.
//!
//! Переиспользование image между кадрами разрешено лишь когда importer умеет
//! явную синхронизацию с внешним писателем; иначе кэш попадает только для того
//! же `FrameResourceHandle` (повтор одного кадра на нескольких vsync), как раньше.

use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceIdentityLookup,
    PresentFrameResourceProviderHandle,
};
use video_core::{
    DecodedFrame, DmaBufFrameDescriptor, DmaBufFrameIdentity, FrameResourceDescriptor,
    FrameResourceIdentity, validate_dma_buf_descriptor_against_frame_contract,
    validate_dma_buf_identity_against_frame_contract,
};

use crate::dma_buf_import::{DmaBufImporter, ImportedDmaBufTexture};

use super::dma_buf_texture_cache::{
    DmaBufImportCacheCapacity, DmaBufImportCacheSnapshot, DmaBufTextureCache,
    DmaBufTextureReusePolicy,
};
use super::{
    DmaBufImportStats, WgpuFrameMaterializationUnsupportedReason, WgpuFrameTextureViewLookup,
    WgpuFrameTextureViewMaterializer, WgpuFrameTextureViews,
};

/// Renderer-side materializer, который импортирует neutral DMA-BUF descriptors в WGPU.
///
/// Generic по importer-у, чтобы вся логика identity/кэша/контракта проверялась
/// hermetic-тестами с fake importer-ом; production тип — `DmaBufImporter`.
pub struct DmaBufWgpuFrameMaterializer<I: DmaBufTextureImporter = DmaBufImporter> {
    /// Backend provider возвращает identity, duplicated descriptors и lock diagnostics.
    resource_provider: PresentFrameResourceProviderHandle,

    /// Владелец unsafe platform import-а; VAAPI/cros types сюда не попадают.
    importer: I,

    /// Renderer-owned bounded cache импортов по идентичности буфера.
    texture_cache: Mutex<DmaBufTextureCache<I::Texture>>,

    /// Счётчики самого materializer-а (кэш считает свои внутри).
    import_counters: Mutex<ImportCounters>,
}

/// Счётчики фактических обращений к importer-у.
#[derive(Debug, Default, Clone, Copy)]
struct ImportCounters {
    /// Сколько раз вызван platform import.
    imports_performed: u64,

    /// Сколько import-ов завершились ошибкой.
    import_failures: u64,
}

/// Результат materialization одного кадра до создания views.
///
/// Те же различимые исходы, что у `WgpuFrameTextureViewLookup`, но с shared
/// image вместо views, чтобы fake importer в тестах не нуждался в GPU.
pub(super) enum DmaBufMaterialization<T> {
    /// Image готов (из кэша или свежеимпортирован).
    Ready {
        /// Shared импортированный image.
        texture: Arc<T>,

        /// Суммарное ожидание lock-ов provider-а и кэша.
        lock_wait: Duration,
    },

    /// Provider или кэш заняты; hot path выбирает fallback без ожидания.
    Busy {
        /// Ожидание lock-а.
        lock_wait: Duration,
    },

    /// Provider не знает такого handle.
    Missing {
        /// Ожидание lock-а.
        lock_wait: Duration,
    },

    /// Descriptor не подходит этому materializer-у (typed причина).
    Unsupported {
        /// Причина отказа.
        reason: WgpuFrameMaterializationUnsupportedReason,

        /// Ожидание lock-а.
        lock_wait: Duration,
    },

    /// Fatal/poison состояние или ошибка импорта.
    Error {
        /// Ожидание lock-а.
        lock_wait: Duration,
    },
}

impl DmaBufWgpuFrameMaterializer {
    /// Создаёт materializer из WGPU handles renderer layer-а и neutral provider-а.
    ///
    /// `cache_capacity` обязан совпадать с ёмкостью zero-copy пула декодера этого
    /// же backend-а, иначе кэш либо промахивается по кругу, либо держит лишние image.
    #[must_use]
    pub fn new(
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resource_provider: PresentFrameResourceProviderHandle,
        cache_capacity: DmaBufImportCacheCapacity,
    ) -> Self {
        Self::new_with_importer(
            resource_provider,
            DmaBufImporter::new(
                device.clone(),
                instance.clone(),
                adapter.clone(),
                queue.clone(),
            ),
            cache_capacity,
        )
    }
}

impl<I: DmaBufTextureImporter> DmaBufWgpuFrameMaterializer<I> {
    /// Создаёт materializer вокруг конкретного importer-а (production или fake).
    pub(super) fn new_with_importer(
        resource_provider: PresentFrameResourceProviderHandle,
        importer: I,
        cache_capacity: DmaBufImportCacheCapacity,
    ) -> Self {
        Self {
            resource_provider,
            importer,
            texture_cache: Mutex::new(DmaBufTextureCache::new(cache_capacity)),
            import_counters: Mutex::new(ImportCounters::default()),
        }
    }

    /// Ёмкость кэша, с которой создан materializer (для `recreate_for_renderer`).
    fn cache_capacity(&self) -> DmaBufImportCacheCapacity {
        self.texture_cache
            .lock()
            .map(|cache| cache.capacity())
            .unwrap_or_else(|_| DmaBufImportCacheCapacity::matching_surface_pool(1))
    }

    /// Снимок счётчиков кэша и импортов для диагностики/тестов.
    ///
    /// Poisoned mutex (паника на UI-потоке) даёт пустой снимок с warn-ом: сам
    /// materializer в таком состоянии уже возвращает `Error` на каждый кадр.
    pub(super) fn import_stats(&self) -> DmaBufImportStats {
        let cache = self.texture_cache.lock().map_or_else(
            |error| {
                tracing::warn!(error = %error, "DMA-BUF texture cache mutex poisoned; stats unavailable");
                DmaBufImportCacheSnapshot::default()
            },
            |cache| cache.snapshot(),
        );
        let counters = self.import_counters.lock().map_or_else(
            |error| {
                tracing::warn!(error = %error, "DMA-BUF import counters mutex poisoned; stats unavailable");
                ImportCounters::default()
            },
            |counters| *counters,
        );
        DmaBufImportStats {
            cache,
            imports_performed: counters.imports_performed,
            import_failures: counters.import_failures,
            image_reuse_enabled: self.importer.supports_image_reuse(),
        }
    }

    /// Политика попаданий кэша: между кадрами только при доказанной синхронизации.
    fn reuse_policy(&self) -> DmaBufTextureReusePolicy {
        if self.importer.supports_image_reuse() {
            DmaBufTextureReusePolicy::AcrossFrames
        } else {
            DmaBufTextureReusePolicy::SameFrameHandleOnly
        }
    }

    /// Полный путь materialization кадра: identity → кэш → (промах) descriptor → импорт.
    pub(super) fn materialize_frame(
        &self,
        frame: &DecodedFrame,
    ) -> DmaBufMaterialization<I::Texture> {
        let handle = frame.resource_handle;

        // Шаг 1: дешёвая идентичность без dup fd.
        let identity_lookup = self.resource_provider.try_resource_identity_lookup(handle);
        let mut lock_wait = identity_lookup.resource_pool_lock_wait();
        let known_identity = match identity_lookup {
            PresentFrameResourceIdentityLookup::Ready {
                identity: FrameResourceIdentity::DmaBuf(identity),
                ..
            } => Some(identity),
            PresentFrameResourceIdentityLookup::Unsupported => None,
            PresentFrameResourceIdentityLookup::Busy { .. } => {
                return DmaBufMaterialization::Busy { lock_wait };
            }
            PresentFrameResourceIdentityLookup::Missing { .. } => {
                return DmaBufMaterialization::Missing { lock_wait };
            }
            PresentFrameResourceIdentityLookup::Fatal { .. } => {
                return DmaBufMaterialization::Error { lock_wait };
            }
        };

        // Шаг 2: кэш. Lock держится до конца импорта, чтобы один промах дал один импорт.
        let cache_lock_started_at = Instant::now();
        let mut texture_cache = match self.texture_cache.try_lock() {
            Ok(texture_cache) => texture_cache,
            Err(TryLockError::WouldBlock) => {
                return DmaBufMaterialization::Busy {
                    lock_wait: lock_wait.saturating_add(cache_lock_started_at.elapsed()),
                };
            }
            Err(TryLockError::Poisoned(error)) => {
                tracing::warn!(error = %error, "WGPU DMA-BUF texture cache mutex poisoned");
                return DmaBufMaterialization::Error {
                    lock_wait: lock_wait.saturating_add(cache_lock_started_at.elapsed()),
                };
            }
        };
        lock_wait = lock_wait.saturating_add(cache_lock_started_at.elapsed());
        let reuse_policy = self.reuse_policy();
        if let Some(identity) = &known_identity
            && let Some(texture) = texture_cache.lookup(identity, handle, reuse_policy)
        {
            // Попадание не отменяет проверку контракта кадра: буфер тот же, но кадр
            // мог прийти с другим coded size или layout contract-ом.
            return match validate_cached_image_against_frame(frame, identity) {
                Ok(()) => DmaBufMaterialization::Ready { texture, lock_wait },
                Err(unsupported) => DmaBufMaterialization::Unsupported {
                    reason: unsupported,
                    lock_wait,
                },
            };
        }

        // Шаг 3: промах — descriptor с dup fd, контракт, импорт.
        let descriptor_lookup = self
            .resource_provider
            .try_resource_descriptor_lookup(handle);
        lock_wait = lock_wait.saturating_add(descriptor_lookup.resource_pool_lock_wait());
        let descriptor = match descriptor_lookup {
            PresentFrameResourceDescriptorLookup::Ready { descriptor, .. } => descriptor,
            PresentFrameResourceDescriptorLookup::Busy { .. } => {
                return DmaBufMaterialization::Busy { lock_wait };
            }
            PresentFrameResourceDescriptorLookup::Missing { .. } => {
                return DmaBufMaterialization::Missing { lock_wait };
            }
            PresentFrameResourceDescriptorLookup::Fatal { .. } => {
                return DmaBufMaterialization::Error { lock_wait };
            }
        };
        let dma_buf_descriptor = match descriptor {
            FrameResourceDescriptor::DmaBuf(dma_buf_descriptor) => dma_buf_descriptor,
            FrameResourceDescriptor::HostPlanar(_) => {
                return DmaBufMaterialization::Unsupported {
                    reason: WgpuFrameMaterializationUnsupportedReason::HostPlanarRequiresUploadMaterializer,
                    lock_wait,
                };
            }
        };
        if let Err(rejection) = validate_dma_buf_descriptor_against_frame_contract(
            frame.frame_contract,
            frame.width,
            frame.height,
            &dma_buf_descriptor,
        ) {
            return DmaBufMaterialization::Unsupported {
                reason: WgpuFrameMaterializationUnsupportedReason::DmaBufDescriptorRejected(
                    rejection,
                ),
                lock_wait,
            };
        }

        // Provider без identity-поддержки: ключ снимается с descriptor-а, и кэш
        // проверяется только теперь (у provider-а с identity промах уже засчитан).
        let identity = match known_identity {
            Some(identity) => identity,
            None => {
                let identity = Arc::new(dma_buf_descriptor.identity());
                if let Some(texture) = texture_cache.lookup(&identity, handle, reuse_policy) {
                    return DmaBufMaterialization::Ready { texture, lock_wait };
                }
                identity
            }
        };

        // Старые image уничтожаются ДО создания нового, чтобы пиковая память не
        // держала два набора буферов при смене формата.
        texture_cache.prepare_for_insert(&identity);
        match self.import_once(&dma_buf_descriptor) {
            Ok(texture) => DmaBufMaterialization::Ready {
                texture: texture_cache.insert(identity, handle, texture),
                lock_wait,
            },
            Err(error) => {
                tracing::warn!(
                    handle_id = handle.0,
                    error = %error,
                    "WGPU DMA-BUF import failed for decoded frame"
                );
                DmaBufMaterialization::Error { lock_wait }
            }
        }
    }

    /// Единственная точка вызова platform import-а со счётчиками.
    fn import_once(&self, descriptor: &DmaBufFrameDescriptor) -> anyhow::Result<I::Texture> {
        let result = self.importer.import(descriptor);
        match self.import_counters.lock() {
            Ok(mut counters) => {
                counters.imports_performed = counters.imports_performed.saturating_add(1);
                if result.is_err() {
                    counters.import_failures = counters.import_failures.saturating_add(1);
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, "DMA-BUF import counters mutex poisoned; import not counted");
            }
        }
        result
    }
}

/// Сверяет кадр с кэшированным image той же идентичности (layout + coded size).
fn validate_cached_image_against_frame(
    frame: &DecodedFrame,
    identity: &DmaBufFrameIdentity,
) -> Result<(), WgpuFrameMaterializationUnsupportedReason> {
    validate_dma_buf_identity_against_frame_contract(
        frame.frame_contract,
        frame.width,
        frame.height,
        identity,
    )
    .map_err(WgpuFrameMaterializationUnsupportedReason::DmaBufDescriptorRejected)
}

impl WgpuFrameTextureViewMaterializer for DmaBufWgpuFrameMaterializer {
    fn try_texture_view_lookup(&self, frame: &DecodedFrame) -> WgpuFrameTextureViewLookup {
        match self.materialize_frame(frame) {
            DmaBufMaterialization::Ready { texture, lock_wait } => {
                WgpuFrameTextureViewLookup::Ready {
                    views: WgpuFrameTextureViews::from_imported_texture(texture),
                    texture_pool_lock_wait: lock_wait,
                }
            }
            DmaBufMaterialization::Busy { lock_wait } => WgpuFrameTextureViewLookup::Busy {
                texture_pool_lock_wait: lock_wait,
            },
            DmaBufMaterialization::Missing { lock_wait } => WgpuFrameTextureViewLookup::Missing {
                texture_pool_lock_wait: lock_wait,
            },
            DmaBufMaterialization::Unsupported { reason, lock_wait } => {
                WgpuFrameTextureViewLookup::Unsupported {
                    reason,
                    texture_pool_lock_wait: lock_wait,
                }
            }
            DmaBufMaterialization::Error { lock_wait } => WgpuFrameTextureViewLookup::Error {
                texture_pool_lock_wait: lock_wait,
            },
        }
    }

    fn recreate_for_renderer(
        &self,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Arc<dyn WgpuFrameTextureViewMaterializer> {
        // Новый renderer = новый device: кэш старых image не переносится.
        Arc::new(Self::new(
            instance,
            adapter,
            device,
            queue,
            self.resource_provider.clone(),
            self.cache_capacity(),
        ))
    }

    fn dma_buf_import_stats(&self) -> Option<DmaBufImportStats> {
        Some(self.import_stats())
    }
}

/// Internal boundary между логикой materializer-а и unsafe platform import-ом.
///
/// Позволяет тесту доказать порядок identity → кэш → контракт → импорт без GPU.
/// Trait `pub` только потому, что он параметр публичного типа materializer-а;
/// наружу из crate он не экспортируется.
pub trait DmaBufTextureImporter: Send + Sync {
    /// Импортированный image; `ImportedDmaBufTexture` в production.
    type Texture: Send + Sync;

    /// Импортирует один уже полностью проверенный neutral DMA-BUF descriptor.
    fn import(&self, descriptor: &DmaBufFrameDescriptor) -> anyhow::Result<Self::Texture>;

    /// `true`, если импортированный image можно показывать повторно после новой
    /// записи декодера (importer оформляет явную синхронизацию).
    fn supports_image_reuse(&self) -> bool;
}

impl DmaBufTextureImporter for DmaBufImporter {
    type Texture = ImportedDmaBufTexture;

    fn import(&self, descriptor: &DmaBufFrameDescriptor) -> anyhow::Result<ImportedDmaBufTexture> {
        self.import_exported_dma_buf_image(descriptor)
    }

    fn supports_image_reuse(&self) -> bool {
        self.external_sync_support().allows_image_reuse()
    }
}

/// HostPlanar descriptor не подходит DMA-BUF materializer-у (общий helper для тестов facade).
#[cfg(test)]
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

#[cfg(test)]
mod tests;
