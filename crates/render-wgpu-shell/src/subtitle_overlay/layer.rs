//! Слой GPU-ресурсов overlay-а: атлас одного формата пикселей, его pipeline и instance buffer.
//!
//! Слой владеет тремя вещами: какие картинки сейчас лежат в атласе (`resident`), куда именно
//! (`resident_slots`) и сколько загрузок он сделал (`LayerStats`). Рендерер знает только
//! «подготовь слой под этот список» и «дай ресурсы для draw»: ни форматов, ни раскладки атласа
//! он не видит. Тот же код обслуживает маски текста (R8) и RGBA-картинки (RGBA8): отличаются
//! только [`LayerKind`] (формат текстуры и шейдер) и источник байтов пикселей.

use subtitle_raster::SubtitleBitmap;

use super::atlas::{AtlasPackError, AtlasSlot, MaskExtent, pack_masks};
use super::instances::{BitmapInstance, INSTANCE_STRIDE_BYTES};
use super::pipeline::{AtlasGpu, LayerKind, OverlayPipeline, create_instance_buffer};

/// Минимальная ёмкость instance buffer-а (в инстансах), чтобы не пересоздавать его ради мелочей.
const MIN_INSTANCE_CAPACITY: u64 = 64;

/// Счётчики слоя (рендерер собирает из них публичные `SubtitleOverlayUploadStats`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct LayerStats {
    /// Сколько раз набор картинок слоя записан в атлас.
    pub(super) atlas_uploads: u64,
    /// Сколько картинок записано через `write_texture`.
    pub(super) bitmap_uploads: u64,
    /// Сколько раз создавалась текстура атласа.
    pub(super) atlas_recreations: u64,
    /// Сколько подготовок слоя потеряли хотя бы одну картинку из-за лимита текстуры GPU.
    pub(super) skipped_prepares: u64,
    /// Сколько картинок суммарно не нарисовано из-за лимита текстуры GPU.
    pub(super) skipped_bitmaps: u64,
}

/// Устройство, очередь и общий uniform: всё, что нужно слою для загрузки.
pub(super) struct LayerGpuContext<'a> {
    pub(super) device: &'a wgpu::Device,
    pub(super) queue: &'a wgpu::Queue,
    pub(super) uniform_buffer: &'a wgpu::Buffer,
}

/// Итог подготовки слоя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayerOutcome {
    /// Слой готов к рисованию `instance_count` инстансов (0 — слою нечего рисовать).
    Ready { instance_count: usize },
    /// Картинки слоя не влезли в атлас максимального размера; слой в этот раз не рисуется.
    TooLarge,
}

/// Ресурсы одного draw-вызова серии.
pub(super) struct LayerDrawResources<'a> {
    pub(super) pipeline: &'a wgpu::RenderPipeline,
    pub(super) bind_group: &'a wgpu::BindGroup,
    pub(super) instances: &'a wgpu::Buffer,
}

/// Атлас + pipeline + instance buffer одного вида пикселей.
pub(super) struct BitmapLayer {
    kind: LayerKind,
    pipeline: OverlayPipeline,
    atlas: Option<AtlasGpu>,
    /// Instance buffer и его ёмкость в инстансах.
    instance_buffer: Option<(wgpu::Buffer, u64)>,
    /// Переиспользуемый буфер байтов инстансов (растёт, не освобождается).
    instance_bytes: Vec<u8>,
    /// Клоны картинок, чьи пиксели сейчас лежат в атласе (в порядке слотов). Клоны держат
    /// буферы пикселей, поэтому адрес не переиспользуется и `shows_same_pixels_as` надёжен.
    resident: Vec<SubtitleBitmap>,
    /// Слоты `resident` в атласе.
    resident_slots: Vec<AtlasSlot>,
    stats: LayerStats,
}

