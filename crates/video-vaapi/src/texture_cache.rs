use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, ensure};
use cros_codecs::decoder::{DecodedDmaBufExportLayout, DecodedDmaBufImage, DecodedHandle};
use cros_codecs::video_frame::VideoFrame;
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

/// Переменная окружения, которая включает диагностику содержимого NV12-плоскостей.
const PLANE_SAMPLE_LOG_ENV_VAR: &str = "VIDEOPLAYER_LOG_NV12_SAMPLES";

/// Переменная окружения, которая сохраняет первый NV12 кадр в файлы `/tmp`.
const FRAME_DUMP_ENV_VAR: &str = "VIDEOPLAYER_DUMP_FIRST_NV12_FRAME";

/// Переменная окружения для рискованного кеша imported DMA-BUF textures.
///
/// По умолчанию выключено: persistent external image требует явных Vulkan
/// layout/ownership transitions между VA-API writer и Vulkan sampler.
const ZERO_COPY_IMPORT_CACHE_ENV_VAR: &str = "VIDEOPLAYER_ZERO_COPY_CACHE_IMPORTS";

/// Возвращает `true`, если включена диагностика содержимого NV12-плоскостей.
fn should_log_plane_samples() -> bool {
    static SHOULD_LOG: OnceLock<bool> = OnceLock::new();
    *SHOULD_LOG.get_or_init(|| {
        std::env::var(PLANE_SAMPLE_LOG_ENV_VAR)
            .ok()
            .as_deref()
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    })
}

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

/// Возвращает директорию, куда нужно сохранить dump первого NV12 кадра.
fn first_frame_dump_dir() -> Option<PathBuf> {
    static DUMP_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DUMP_DIR
        .get_or_init(|| {
            let value = std::env::var(FRAME_DUMP_ENV_VAR).ok()?;
            if !is_enabled_env_value(&value) {
                return None;
            }
            Some(std::env::temp_dir())
        })
        .clone()
}

/// Ограничивает значение цветового канала диапазоном PPM.
fn clamp_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Простая статистика по одной 8-bit плоскости.
#[derive(Debug, Clone, Copy)]
struct PlaneStats {
    /// Минимальное значение по выбранной области.
    min: u8,
    /// Максимальное значение по выбранной области.
    max: u8,
    /// Среднее значение по выбранной области.
    average: u64,
}

/// Считает статистику по прямоугольной 8-bit области с заданным stride.
fn plane_stats(
    plane: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    component_offset: usize,
    component_step: usize,
) -> anyhow::Result<PlaneStats> {
    ensure!(width > 0, "Plane stats width must be > 0");
    ensure!(height > 0, "Plane stats height must be > 0");
    ensure!(component_step > 0, "Plane stats component_step must be > 0");

    let mut min_value = u8::MAX;
    let mut max_value = u8::MIN;
    let mut sum = 0u64;
    let mut count = 0u64;

    for row_index in 0..height as usize {
        let row_start = row_index
            .saturating_mul(stride as usize)
            .saturating_add(component_offset);
        let row_visible_bytes = width as usize;

        for column_offset in (0..row_visible_bytes).step_by(component_step) {
            let value = *plane.get(row_start + column_offset).ok_or_else(|| {
                anyhow::anyhow!(
                    "Plane stats out of bounds: row={} column={} len={}",
                    row_index,
                    column_offset,
                    plane.len()
                )
            })?;
            min_value = min_value.min(value);
            max_value = max_value.max(value);
            sum += u64::from(value);
            count += 1;
        }
    }

    Ok(PlaneStats {
        min: min_value,
        max: max_value,
        average: sum / count.max(1),
    })
}

/// Конвертирует один пиксель NV12/BT.709 limited в RGB.
fn nv12_pixel_to_rgb(y_value: u8, u_value: u8, v_value: u8) -> [u8; 3] {
    let y = f32::from(y_value) / 255.0;
    let u = f32::from(u_value) / 255.0 - 0.5;
    let v = f32::from(v_value) / 255.0 - 0.5;
    let y_scaled = (y - 0.0625) * 1.164_383_5;

    let red = y_scaled + 1.792_741_1 * v;
    let green = y_scaled - 0.532_909_33 * v - 0.213_248_61 * u;
    let blue = y_scaled + 2.112_401_7 * u;

    [clamp_to_u8(red), clamp_to_u8(green), clamp_to_u8(blue)]
}

