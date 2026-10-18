//! GPU-pass субтитров между video и egui.
//!
//! # Нейтральный контракт (его же реализует будущий GLES2-рендерер, issue #6)
//!
//! Вход — `SubtitleOverlay` (список картинок с позицией на холсте: маски покрытия текста с
//! цветом и полноцветные премультиплицированные RGBA-картинки PGS) +
//! прямоугольник картинки видео на экране (`PictureRect`) + размер экрана. Правила:
//! - левый верхний угол холста лежит в левом верхнем углу картинки видео плюс сдвиг слоя
//!   (`SubtitleLayerOffsets`, пиксели surface, x вправо, y вниз), сумма округляется до
//!   целого пикселя surface; слой может выходить за картинку (на чёрные полосы) — рисуется
//!   всё, что попало в видимую область видео;
//! - у каждой группы размещения (`OverlayPlacementGroup`) свой сдвиг: основной диалог можно
//!   поднять, а надписи оставить на месте; без разделения обе группы едут одним сдвигом;
//! - 1 пиксель холста = 1 пиксель surface (вызывающий растеризует холст размером
//!   округлённой картинки), поэтому масштабирования и фильтрации нет;
//! - картинки рисуются строго в порядке списка (первая — самая нижняя), смешивание «over»
//!   (`ONE, ONE_MINUS_SRC_ALPHA`, премультиплицированный источник) для обоих видов пикселей;
//! - маска покрытия: вклад `color.rgb * (coverage * color.a)`, альфа `coverage * color.a`;
//! - RGBA-картинка: вклад `texel * color.a` по всем четырём каналам (texel уже
//!   премультиплицирован, `color.rgb` игнорируется, `color.a` по умолчанию 1); хранится в
//!   `RGBA8` без sRGB, смешивание в гамма-пространстве, как у текста;
//! - картинка со стороной больше лимита текстуры GPU пропускается (учитывается в счётчиках),
//!   остальные рисуются;
//! - рисуется только внутри видимой области видео (sidebar и прочий UI не затираются).
//!
//! # Владение состоянием
//!
//! Модуль владеет двумя слоями (`layer.rs`): масками текста (R8) и RGBA-картинками (RGBA8).
//! Слой картинок со своим pipeline и атласом создаётся лениво, только когда в overlay-е
//! появилась первая картинка. Оба слоя рисуются одним render pass: подряд идущие картинки
//! одного вида — один draw-вызов (`runs.rs`), поэтому порядок списка соблюдается и при
//! чередовании текста с картинками. Модуль также владеет uniform-ом и учётом «что уже
//! загружено». Снаружи виден только [`SubtitleOverlayRenderer::render`]: shell не знает ни
//! про атлас, ни про ревизии.
//!
//! # Загрузка только при изменении
//!
//! Загруженный overlay опознаётся по паре (ревизия, буфер и окно каждой картинки). Если
//! изменился только текст или только картинки, пиксели второго слоя в атлас не пишутся
//! повторно (пересобираются лишь инстансы). Ревизия одна не
//! годится: после пересоздания растеризатора она начинается заново и совпала бы с чужим
//! содержимым; `SubtitleBitmap::shows_same_pixels_as` (мы держим клоны, адрес не переиспользуется) это
//! исключает. Тот же overlay на следующем кадре — ноль `write_texture`/`write_buffer`
//! инстансов и ни одной аллокации, только draw. Сдвиг картинки видео меняет лишь uniform
//! (32 байта); то же верно для сдвигов групп слоя (`layer_offsets`).

mod atlas;
mod instances;
mod layer;
mod pipeline;
mod runs;
#[cfg(test)]
mod tests;

use render_core::{PictureRect, RenderViewport};
use subtitle_raster::{OverlayRevision, SubtitleBitmap, SubtitleOverlay};

use self::instances::placement_origins;
pub use self::instances::{SubtitleLayerOffset, SubtitleLayerOffsets};
use self::layer::{BitmapLayer, LayerGpuContext, LayerOutcome};
use self::pipeline::{LayerKind, UNIFORM_SIZE_BYTES, create_uniform_buffer};
use self::runs::{DrawRun, partition_overlay};

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
    /// Сколько раз набор масок текста записан в атлас масок.
    pub atlas_uploads: u64,
    /// Сколько отдельных масок записано через `write_texture`.
    pub bitmap_uploads: u64,
    /// Сколько раз создавалась текстура атласа масок (первое создание тоже считается).
    pub atlas_recreations: u64,
    /// Сколько ревизий потеряли хотя бы одну маску текста из-за лимита текстуры GPU.
    pub skipped_overlays_too_large: u64,
    /// Сколько раз записывался uniform (смена размера экрана или позиции картинки).
    pub uniform_uploads: u64,
    /// Сколько раз набор RGBA-картинок записан в RGBA-атлас.
    pub picture_atlas_uploads: u64,
    /// Сколько отдельных RGBA-картинок записано через `write_texture`.
    pub picture_bitmap_uploads: u64,
    /// Сколько раз создавалась текстура RGBA-атласа; 0 — у рендерера не было ни одной картинки.
    pub picture_atlas_recreations: u64,
    /// Сколько RGBA-картинок не нарисовано из-за лимита текстуры GPU.
    pub skipped_picture_bitmaps_too_large: u64,
}

