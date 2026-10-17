//! GPU-pass субтитров между video и egui.
//!
//! # Нейтральный контракт (его же реализует будущий GLES2-рендерер, issue #6)
//!
//! Вход — `SubtitleOverlay` (список масок покрытия с цветом и позицией на холсте) +
//! прямоугольник картинки видео на экране (`PictureRect`) + размер экрана. Правила:
//! - левый верхний угол холста лежит в левом верхнем углу картинки видео плюс сдвиг слоя
//!   (`SubtitleLayerOffsets`, пиксели surface, x вправо, y вниз), сумма округляется до
//!   целого пикселя surface; слой может выходить за картинку (на чёрные полосы) — рисуется
//!   всё, что попало в видимую область видео;
//! - у каждой группы размещения (`OverlayPlacementGroup`) свой сдвиг: основной диалог можно
//!   поднять, а надписи оставить на месте; без разделения обе группы едут одним сдвигом;
//! - 1 пиксель холста = 1 пиксель surface (вызывающий растеризует холст размером
//!   округлённой картинки), поэтому масштабирования и фильтрации нет;
//! - картинки рисуются строго в порядке списка (первая — самая нижняя), смешивание «over»;
//! - рисуется только внутри видимой области видео (sidebar и прочий UI не затираются).
//!
//! # Владение состоянием
//!
//! Модуль владеет атласом масок, instance buffer-ом, uniform-ом и учётом «что уже
//! загружено». Снаружи виден только [`SubtitleOverlayRenderer::render`]: shell не знает ни
//! про атлас, ни про ревизии.
//!
//! # Загрузка только при изменении
//!
//! Загруженный overlay опознаётся по паре (ревизия, `Arc` каждой маски). Ревизия одна не
//! годится: после пересоздания растеризатора она начинается заново и совпала бы с чужим
//! содержимым; `Arc::ptr_eq` по маскам (мы держим клоны, адрес не переиспользуется) это
//! исключает. Тот же overlay на следующем кадре — ноль `write_texture`/`write_buffer`
//! инстансов и ни одной аллокации, только draw. Сдвиг картинки видео меняет лишь uniform
//! (32 байта); то же верно для сдвигов групп слоя (`layer_offsets`).

mod atlas;
mod instances;
mod pipeline;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use render_core::{PictureRect, RenderViewport};
use subtitle_raster::{OverlayRevision, SubtitleOverlay};

use self::atlas::{AtlasLayout, AtlasPackError, MaskExtent, pack_masks};
use self::instances::{BitmapInstance, INSTANCE_STRIDE_BYTES, placement_origins};
pub use self::instances::{SubtitleLayerOffset, SubtitleLayerOffsets};
use self::pipeline::{
    AtlasGpu, OverlayPipeline, UNIFORM_SIZE_BYTES, create_instance_buffer, create_uniform_buffer,
};

/// Минимальная ёмкость instance buffer-а (в инстансах), чтобы не пересоздавать его ради мелочей.
const MIN_INSTANCE_CAPACITY: u64 = 64;

/// Субтитры одного кадра: растеризованный overlay и где на экране лежит картинка видео.
#[derive(Debug, Clone, Copy)]
pub struct SubtitleOverlayFrame<'frame> {
    /// Результат растеризатора; холст = округлённый размер `picture_rect`.
    pub overlay: &'frame SubtitleOverlay,
    /// Картинка видео без чёрных полос в пикселях surface; её округлённый левый верхний
    /// угол — точка, куда ложится (0, 0) холста.
    pub picture_rect: PictureRect,
    /// Сдвиги групп слоя относительно `picture_rect` в пикселях surface (позиция по выбору
    /// пользователя, подъём над панелью управления). Меняется только uniform.
    /// `SubtitleLayerOffsets::uniform` двигает слой целиком.
    pub layer_offsets: SubtitleLayerOffsets,
}

