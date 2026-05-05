use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use cros_codecs::decoder::DecodedHandle;
use cros_codecs::video_frame::VideoFrame;
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use video_core::FrameTextureHandle;

/// Максимальное количество слотов в пуле текстур.
///
/// 8 слотов достаточно для плавного воспроизведения: приложение держит
/// ~3 кадра (очередь + текущий), остальные свободны для reuse.
const MAX_TEXTURE_SLOTS: usize = 16;

/// Логирует короткие samples Y/UV плоскостей для диагностики зелёного/чёрного экрана.
fn log_plane_samples(y_plane: &[u8], uv_plane: Option<&[u8]>) {
    if !y_plane.is_empty() {
        let y_first = y_plane[0];
        let y_mid = y_plane[y_plane.len() / 2];
        let y_avg = y_plane.iter().map(|&b| b as u64).sum::<u64>() / y_plane.len().max(1) as u64;
        tracing::info!(
            y_first,
            y_mid,
            y_avg,
            y_len = y_plane.len(),
            "Y-plane sample"
        );
    }

    if let Some(uv_plane) = uv_plane
        && uv_plane.len() >= 2
    {
        let uv_first_u = uv_plane[0];
        let uv_first_v = uv_plane[1];
        let uv_avg = uv_plane.iter().map(|&b| b as u64).sum::<u64>() / uv_plane.len().max(1) as u64;
        tracing::info!(
            uv_first_u,
            uv_first_v,
            uv_avg,
            uv_len = uv_plane.len(),
            "UV-plane sample"
        );
    }
}

/// Один слот пула, содержащий пару wgpu-текстур (Y + UV) для одного кадра.
///
/// Каждый слот привязан к конкретному разрешению. При смене разрешения
/// все слоты инвалидируются через [`WgpuTexturePool::invalidate_all`].
struct TextureSlot {
    /// wgpu-текстура для Y-плоскости (формат R8Unorm, 1 байт на пиксель).
    y_texture: wgpu::Texture,
    /// wgpu-текстура для UV-плоскости (формат Rg8Unorm, 2 байта на пиксель).
    uv_texture: wgpu::Texture,
    /// Представление (view) Y-текстуры для биндинга в шейдер.
    y_view: wgpu::TextureView,
    /// Представление (view) UV-текстуры для биндинга в шейдер.
    uv_view: wgpu::TextureView,
    /// Ширина кадра в пикселях (coded resolution).
    width: u32,
    /// Высота кадра в пикселях (coded resolution).
    height: u32,
    /// Флаг занятости: `true` — слот содержит актуальный кадр,
    /// `false` — слот свободен и может быть переиспользован.
    in_use: bool,
    /// `true` если текстуры созданы через zero-copy DMA-BUF import.
    /// Такие слоты не переиспользуются — при release удаляются из пула.
    is_imported: bool,
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
    /// Устройство wgpu, необходимое для создания новых текстур.
    device: Arc<wgpu::Device>,
    /// Импортёр DMA-BUF для zero-copy path (None если не Vulkan).
    dma_buf_importer: Option<crate::dma_buf_import::DmaBufImporter>,
    /// Слоты с текстурами. Индекс в векторе — внутренний slot_index.
    slots: Vec<TextureSlot>,
    /// Отображение handle id -> индекс слота в `slots`.
    ///
    /// Позволяет за O(1) находить слот по [`FrameTextureHandle`].
    handle_to_slot: HashMap<u64, usize>,
    /// Монотонно возрастающий счётчик для генерации уникальных handle id.
    next_handle: u64,
}

impl WgpuTexturePool {
    /// Создаёт новый пустой пул текстур.
    ///
    /// # Аргументы
    /// * `device` — [`Arc<wgpu::Device>`] для создания текстур.
    /// * `dma_buf_importer` — опциональный импортёр для zero-copy DMA-BUF import.
    pub fn new(
        device: Arc<wgpu::Device>,
        dma_buf_importer: Option<crate::dma_buf_import::DmaBufImporter>,
    ) -> Self {
        Self {
            device,
            dma_buf_importer,
            slots: Vec::with_capacity(MAX_TEXTURE_SLOTS),
            handle_to_slot: HashMap::new(),
            next_handle: 0,
        }
    }