/// Что сделал `render` (различаем причины, чтобы тесты и логи не гадали).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleOverlayOutcome {
    /// Рисовать нечего: нет входа, пустой overlay, пустая видимая область или битый rect.
    /// Render pass не открывался.
    NothingToDraw,
    /// Ни одна картинка overlay-а не влезла в атлас; overlay на этой ревизии не рисуется.
    SkippedTooLarge,
    /// Pass записан, нарисовано `bitmaps` картинок (пропущенные по размеру не считаются).
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

/// Опознавательные признаки загруженного overlay-а и план его рисования.
struct UploadedOverlay {
    revision: OverlayRevision,
    /// Клоны картинок (дёшево: пиксели общие через `Arc`). Они удерживают буферы, поэтому
    /// адрес буфера не может быть переиспользован другой картинкой, и
    /// `shows_same_pixels_as` (тот же буфер, окно, размер, шаг) надёжно отличает содержимое.
    bitmaps: Vec<SubtitleBitmap>,
    /// Серии рисования в порядке списка. Серии слоя, который не удалось подготовить, сюда
    /// не попадают; пусто — рисовать нечего.
    runs: Vec<DrawRun>,
}

impl UploadedOverlay {
    /// Тот же ли это overlay (без аллокаций).
    fn is_same_as(&self, overlay: &SubtitleOverlay) -> bool {
        self.revision == overlay.revision()
            && self.bitmaps.len() == overlay.bitmaps().len()
            && self
                .bitmaps
                .iter()
                .zip(overlay.bitmaps())
                .all(|(uploaded, bitmap)| uploaded.shows_same_pixels_as(bitmap))
    }

    /// Сколько картинок будет нарисовано.
    fn drawn_bitmaps(&self) -> usize {
        self.runs.iter().map(|run| run.instance_count).sum()
    }
}

/// Рендерер субтитров; создаётся один раз под формат surface.
///
/// Это переиспользуемая wgpu-реализация нейтрального контракта (см. документацию модуля):
/// `Renderer` держит её внутри, а приложение и тесты могут создать собственный экземпляр
/// на любом `wgpu::Device` (например, для readback-теста всей цепочки до пикселей).
pub struct SubtitleOverlayRenderer {
    target_format: wgpu::TextureFormat,
    uniform_buffer: wgpu::Buffer,
    /// Маски текста (создаётся сразу).
    coverage_layer: BitmapLayer,
    /// RGBA-картинки; `None`, пока в overlay-ях не было ни одной картинки.
    picture_layer: Option<BitmapLayer>,
    uploaded: Option<UploadedOverlay>,
    /// Последние записанные `[screen_w, screen_h, main_x, main_y, fixed_x, fixed_y, 0, 0]`.
    last_uniform_values: Option<[f32; 8]>,
    uniform_uploads: u64,
}

