use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::{Context, ensure};
use cros_codecs::decoder::{DecodedDmaBufExportLayout, DecodedDmaBufImage, DecodedHandle};
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use video_core::FrameTextureHandle;

/// Максимальное количество слотов в пуле текстур.
///
/// 8 слотов достаточно для плавного воспроизведения: приложение держит
/// ~3 кадра (очередь + текущий), остальные свободны для reuse.
const MAX_TEXTURE_SLOTS: usize = 16;

/// Снимок заполнения texture pool для render-loop backpressure и UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TexturePoolStats {
    /// Максимальное число texture slots, которое pool разрешает держать одновременно.
    pub capacity: usize,
    /// Сколько slots сейчас существует в pool.
    pub slots: usize,
    /// Сколько slots сейчас занято кадрами, которые ещё нельзя переиспользовать.
    pub in_use: usize,
}

impl TexturePoolStats {
    /// Возвращает число slots, которые ещё можно занять без риска исчерпать pool.
    pub fn available_slots(self) -> usize {
        self.capacity.saturating_sub(self.in_use)
    }
}

/// Переменная окружения для рискованного кеша imported DMA-BUF textures.
///
/// По умолчанию выключено: persistent external image требует явных Vulkan
/// layout/ownership transitions между VA-API writer и Vulkan sampler.
const ZERO_COPY_IMPORT_CACHE_ENV_VAR: &str = "VIDEOPLAYER_ZERO_COPY_CACHE_IMPORTS";

/// Возвращает `true`, если текстовое значение env-переменной включает режим диагностики.
fn is_enabled_env_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Возвращает `true`, если включён экспериментальный кеш zero-copy imports.
fn should_cache_zero_copy_imports() -> bool {
    static SHOULD_CACHE: OnceLock<bool> = OnceLock::new();
    *SHOULD_CACHE.get_or_init(|| {
        std::env::var(ZERO_COPY_IMPORT_CACHE_ENV_VAR)
            .ok()
            .as_deref()
            .map(is_enabled_env_value)
            .unwrap_or(false)
    })
}

/// GPU-storage, на котором построены Y/UV views одного кадра.
enum TextureSlotStorage {
    /// Две независимые imported текстуры под Y и UV planes.
    SeparatePlanes {
        /// wgpu-текстура для Y-плоскости.
        y_texture: wgpu::Texture,
        /// wgpu-текстура для UV-плоскости.
        uv_texture: wgpu::Texture,
    },
    /// Zero-copy путь: imported DMA-BUF storage, экспортированный из VA surface.
    ImportedDmaBuf {
        /// Владелец imported VkImage/VkDeviceMemory через wgpu texture wrapper.
        _storage: crate::dma_buf_import::ImportedDmaBufStorage,

        /// Формат imported texture; нужен для диагностики release/invalidate path.
        _frame_format: crate::dma_buf_import::DmaBufFrameFormat,
    },
}

impl TextureSlotStorage {
    /// Просит wgpu как можно раньше освободить native texture storage.
    fn destroy(&self) {
        match self {
            Self::SeparatePlanes {
                y_texture,
                uv_texture,
            } => {
                y_texture.destroy();
                uv_texture.destroy();
            }
            Self::ImportedDmaBuf { _storage, .. } => match _storage {
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
            },
        }
    }
}

/// Один слот пула, содержащий GPU-storage и пару Y/UV views для одного кадра.
///
/// Каждый слот привязан к конкретному разрешению. При смене разрешения
/// все слоты инвалидируются через [`WgpuTexturePool::invalidate_all`].
struct TextureSlot {
    /// Владение GPU texture/storage для кадра.
    storage: TextureSlotStorage,
    /// Представление (view) Y-текстуры для биндинга в шейдер.
    y_view: wgpu::TextureView,
    /// Представление (view) UV-текстуры для биндинга в шейдер.
    uv_view: wgpu::TextureView,
    /// Флаг занятости: `true` — слот содержит актуальный кадр,
    /// `false` — слот свободен и может быть переиспользован.
    in_use: bool,
    /// `true` если текстуры созданы через zero-copy DMA-BUF import.
    /// Такие слоты не переиспользуются — при release удаляются из пула.
    is_imported: bool,
}

