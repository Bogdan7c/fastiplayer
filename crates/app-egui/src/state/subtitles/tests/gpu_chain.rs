//! Главный функциональный тест сессии 03: субтитр доходит до пикселей.
//!
//! Цепочка целиком, без подмен: файлы `Movie.mkv` + `Movie.srt` в каталоге -> фоновая задача
//! владельца -> решение по pts кадра -> растеризатор -> настоящий `SubtitleOverlayRenderer`
//! (wgpu) -> readback пикселей. «Видео» — известные цвета: картинка 2.39:1 в окне 16:9 с
//! чёрными полосами. Тесты пропускаются (с сообщением), если на машине нет GPU adapter-а,
//! как тесты угловой маски `render-wgpu-shell`.

use std::sync::mpsc;

use render_core::PictureRect;
use render_shell_api::{SubtitleLayerOffset, SubtitleLayerOffsets, SubtitleOverlayFrame};
use render_wgpu_shell::{SubtitleOverlayOutcome, SubtitleOverlayRenderer, SubtitleOverlayTarget};

use super::*;

const TARGET_WIDTH: u32 = 256;
const TARGET_HEIGHT: u32 = 144;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const GPU_TIMEOUT: Duration = Duration::from_secs(10);
const GENERATION: u64 = 5;
/// Цвет «видео» внутри картинки и цвет чёрных полос (RGBA).
const VIDEO_PIXEL: [u8; 4] = [64, 128, 192, 255];
const BAR_PIXEL: [u8; 4] = [0, 0, 0, 255];

/// Headless GPU: целевая текстура, которую мы заполняем «видео», и рендерер субтитров.
pub(super) struct Harness {
    /// Размер целевой текстуры (для real-media теста — 1920x1080, для остальных — крошечный).
    pub(super) width: u32,
    pub(super) height: u32,
    device: wgpu::Device,
    queue: wgpu::Queue,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    pub(super) renderer: SubtitleOverlayRenderer,
}

impl Harness {
    /// `None`, если нет GPU adapter-а.
    pub(super) fn new() -> Option<Self> {
        Self::with_size(TARGET_WIDTH, TARGET_HEIGHT)
    }

    /// То же, но с целевой текстурой заданного размера.
    pub(super) fn with_size(width: u32, height: u32) -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("subtitle chain test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .ok()?;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("subtitle chain test target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let renderer = SubtitleOverlayRenderer::new(&device, TARGET_FORMAT);
        Some(Self {
            width,
            height,
            device,
            queue,
            texture,
            view,
            renderer,
        })
    }

    /// Один кадр: «видео» (картинка в `picture`, чёрные полосы вокруг) -> субтитры -> readback.
    pub(super) fn frame(
        &mut self,
        picture: PictureRect,
        overlay: Option<&subtitle_raster::SubtitleOverlay>,
    ) -> (SubtitleOverlayOutcome, Vec<u8>) {
        self.frame_with_offset(picture, overlay, SubtitleLayerOffset::NONE)
    }

    /// То же, но слой субтитров целиком сдвинут на `layer_offset` (как это делает размещение).
    pub(super) fn frame_with_offset(
        &mut self,
        picture: PictureRect,
        overlay: Option<&subtitle_raster::SubtitleOverlay>,
        layer_offset: SubtitleLayerOffset,
    ) -> (SubtitleOverlayOutcome, Vec<u8>) {
        self.frame_with_offsets(
            picture,
            overlay,
            SubtitleLayerOffsets::uniform(layer_offset),
        )
    }

    /// То же, но у групп слоя свои сдвиги (режим «поднять основной диалог»).
    pub(super) fn frame_with_offsets(
        &mut self,
        picture: PictureRect,
        overlay: Option<&subtitle_raster::SubtitleOverlay>,
        layer_offsets: SubtitleLayerOffsets,
    ) -> (SubtitleOverlayOutcome, Vec<u8>) {
        self.write_video(picture);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("subtitle chain test encoder"),
            });
        let overlay_frame = overlay.map(|overlay| SubtitleOverlayFrame {
            overlay,
            picture_rect: picture,
            layer_offsets,
        });
        let outcome = self.renderer.render(
            SubtitleOverlayTarget {
                device: &self.device,
                queue: &self.queue,
                encoder: &mut encoder,
                target: &self.view,
                screen_size: [self.width, self.height],
                visible_area: RenderViewport::full_surface(self.width, self.height),
            },
            overlay_frame.as_ref(),
        );
        (outcome, self.read_back(encoder))
    }

    /// Заливает «видео»: картинка своим цветом, всё вокруг — чёрные полосы.
    fn write_video(&self, picture: PictureRect) {
        let (left, top) = (picture.x.round() as u32, picture.y.round() as u32);
        let (right, bottom) = (
            left + picture.width.round() as u32,
            top + picture.height.round() as u32,
        );
        let mut pixels = Vec::with_capacity((self.width * self.height * 4) as usize);
        for y in 0..self.height {
            for x in 0..self.width {
                let inside = x >= left && x < right && y >= top && y < bottom;
                pixels.extend_from_slice(&if inside { VIDEO_PIXEL } else { BAR_PIXEL });
            }
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.width * 4),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn read_back(&self, mut encoder: wgpu::CommandEncoder) -> Vec<u8> {
        let bytes_per_row = self.width * 4;
        let padded = bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("subtitle chain test readback"),
            size: u64::from(padded) * u64::from(self.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        let submission = self.queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (sender, receiver) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).expect("send readback result");
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(GPU_TIMEOUT),
            })
            .expect("wait for GPU");
        receiver
            .recv_timeout(GPU_TIMEOUT)
            .expect("readback callback")
            .expect("map readback");
        let mapped = slice.get_mapped_range().expect("mapped range");
        let pixels = mapped
            .chunks_exact(padded as usize)
            .flat_map(|row| row[..bytes_per_row as usize].iter().copied())
            .collect::<Vec<_>>();
        drop(mapped);
        readback.unmap();
        pixels
    }
}

