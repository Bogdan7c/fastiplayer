use std::os::fd::AsRawFd;

use anyhow::Context;
use cros_codecs::decoder::{DecodedDmaBufExportLayout, DecodedDmaBufImage};
use nix::sys::stat::fstat;
use video_core::{
    DmaBufFrameDescriptor, DmaBufFrameExportLayout, DmaBufLayerDescriptor, DmaBufObjectDescriptor,
    DmaBufObjectIdentity, FrameResourceDescriptor, FrameResourceHandle,
};

use crate::zero_copy_surface_pool::{
    ZeroCopyGpuCompletionLease, ZeroCopyImportPlan, ZeroCopyImportReuseContract,
    ZeroCopySurfaceDescriptor, ZeroCopySurfacePool, ZeroCopySurfacePoolError,
};

/// Production default количества tracked zero-copy resources.
///
/// 24 slots покрывают decoder references, bounded decoded frame channel,
/// presentation queue и один-два render leases без unbounded external resources.
/// Значение можно сузить/расширить через `VideoDecodeThreadConfig`.
pub const DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS: usize = 24;

/// Снимок заполнения zero-copy resource pool для backpressure и UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourcePoolStats {
    /// Максимальное число tracked resource slots.
    pub capacity: usize,

    /// Сколько resource slots сейчас создано.
    pub slots: usize,

    /// Сколько surfaces сейчас нельзя переиспользовать decoder-у.
    pub in_use: usize,

    /// Сколько tracked surfaces свободно для reuse.
    pub free_surfaces: usize,

    /// Сколько releases ждёт GPU completion callback.
    pub waiting_gpu_completion: usize,

    /// Сколько releases ждёт возврата decoded handle в decoder pool.
    pub waiting_decoder_reuse: usize,

    /// Сколько descriptor/register операций завершилось ошибкой.
    pub import_failures: u64,

    /// Сколько resource descriptors реально создано.
    pub imports_created: u64,

    /// Сколько кадров переиспользовало existing descriptor.
    pub imports_reused: u64,

    /// Сколько free descriptors было заменено из-за смены backing object/layout.
    pub imports_replaced: u64,
}

impl ResourcePoolStats {
    /// Возвращает число slots, которые ещё можно занять или переиспользовать.
    pub const fn available_slots(self) -> usize {
        self.capacity.saturating_sub(self.in_use)
    }
}

/// Descriptor storage slot, которым владеет VAAPI provider.
struct ResourceSlot {
    /// Neutral descriptor с owned fd, живущими до replacement/invalidation.
    descriptor: FrameResourceDescriptor,
}

/// VAAPI-side таблица decoded resources, индексируемых через [`FrameResourceHandle`].
///
/// Pool владеет exported DMA-BUF descriptors и lifecycle state. Platform graphics
/// import намеренно отсутствует: renderer получает только duplicated descriptors.
pub struct FrameResourcePool {
    /// Codec-neutral lifecycle persistent surface/resource slots.
    surface_pool: ZeroCopySurfacePool,

    /// Neutral descriptors; индексы совпадают с lifecycle slots.
    slots: Vec<ResourceSlot>,

    /// Монотонный счётчик opaque frame resource handles.
    next_handle: u64,
}

impl FrameResourcePool {
    /// Создаёт новый empty resource pool.
    pub fn new() -> Self {
        Self::new_with_capacity(DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS)
    }

    /// Создаёт resource pool с явным bounded capacity.
    pub fn new_with_capacity(surface_pool_capacity: usize) -> Self {
        Self {
            surface_pool: ZeroCopySurfacePool::new(
                surface_pool_capacity,
                ZeroCopyImportReuseContract::vaapi_vulkan_dma_buf(),
            ),
            slots: Vec::with_capacity(surface_pool_capacity),
            next_handle: 0,
        }
    }

    /// Возвращает явный backend contract persistent resource reuse.
    #[must_use]
    pub const fn reuse_contract(&self) -> ZeroCopyImportReuseContract {
        self.surface_pool.reuse_contract()
    }

    /// Выделяет новый monotonic frame resource handle.
    fn allocate_handle(&mut self) -> anyhow::Result<FrameResourceHandle> {
        let handle_id = self.next_handle;
        self.next_handle = self
            .next_handle
            .checked_add(1)
            .context("frame resource handle counter exhausted")?;
        Ok(FrameResourceHandle(handle_id))
    }

    /// Возвращает `true`, если handle сейчас указывает на leased resource slot.
    pub fn is_registered_handle(&self, handle: FrameResourceHandle) -> bool {
        self.surface_pool.leased_slot_index(handle).is_some()
    }