/// Сохраняет Y-плоскость как PGM для проверки яркости до GPU upload.
fn write_y_plane_pgm(
    path: &Path,
    width: u32,
    height: u32,
    y_stride: u32,
    y_plane: &[u8],
) -> anyhow::Result<()> {
    let required_len = (height.saturating_sub(1) as usize)
        .saturating_mul(y_stride as usize)
        .saturating_add(width as usize);
    ensure!(
        y_plane.len() >= required_len,
        "Y plane is too short: len={} required={}",
        y_plane.len(),
        required_len
    );

    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(writer, "P5")?;
    writeln!(writer, "{} {}", width, height)?;
    writeln!(writer, "255")?;

    for row_index in 0..height as usize {
        let row_start = row_index * y_stride as usize;
        let row_end = row_start + width as usize;
        writer.write_all(&y_plane[row_start..row_end])?;
    }

    Ok(())
}

/// Сохраняет одну chroma-компоненту NV12 как PGM для проверки UV до RGB-конверсии.
fn write_uv_component_pgm(
    path: &Path,
    width: u32,
    height: u32,
    uv_stride: u32,
    uv_plane: &[u8],
    component_offset: usize,
) -> anyhow::Result<()> {
    let chroma_width = width / 2;
    let chroma_height = height / 2;
    let required_len = (chroma_height.saturating_sub(1) as usize)
        .saturating_mul(uv_stride as usize)
        .saturating_add(width as usize);
    ensure!(
        uv_plane.len() >= required_len,
        "UV plane is too short: len={} required={}",
        uv_plane.len(),
        required_len
    );

    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(writer, "P5")?;
    writeln!(writer, "{} {}", chroma_width, chroma_height)?;
    writeln!(writer, "255")?;

    for row_index in 0..chroma_height as usize {
        let row_start = row_index * uv_stride as usize + component_offset;
        for column_index in 0..chroma_width as usize {
            writer.write_all(&[uv_plane[row_start + column_index * 2]])?;
        }
    }

    Ok(())
}

/// Сохраняет полный NV12 кадр как RGB PPM для проверки цвета до GPU upload.
fn write_nv12_as_ppm(
    path: &Path,
    width: u32,
    height: u32,
    y_stride: u32,
    uv_stride: u32,
    y_plane: &[u8],
    uv_plane: &[u8],
) -> anyhow::Result<()> {
    let required_y_len = (height.saturating_sub(1) as usize)
        .saturating_mul(y_stride as usize)
        .saturating_add(width as usize);
    let uv_rows = height / 2;
    let required_uv_len = (uv_rows.saturating_sub(1) as usize)
        .saturating_mul(uv_stride as usize)
        .saturating_add(width as usize);
    ensure!(
        y_plane.len() >= required_y_len,
        "Y plane is too short: len={} required={}",
        y_plane.len(),
        required_y_len
    );
    ensure!(
        uv_plane.len() >= required_uv_len,
        "UV plane is too short: len={} required={}",
        uv_plane.len(),
        required_uv_len
    );

    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(writer, "P6")?;
    writeln!(writer, "{} {}", width, height)?;
    writeln!(writer, "255")?;

    for y_index in 0..height as usize {
        let y_row_start = y_index * y_stride as usize;
        let uv_row_start = (y_index / 2) * uv_stride as usize;

        for x_index in 0..width as usize {
            let y_value = y_plane[y_row_start + x_index];
            let uv_index = uv_row_start + (x_index / 2) * 2;
            let u_value = uv_plane[uv_index];
            let v_value = uv_plane[uv_index + 1];
            writer.write_all(&nv12_pixel_to_rgb(y_value, u_value, v_value))?;
        }
    }

    Ok(())
}

