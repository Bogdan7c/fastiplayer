//! Замер растеризации ASS-сцены кадр за кадром (бюджет сессии 10: «не медленнее C-версии libass»).
//!
//! Запуск (обязательно release, debug-цифры бессмысленны):
//! `cargo run -p subtitle-raster --release --example measure_ass_scene -- <файл.ass> <старт_мс> <кадров>`.
//!
//! Условия совпадают с замером системного libass из отчёта `user/subtitles/results/10.md`:
//! холст 1920x1080, шаг кадра 1000/24 мс (целочисленный, как `i*1000/24` в C), только
//! встроенные шрифты Noto (у libass — провайдер NONE + каталог `fonts/`), один растеризатор
//! на весь прогон (кэши живут между кадрами, как кэши `ASS_Renderer`).
//!
//! Печатает среднее и худшее время кадра и среднее число картинок в кадре.

use std::error::Error;
use std::time::{Duration, Instant};

use subtitle_core::{SubtitleContent, SubtitleTime, SubtitleTimeline, TimelineInsertOutcome};
use subtitle_formats::{
    AssParseLimits, LegacyEncodingHint, SubtitleParseLimits, TextSubtitleFormat, load_text_subtitle,
};
use subtitle_raster::{
    AssEventTiming, AssRenderOptions, CanvasSize, CueKey, CueSourceId, FontLookupCacheLimits,
    RasterCacheLimits, RasterInputLimits, RasterLimits, RenderRequest, RenderableCue,
    SubtitleAppearance, SubtitleFontLibrary, SubtitleRasterizer, TextLanguagePreference,
};

/// Размер кадра замера (как `ass_set_frame_size(1920, 1080)` в замере libass).
const FRAME_WIDTH: u32 = 1920;
const FRAME_HEIGHT: u32 = 1080;
/// Частота кадров замера: время кадра `i` = старт + i*1000/24 мс.
const FRAMES_PER_SECOND: i64 = 24;
/// Кэш масок реплик (как `RASTER_CUE_CACHE_BYTES` в app-egui).
const CUE_CACHE_BYTES: usize = 32 * 1024 * 1024;
/// Подготовленные лица шрифта (как в app-egui).
const SHAPER_FACES: usize = 16;
/// Кэш поиска глифов (как в app-egui).
const GLYPH_LOOKUP_CACHE: usize = 65_536;
/// Лимит событий ASS в кадре: с запасом, чтобы тяжёлые сцены не обрезались.
const MAX_ASS_EVENTS_PER_FRAME: usize = 2_048;

/// Аргументы командной строки.
struct Arguments {
    /// Путь к `.ass`.
    script_path: String,
    /// Время первого кадра, мс.
    start_ms: i64,
    /// Сколько кадров подряд рендерить.
    frames: i64,
}

fn parse_arguments() -> Result<Arguments, Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: measure_ass_scene <file.ass> <start_ms> <frames>";
    let script_path = args.next().ok_or(usage)?;
    let start_ms = args.next().ok_or(usage)?.parse()?;
    let frames = args.next().ok_or(usage)?.parse()?;
    Ok(Arguments {
        script_path,
        start_ms,
        frames,
    })
}

/// Разобранная сцена: таймлайн событий ASS.
fn load_timeline(path: &str) -> Result<SubtitleTimeline, Box<dyn Error>> {
    let bytes = std::fs::read(path)?;
    // Лимиты разбора заведомо выше тяжёлых сцен замера: проверяется скорость, а не отсечение.
    let limits =
        SubtitleParseLimits::new(1 << 24, 1_000_000, 100).with_ass_limits(AssParseLimits {
            max_header_bytes: 1 << 20,
            max_styles: 1_000,
            max_override_tags_per_event: 8_192,
            max_tracked_unknown_tag_names: 16,
        });
    let loaded = load_text_subtitle(
        &bytes,
        TextSubtitleFormat::Ass,
        LegacyEncodingHint::Generic,
        &limits,
    )?;
    let mut timeline = SubtitleTimeline::new();
    for event in loaded.document().events() {
        if !matches!(
            timeline.insert(event.clone()),
            TimelineInsertOutcome::Inserted(_)
        ) {
            return Err("event was not inserted".into());
        }
    }
    Ok(timeline)
}