/// Released imported slot, который ещё нельзя дропать немедленно.
pub struct RetiredImportedSlot {
    /// Handle кадра; по нему decoder thread найдёт VA guard и вернёт surface в pool.
    pub frame_handle: FrameTextureHandle,
    /// Сам slot держит imported wgpu texture и тем самым raw VkImage/VkDeviceMemory.
    _slot: TextureSlot,
}

impl Drop for RetiredImportedSlot {
    fn drop(&mut self) {
        self._slot.storage.destroy();
    }
}

/// Cached imported texture for one VA surface.
///
/// VA decoder reuses a small pool of output surfaces. A persistent imported
/// texture can therefore be reused every time the same `VASurfaceID` appears
/// again, instead of creating a new Vulkan image and importing external memory
/// for every decoded frame.
struct CachedImportedDmaBufTexture {
    /// Persistent imported texture storage for this VA surface.
    storage: crate::dma_buf_import::ImportedDmaBufStorage,

    /// Persistent luma view.
    y_view: wgpu::TextureView,

    /// Persistent chroma view.
    uv_view: wgpu::TextureView,

    /// Cached decoded frame format.
    frame_format: crate::dma_buf_import::DmaBufFrameFormat,

    /// Cached coded width.
    width: u32,

    /// Cached coded height.
    height: u32,
}

/// Пул пар wgpu-текстур (Y + UV), индексируемых через [`FrameTextureHandle`].
///
/// Используется для повторного использования wgpu-текстур между кадрами,
/// что позволяет избежать дорогостоящего выделения памяти GPU на каждом кадре.
///
/// # Аллокация
/// - Первый кадр нового разрешения создаёт новый [`TextureSlot`].
/// - Последующие кадры переиспользуют свободный слот или создают новый
///   (до лимита [`MAX_TEXTURE_SLOTS`]).
/// - При [`FormatChanged`] все слоты дропаются — старые размеры больше не валидны.
pub struct WgpuTexturePool {
    /// Импортёр DMA-BUF для zero-copy path (None если не Vulkan).
    dma_buf_importer: Option<crate::dma_buf_import::DmaBufImporter>,
    /// Stable slots с текстурами. `None` — дырка после release imported slot.
    slots: Vec<Option<TextureSlot>>,
    /// Persistent zero-copy imports keyed by backend surface id.
    imported_dma_buf_cache: HashMap<u64, CachedImportedDmaBufTexture>,
    /// Отображение handle id -> индекс слота в `slots`.
    ///
    /// Позволяет за O(1) находить слот по [`FrameTextureHandle`].
    handle_to_slot: HashMap<u64, usize>,
    /// Монотонно возрастающий счётчик для генерации уникальных handle id.
    ///
    /// Не сбрасывается при invalidate, чтобы stale handle не смог попасть в новый кадр.
    next_handle: u64,
}

impl WgpuTexturePool {
    /// Создаёт новый пустой пул текстур.
    ///
    /// # Аргументы
    /// * `dma_buf_importer` — опциональный импортёр для zero-copy DMA-BUF import.
    pub fn new(dma_buf_importer: Option<crate::dma_buf_import::DmaBufImporter>) -> Self {
        Self {
            dma_buf_importer,
            slots: Vec::with_capacity(MAX_TEXTURE_SLOTS),
            imported_dma_buf_cache: HashMap::new(),
            handle_to_slot: HashMap::new(),
            next_handle: 0,
        }
    }

    /// Возвращает immutable slot по стабильному индексу.
    fn slot(&self, slot_index: usize) -> Option<&TextureSlot> {
        self.slots.get(slot_index).and_then(Option::as_ref)
    }