macro_rules! gpu_or_skip {
    () => {
        match Harness::new() {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the subtitle chain test");
                return;
            }
        }
    };
}

fn pixel_at(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let offset = ((y * TARGET_WIDTH + x) * 4) as usize;
    [
        pixels[offset],
        pixels[offset + 1],
        pixels[offset + 2],
        pixels[offset + 3],
    ]
}

/// Строки, в которых хотя бы один пиксель отличается от «видео» без субтитров.
fn changed_rows(with_subtitles: &[u8], video_only: &[u8]) -> Vec<u32> {
    (0..TARGET_HEIGHT)
        .filter(|&y| {
            (0..TARGET_WIDTH).any(|x| pixel_at(with_subtitles, x, y) != pixel_at(video_only, x, y))
        })
        .collect()
}

#[test]
fn subtitle_file_next_to_the_video_reaches_the_bottom_of_the_picture_and_only_while_the_cue_lasts()
{
    let mut harness = gpu_or_skip!();
    let media = instance(1);
    let (mut owner, _directory) = ready_owner(media);
    let rect = letterboxed_rect(TARGET_WIDTH, TARGET_HEIGHT);
    let picture_top = rect.y.round() as u32;
    let picture_bottom = picture_top + rect.height.round() as u32;
    assert!(
        picture_top > 0 && picture_bottom < TARGET_HEIGHT,
        "fixture has bars"
    );
    let (_, video_only) = harness.frame(rect, None);

    // Кадр внутри первой реплики: субтитр рисуется в нижней части картинки.
    let overlay = owner
        .overlay_for_frame(current(media, GENERATION), &frame(2_000, GENERATION, rect))
        .expect("overlay inside the cue")
        .clone();
    let (outcome, with_cue) = harness.frame(rect, Some(&overlay));
    assert_eq!(
        outcome,
        SubtitleOverlayOutcome::Drawn {
            bitmaps: overlay.bitmaps().len()
        }
    );
    let rows = changed_rows(&with_cue, &video_only);
    assert!(
        rows.len() > 5,
        "the plate and text must change pixels, got {rows:?}"
    );
    let first_changed = *rows.first().expect("changed rows");
    let last_changed = *rows.last().expect("changed rows");
    assert!(
        first_changed >= picture_top + (picture_bottom - picture_top) / 2,
        "subtitle starts in the lower half of the picture (row {first_changed})"
    );
    assert!(
        last_changed < picture_bottom,
        "subtitle stays inside the picture, not on the bar (row {last_changed})"
    );
    // Верхняя часть картинки и обе полосы остаются нетронутыми «видео».
    for y in (0..picture_top)
        .chain(picture_top..picture_top + 20)
        .chain(picture_bottom..TARGET_HEIGHT)
    {
        for x in 0..TARGET_WIDTH {
            assert_eq!(
                pixel_at(&with_cue, x, y),
                pixel_at(&video_only, x, y),
                "pixel ({x},{y}) outside the subtitle area must stay untouched"
            );
        }
    }
    // Плашка затемняет видео (красный канал видео 64 падает ниже), а светлые штрихи текста
    // поднимают его выше: оба эффекта в строках субтитра означают плашку и текст.
    let reds: Vec<u8> = rows
        .iter()
        .flat_map(|&y| (0..TARGET_WIDTH).map(move |x| (x, y)))
        .map(|(x, y)| pixel_at(&with_cue, x, y)[0])
        .collect();
    assert!(
        reds.iter().any(|&red| red < VIDEO_PIXEL[0] / 2),
        "dark plate pixels present"
    );
    assert!(
        reds.iter().any(|&red| red > VIDEO_PIXEL[0] + 30),
        "light text pixels present"
    );

    // Вне реплики overlay нет, и итоговая картинка равна чистому «видео».
    assert!(
        owner
            .overlay_for_frame(current(media, GENERATION), &frame(4_000, GENERATION, rect))
            .is_none()
    );
    let (outcome, without_cue) = harness.frame(rect, None);
    assert_eq!(outcome, SubtitleOverlayOutcome::NothingToDraw);
    assert_eq!(
        without_cue, video_only,
        "no cue: target equals the plain video"
    );
}