fn rasterizer() -> Result<SubtitleRasterizer, Box<dyn Error>> {
    // Только встроенные шрифты: так же, как libass с провайдером NONE в замере.
    let fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: GLYPH_LOOKUP_CACHE,
    })?;
    Ok(SubtitleRasterizer::new(
        fonts,
        RasterLimits {
            cache: RasterCacheLimits {
                cue_bitmaps_bytes: CUE_CACHE_BYTES,
                shaper_faces: SHAPER_FACES,
                blur_scratch_retained_bytes: 32 * 1024 * 1024,
                glyph_bitmaps_bytes: 40 * 1024 * 1024,
                glyph_composites_bytes: 20 * 1024 * 1024,
                glyph_outline_items: 10_000,
                shaping_cache_bytes: 4 * 1024 * 1024,
            },
            input: RasterInputLimits {
                max_chars_per_cue: 2_000,
                max_cues_per_frame: 16,
                max_ass_events_per_frame: MAX_ASS_EVENTS_PER_FRAME,
                max_canvas_side: 8_192,
            },
        },
    ))
}

/// Один кадр: активные события в момент `moment` -> `render`. Возвращает время и число картинок.
fn render_frame(
    rasterizer: &mut SubtitleRasterizer,
    timeline: &SubtitleTimeline,
    moment: SubtitleTime,
) -> Result<(Duration, usize), Box<dyn Error>> {
    let mut active = Vec::new();
    timeline.collect_active_at(moment, &mut active);
    let mut events = Vec::with_capacity(active.len());
    for id in active {
        let event = timeline.event(id).ok_or("active event is missing")?;
        let SubtitleContent::Ass(cue) = event.content() else {
            return Err("scene must contain only ASS events".into());
        };
        events.push((id, event.layer(), cue, event.start(), event.end()));
    }
    let renderable: Vec<RenderableCue<'_>> = events
        .iter()
        .map(|(id, layer, cue, start, end)| {
            RenderableCue::ass(
                CueKey::new(CueSourceId::new(1), *id),
                *layer,
                cue,
                AssEventTiming::new(*start, *end),
            )
        })
        .collect();
    let canvas = CanvasSize::new(FRAME_WIDTH, FRAME_HEIGHT).ok_or("invalid canvas")?;
    let appearance = SubtitleAppearance::youtube_like();
    // Замер покрывает только `render`: сбор активных событий выше в него не входит.
    let started = Instant::now();
    let overlay = rasterizer.render(&RenderRequest {
        moment,
        cues: &renderable,
        canvas,
        appearance: &appearance,
        text_language: TextLanguagePreference::unknown(),
        ass: AssRenderOptions::default(),
    });
    let elapsed = started.elapsed();
    Ok((elapsed, overlay.bitmaps().len()))
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = parse_arguments()?;
    let timeline = load_timeline(&arguments.script_path)?;
    let mut rasterizer = rasterizer()?;
    let mut total = Duration::ZERO;
    let mut worst = Duration::ZERO;
    let mut images = 0usize;
    for frame in 0..arguments.frames {
        let moment =
            SubtitleTime::from_millis(arguments.start_ms + frame * 1000 / FRAMES_PER_SECOND);
        let (elapsed, frame_images) = render_frame(&mut rasterizer, &timeline, moment)?;
        total += elapsed;
        worst = worst.max(elapsed);
        images += frame_images;
    }
    let frames = arguments.frames.max(1) as f64;
    println!(
        "{}: mean {:.2} ms, worst {:.2} ms, images/frame {:.0}",
        arguments.script_path,
        total.as_secs_f64() * 1000.0 / frames,
        worst.as_secs_f64() * 1000.0,
        images as f64 / frames,
    );
    Ok(())
}