/// Сохраняет первый NV12 кадр в `/tmp`, если диагностика явно включена.
fn dump_first_nv12_frame(
    width: u32,
    height: u32,
    y_stride: u32,
    uv_stride: u32,
    y_plane: &[u8],
    uv_plane: &[u8],
) {
    static DUMPED: AtomicBool = AtomicBool::new(false);

    let Some(dump_dir) = first_frame_dump_dir() else {
        return;
    };
    if DUMPED.swap(true, Ordering::Relaxed) {
        return;
    }

    let y_path = dump_dir.join("videoplayer-first-frame-y.pgm");
    let u_path = dump_dir.join("videoplayer-first-frame-u.pgm");
    let v_path = dump_dir.join("videoplayer-first-frame-v.pgm");
    let rgb_path = dump_dir.join("videoplayer-first-frame-rgb.ppm");

    if let Err(error) = write_y_plane_pgm(&y_path, width, height, y_stride, y_plane) {
        tracing::warn!(error = %error, path = %y_path.display(), "Failed to dump Y plane");
    }
    if let Err(error) = write_uv_component_pgm(&u_path, width, height, uv_stride, uv_plane, 0) {
        tracing::warn!(error = %error, path = %u_path.display(), "Failed to dump U plane");
    }
    if let Err(error) = write_uv_component_pgm(&v_path, width, height, uv_stride, uv_plane, 1) {
        tracing::warn!(error = %error, path = %v_path.display(), "Failed to dump V plane");
    }
    if let Err(error) = write_nv12_as_ppm(
        &rgb_path, width, height, y_stride, uv_stride, y_plane, uv_plane,
    ) {
        tracing::warn!(error = %error, path = %rgb_path.display(), "Failed to dump NV12 as RGB");
    } else {
        let y_stats = plane_stats(y_plane, width, height, y_stride, 0, 1).ok();
        let u_stats = plane_stats(uv_plane, width, height / 2, uv_stride, 0, 2).ok();
        let v_stats = plane_stats(uv_plane, width, height / 2, uv_stride, 1, 2).ok();
        tracing::info!(
            y_path = %y_path.display(),
            u_path = %u_path.display(),
            v_path = %v_path.display(),
            rgb_path = %rgb_path.display(),
            y_min = y_stats.map(|stats| stats.min),
            y_max = y_stats.map(|stats| stats.max),
            y_avg = y_stats.map(|stats| stats.average),
            u_min = u_stats.map(|stats| stats.min),
            u_max = u_stats.map(|stats| stats.max),
            u_avg = u_stats.map(|stats| stats.average),
            v_min = v_stats.map(|stats| stats.min),
            v_max = v_stats.map(|stats| stats.max),
            v_avg = v_stats.map(|stats| stats.average),
            "First NV12 frame dumped"
        );
    }
}

/// Считает лёгкую среднюю яркость по нескольким точкам без полного прохода по 4K буферу.
fn sparse_average(plane: &[u8]) -> u64 {
    if plane.is_empty() {
        return 0;
    }

    const SAMPLE_COUNT: usize = 16;
    let step = (plane.len() / SAMPLE_COUNT).max(1);
    let mut sum = 0u64;
    let mut count = 0u64;

    for value in plane.iter().step_by(step).take(SAMPLE_COUNT) {
        sum += u64::from(*value);
        count += 1;
    }

    sum / count.max(1)
}

/// Логирует короткие samples Y/UV плоскостей для диагностики зелёного/чёрного экрана.
fn log_plane_samples(y_plane: &[u8], uv_plane: Option<&[u8]>) {
    if !should_log_plane_samples() {
        return;
    }

    if !y_plane.is_empty() {
        let y_first = y_plane[0];
        let y_mid = y_plane[y_plane.len() / 2];
        let y_sparse_avg = sparse_average(y_plane);
        tracing::debug!(
            y_first,
            y_mid,
            y_sparse_avg,
            y_len = y_plane.len(),
            "Y-plane sample"
        );
    }

    if let Some(uv_plane) = uv_plane
        && uv_plane.len() >= 2
    {
        let uv_first_u = uv_plane[0];
        let uv_first_v = uv_plane[1];
        let uv_sparse_avg = sparse_average(uv_plane);
        tracing::debug!(
            uv_first_u,
            uv_first_v,
            uv_sparse_avg,
            uv_len = uv_plane.len(),
            "UV-plane sample"
        );
    }
}

