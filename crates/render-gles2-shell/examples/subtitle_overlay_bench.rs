//! Стенд замера прохода субтитров GLES2 (сессия 08 плана `user/gles2-renderer/`).
//!
//! Настоящий растеризатор субтитров → адаптер `render-gles2-shell` → слой оверлеев `gles2-video`
//! → offscreen-цель EGL (по умолчанию 1440×900, как панель T410). Картинка видео — 16:9 по ширине
//! цели (letterbox сверху и снизу), «видео» — заливка цели, как в GPU-тестах прохода.
//!
//! Сцены:
//! - `text` — две строки текста в стиле по умолчанию (YouTube-плашка), как обычный SRT;
//! - `pgs` — полупрозрачная RGBA-картинка 1200×160 (типичная строка Blu-ray PGS) поверх текста.
//!
//! Что меряется (release-сборка):
//! - кадр с новым overlay (упаковка атласа, загрузка, вершины): `submit` — до возврата `present`
//!   (сколько держит главный поток) и CPU-время потока (`/proc/thread-self/schedstat`), `GPU done`
//!   — до сигнала fence этого кадра; сцена `text-next-cue` — новая реплика
//!   при уже выделенном атласе (так меняются реплики в фильме), `text`/`pgs` — с выделением атласа;
//! - `gpu` — серия из `--frames` кадров «заливка + субтитры» с одним fence в конце, минус та же
//!   серия «только заливка»: цена прохода субтитров на GPU в мс на кадр;
//! - `shift` — та же серия, но угол холста меняется каждый кадр (только uniform-ы): сдвиг слоя
//!   не должен стоить загрузок.
//!
//! Пример: `cargo run --release -p render-gles2-shell --example subtitle_overlay_bench --
//! --frames 300`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "стенд замера (пример, не production): отказ окружения или аргументов прерывает \
              прогон с понятным сообщением"
)]

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gles2_egui::{FrameStart, Gles2Context, SurfaceSize};
use gles2_video::OverlayLayerOptions;
use render_core::{PictureLayout, PictureSize, RenderViewport};
use render_gles2_shell::{Gles2SubtitleOverlayRenderer, SubtitleOverlayOutcome};
use render_shell_api::{SubtitleLayerOffset, SubtitleLayerOffsets, SubtitleOverlayFrame};
use subtitle_core::{
    CuePlacement, IndexedPicture, PaletteColor, PictureEncoding, PictureForcedFlag, PicturePalette,
    PicturePosition, PictureRect as VisibleRect, PictureReferenceSize, SpanStyle, StyledText,
    SubtitleContent, SubtitleEvent, SubtitleLayer, SubtitlePictureCue, SubtitleTextCue,
    SubtitleTime, SubtitleTimeline, TextSpan, TimelineInsertOutcome,
};
use subtitle_pgs::stream_builder::encode_rle;
use subtitle_raster::{
    AssRenderOptions, CanvasSize, CueKey, CueSourceId, FontLookupCacheLimits, RasterCacheLimits,
    RasterInputLimits, RasterLimits, RenderRequest, RenderableCue, SubtitleAppearance,
    SubtitleFontLibrary, SubtitleOverlay, SubtitleRasterizer, TextLanguagePreference,
};

/// Заливка «видео».
const VIDEO_COLOR: [f32; 4] = [0.25, 0.5, 0.75, 1.0];
/// Реплика из двух строк.
const TEXT: &str =
    "The quick brown fox jumps over the lazy dog\nи съел ещё этих мягких французских булок";
/// Следующая реплика той же длины строк: новые пиксели при уже выделенном атласе.
const NEXT_TEXT: &str =
    "Pack my box with five dozen liquor jugs now\nвъешь же ещё этих мягких французских булок";

struct Arguments {
    target: [u32; 2],
    frames: usize,
}

fn parse_arguments() -> Arguments {
    let mut arguments = Arguments {
        target: [1440, 900],
        frames: 300,
    };
    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().expect("flag needs a value");
        match flag.as_str() {
            "--frames" => arguments.frames = value().parse().expect("--frames is a number"),
            "--width" => arguments.target[0] = value().parse().expect("--width is a number"),
            "--height" => arguments.target[1] = value().parse().expect("--height is a number"),
            other => panic!("unknown flag {other}"),
        }
    }
    arguments
}

