use anyhow::{Context, ensure};
use cros_codecs::decoder::{DecodedDmaBufExportLayout, DecodedDmaBufImage};
use video_core::FrameTextureHandle;

use crate::zero_copy_surface_pool::{
    ZeroCopyGpuCompletionLease, ZeroCopyImportPlan, ZeroCopyImportReuseContract,
    ZeroCopySurfaceDescriptor, ZeroCopySurfacePool, ZeroCopySurfacePoolError,
};

/// Production default количества persistent zero-copy imports.
///
/// 24 slots покрывают decoder references, bounded decoded frame channel,
/// presentation queue и один-два render leases без unbounded внешних GPU
/// resources. Значение можно сузить/расширить через `VideoDecodeThreadConfig`.
pub const DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS: usize = 24;

/// Снимок заполнения zero-copy surface/import pool для backpressure и UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TexturePoolStats {
    /// Максимальное число persistent import slots.
    pub capacity: usize,

    /// Сколько persistent import slots сейчас создано.
    pub slots: usize,

    /// Сколько surfaces сейчас нельзя переиспользовать decoder-у.
    pub in_use: usize,

    /// Сколько imported surfaces свободно для reuse.
    pub free_surfaces: usize,

    /// Сколько releases ждёт GPU completion callback.
    pub waiting_gpu_completion: usize,

    /// Сколько releases ждёт возврата decoded handle в decoder pool.
    pub waiting_decoder_reuse: usize,

    /// Сколько external imports завершилось ошибкой.
    pub import_failures: u64,

    /// Сколько external imports реально создано.
    pub imports_created: u64,

    /// Сколько кадров переиспользовало existing import.
    pub imports_reused: u64,

    /// Сколько free imports было заменено из-за смены backing object/layout.
    pub imports_replaced: u64,
}

impl TexturePoolStats {
    /// Возвращает число slots, которые ещё можно занять или переиспользовать.
    pub const fn available_slots(self) -> usize {
        self.capacity.saturating_sub(self.in_use)
    }
}

/// Один persistent imported texture slot.
struct TextureSlot {
    /// Владение imported VkImage/VkDeviceMemory через wgpu texture wrapper.
    storage: crate::dma_buf_import::ImportedDmaBufStorage,

    /// Формат imported texture для diagnostics и consistency checks.
    frame_format: crate::dma_buf_import::DmaBufFrameFormat,

    /// Представление luma/Y plane для shader binding.
    y_view: wgpu::TextureView,

    /// Представление chroma/UV plane для shader binding.
    uv_view: wgpu::TextureView,
}

impl TextureSlot {
    /// Просит wgpu освободить native texture storage при уничтожении slot-а.
    fn destroy(&self) {
        match &self.storage {
            crate::dma_buf_import::ImportedDmaBufStorage::Multiplanar(texture) => {
                texture.destroy();
            }
            crate::dma_buf_import::ImportedDmaBufStorage::SeparatePlanes {
                y_texture,
                uv_texture,
            } => {
                y_texture.destroy();
                uv_texture.destroy();
            }
        }
    }
}

impl Drop for TextureSlot {
    /// Уничтожает persistent import только при invalidation/drop pool-а.
    fn drop(&mut self) {
        self.destroy();
    }
}

/// Пул persistent zero-copy imports, индексируемых через [`FrameTextureHandle`].
///
/// Lifecycle хранится в [`ZeroCopySurfacePool`], а этот слой владеет только
/// backend-specific wgpu texture storage и plane views.
pub struct WgpuTexturePool {
    /// Импортёр DMA-BUF для zero-copy path; `None` разрешён только для startup tests.
    dma_buf_importer: Option<crate::dma_buf_import::DmaBufImporter>,

    /// Codec-neutral lifecycle persistent surface/import slots.
    surface_pool: ZeroCopySurfacePool,

    /// Backend-specific imported textures; индексы совпадают с lifecycle slots.
    slots: Vec<TextureSlot>,

    /// Монотонный счётчик opaque frame handles.
    next_handle: u64,
}

impl WgpuTexturePool {
    /// Создаёт новый пустой zero-copy texture/import pool.
    pub fn new(dma_buf_importer: Option<crate::dma_buf_import::DmaBufImporter>) -> Self {
        Self::new_with_capacity(dma_buf_importer, DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS)
    }

    /// Создаёт новый zero-copy texture/import pool с явным bounded capacity.
    pub fn new_with_capacity(
        dma_buf_importer: Option<crate::dma_buf_import::DmaBufImporter>,
        surface_pool_capacity: usize,
    ) -> Self {
        Self {
            dma_buf_importer,
            surface_pool: ZeroCopySurfacePool::new(
                surface_pool_capacity,
                ZeroCopyImportReuseContract::vaapi_vulkan_wgpu(),
            ),
            slots: Vec::with_capacity(surface_pool_capacity),
            next_handle: 0,
        }
    }