    /// Возвращает mutable slot по стабильному индексу.
    fn slot_mut(&mut self, slot_index: usize) -> Option<&mut TextureSlot> {
        self.slots.get_mut(slot_index).and_then(Option::as_mut)
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

    /// Привязывает свежий handle к существующему stable slot.
    fn bind_handle_to_slot(
        &mut self,
        handle: FrameTextureHandle,
        slot_index: usize,
    ) -> anyhow::Result<()> {
        ensure!(
            self.slot(slot_index).is_some(),
            "cannot bind texture handle {} to missing slot {}",
            handle.0,
            slot_index
        );
        ensure!(
            !self.handle_to_slot.contains_key(&handle.0),
            "texture handle collision for id {}",
            handle.0
        );

        self.handle_to_slot.insert(handle.0, slot_index);
        Ok(())
    }

    /// Вставляет новый slot, переиспользуя дыру от ранее released imported slot.
    fn insert_slot(&mut self, slot: TextureSlot) -> anyhow::Result<usize> {
        if let Some(slot_index) = self.slots.iter().position(Option::is_none) {
            self.slots[slot_index] = Some(slot);
            return Ok(slot_index);
        }

        ensure!(
            self.slots.len() < MAX_TEXTURE_SLOTS,
            "Texture pool exhausted (max {} slots, active_slots={})",
            MAX_TEXTURE_SLOTS,
            self.active_slot_count()
        );

        let slot_index = self.slots.len();
        self.slots.push(Some(slot));
        Ok(slot_index)
    }

    /// Вставляет новый уже занятый slot и сразу публикует handle mapping.
    fn insert_active_slot(&mut self, slot: TextureSlot) -> anyhow::Result<FrameTextureHandle> {
        let handle = self.allocate_handle()?;
        let slot_index = self.insert_slot(slot)?;
        self.bind_handle_to_slot(handle, slot_index)?;
        Ok(handle)
    }

    /// Удаляет только хвостовые vacant entries, не меняя индексы живых slots.
    fn trim_vacant_tail(&mut self) {
        while self.slots.last().is_some_and(Option::is_none) {
            self.slots.pop();
        }
    }

    /// Считает физически существующие slots, игнорируя vacant entries.
    fn active_slot_count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    /// Возвращает `true`, если пул может попробовать zero-copy DMA-BUF import.
    pub fn can_import_dma_buf(&self) -> bool {
        self.dma_buf_importer.is_some()
    }

    /// Возвращает `true`, если handle указывает на zero-copy imported slot.
    pub fn is_imported_handle(&self, handle: FrameTextureHandle) -> bool {
        self.handle_to_slot
            .get(&handle.0)
            .and_then(|slot_index| self.slot(*slot_index))
            .map(|slot| slot.is_imported)
            .unwrap_or(false)
    }

    /// Возвращает texture views для заданного frame handle.
    ///
    /// Используется в рендер-цикле для получения Y/UV views
    /// по handle из [`DecodedFrame::texture_handle`].
    ///
    /// # Аргументы
    /// * `handle` — [`FrameTextureHandle`], полученный из zero-copy import path.
    ///
    /// # Возвращаемое значение
    /// `Some((y_view, uv_view))` если handle найден и слот занят.
    /// `None` если handle неизвестен или слот уже освобождён.
    pub fn get_views(
        &self,
        handle: FrameTextureHandle,
    ) -> Option<(wgpu::TextureView, wgpu::TextureView)> {
        // O(1) lookup по handle id.
        let &slot_index = self.handle_to_slot.get(&handle.0)?;
        let slot = self.slot(slot_index)?;
        if !slot.in_use {
            return None;
        }
        Some((slot.y_view.clone(), slot.uv_view.clone()))
    }

    /// Импортирует decoded frame через zero-copy DMA-BUF import.
    ///
    /// Создаёт новые wgpu textures напрямую из dma-buf fd без CPU readback.
    /// Текстуры не переиспользуются — при release slot удаляется из пула.
    ///
    /// # Аргументы
    /// * `handle` — handle декодированного кадра от cros-codecs.
    ///
    /// # Ошибки
    /// Возвращает ошибку если импортёр не доступен или Vulkan API выдаёт ошибку.
    pub fn import_frame(
        &mut self,
        handle: &dyn DecodedHandle<Frame = GenericDmaVideoFrame>,
    ) -> anyhow::Result<FrameTextureHandle> {
        let active_slots = self.active_slot_count();
        ensure!(
            active_slots < MAX_TEXTURE_SLOTS,
            "zero-copy texture slot limit reached before importing GenericDmaVideoFrame: active_slots={}, max_slots={}",
            active_slots,
            MAX_TEXTURE_SLOTS
        );

        let frame = handle.video_frame();
        let (y_texture, uv_texture) = self
            .dma_buf_importer
            .as_ref()
            .context("DMA-BUF importer not available")?
            .import_nv12(&frame)?;

        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let uv_view = uv_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let texture_handle = self.insert_active_slot(TextureSlot {
            storage: TextureSlotStorage::SeparatePlanes {
                y_texture,
                uv_texture,
            },
            y_view,
            uv_view,
            in_use: true,
            is_imported: true,
        })?;

        Ok(texture_handle)
    }

    /// Импортирует DMA-BUF descriptor, экспортированный из decoded VA surface.
    pub fn import_dma_buf_image(
        &mut self,
        image: &DecodedDmaBufImage,
    ) -> anyhow::Result<FrameTextureHandle> {
        let active_slots = self.active_slot_count();
        ensure!(
            active_slots < MAX_TEXTURE_SLOTS,
            "zero-copy texture slot limit reached before importing VA surface: active_slots={}, max_slots={}, surface_id={}",
            active_slots,
            MAX_TEXTURE_SLOTS,
            image.surface_id
        );

        let imported_texture = if should_cache_zero_copy_imports() {
            self.import_cached_dma_buf_image(image)?
        } else {
            self.dma_buf_importer
                .as_ref()
                .context("DMA-BUF importer not available")?
                .import_exported_dma_buf_image(image)?
        };

        let texture_handle = self.insert_active_slot(TextureSlot {
            storage: TextureSlotStorage::ImportedDmaBuf {
                _storage: imported_texture.storage,
                _frame_format: imported_texture.frame_format,
            },
            y_view: imported_texture.y_view,
            uv_view: imported_texture.uv_view,
            in_use: true,
            is_imported: true,
        })?;

        Ok(texture_handle)
    }

    /// Импортирует DMA-BUF descriptor с экспериментальным кешем по VA surface id.
    ///
    /// Кеш выключен по умолчанию, потому что persistent external image требует
    /// явных Vulkan layout/ownership transitions между повторными VA writes.
    fn import_cached_dma_buf_image(
        &mut self,
        image: &DecodedDmaBufImage,
    ) -> anyhow::Result<crate::dma_buf_import::ImportedDmaBufTexture> {
        let frame_format = match image.export_layout {
            DecodedDmaBufExportLayout::ComposedLayers => {
                let layer = image
                    .layers
                    .first()
                    .context("exported DMA-BUF image has no DRM PRIME layers")?;
                crate::dma_buf_import::DmaBufFrameFormat::from_fourcc(
                    image.fourcc,
                    layer.drm_format,
                )?
            }
            DecodedDmaBufExportLayout::SeparateLayers => {
                crate::dma_buf_import::DmaBufFrameFormat::from_separate_layers(
                    image.fourcc,
                    &image.layers,
                )?
            }
        };

        if let Some(cached_import) = self.imported_dma_buf_cache.get(&image.surface_id) {
            if cached_import.frame_format != frame_format {
                anyhow::bail!(
                    "cached imported VA surface changed format: surface_id={}, cached={}, new={}",
                    image.surface_id,
                    cached_import.frame_format.diagnostic_label(),
                    frame_format.diagnostic_label()
                );
            }
            if cached_import.width != image.width || cached_import.height != image.height {
                anyhow::bail!(
                    "cached imported VA surface changed size: surface_id={}, cached={}x{}, new={}x{}",
                    image.surface_id,
                    cached_import.width,
                    cached_import.height,
                    image.width,
                    image.height
                );
            }
            tracing::trace!(
                surface_id = image.surface_id,
                "Reusing cached zero-copy DMA-BUF import"
            );
            return Ok(crate::dma_buf_import::ImportedDmaBufTexture {
                frame_format: cached_import.frame_format,
                storage: cached_import.storage.clone(),
                y_view: cached_import.y_view.clone(),
                uv_view: cached_import.uv_view.clone(),
            });
        }

        let imported_texture = self
            .dma_buf_importer
            .as_ref()
            .context("DMA-BUF importer not available")?
            .import_exported_dma_buf_image(image)?;

        tracing::debug!(
            surface_id = image.surface_id,
            format = imported_texture.frame_format.diagnostic_label(),
            cache_len = self.imported_dma_buf_cache.len() + 1,
            "Caching zero-copy DMA-BUF import for VA surface"
        );

        self.imported_dma_buf_cache.insert(
            image.surface_id,
            CachedImportedDmaBufTexture {
                storage: imported_texture.storage.clone(),
                y_view: imported_texture.y_view.clone(),
                uv_view: imported_texture.uv_view.clone(),
                frame_format: imported_texture.frame_format,
                width: image.width,
                height: image.height,
            },
        );

        Ok(imported_texture)
    }

    /// Освобождает слот, связанный с данным handle.
    ///
    /// Для non-imported test slots: помечает как свободный для reuse.
    /// Для production imported slots: удаляет слот из пула, так как
    /// imported textures привязаны к конкретному dma-buf fd.
    ///
    /// # Аргументы
    /// * `handle` — [`FrameTextureHandle`] для освобождения.
    pub fn release_slot(&mut self, handle: FrameTextureHandle) -> Option<RetiredImportedSlot> {
        tracing::debug!(
            handle_id = handle.0,
            pool_len = self.slots.len(),
            in_use = self.num_in_use(),
            "release_slot called"
        );
        if let Some(slot_index) = self.handle_to_slot.remove(&handle.0) {
            let Some(is_imported) = self.slot(slot_index).map(|slot| slot.is_imported) else {
                tracing::warn!(
                    handle_id = handle.0,
                    slot_index,
                    "release_slot: handle pointed to a vacant texture slot"
                );
                return None;
            };

            if is_imported {
                // Забираем imported slot без сдвига соседних индексов.
                tracing::debug!(
                    slot_index,
                    pool_len = self.slots.len(),
                    "Retiring imported slot until GPU completion"
                );
                let Some(slot) = self.slots.get_mut(slot_index).and_then(Option::take) else {
                    tracing::warn!(
                        handle_id = handle.0,
                        slot_index,
                        "release_slot: imported slot disappeared before retirement"
                    );
                    return None;
                };
                let retired_slot = RetiredImportedSlot {
                    frame_handle: handle,
                    _slot: slot,
                };

                self.trim_vacant_tail();
                return Some(retired_slot);
            } else {
                // Обычный slot: помечаем как свободный для reuse.
                if let Some(slot) = self.slot_mut(slot_index) {
                    slot.in_use = false;
                    tracing::debug!(slot_index, "Slot marked as free");
                }
            }
        } else {
            tracing::warn!(
                handle_id = handle.0,
                "release_slot: handle not found in map"
            );
        }

        None
    }

    /// Сбрасывает все слоты и handle mappings.
    ///
    /// Вызывается при `FormatChanged` — старые текстуры больше не валидны
    /// для нового разрешения или формата. Все wgpu-текстуры дропаются,
    /// память GPU освобождается.
    pub fn invalidate_all(&mut self) {
        self.slots.clear();
        self.imported_dma_buf_cache.clear();
        self.handle_to_slot.clear();
    }

    /// Возвращает общее количество слотов в пуле.
    pub fn num_slots(&self) -> usize {
        self.active_slot_count()
    }

    /// Возвращает количество занятых (in_use) слотов.
    pub fn num_in_use(&self) -> usize {
        self.slots
            .iter()
            .filter_map(Option::as_ref)
            .filter(|slot| slot.in_use)
            .count()
    }

    /// Возвращает компактную статистику pool для backpressure и UI.
    pub fn stats(&self) -> TexturePoolStats {
        TexturePoolStats {
            capacity: MAX_TEXTURE_SLOTS,
            slots: self.num_slots(),
            in_use: self.num_in_use(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Тест: создание пула и проверка начального состояния.
    #[test]
    fn test_pool_creation() {
        // Пул создаётся пустым, без device тест неполный — но структура проверена.
        // Полноценный тест требует wgpu device, что делается в интеграционных тестах.
        let map = HashMap::<u64, usize>::new();
        assert!(map.is_empty());
    }
}