    /// Возвращает duplicated descriptor для active renderer lease-а.
    pub fn duplicate_descriptor(
        &self,
        handle: FrameResourceHandle,
    ) -> anyhow::Result<Option<FrameResourceDescriptor>> {
        let Some(slot_index) = self.surface_pool.leased_slot_index(handle) else {
            return Ok(None);
        };
        let slot = self
            .slots
            .get(slot_index)
            .ok_or(ZeroCopySurfacePoolError::MissingStorageSlot { slot_index })?;

        slot.descriptor
            .try_clone_for_lookup()
            .map(Some)
            .with_context(|| format!("duplicate DMA-BUF descriptor for handle {}", handle.0))
    }

    /// Регистрирует или переиспользует exported DMA-BUF descriptor decoded VA surface-а.
    pub fn register_dma_buf_image(
        &mut self,
        image: DecodedDmaBufImage,
    ) -> anyhow::Result<FrameResourceHandle> {
        let lifecycle_descriptor =
            ZeroCopySurfaceDescriptor::from_dma_buf_image(&image).map_err(anyhow::Error::from)?;
        let resource_descriptor = frame_resource_descriptor_from_dma_buf_image(image)?;
        let resource_handle = self.allocate_handle()?;

        match self
            .surface_pool
            .begin_import_or_reuse(lifecycle_descriptor.clone(), resource_handle)
            .map_err(anyhow::Error::from)?
        {
            ZeroCopyImportPlan::Reuse { slot_index } => {
                self.slots
                    .get(slot_index)
                    .ok_or(ZeroCopySurfacePoolError::MissingStorageSlot { slot_index })?;
                tracing::trace!(
                    surface_id = lifecycle_descriptor.surface_id(),
                    handle_id = resource_handle.0,
                    slot_index,
                    "Reusing VAAPI DMA-BUF resource descriptor"
                );
                Ok(resource_handle)
            }
            ZeroCopyImportPlan::Create => {
                let slot_index = self.slots.len();
                self.surface_pool
                    .register_imported_surface(
                        lifecycle_descriptor.clone(),
                        resource_handle,
                        slot_index,
                    )
                    .map_err(anyhow::Error::from)?;
                self.slots.push(ResourceSlot {
                    descriptor: resource_descriptor,
                });

                tracing::debug!(
                    surface_id = lifecycle_descriptor.surface_id(),
                    handle_id = resource_handle.0,
                    slot_index,
                    imports_created = self.surface_pool.stats().imports_created,
                    "Registered VAAPI DMA-BUF resource descriptor"
                );

                Ok(resource_handle)
            }
            ZeroCopyImportPlan::Replace { slot_index } => {
                self.slots
                    .get(slot_index)
                    .ok_or(ZeroCopySurfacePoolError::MissingStorageSlot { slot_index })?;

                self.surface_pool
                    .register_replaced_surface(
                        lifecycle_descriptor.clone(),
                        resource_handle,
                        slot_index,
                    )
                    .map_err(anyhow::Error::from)?;

                self.slots[slot_index] = ResourceSlot {
                    descriptor: resource_descriptor,
                };

                tracing::debug!(
                    surface_id = lifecycle_descriptor.surface_id(),
                    handle_id = resource_handle.0,
                    slot_index,
                    imports_created = self.surface_pool.stats().imports_created,
                    imports_replaced = self.surface_pool.stats().imports_replaced,
                    "Replaced VAAPI DMA-BUF resource descriptor"
                );

                Ok(resource_handle)
            }
        }
    }

    /// Релизит renderer-owned frame после submit-а GPU work.
    pub fn release_after_gpu_submission(
        &mut self,
        handle: FrameResourceHandle,
    ) -> Result<ZeroCopyGpuCompletionLease, ZeroCopySurfacePoolError> {
        self.surface_pool.release_after_gpu_submission(handle)
    }

    /// Релизит frame, который не был передан renderer GPU work-у.
    pub fn release_without_gpu_submission(
        &mut self,
        handle: FrameResourceHandle,
    ) -> Result<(), ZeroCopySurfacePoolError> {
        self.surface_pool.release_without_gpu_submission(handle)
    }

    /// Подтверждает GPU completion для renderer-owned frame-а.
    pub fn acknowledge_gpu_completion(
        &mut self,
        handle: FrameResourceHandle,
    ) -> Result<(), ZeroCopySurfacePoolError> {
        self.surface_pool.acknowledge_gpu_completion(handle)
    }

    /// Подтверждает, что decoded surface вернулась в decoder pool.
    pub fn acknowledge_decoder_reuse(
        &mut self,
        handle: FrameResourceHandle,
    ) -> Result<(), ZeroCopySurfacePoolError> {
        self.surface_pool.acknowledge_decoder_reuse(handle)
    }