fn rasterizer() -> SubtitleRasterizer {
    let fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 4096,
    })
    .expect("embedded fonts");
    SubtitleRasterizer::new(
        fonts,
        RasterLimits {
            cache: RasterCacheLimits {
                cue_bitmaps_bytes: 64 * 1024 * 1024,
                shaper_faces: 8,
                blur_scratch_retained_bytes: 32 * 1024 * 1024,
                glyph_bitmaps_bytes: 40 * 1024 * 1024,
                glyph_composites_bytes: 20 * 1024 * 1024,
                glyph_outline_items: 10_000,
                shaping_cache_bytes: 4 * 1024 * 1024,
            },
            input: RasterInputLimits {
                max_chars_per_cue: 1000,
                max_cues_per_frame: 8,
                max_ass_events_per_frame: 8,
                max_canvas_side: 8192,
            },
        },
    )
}

fn event_id(content: SubtitleContent) -> subtitle_core::SubtitleEventId {
    let mut timeline = SubtitleTimeline::new();
    let event = SubtitleEvent::new(
        SubtitleTime::from_millis(0),
        SubtitleTime::from_millis(5_000),
        SubtitleLayer::DEFAULT,
        content,
    )
    .expect("interval");
    let TimelineInsertOutcome::Inserted(id) = timeline.insert(event) else {
        panic!("event must be inserted");
    };
    id
}

fn non_zero(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("non-zero")
}

/// Полупрозрачная картинка PGS (опорный кадр 1920×1080) — типичная строка Blu-ray.
fn pgs_line() -> SubtitlePictureCue {
    let (width, height) = (1200_u16, 160_u16);
    let indices: Vec<u8> = (0..usize::from(width) * usize::from(height))
        .map(|index| if index % 7 == 0 { 2 } else { 1 })
        .collect();
    let encoded = encode_rle(&indices, width, height);
    let picture = IndexedPicture::new(
        non_zero(u32::from(width)),
        non_zero(u32::from(height)),
        PictureEncoding::HdmvRle(Arc::from(encoded)),
    );
    let mut entries = [PaletteColor::TRANSPARENT; PicturePalette::LEN];
    entries[1] = PaletteColor {
        red: 240,
        green: 240,
        blue: 40,
        alpha: 200,
    };
    entries[2] = PaletteColor {
        red: 10,
        green: 10,
        blue: 10,
        alpha: 255,
    };
    SubtitlePictureCue::new(
        PictureReferenceSize::new(non_zero(1920), non_zero(1080)),
        PicturePosition { x: 360, y: 880 },
        VisibleRect::new(
            0,
            0,
            non_zero(u32::from(width)),
            non_zero(u32::from(height)),
        ),
        Arc::new(picture),
        Arc::new(PicturePalette::new(entries)),
        PictureForcedFlag::Regular,
    )
    .expect("visible region is the whole picture")
}

fn render_overlay(
    rasterizer: &mut SubtitleRasterizer,
    canvas: CanvasSize,
    text: &str,
    picture: Option<&SubtitlePictureCue>,
) -> SubtitleOverlay {
    let text_cue = SubtitleTextCue {
        text: StyledText::new(vec![TextSpan::text(text, SpanStyle::default())]),
        placement: CuePlacement::default(),
    };
    // Разный текст — разный источник: у растеризатора кэш по источнику и событию.
    let text_key = CueKey::new(
        CueSourceId::new(1 + text.len() as u64),
        event_id(SubtitleContent::Text(text_cue.clone())),
    );
    let mut cues = vec![RenderableCue::text(text_key, &text_cue)];
    if let Some(picture) = picture {
        let key = CueKey::new(
            CueSourceId::new(100),
            event_id(SubtitleContent::Picture(picture.clone())),
        );
        cues.push(RenderableCue::picture(key, picture));
    }
    rasterizer
        .render(&RenderRequest {
            moment: SubtitleTime::ZERO,
            cues: &cues,
            canvas,
            appearance: &SubtitleAppearance::youtube_like(),
            text_language: TextLanguagePreference::unknown(),
            ass: AssRenderOptions::default(),
        })
        .clone()
}

/// CPU-время этого потока в мс (`/proc/thread-self/schedstat`, первое поле — наносекунды).
fn thread_cpu_ms() -> f64 {
    let stat = std::fs::read_to_string("/proc/thread-self/schedstat").expect("schedstat");
    let nanoseconds: u64 = stat
        .split_whitespace()
        .next()
        .and_then(|field| field.parse().ok())
        .expect("schedstat run time");
    nanoseconds as f64 / 1e6
}

/// Один кадр: заливка, субтитры (если есть), показ; fence кадра.
fn draw(
    context: &mut Gles2Context,
    renderer: &mut Gles2SubtitleOverlayRenderer,
    overlay_frame: Option<&SubtitleOverlayFrame<'_>>,
    visible_area: RenderViewport,
) -> (SubtitleOverlayOutcome, gles2_egui::GpuFence) {
    let outcome = match context.begin_frame().expect("frame") {
        FrameStart::Ready(frame) => {
            frame.clear(VIDEO_COLOR);
            let outcome = renderer
                .render(&frame, overlay_frame, visible_area)
                .expect("subtitles render");
            frame.present().expect("present");
            outcome
        }
        FrameStart::Skipped(reason) => panic!("frame skipped: {reason:?}"),
    };
    (outcome, context.insert_fence().expect("fence"))
}