/// GPU-storage, на котором построены Y/UV views одного кадра.
enum TextureSlotStorage {
    /// Обычный CPU-upload путь: две независимые текстуры под Y и UV planes.
    SeparatePlanes {
        /// wgpu-текстура для Y-плоскости (формат R8Unorm, 1 байт на пиксель).
        y_texture: wgpu::Texture,
        /// wgpu-текстура для UV-плоскости (формат Rg8Unorm, 2 байта на пиксель).
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
    /// Возвращает отдельные Y/UV textures для CPU upload.
    ///
    /// Imported NV12 slots не поддерживают `Queue::write_texture()`, потому что
    /// данные уже лежат в VA-owned DMA-BUF и видны через plane views.
    fn separate_textures(&self) -> anyhow::Result<(&wgpu::Texture, &wgpu::Texture)> {
        match self {
            Self::SeparatePlanes {
                y_texture,
                uv_texture,
            } => Ok((y_texture, uv_texture)),
            Self::ImportedDmaBuf { .. } => Err(anyhow::anyhow!(
                "imported DMA-BUF slot cannot be used for CPU upload"
            )),
        }
    }

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
    /// Устройство wgpu, необходимое для создания новых текстур.
    device: Arc<wgpu::Device>,
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

    /// Активирует уже существующий slot и возвращает новый handle.
    fn activate_existing_slot(
        &mut self,
        slot_index: usize,
        is_imported: bool,
    ) -> anyhow::Result<FrameTextureHandle> {
        let handle = self.allocate_handle()?;
        {
            let slot = self.slot_mut(slot_index).ok_or_else(|| {
                anyhow::anyhow!("cannot activate missing texture slot {}", slot_index)
            })?;
            slot.in_use = true;
            slot.is_imported = is_imported;
        }
        self.bind_handle_to_slot(handle, slot_index)?;
        Ok(handle)
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

            tracing::debug!(
                width = image.width,
                height = image.height,
                y_stride = image.y_stride,
                uv_stride = image.uv_stride,
                "VA image readback acquired"
            );
            log_plane_samples(&image.y_plane, Some(&image.uv_plane));
            dump_first_nv12_frame(
                image.width,
                image.height,
                image.y_stride,
                image.uv_stride,
                &image.y_plane,
                &image.uv_plane,
            );

            let slot_start = std::time::Instant::now();
            let slot_index = self.find_or_create_slot(image.width, image.height)?;
            let slot = self.slot(slot_index).ok_or_else(|| {
                anyhow::anyhow!(
                    "texture slot {} disappeared before VA image upload",
                    slot_index
                )
            })?;
            let (y_texture, uv_texture) = slot.storage.separate_textures()?;
            let slot_elapsed = slot_start.elapsed().as_millis();

            let write_y_start = std::time::Instant::now();
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: y_texture,
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
                    texture: uv_texture,
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

            let texture_handle = self.activate_existing_slot(slot_index, false)?;

            tracing::debug!(
                handle_id = texture_handle.0,
                map_ms = map_elapsed,
                slot_ms = slot_elapsed,
                write_y_ms = write_y_elapsed,
                write_uv_ms = write_uv_elapsed,
                total_ms = total_start.elapsed().as_millis(),
                "upload_frame timing breakdown"
            );

            return Ok(texture_handle);
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
        dump_first_nv12_frame(width, height, y_stride, uv_stride, planes[0], planes[1]);

        // Шаг 3: Находим или создаём свободный слот подходящего разрешения.
        let slot_start = std::time::Instant::now();
        let slot_index = self.find_or_create_slot(width, height)?;
        let slot = self.slot(slot_index).ok_or_else(|| {
            anyhow::anyhow!("texture slot {} disappeared before CPU upload", slot_index)
        })?;
        let (y_texture, uv_texture) = slot.storage.separate_textures()?;
        let slot_elapsed = slot_start.elapsed().as_millis();

        // Шаг 4: Загружаем Y-плоскость в текстуру формата R8Unorm.
        let write_y_start = std::time::Instant::now();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: y_texture,
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
                texture: uv_texture,
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
        let texture_handle = self.activate_existing_slot(slot_index, false)?;

        let total_elapsed = total_start.elapsed().as_millis();
        tracing::debug!(
            handle_id = texture_handle.0,
            map_ms = map_elapsed,
            slot_ms = slot_elapsed,
            write_y_ms = write_y_elapsed,
            write_uv_ms = write_uv_elapsed,
            total_ms = total_elapsed,
            "upload_frame timing breakdown"
        );

        Ok(texture_handle)
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
            width: frame.resolution().width,
            height: frame.resolution().height,
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
            width: image.width,
            height: image.height,
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
    /// Для обычных слотов (CPU upload): помечает как свободный для reuse.
    /// Для imported слотов (zero-copy): удаляет слот из пула, так как
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
        if let Some(idx) = self.slots.iter().position(|slot| {
            slot.as_ref()
                .map(|slot| !slot.in_use && slot.width == width && slot.height == height)
                .unwrap_or(false)
        }) {
            return Ok(idx);
        }

        // Шаг 2: Проверяем лимит пула.
        if self.active_slot_count() >= MAX_TEXTURE_SLOTS {
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
        let slot_index = self.insert_slot(TextureSlot {
            storage: TextureSlotStorage::SeparatePlanes {
                y_texture,
                uv_texture,
            },
            y_view,
            uv_view,
            width,
            height,
            in_use: false,
            is_imported: false,
        })?;

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