/// Счётчики загрузок для тестов и диагностики.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubtitleOverlayUploadStats {
    /// Сколько ревизий overlay-а загружено в атлас (одна загрузка = все маски ревизии).
    pub atlas_uploads: u64,
    /// Сколько отдельных масок записано через `write_texture`.
    pub bitmap_uploads: u64,
    /// Сколько раз создавалась текстура атласа (первое создание тоже считается).
    pub atlas_recreations: u64,
    /// Сколько ревизий пропущено из-за того, что маски не влезли в лимит текстуры GPU.
    pub skipped_overlays_too_large: u64,
    /// Сколько раз записывался uniform (смена размера экрана или позиции картинки).
    pub uniform_uploads: u64,
}

/// Что сделал `render` (различаем причины, чтобы тесты и логи не гадали).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleOverlayOutcome {
    /// Рисовать нечего: нет входа, пустой overlay, пустая видимая область или битый rect.
    /// Render pass не открывался.
    NothingToDraw,
    /// Маски не влезли в атлас; overlay на этой ревизии не рисуется.
    SkippedTooLarge,
    /// Pass записан, нарисовано `bitmaps` картинок.
    Drawn { bitmaps: usize },
}

/// Куда и с какими границами рисовать (чтобы не раздувать сигнатуру позиционными аргументами).
pub struct SubtitleOverlayTarget<'a> {
    /// Устройство, которому принадлежат ресурсы рендерера.
    pub device: &'a wgpu::Device,
    /// Очередь для загрузки масок, инстансов и uniform-а.
    pub queue: &'a wgpu::Queue,
    /// Encoder, в который записывается pass (видео уже записано в него раньше).
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// Целевая текстура; pass идёт с `LoadOp::Load`, поэтому видео под субтитрами сохраняется.
    pub target: &'a wgpu::TextureView,
    /// Размер surface в пикселях.
    pub screen_size: [u32; 2],
    /// Область, за которую субтитры не выходят (зажатый video viewport).
    pub visible_area: RenderViewport,
}

/// Результат попытки загрузки overlay-а.
#[derive(Debug, Clone, Copy)]
enum UploadedDraw {
    Ready { instance_count: u32 },
    TooLarge,
}

/// Опознавательные признаки загруженного overlay-а и итог его загрузки.
struct UploadedOverlay {
    revision: OverlayRevision,
    /// Клоны `Arc` масок: удерживают адреса, поэтому `ptr_eq` надёжно отличает содержимое.
    masks: Vec<Arc<[u8]>>,
    draw: UploadedDraw,
}

impl UploadedOverlay {
    /// Тот же ли это overlay (без аллокаций).
    fn is_same_as(&self, overlay: &SubtitleOverlay) -> bool {
        self.revision == overlay.revision()
            && self.masks.len() == overlay.bitmaps().len()
            && self
                .masks
                .iter()
                .zip(overlay.bitmaps())
                .all(|(mask, bitmap)| Arc::ptr_eq(mask, bitmap.shared_coverage()))
    }
}

/// Рендерер субтитров; создаётся один раз под формат surface.
///
/// Это переиспользуемая wgpu-реализация нейтрального контракта (см. документацию модуля):
/// `Renderer` держит её внутри, а приложение и тесты могут создать собственный экземпляр
/// на любом `wgpu::Device` (например, для readback-теста всей цепочки до пикселей).
pub struct SubtitleOverlayRenderer {
    pipeline: OverlayPipeline,
    uniform_buffer: wgpu::Buffer,
    atlas: Option<AtlasGpu>,
    /// Instance buffer и его ёмкость в инстансах.
    instance_buffer: Option<(wgpu::Buffer, u64)>,
    uploaded: Option<UploadedOverlay>,
    /// Последние записанные `[screen_w, screen_h, main_x, main_y, fixed_x, fixed_y, 0, 0]`.
    last_uniform_values: Option<[f32; 8]>,
    /// Переиспользуемый буфер байтов инстансов (растёт, не освобождается).
    instance_bytes: Vec<u8>,
    stats: SubtitleOverlayUploadStats,
}