    /// Возвращает `true`, если пул может попробовать DMA-BUF import.
    pub fn can_import_dma_buf(&self) -> bool {
        self.dma_buf_importer.is_some()
    }

    /// Возвращает явный backend contract persistent import reuse.
    #[must_use]
    pub const fn reuse_contract(&self) -> ZeroCopyImportReuseContract {
        self.surface_pool.reuse_contract()
    }

    /// Выделяет новый monotonic frame handle.
    fn allocate_handle(&mut self) -> anyhow::Result<FrameTextureHandle> {
        let handle_id = self.next_handle;
        self.next_handle = self
            .next_handle
            .checked_add(1)
            .context("texture handle counter exhausted")?;
        Ok(FrameTextureHandle(handle_id))
    }

    /// Возвращает `true`, если handle сейчас указывает на leased imported slot.
    pub fn is_imported_handle(&self, handle: FrameTextureHandle) -> bool {
        self.surface_pool.leased_slot_index(handle).is_some()
    }

    /// Возвращает texture views для active renderer lease-а.
    pub fn get_views(
        &self,
        handle: FrameTextureHandle,
    ) -> Option<(wgpu::TextureView, wgpu::TextureView)> {
        let slot_index = self.surface_pool.leased_slot_index(handle)?;
        let slot = self.slots.get(slot_index)?;
        Some((slot.y_view.clone(), slot.uv_view.clone()))
    }

    /// Импортирует или переиспользует DMA-BUF descriptor decoded VA surface-а.
    pub fn import_dma_buf_image(
        &mut self,
        image: &DecodedDmaBufImage,
    ) -> anyhow::Result<FrameTextureHandle> {
        let descriptor =
            ZeroCopySurfaceDescriptor::from_dma_buf_image(image).map_err(anyhow::Error::from)?;
        let frame_format = frame_format_for_dma_buf_image(image)?;
        let texture_handle = self.allocate_handle()?;

        match self
            .surface_pool
            .begin_import_or_reuse(descriptor.clone(), texture_handle)
            .map_err(anyhow::Error::from)?
        {
            ZeroCopyImportPlan::Reuse { slot_index } => {
                let slot = self
                    .slots
                    .get(slot_index)
                    .ok_or(ZeroCopySurfacePoolError::MissingStorageSlot { slot_index })?;
                ensure!(
                    slot.frame_format == frame_format,
                    "reused zero-copy import changed frame format: surface_id={}, cached={}, new={}",
                    descriptor.surface_id(),
                    slot.frame_format.diagnostic_label(),
                    frame_format.diagnostic_label()
                );
                tracing::trace!(
                    surface_id = descriptor.surface_id(),
                    handle_id = texture_handle.0,
                    slot_index,
                    "Reusing persistent zero-copy DMA-BUF import"
                );
                Ok(texture_handle)
            }
            ZeroCopyImportPlan::Create => {
                let imported_texture = self.import_texture_or_record_failure(image)?;

                let slot_index = self.slots.len();
                self.surface_pool
                    .register_imported_surface(descriptor.clone(), texture_handle, slot_index)
                    .map_err(anyhow::Error::from)?;
                self.slots.push(TextureSlot {
                    storage: imported_texture.storage,
                    frame_format: imported_texture.frame_format,
                    y_view: imported_texture.y_view,
                    uv_view: imported_texture.uv_view,
                });

                tracing::debug!(
                    surface_id = descriptor.surface_id(),
                    handle_id = texture_handle.0,
                    slot_index,
                    imports_created = self.surface_pool.stats().imports_created,
                    "Created persistent zero-copy DMA-BUF import"
                );

                Ok(texture_handle)
            }
            ZeroCopyImportPlan::Replace { slot_index } => {
                self.slots
                    .get(slot_index)
                    .ok_or(ZeroCopySurfacePoolError::MissingStorageSlot { slot_index })?;

                let imported_texture = self.import_texture_or_record_failure(image)?;
                self.surface_pool
                    .register_replaced_surface(descriptor.clone(), texture_handle, slot_index)
                    .map_err(anyhow::Error::from)?;

                let replacement_slot = TextureSlot {
                    storage: imported_texture.storage,
                    frame_format: imported_texture.frame_format,
                    y_view: imported_texture.y_view,
                    uv_view: imported_texture.uv_view,
                };
                let old_slot = std::mem::replace(&mut self.slots[slot_index], replacement_slot);
                drop(old_slot);

                tracing::debug!(
                    surface_id = descriptor.surface_id(),
                    handle_id = texture_handle.0,
                    slot_index,
                    imports_created = self.surface_pool.stats().imports_created,
                    imports_replaced = self.surface_pool.stats().imports_replaced,
                    "Replaced zero-copy DMA-BUF import after descriptor/object identity change"
                );

                Ok(texture_handle)
            }
        }
    }