    /// Сбрасывает все idle descriptors после format/resolution change.
    pub fn invalidate_all(&mut self) -> Result<(), ZeroCopySurfacePoolError> {
        self.surface_pool.invalidate_all()?;
        self.slots.clear();
        Ok(())
    }

    /// Возвращает общее количество tracked descriptors.
    pub fn num_slots(&self) -> usize {
        self.surface_pool.stats().slots
    }

    /// Возвращает количество live/releasing surfaces.
    pub fn num_in_use(&self) -> usize {
        self.surface_pool.stats().active_surfaces
    }

    /// Возвращает компактную статистику pool для backpressure, UI и diagnostics.
    pub fn stats(&self) -> ResourcePoolStats {
        let stats = self.surface_pool.stats();
        ResourcePoolStats {
            capacity: stats.capacity,
            slots: stats.slots,
            in_use: stats.active_surfaces,
            free_surfaces: stats.free_surfaces,
            waiting_gpu_completion: stats.waiting_gpu_completion,
            waiting_decoder_reuse: stats.waiting_decoder_reuse,
            import_failures: stats.import_failures,
            imports_created: stats.imports_created,
            imports_reused: stats.imports_reused,
            imports_replaced: stats.imports_replaced,
        }
    }
}

impl Default for FrameResourcePool {
    fn default() -> Self {
        Self::new()
    }
}