impl BitmapLayer {
    /// Создаёт pipeline слоя; атлас и instance buffer появятся при первой загрузке.
    pub(super) fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        kind: LayerKind,
    ) -> Self {
        Self {
            kind,
            pipeline: OverlayPipeline::new(device, target_format, kind),
            atlas: None,
            instance_buffer: None,
            instance_bytes: Vec::new(),
            resident: Vec::new(),
            resident_slots: Vec::new(),
            stats: LayerStats::default(),
        }
    }

    pub(super) fn stats(&self) -> LayerStats {
        self.stats
    }

    /// Готовит слой к рисованию `bitmaps` (в порядке списка; все уже влезают по размеру).
    ///
    /// Пиксели пишутся в атлас только если набор отличается от уже лежащего там: смена только
    /// геометрии/цвета или только соседнего слоя пиксели не трогает. Инстансы пересобираются
    /// всегда (это десятки байт). `dropped_by_size` — сколько картинок слоя отброшено
    /// раньше из-за размера; нужно только для учёта.
    pub(super) fn prepare(
        &mut self,
        gpu: &LayerGpuContext<'_>,
        bitmaps: &[&SubtitleBitmap],
        dropped_by_size: usize,
    ) -> LayerOutcome {
        let mut lost_bitmaps = dropped_by_size;
        let outcome = if bitmaps.is_empty() {
            // Нечего рисовать: освобождаем удерживаемые буферы пикселей, атлас оставляем.
            self.forget_residents();
            LayerOutcome::Ready { instance_count: 0 }
        } else {
            match self.ensure_resident(gpu, bitmaps) {
                Ok(()) => self.write_instances(gpu, bitmaps),
                Err(AtlasPackError::DoesNotFit { max_side }) => {
                    self.forget_residents();
                    lost_bitmaps += bitmaps.len();
                    // Текст реплик в лог не пишем: только метаданные.
                    tracing::warn!(
                        layer = self.kind.label(),
                        bitmaps = bitmaps.len(),
                        max_texture_side = max_side,
                        "Subtitle bitmaps do not fit the GPU atlas; layer is skipped"
                    );
                    LayerOutcome::TooLarge
                }
            }
        };
        if lost_bitmaps > 0 {
            self.stats.skipped_prepares += 1;
            self.stats.skipped_bitmaps += lost_bitmaps as u64;
        }
        outcome
    }

    /// Ресурсы для draw; `None`, если слой ещё ничего не загружал.
    pub(super) fn draw_resources(&self) -> Option<LayerDrawResources<'_>> {
        let atlas = self.atlas.as_ref()?;
        let (instances, _) = self.instance_buffer.as_ref()?;
        Some(LayerDrawResources {
            pipeline: &self.pipeline.pipeline,
            bind_group: &atlas.bind_group,
            instances,
        })
    }

    fn forget_residents(&mut self) {
        self.resident.clear();
        self.resident_slots.clear();
    }

    /// Те же ли пиксели уже лежат в атласе (без аллокаций).
    fn is_resident(&self, bitmaps: &[&SubtitleBitmap]) -> bool {
        self.atlas.is_some()
            && self.resident.len() == bitmaps.len()
            && self
                .resident
                .iter()
                .zip(bitmaps)
                .all(|(resident, bitmap)| resident.shows_same_pixels_as(bitmap))
    }

    /// Гарантирует, что пиксели `bitmaps` лежат в атласе по слотам `resident_slots`.
    fn ensure_resident(
        &mut self,
        gpu: &LayerGpuContext<'_>,
        bitmaps: &[&SubtitleBitmap],
    ) -> Result<(), AtlasPackError> {
        if self.is_resident(bitmaps) {
            return Ok(());
        }
        let extents: Vec<MaskExtent> = bitmaps
            .iter()
            .map(|bitmap| MaskExtent {
                width: bitmap.width(),
                height: bitmap.height(),
            })
            .collect();
        let current_side = self.atlas.as_ref().map_or(0, |atlas| atlas.side);
        let max_side = gpu.device.limits().max_texture_dimension_2d;
        let layout = pack_masks(&extents, current_side, max_side)?;
        self.ensure_atlas(gpu, layout.side);
        let Some(atlas) = &self.atlas else {
            return Err(AtlasPackError::DoesNotFit { max_side });
        };
        for (bitmap, slot) in bitmaps.iter().zip(&layout.slots) {
            let Some(pixels) = self.kind.pixels_of(bitmap) else {
                // Недостижимо: слой выбирается по формату пикселей картинки.
                tracing::error!(
                    layer = self.kind.label(),
                    "Bitmap has no pixels for this layer"
                );
                continue;
            };
            // write_texture не требует выравнивания строк по 256 байт, поэтому картинка с её
            // `stride` (в байтах) уходит как есть, без промежуточной копии.
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &atlas.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: slot.x,
                        y: slot.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: u32::try_from(bitmap.stride()).ok(),
                    rows_per_image: Some(bitmap.height()),
                },
                wgpu::Extent3d {
                    width: bitmap.width(),
                    height: bitmap.height(),
                    depth_or_array_layers: 1,
                },
            );
        }
        self.stats.bitmap_uploads += bitmaps.len() as u64;
        self.stats.atlas_uploads += 1;
        self.resident = bitmaps.iter().map(|bitmap| (*bitmap).clone()).collect();
        self.resident_slots = layout.slots;
        Ok(())
    }

    /// Пишет инстансы в порядке списка: порядок рисования внутри слоя = порядок картинок.
    fn write_instances(
        &mut self,
        gpu: &LayerGpuContext<'_>,
        bitmaps: &[&SubtitleBitmap],
    ) -> LayerOutcome {
        if u32::try_from(bitmaps.len()).is_err() {
            self.forget_residents();
            return LayerOutcome::TooLarge;
        }
        self.instance_bytes.clear();
        for (bitmap, slot) in bitmaps.iter().zip(&self.resident_slots) {
            BitmapInstance::from_bitmap(bitmap, *slot).append_bytes(&mut self.instance_bytes);
        }
        debug_assert_eq!(
            self.instance_bytes.len(),
            bitmaps.len() * INSTANCE_STRIDE_BYTES
        );
        self.ensure_instance_capacity(gpu.device, bitmaps.len() as u64);
        if let Some((buffer, _)) = &self.instance_buffer {
            gpu.queue.write_buffer(buffer, 0, &self.instance_bytes);
        }
        LayerOutcome::Ready {
            instance_count: bitmaps.len(),
        }
    }

    /// Создаёт атлас нужной стороны, если текущего нет или он другой стороны.
    fn ensure_atlas(&mut self, gpu: &LayerGpuContext<'_>, side: u32) {
        if self.atlas.as_ref().is_some_and(|atlas| atlas.side == side) {
            return;
        }
        self.atlas = Some(AtlasGpu::new(
            gpu.device,
            &self.pipeline.bind_group_layout,
            gpu.uniform_buffer,
            side,
            self.kind,
        ));
        self.stats.atlas_recreations += 1;
    }

    /// Гарантирует ёмкость instance buffer-а; растёт степенями двойки.
    fn ensure_instance_capacity(&mut self, device: &wgpu::Device, needed_instances: u64) {
        if self
            .instance_buffer
            .as_ref()
            .is_some_and(|(_, capacity)| *capacity >= needed_instances)
        {
            return;
        }
        let capacity = needed_instances
            .next_power_of_two()
            .max(MIN_INSTANCE_CAPACITY);
        self.instance_buffer = Some((create_instance_buffer(device, capacity), capacity));
    }
}