impl SubtitleOverlayRenderer {
    /// Создаёт pipeline масок и uniform; атласы и слой картинок появятся при первой загрузке.
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        Self {
            target_format,
            uniform_buffer: create_uniform_buffer(device),
            coverage_layer: BitmapLayer::new(device, target_format, LayerKind::Coverage),
            picture_layer: None,
            uploaded: None,
            last_uniform_values: None,
            uniform_uploads: 0,
        }
    }

    /// Снимок счётчиков загрузок.
    pub fn upload_stats(&self) -> SubtitleOverlayUploadStats {
        let coverage = self.coverage_layer.stats();
        let picture = self
            .picture_layer
            .as_ref()
            .map(BitmapLayer::stats)
            .unwrap_or_default();
        SubtitleOverlayUploadStats {
            atlas_uploads: coverage.atlas_uploads,
            bitmap_uploads: coverage.bitmap_uploads,
            atlas_recreations: coverage.atlas_recreations,
            skipped_overlays_too_large: coverage.skipped_prepares,
            uniform_uploads: self.uniform_uploads,
            picture_atlas_uploads: picture.atlas_uploads,
            picture_bitmap_uploads: picture.bitmap_uploads,
            picture_atlas_recreations: picture.atlas_recreations,
            skipped_picture_bitmaps_too_large: picture.skipped_bitmaps,
        }
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
        let screen_area =
            RenderViewport::full_surface(target.screen_size[0], target.screen_size[1]);
        let (Some(origins), Some(scissor)) = (
            placement_origins(frame.picture_rect, frame.layer_offsets),
            target.visible_area.intersection(screen_area),
        ) else {
            return SubtitleOverlayOutcome::NothingToDraw;
        };
        if frame.overlay.bitmaps().is_empty() || scissor.is_empty() {
            return SubtitleOverlayOutcome::NothingToDraw;
        }

        self.ensure_uploaded(target.device, target.queue, frame.overlay);
        let drawn_bitmaps = match &self.uploaded {
            Some(uploaded) if !uploaded.runs.is_empty() => uploaded.drawn_bitmaps(),
            _ => return SubtitleOverlayOutcome::SkippedTooLarge,
        };
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
        render_pass.set_scissor_rect(scissor.x, scissor.y, scissor.width, scissor.height);
        let Some(uploaded) = &self.uploaded else {
            return SubtitleOverlayOutcome::NothingToDraw;
        };
        // Серии идут в порядке списка; каждая — один draw-вызов pipeline своего слоя.
        for run in &uploaded.runs {
            // Ресурсы гарантированы `ensure_uploaded` для серий в плане; отсутствие —
            // нарушение инварианта, которое безопаснее пропустить, чем паниковать в кадре.
            let (Some(resources), Some(instance_range)) = (
                self.layer(run.kind).and_then(BitmapLayer::draw_resources),
                run.instance_range(),
            ) else {
                tracing::error!(
                    layer = ?run.kind,
                    "Subtitle overlay run is planned, but GPU resources are missing"
                );
                continue;
            };
            render_pass.set_pipeline(resources.pipeline);
            render_pass.set_bind_group(0, resources.bind_group, &[]);
            render_pass.set_vertex_buffer(0, resources.instances.slice(..));
            // 4 вершины triangle strip на каждый инстанс.
            render_pass.draw(0..4, instance_range);
        }
        SubtitleOverlayOutcome::Drawn {
            bitmaps: drawn_bitmaps,
        }
    }

    /// Слой по виду; у картинок его может ещё не быть.
    fn layer(&self, kind: LayerKind) -> Option<&BitmapLayer> {
        match kind {
            LayerKind::Coverage => Some(&self.coverage_layer),
            LayerKind::Picture => self.picture_layer.as_ref(),
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
        let max_side = device.limits().max_texture_dimension_2d;
        let partition = partition_overlay(overlay.bitmaps(), max_side);
        let gpu = LayerGpuContext {
            device,
            queue,
            uniform_buffer: &self.uniform_buffer,
        };
        let coverage_outcome =
            self.coverage_layer
                .prepare(&gpu, &partition.coverage, partition.dropped_coverage);
        // RGBA-слой создаётся лениво: overlay без картинок не порождает ни pipeline, ни атласа.
        let needs_picture_layer = !partition.pictures.is_empty() || partition.dropped_pictures > 0;
        let target_format = self.target_format;
        let picture_outcome = if needs_picture_layer || self.picture_layer.is_some() {
            let layer = self
                .picture_layer
                .get_or_insert_with(|| BitmapLayer::new(device, target_format, LayerKind::Picture));
            layer.prepare(&gpu, &partition.pictures, partition.dropped_pictures)
        } else {
            LayerOutcome::Ready { instance_count: 0 }
        };
        if matches!(coverage_outcome, LayerOutcome::TooLarge)
            || matches!(picture_outcome, LayerOutcome::TooLarge)
        {
            tracing::warn!(
                revision = overlay.revision().value(),
                bitmaps = overlay.bitmaps().len(),
                "Subtitle overlay lost bitmaps that do not fit the GPU atlas"
            );
        }
        // Серии слоя, который не удалось подготовить, выбрасываем: остальной слой рисуется.
        let runs: Vec<DrawRun> = partition
            .runs
            .into_iter()
            .filter(|run| {
                let outcome = match run.kind {
                    LayerKind::Coverage => coverage_outcome,
                    LayerKind::Picture => picture_outcome,
                };
                matches!(outcome, LayerOutcome::Ready { .. })
            })
            .collect();
        self.uploaded = Some(UploadedOverlay {
            revision: overlay.revision(),
            bitmaps: overlay.bitmaps().to_vec(),
            runs,
        });
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
        self.uniform_uploads += 1;
    }
}