    /// Импортирует exported DMA-BUF и учитывает ошибку в observable pool stats.
    fn import_texture_or_record_failure(
        &mut self,
        image: &DecodedDmaBufImage,
    ) -> anyhow::Result<crate::dma_buf_import::ImportedDmaBufTexture> {
        match self
            .dma_buf_importer
            .as_ref()
            .context("DMA-BUF importer not available")?
            .import_exported_dma_buf_image(image)
        {
            Ok(imported_texture) => Ok(imported_texture),
            Err(error) => {
                self.surface_pool.record_import_failure();
                Err(error)
            }
        }
    }

    /// Релизит renderer-owned frame после submit-а GPU work.
    pub fn release_after_gpu_submission(
        &mut self,
        handle: FrameTextureHandle,
    ) -> Result<ZeroCopyGpuCompletionLease, ZeroCopySurfacePoolError> {
        self.surface_pool.release_after_gpu_submission(handle)
    }

    /// Релизит frame, который не был передан renderer GPU work-у.
    pub fn release_without_gpu_submission(
        &mut self,
        handle: FrameTextureHandle,
    ) -> Result<(), ZeroCopySurfacePoolError> {
        self.surface_pool.release_without_gpu_submission(handle)
    }

    /// Подтверждает GPU completion для renderer-owned frame-а.
    pub fn acknowledge_gpu_completion(
        &mut self,
        handle: FrameTextureHandle,
    ) -> Result<(), ZeroCopySurfacePoolError> {
        self.surface_pool.acknowledge_gpu_completion(handle)
    }

    /// Подтверждает, что decoded surface вернулась в decoder pool.
    pub fn acknowledge_decoder_reuse(
        &mut self,
        handle: FrameTextureHandle,
    ) -> Result<(), ZeroCopySurfacePoolError> {
        self.surface_pool.acknowledge_decoder_reuse(handle)
    }

    /// Сбрасывает все idle imports после format/resolution change.
    pub fn invalidate_all(&mut self) -> Result<(), ZeroCopySurfacePoolError> {
        self.surface_pool.invalidate_all()?;
        self.slots.clear();
        Ok(())
    }

    /// Возвращает общее количество persistent imports.
    pub fn num_slots(&self) -> usize {
        self.surface_pool.stats().slots
    }

    /// Возвращает количество live/releasing surfaces.
    pub fn num_in_use(&self) -> usize {
        self.surface_pool.stats().active_surfaces
    }

    /// Возвращает компактную статистику pool для backpressure, UI и diagnostics.
    pub fn stats(&self) -> TexturePoolStats {
        let stats = self.surface_pool.stats();
        TexturePoolStats {
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

/// Определяет imported frame format до actual import/reuse.
fn frame_format_for_dma_buf_image(
    image: &DecodedDmaBufImage,
) -> anyhow::Result<crate::dma_buf_import::DmaBufFrameFormat> {
    match image.export_layout {
        DecodedDmaBufExportLayout::ComposedLayers => {
            let layer = image
                .layers
                .first()
                .context("exported DMA-BUF image has no DRM PRIME layers")?;
            crate::dma_buf_import::DmaBufFrameFormat::from_fourcc(image.fourcc, layer.drm_format)
        }
        DecodedDmaBufExportLayout::SeparateLayers => {
            crate::dma_buf_import::DmaBufFrameFormat::from_separate_layers(
                image.fourcc,
                &image.layers,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что high-level stats сохраняют поля lifecycle pool-а.
    #[test]
    fn texture_pool_stats_available_slots_use_active_surfaces() {
        let stats = TexturePoolStats {
            capacity: 16,
            slots: 8,
            in_use: 3,
            free_surfaces: 5,
            waiting_gpu_completion: 1,
            waiting_decoder_reuse: 1,
            import_failures: 2,
            imports_created: 8,
            imports_reused: 21,
            imports_replaced: 3,
        };

        assert_eq!(stats.available_slots(), 13);
        assert_eq!(stats.free_surfaces, 5);
        assert_eq!(stats.imports_reused, 21);
        assert_eq!(stats.imports_replaced, 3);
    }
}