#[test]
fn pause_uploads_the_subtitle_to_the_gpu_exactly_once() {
    let mut harness = gpu_or_skip!();
    let media = instance(1);
    let (mut owner, _directory) = ready_owner(media);
    let rect = letterboxed_rect(TARGET_WIDTH, TARGET_HEIGHT);

    let mut previous: Option<Vec<u8>> = None;
    for _ in 0..6 {
        let overlay = owner
            .overlay_for_frame(current(media, GENERATION), &frame(2_000, GENERATION, rect))
            .expect("overlay while paused")
            .clone();
        let (_, pixels) = harness.frame(rect, Some(&overlay));
        if let Some(previous) = &previous {
            assert_eq!(&pixels, previous, "paused frames are pixel-identical");
        }
        previous = Some(pixels);
    }
    assert_eq!(
        harness.renderer.upload_stats().atlas_uploads,
        1,
        "six paused frames: one texture upload"
    );
    assert_eq!(owner.presentation_stats().rasterizer_calls, 1);

    // Другая реплика даёт ровно одну новую загрузку, кадры без реплик — ни одной.
    let second = owner
        .overlay_for_frame(current(media, GENERATION), &frame(6_000, GENERATION, rect))
        .expect("second cue")
        .clone();
    harness.frame(rect, Some(&second));
    assert_eq!(harness.renderer.upload_stats().atlas_uploads, 2);
    for _ in 0..4 {
        assert!(
            owner
                .overlay_for_frame(current(media, GENERATION), &frame(4_000, GENERATION, rect))
                .is_none()
        );
        harness.frame(rect, None);
    }
    assert_eq!(harness.renderer.upload_stats().atlas_uploads, 2);
}

#[test]
fn frames_without_subtitles_cause_no_gpu_uploads_and_no_rasterization() {
    let mut harness = gpu_or_skip!();
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), None);
    let media = instance(1);
    let mut owner = owner();
    begin_and_wait(&mut owner, media, &video);
    let rect = letterboxed_rect(TARGET_WIDTH, TARGET_HEIGHT);

    for pts in [0, 2_000, 6_000] {
        let overlay = owner
            .overlay_for_frame(current(media, GENERATION), &frame(pts, GENERATION, rect))
            .cloned();
        assert!(overlay.is_none(), "no subtitle file: nothing at {pts} ms");
        let (outcome, _) = harness.frame(rect, overlay.as_ref());
        assert_eq!(outcome, SubtitleOverlayOutcome::NothingToDraw);
    }

    assert_eq!(harness.renderer.upload_stats().atlas_uploads, 0);
    assert_eq!(harness.renderer.upload_stats().bitmap_uploads, 0);
    assert_eq!(owner.presentation_stats().rasterizer_calls, 0);
}

#[test]
fn toggling_subtitles_on_and_off_adds_and_removes_their_pixels_in_the_rendered_frame() {
    let mut harness = gpu_or_skip!();
    let media = instance(1);
    // Субтитры выключены по умолчанию, как в продукте.
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(TWO_CUE_SRT));
    let mut owner = owner_with_config(&SubtitlesConfig::default());
    begin_and_wait(&mut owner, media, &video);
    let rect = letterboxed_rect(TARGET_WIDTH, TARGET_HEIGHT);
    let (_, video_only) = harness.frame(rect, None);
    let render_at_cue = |owner: &mut SubtitleOwner, harness: &mut Harness| {
        let overlay = owner
            .overlay_for_frame(current(media, GENERATION), &frame(2_000, GENERATION, rect))
            .cloned();
        harness.frame(rect, overlay.as_ref()).1
    };

    // Выключено: кадр равен чистому видео.
    assert_eq!(render_at_cue(&mut owner, &mut harness), video_only);

    // Включили: файл читается лениво, затем пиксели субтитра появляются в кадре.
    owner.set_visibility(SubtitleVisibility::Shown);
    let deadline = Instant::now() + LOAD_TIMEOUT;
    while owner.lifecycle_view() != SubtitleLifecycleView::Ready {
        assert!(Instant::now() < deadline, "track must load after enabling");
        owner.poll_jobs();
        std::thread::sleep(Duration::from_millis(2));
    }
    let shown = render_at_cue(&mut owner, &mut harness);
    assert!(
        changed_rows(&shown, &video_only).len() > 5,
        "subtitle pixels appear after enabling"
    );

    // Выключили: пиксели исчезли, кадр снова равен чистому видео.
    owner.set_visibility(SubtitleVisibility::Hidden);
    assert_eq!(render_at_cue(&mut owner, &mut harness), video_only);
}