/// Серия кадров с одним ожиданием fence в конце: мс GPU на кадр (пропускная способность).
fn series(frames: usize, mut one_frame: impl FnMut(usize) -> gles2_egui::GpuFence) -> f64 {
    let started = Instant::now();
    let mut last = None;
    for index in 0..frames {
        last = Some(one_frame(index));
    }
    if let Some(fence) = last {
        fence.wait(Duration::from_secs(30)).expect("series fence");
    }
    started.elapsed().as_secs_f64() * 1000.0 / frames as f64
}

fn main() {
    let arguments = parse_arguments();
    let [width, height] = arguments.target;
    let mut context =
        Gles2Context::offscreen(SurfaceSize::new(width, height).expect("non-zero target"))
            .expect("offscreen GLES2 context");
    println!(
        "renderer: {} | {}",
        context.capabilities().gl_renderer(),
        context.capabilities().gl_version_string()
    );
    let mut renderer = Gles2SubtitleOverlayRenderer::new(&context, OverlayLayerOptions::default())
        .expect("subtitle overlay");
    let screen = RenderViewport::full_surface(width, height);
    let layout = PictureLayout::fit(PictureSize::new(1920, 1080), screen).expect("layout");
    let picture_rect = layout.rect();
    let canvas = CanvasSize::new(
        picture_rect.width.round() as u32,
        picture_rect.height.round() as u32,
    )
    .expect("canvas");
    let mut rasterizer = rasterizer();
    let pgs = pgs_line();
    let scenes = [
        ("text", render_overlay(&mut rasterizer, canvas, TEXT, None)),
        (
            "text-next-cue",
            render_overlay(&mut rasterizer, canvas, NEXT_TEXT, None),
        ),
        (
            "pgs",
            render_overlay(&mut rasterizer, canvas, TEXT, Some(&pgs)),
        ),
    ];
    let clear_only = series(arguments.frames, |_| {
        draw(&mut context, &mut renderer, None, screen).1
    });
    println!("clear only: {clear_only:.3} ms/frame");
    for (name, overlay) in &scenes {
        let frame = SubtitleOverlayFrame {
            overlay,
            picture_rect,
            layer_offsets: SubtitleLayerOffsets::NONE,
        };
        let cpu_before = thread_cpu_ms();
        let upload_started = Instant::now();
        let (outcome, fence) = draw(&mut context, &mut renderer, Some(&frame), screen);
        // До возврата `present`: столько кадр с новой репликой держит главный поток.
        let submit = upload_started.elapsed().as_secs_f64() * 1000.0;
        let upload_cpu = thread_cpu_ms() - cpu_before;
        fence.wait(Duration::from_secs(5)).expect("upload fence");
        // До конца работы GPU над этим кадром (заливка + загрузка атласа + проход).
        let upload = upload_started.elapsed().as_secs_f64() * 1000.0;
        let pixels: usize = overlay
            .bitmaps()
            .iter()
            .map(|bitmap| bitmap.width() as usize * bitmap.height() as usize)
            .sum();
        let still = series(arguments.frames, |_| {
            draw(&mut context, &mut renderer, Some(&frame), screen).1
        });
        let before_shift = renderer.upload_stats();
        let shift = series(arguments.frames, |index| {
            let moving = SubtitleOverlayFrame {
                layer_offsets: SubtitleLayerOffsets::uniform(SubtitleLayerOffset {
                    x: 0.0,
                    y: -((index % 40) as f32),
                }),
                ..frame
            };
            draw(&mut context, &mut renderer, Some(&moving), screen).1
        });
        let after_shift = renderer.upload_stats();
        println!(
            "{name}: {outcome:?}, {} bitmaps, {pixels} px | new overlay frame: submit {submit:.2} ms (CPU {upload_cpu:.2}), GPU done {upload:.2} ms | \
             gpu {:.3} ms/frame (frame {still:.3} − clear {clear_only:.3}) | shift {:.3} ms/frame, \
             atlas uploads during shift {}",
            overlay.bitmaps().len(),
            still - clear_only,
            shift - clear_only,
            after_shift.atlas_uploads + after_shift.picture_atlas_uploads
                - before_shift.atlas_uploads
                - before_shift.picture_atlas_uploads,
        );
    }
    println!("stats: {:?}", renderer.upload_stats());
    renderer.destroy(&context).expect("destroy");
}