/// Строит neutral frame resource descriptor, сохраняя owned fd внутри provider-а.
fn frame_resource_descriptor_from_dma_buf_image(
    image: DecodedDmaBufImage,
) -> anyhow::Result<FrameResourceDescriptor> {
    let object_identities = image
        .objects
        .iter()
        .enumerate()
        .map(|(object_index, object)| {
            let file_stat = fstat(object.fd.as_raw_fd()).with_context(|| {
                format!(
                    "query DMA-BUF object identity for surface {} object {}",
                    image.surface_id, object_index
                )
            })?;
            Ok(DmaBufObjectIdentity {
                device: file_stat.st_dev,
                inode: file_stat.st_ino,
                special_device: file_stat.st_rdev,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let objects = image
        .objects
        .into_iter()
        .zip(object_identities)
        .map(|(object, identity)| DmaBufObjectDescriptor {
            fd: object.fd,
            size: object.size,
            drm_format_modifier: object.drm_format_modifier,
            identity,
        })
        .collect();

    let layers = image
        .layers
        .into_iter()
        .map(|layer| DmaBufLayerDescriptor {
            drm_format: layer.drm_format,
            num_planes: layer.num_planes,
            object_index: layer.object_index,
            offset: layer.offset,
            pitch: layer.pitch,
        })
        .collect();

    Ok(FrameResourceDescriptor::DmaBuf(DmaBufFrameDescriptor {
        resource_id: image.surface_id,
        fourcc: image.fourcc,
        export_layout: match image.export_layout {
            DecodedDmaBufExportLayout::ComposedLayers => DmaBufFrameExportLayout::ComposedLayers,
            DecodedDmaBufExportLayout::SeparateLayers => DmaBufFrameExportLayout::SeparateLayers,
        },
        width: image.width,
        height: image.height,
        objects,
        layers,
    }))
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::{AsFd, AsRawFd, OwnedFd};

    use super::*;

    /// Создаёт owned fd для synthetic DMA-BUF descriptor tests.
    fn open_test_dma_buf_fd() -> OwnedFd {
        let file = File::open("/dev/null").expect("test fd source must be readable");
        file.into()
    }

    /// Создаёт synthetic decoded DMA-BUF image без VAAPI device.
    fn decoded_dma_buf_image(surface_id: u64) -> DecodedDmaBufImage {
        DecodedDmaBufImage {
            surface_id,
            fourcc: 0x3231_564e,
            export_layout: DecodedDmaBufExportLayout::ComposedLayers,
            width: 1280,
            height: 720,
            objects: vec![cros_codecs::decoder::DecodedDmaBufObject {
                fd: open_test_dma_buf_fd(),
                size: 4096,
                drm_format_modifier: 0,
            }],
            layers: vec![cros_codecs::decoder::DecodedDmaBufLayer {
                drm_format: 0x3231_564e,
                num_planes: 2,
                object_index: [0, 0, 0, 0],
                offset: [0, 2048, 0, 0],
                pitch: [1280, 1280, 0, 0],
            }],
        }
    }

    /// Проверяет, что high-level stats сохраняют поля lifecycle pool-а.
    #[test]
    fn resource_pool_stats_available_slots_use_active_surfaces() {
        let stats = ResourcePoolStats {
            capacity: 24,
            slots: 8,
            in_use: 5,
            free_surfaces: 3,
            waiting_gpu_completion: 2,
            waiting_decoder_reuse: 1,
            import_failures: 0,
            imports_created: 8,
            imports_reused: 4,
            imports_replaced: 2,
        };

        assert_eq!(stats.available_slots(), 19);
    }

    /// Проверяет, что provider-owned descriptor не отдаёт renderer-у исходный fd.
    #[test]
    fn duplicate_descriptor_returns_renderer_owned_fd_without_releasing_provider_fd() {
        let mut resource_pool = FrameResourcePool::new_with_capacity(2);
        let handle = resource_pool
            .register_dma_buf_image(decoded_dma_buf_image(91))
            .expect("synthetic DMA-BUF image must register");

        let first_descriptor = resource_pool
            .duplicate_descriptor(handle)
            .expect("descriptor duplication must not fail")
            .expect("registered handle must have descriptor");
        let FrameResourceDescriptor::DmaBuf(first_dma_buf) = first_descriptor;
        let first_lookup_fd = first_dma_buf.objects[0].fd.as_raw_fd();

        let second_descriptor = resource_pool
            .duplicate_descriptor(handle)
            .expect("provider-owned descriptor must support repeated renderer lookups")
            .expect("registered handle must still have descriptor");
        let FrameResourceDescriptor::DmaBuf(second_dma_buf) = second_descriptor;

        assert_ne!(first_lookup_fd, second_dma_buf.objects[0].fd.as_raw_fd());
        drop(first_dma_buf);
        second_dma_buf.objects[0]
            .fd
            .as_fd()
            .try_clone_to_owned()
            .expect("renderer-owned duplicated fd must be valid");
    }

    /// Проверяет typed absent-resource path без descriptor synthesis.
    #[test]
    fn duplicate_descriptor_returns_none_for_missing_handle() {
        let resource_pool = FrameResourcePool::new_with_capacity(2);

        let descriptor = resource_pool
            .duplicate_descriptor(FrameResourceHandle(404))
            .expect("missing handle lookup must not poison resource pool");

        assert!(descriptor.is_none());
    }

    /// Проверяет accounting submitted release path на уровне neutral resource pool-а.
    #[test]
    fn submitted_release_waits_for_gpu_then_decoder_reuse_ack() {
        let mut resource_pool = FrameResourcePool::new_with_capacity(2);
        let handle = resource_pool
            .register_dma_buf_image(decoded_dma_buf_image(92))
            .expect("synthetic DMA-BUF image must register");

        resource_pool
            .release_after_gpu_submission(handle)
            .expect("submitted release must enter GPU wait state");

        let waiting_gpu_stats = resource_pool.stats();
        assert_eq!(waiting_gpu_stats.waiting_gpu_completion, 1);
        assert_eq!(waiting_gpu_stats.waiting_decoder_reuse, 0);

        resource_pool
            .acknowledge_gpu_completion(handle)
            .expect("GPU completion must move resource to decoder reuse wait");

        let waiting_decoder_stats = resource_pool.stats();
        assert_eq!(waiting_decoder_stats.waiting_gpu_completion, 0);
        assert_eq!(waiting_decoder_stats.waiting_decoder_reuse, 1);

        resource_pool
            .acknowledge_decoder_reuse(handle)
            .expect("decoder reuse ack must release active resource");

        let final_stats = resource_pool.stats();
        assert_eq!(final_stats.in_use, 0);
        assert_eq!(final_stats.free_surfaces, 1);
    }

    /// Проверяет accounting acquired-but-unsubmitted release path без GPU wait.
    #[test]
    fn unsubmitted_release_skips_gpu_wait_and_waits_only_decoder_reuse_ack() {
        let mut resource_pool = FrameResourcePool::new_with_capacity(2);
        let handle = resource_pool
            .register_dma_buf_image(decoded_dma_buf_image(93))
            .expect("synthetic DMA-BUF image must register");

        resource_pool
            .release_without_gpu_submission(handle)
            .expect("unsubmitted release must not require GPU completion");

        let waiting_decoder_stats = resource_pool.stats();
        assert_eq!(waiting_decoder_stats.waiting_gpu_completion, 0);
        assert_eq!(waiting_decoder_stats.waiting_decoder_reuse, 1);

        resource_pool
            .acknowledge_decoder_reuse(handle)
            .expect("decoder reuse ack must release unsubmitted resource");

        let final_stats = resource_pool.stats();
        assert_eq!(final_stats.in_use, 0);
        assert_eq!(final_stats.free_surfaces, 1);
    }
}