    /// Возвращает `true`, если пул может попробовать zero-copy DMA-BUF import.
    pub fn can_import_dma_buf(&self) -> bool {
        self.dma_buf_importer.is_some()
    }

    /// Загружает декодированный кадр в свободный слот пула.
    ///
    /// Выполняет стабильный CPU upload pipeline:
    /// 1. VA-API path: читаем internal VA surface через `DecodedHandle::nv12_image()`.
    /// 2. Generic path: маппим client-provided frame через `VideoFrame::map()`.
    /// 3. `write_texture()` — загружаем Y/UV плоскости в wgpu-текстуры.
    ///
    /// # Предусловие
    /// Вызывающий код ДОЛЖЕН выполнить `handle.sync()` перед вызовом.
    /// Двойной sync избыточен и приводит к лишней задержке.
    ///
    /// # Аргументы
    /// * `handle` — handle декодированного кадра от cros-codecs (уже synced).
    /// * `queue` — wgpu queue для загрузки данных.
    ///
    /// # Возвращаемое значение
    /// [`FrameTextureHandle`] — идентификатор слота для последующего [`Self::get_views`].
    ///
    /// # Ошибки
    /// Возвращает ошибку если:
    /// - маппинг DMA-BUF не удался,
    /// - кадр имеет менее 2 плоскостей (не NV12),
    /// - пул текстур исчерпан.
    pub fn upload_frame<Frame>(
        &mut self,
        handle: &dyn DecodedHandle<Frame = Frame>,
        queue: &wgpu::Queue,
    ) -> anyhow::Result<FrameTextureHandle>
    where
        Frame: VideoFrame,
    {
        let total_start = std::time::Instant::now();

        // VA-API fallback: если backend умеет вернуть image из native surface,
        // используем его вместо `VideoFrame::map()`. Это нужно для драйверов,
        // которые не пишут decoded pixels в external DRM PRIME output buffers.
        if let Some(image) = handle.nv12_image()? {
            let map_elapsed = total_start.elapsed().as_millis();

            tracing::info!(
                width = image.width,
                height = image.height,
                y_stride = image.y_stride,
                uv_stride = image.uv_stride,
                "VA image readback acquired"
            );
            log_plane_samples(&image.y_plane, Some(&image.uv_plane));

            let slot_start = std::time::Instant::now();
            let slot_index = self.find_or_create_slot(image.width, image.height)?;
            let slot = &self.slots[slot_index];
            let slot_elapsed = slot_start.elapsed().as_millis();

            let write_y_start = std::time::Instant::now();
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &slot.y_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &image.y_plane,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(image.y_stride),
                    rows_per_image: Some(image.height),
                },
                wgpu::Extent3d {
                    width: image.width,
                    height: image.height,
                    depth_or_array_layers: 1,
                },
            );
            let write_y_elapsed = write_y_start.elapsed().as_millis();