impl SubtitleOverlayRenderer {
    /// Создаёт pipeline и uniform; атлас появится при первой загрузке.
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        Self {
            pipeline: OverlayPipeline::new(device, target_format),
            uniform_buffer: create_uniform_buffer(device),
            atlas: None,
            instance_buffer: None,
            uploaded: None,
            last_uniform_values: None,
            instance_bytes: Vec::new(),
            stats: SubtitleOverlayUploadStats::default(),
        }
    }

    /// Снимок счётчиков загрузок.
    pub fn upload_stats(&self) -> SubtitleOverlayUploadStats {
        self.stats
    }

    /// Рисует субтитры поверх уже записанного видео; `LoadOp::Load`, без очистки.
    pub fn render(
        &mut self,
        target: SubtitleOverlayTarget<'_>,
        frame: Option<&SubtitleOverlayFrame<'_>>,
    ) -> SubtitleOverlayOutcome {
        let Some(frame) = frame else {
            return SubtitleOverlayOutcome::NothingToDraw;
        };
        let bitmap_count = frame.overlay.bitmaps().len();
        let screen_area =
            RenderViewport::full_surface(target.screen_size[0], target.screen_size[1]);
        let (Some(origins), Some(scissor)) = (
            placement_origins(frame.picture_rect, frame.layer_offsets),
            target.visible_area.intersection(screen_area),
        ) else {
            return SubtitleOverlayOutcome::NothingToDraw;
        };
        if bitmap_count == 0 || scissor.is_empty() {
            return SubtitleOverlayOutcome::NothingToDraw;
        }

        self.ensure_uploaded(target.device, target.queue, frame.overlay);
        let Some(UploadedOverlay {
            draw: UploadedDraw::Ready { instance_count },
            ..
        }) = &self.uploaded
        else {
            return SubtitleOverlayOutcome::SkippedTooLarge;
        };
        let instance_count = *instance_count;
        self.write_uniform_if_changed(
            target.queue,
            [
                target.screen_size[0] as f32,
                target.screen_size[1] as f32,
                origins[0][0],
                origins[0][1],
                origins[1][0],
                origins[1][1],
                0.0,
                0.0,
            ],
        );

        // Ресурсы гарантированы `ensure_uploaded` при `Ready`; отсутствие — нарушение
        // инварианта, которое безопаснее пропустить, чем паниковать в кадре.
        let (Some(atlas), Some((instance_buffer, _))) = (&self.atlas, &self.instance_buffer) else {
            tracing::error!("Subtitle overlay is marked ready, but GPU resources are missing");
            return SubtitleOverlayOutcome::NothingToDraw;
        };
        let mut render_pass = target
            .encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("subtitle overlay pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        render_pass.set_pipeline(&self.pipeline.pipeline);
        render_pass.set_bind_group(0, &atlas.bind_group, &[]);
        render_pass.set_vertex_buffer(0, instance_buffer.slice(..));
        render_pass.set_scissor_rect(scissor.x, scissor.y, scissor.width, scissor.height);
        // 4 вершины triangle strip на каждый инстанс.
        render_pass.draw(0..4, 0..instance_count);
        SubtitleOverlayOutcome::Drawn {
            bitmaps: bitmap_count,
        }
    }

    /// Загружает overlay, если это не тот же, что загружен ранее.
    fn ensure_uploaded(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        overlay: &SubtitleOverlay,
    ) {
        if self
            .uploaded
            .as_ref()
            .is_some_and(|uploaded| uploaded.is_same_as(overlay))
        {
            return;
        }
        let masks: Vec<Arc<[u8]>> = overlay
            .bitmaps()
            .iter()
            .map(|bitmap| Arc::clone(bitmap.shared_coverage()))
            .collect();
        let extents: Vec<MaskExtent> = overlay
            .bitmaps()
            .iter()
            .map(|bitmap| MaskExtent {
                width: bitmap.width(),
                height: bitmap.height(),
            })
            .collect();
        let current_side = self.atlas.as_ref().map_or(0, |atlas| atlas.side);
        let max_side = device.limits().max_texture_dimension_2d;

        let draw = match pack_masks(&extents, current_side, max_side) {
            Ok(layout) => self.upload_layout(device, queue, overlay, &layout),
            Err(AtlasPackError::DoesNotFit { max_side }) => {
                self.stats.skipped_overlays_too_large += 1;
                // Текст реплик в лог не пишем: только метаданные.
                tracing::warn!(
                    revision = overlay.revision().value(),
                    bitmaps = overlay.bitmaps().len(),
                    max_texture_side = max_side,
                    "Subtitle overlay does not fit the GPU atlas; overlay is skipped"
                );
                UploadedDraw::TooLarge
            }
        };
        self.uploaded = Some(UploadedOverlay {
            revision: overlay.revision(),
            masks,
            draw,
        });
    }

    /// Заливает маски по слотам раскладки и пишет инстансы.
    fn upload_layout(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        overlay: &SubtitleOverlay,
        layout: &AtlasLayout,
    ) -> UploadedDraw {
        self.ensure_atlas(device, layout.side);
        let Some(atlas) = &self.atlas else {
            return UploadedDraw::TooLarge;
        };
        for (bitmap, slot) in overlay.bitmaps().iter().zip(&layout.slots) {
            // write_texture не требует выравнивания строк по 256 байт, поэтому маска с
            // её `stride` уходит как есть, без промежуточной копии.
            queue.write_texture(
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
                bitmap.coverage(),
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
        self.stats.bitmap_uploads += overlay.bitmaps().len() as u64;
        self.stats.atlas_uploads += 1;

        // Инстансы строим в порядке списка: порядок рисования = порядок картинок.
        self.instance_bytes.clear();
        for (bitmap, slot) in overlay.bitmaps().iter().zip(&layout.slots) {
            BitmapInstance::from_bitmap(bitmap, *slot).append_bytes(&mut self.instance_bytes);
        }
        let instance_count = overlay.bitmaps().len();
        self.ensure_instance_capacity(device, instance_count as u64);
        if let Some((buffer, _)) = &self.instance_buffer {
            queue.write_buffer(buffer, 0, &self.instance_bytes);
        }
        debug_assert_eq!(
            self.instance_bytes.len(),
            instance_count * INSTANCE_STRIDE_BYTES
        );
        match u32::try_from(instance_count) {
            Ok(instance_count) => UploadedDraw::Ready { instance_count },
            Err(_) => UploadedDraw::TooLarge,
        }
    }

    /// Создаёт атлас нужной стороны, если текущего нет или он меньше.
    fn ensure_atlas(&mut self, device: &wgpu::Device, side: u32) {
        if self.atlas.as_ref().is_some_and(|atlas| atlas.side == side) {
            return;
        }
        self.atlas = Some(AtlasGpu::new(
            device,
            &self.pipeline.bind_group_layout,
            &self.uniform_buffer,
            side,
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

    /// Пишет uniform только если размер экрана или позиция картинки изменились.
    fn write_uniform_if_changed(&mut self, queue: &wgpu::Queue, values: [f32; 8]) {
        if self.last_uniform_values == Some(values) {
            return;
        }
        let mut bytes = [0_u8; UNIFORM_SIZE_BYTES as usize];
        let (chunks, _) = bytes.as_chunks_mut::<4>();
        for (chunk, value) in chunks.iter_mut().zip(values) {
            *chunk = value.to_ne_bytes();
        }
        queue.write_buffer(&self.uniform_buffer, 0, &bytes);
        self.last_uniform_values = Some(values);
        self.stats.uniform_uploads += 1;
    }
}