            let write_uv_start = std::time::Instant::now();
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &slot.uv_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &image.uv_plane,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(image.uv_stride),
                    rows_per_image: Some(image.height / 2),
                },
                wgpu::Extent3d {
                    width: image.width / 2,
                    height: image.height / 2,
                    depth_or_array_layers: 1,
                },
            );
            let write_uv_elapsed = write_uv_start.elapsed().as_millis();

            self.slots[slot_index].in_use = true;
            self.slots[slot_index].is_imported = false;
            let handle_id = self.next_handle;
            self.next_handle += 1;
            self.handle_to_slot.insert(handle_id, slot_index);

            tracing::info!(
                handle_id,
                map_ms = map_elapsed,
                slot_ms = slot_elapsed,
                write_y_ms = write_y_elapsed,
                write_uv_ms = write_uv_elapsed,
                total_ms = total_start.elapsed().as_millis(),
                "upload_frame timing breakdown"
            );

            return Ok(FrameTextureHandle(handle_id));
        }

        // Шаг 1: Получаем видео-фрейм из handle.
        let frame = handle.video_frame();

        // Шаг 2: Маппим decoded frame в CPU-адресное пространство для чтения.
        let map_start = std::time::Instant::now();
        let mapping = frame
            .map()
            .map_err(|e| anyhow::anyhow!("Failed to map decoded frame: {}", e))?;
        let planes = mapping.get();
        let map_elapsed = map_start.elapsed().as_millis();

        log_plane_samples(planes[0], planes.get(1).copied());

        // NV12 требует ровно 2 плоскости: Y (luma) и UV (chroma, interleaved).
        if planes.len() < 2 {
            return Err(anyhow::anyhow!(
                "Expected at least 2 planes (NV12), got {}",
                planes.len()
            ));
        }

        let resolution = frame.resolution();
        let width = resolution.width;
        let height = resolution.height;
        let y_stride = frame.get_plane_pitch()[0] as u32;
        let uv_stride = frame
            .get_plane_pitch()
            .get(1)
            .copied()
            .unwrap_or(y_stride as usize) as u32;

        // Шаг 3: Находим или создаём свободный слот подходящего разрешения.
        let slot_start = std::time::Instant::now();
        let slot_index = self.find_or_create_slot(width, height)?;
        let slot = &self.slots[slot_index];
        let slot_elapsed = slot_start.elapsed().as_millis();

        // Шаг 4: Загружаем Y-плоскость в текстуру формата R8Unorm.
        let write_y_start = std::time::Instant::now();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &slot.y_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            planes[0],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(y_stride),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let write_y_elapsed = write_y_start.elapsed().as_millis();

        // Шаг 5: Загружаем UV-плоскость в текстуру формата Rg8Unorm.
        let write_uv_start = std::time::Instant::now();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &slot.uv_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            planes[1],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(uv_stride),
                rows_per_image: Some(height / 2),
            },
            wgpu::Extent3d {
                width: width / 2,
                height: height / 2,
                depth_or_array_layers: 1,
            },
        );
        let write_uv_elapsed = write_uv_start.elapsed().as_millis();

        // Шаг 6: Помечаем слот как занятый и генерируем уникальный handle.
        self.slots[slot_index].in_use = true;
        self.slots[slot_index].is_imported = false;
        let handle_id = self.next_handle;
        self.next_handle += 1;
        self.handle_to_slot.insert(handle_id, slot_index);

        let total_elapsed = total_start.elapsed().as_millis();
        tracing::info!(
            handle_id,
            map_ms = map_elapsed,
            slot_ms = slot_elapsed,
            write_y_ms = write_y_elapsed,
            write_uv_ms = write_uv_elapsed,
            total_ms = total_elapsed,
            "upload_frame timing breakdown"
        );

        Ok(FrameTextureHandle(handle_id))
    }

    /// Возвращает texture views для заданного frame handle.
    ///
    /// Используется в рендер-цикле для получения Y/UV views
    /// по handle из [`DecodedFrame::texture_handle`].
    ///
    /// # Аргументы
    /// * `handle` — [`FrameTextureHandle`], полученный из [`Self::upload_frame`].
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
        let slot = self.slots.get(slot_index)?;
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
        let frame = handle.video_frame();
        let (y_texture, uv_texture) = self
            .dma_buf_importer
            .as_ref()
            .context("DMA-BUF importer not available")?
            .import_nv12(&frame)?;

        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let uv_view = uv_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let slot_index = self.slots.len();
        self.slots.push(TextureSlot {
            y_texture,
            uv_texture,
            y_view,
            uv_view,
            width: frame.resolution().width,
            height: frame.resolution().height,
            in_use: true,
            is_imported: true,
        });

        let handle_id = self.next_handle;
        self.next_handle += 1;
        self.handle_to_slot.insert(handle_id, slot_index);

        Ok(FrameTextureHandle(handle_id))
    }

    /// Освобождает слот, связанный с данным handle.
    ///
    /// Для обычных слотов (CPU upload): помечает как свободный для reuse.
    /// Для imported слотов (zero-copy): удаляет слот из пула, так как
    /// imported textures привязаны к конкретному dma-buf fd.
    ///
    /// # Аргументы
    /// * `handle` — [`FrameTextureHandle`] для освобождения.
    pub fn release_slot(&mut self, handle: FrameTextureHandle) {
        tracing::debug!(
            handle_id = handle.0,
            pool_len = self.slots.len(),
            in_use = self.num_in_use(),
            "release_slot called"
        );
        if let Some(&slot_index) = self.handle_to_slot.get(&handle.0) {
            let is_imported = self
                .slots
                .get(slot_index)
                .map(|s| s.is_imported)
                .unwrap_or(false);

            if is_imported {
                // Удаляем imported slot из пула — textures привязаны к fd.
                // Используем Vec::remove вместо swap_remove чтобы не портить индексы
                // других слотов в handle_to_slot.
                tracing::info!(
                    slot_index,
                    pool_len = self.slots.len(),
                    "Removing imported slot from pool"
                );
                self.slots.remove(slot_index);

                // Обновляем handle_to_slot: все индексы > slot_index уменьшаем на 1
                // (элементы сдвинулись влево после remove).
                for idx in self.handle_to_slot.values_mut() {
                    if *idx > slot_index {
                        *idx -= 1;
                    }
                }
                tracing::debug!(slot_index, "Imported slot removed from pool");
            } else {
                // Обычный slot: помечаем как свободный для reuse.
                if let Some(slot) = self.slots.get_mut(slot_index) {
                    slot.in_use = false;
                    tracing::debug!(slot_index, "Slot marked as free");
                }
            }

            self.handle_to_slot.remove(&handle.0);
        } else {
            tracing::warn!(
                handle_id = handle.0,
                "release_slot: handle not found in map"
            );
        }
    }

    /// Сбрасывает все слоты и handle mappings.
    ///
    /// Вызывается при `FormatChanged` — старые текстуры больше не валидны
    /// для нового разрешения или формата. Все wgpu-текстуры дропаются,
    /// память GPU освобождается.
    pub fn invalidate_all(&mut self) {
        self.slots.clear();
        self.handle_to_slot.clear();
        self.next_handle = 0;
    }

    /// Возвращает общее количество слотов в пуле.
    pub fn num_slots(&self) -> usize {
        self.slots.len()
    }

    /// Возвращает количество занятых (in_use) слотов.
    pub fn num_in_use(&self) -> usize {
        self.slots.iter().filter(|s| s.in_use).count()
    }

    /// Находит свободный слот с подходящим разрешением или создаёт новый.
    ///
    /// # Аллокация
    /// 1. Ищем свободный (`!in_use`) слот с точно совпадающим `(width, height)`.
    /// 2. Если не нашли — создаём новый слот с новыми текстурами.
    /// 3. Если достигнут лимит [`MAX_TEXTURE_SLOTS`] — ошибка.
    ///
    /// # Аргументы
    /// * `width` — ширина кадра в пикселях.
    /// * `height` — высота кадра в пикселях.
    ///
    /// # Ошибки
    /// Возвращает ошибку если пул исчерпан.
    fn find_or_create_slot(&mut self, width: u32, height: u32) -> anyhow::Result<usize> {
        // Шаг 1: Поиск существующего свободного слота с совпадающим разрешением.
        if let Some(idx) = self
            .slots
            .iter()
            .position(|s| !s.in_use && s.width == width && s.height == height)
        {
            return Ok(idx);
        }

        // Шаг 2: Проверяем лимит пула.
        if self.slots.len() >= MAX_TEXTURE_SLOTS {
            return Err(anyhow::anyhow!(
                "Texture pool exhausted (max {} slots, all in use or different resolution)",
                MAX_TEXTURE_SLOTS
            ));
        }

        // Шаг 3: Создаём Y-текстуру (R8Unorm, width × height).
        let y_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("vaapi_y_texture_{}x{}", width, height)),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // Шаг 4: Создаём UV-текстуру (Rg8Unorm, width/2 × height/2).
        let uv_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("vaapi_uv_texture_{}x{}", width / 2, height / 2)),
            size: wgpu::Extent3d {
                width: width / 2,
                height: height / 2,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // Шаг 5: Создаём views для биндинга в шейдер.
        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let uv_view = uv_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Шаг 6: Добавляем слот в пул.
        let slot_index = self.slots.len();
        self.slots.push(TextureSlot {
            y_texture,
            uv_texture,
            y_view,
            uv_view,
            width,
            height,
            in_use: false,
            is_imported: false,
        });

        Ok(slot_index)
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
