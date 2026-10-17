//! Замер времени `SubtitleRasterizer::render` (решение владельца: растеризация идёт синхронно
//! на потоке UI с бюджетом 8 мс, поэтому нужны реальные цифры).
//!
//! Запуск (обязательно release, debug-цифры бессмысленны):
//! `cargo run -p subtitle-raster --release --example measure_render_time`.
//!
//! Три режима на каждую пару (письменность, холст):
//! - `cold`: свежая библиотека шрифтов (встроенные + системные) и свежий растеризатор,
//!   первая реплика после запуска (пустые кэши масок и подготовленных лиц шрифта);
//! - `new cue`: новая реплика на уже прогретом растеризаторе (кэш лиц шрифта тёплый,
//!   кэша масок для этой реплики нет);
//! - `same cue`: та же реплика ещё раз (быстрый путь по кэшу).
//!
//! Тексты — строки из «Sintel» (c) Blender Foundation, CC-BY 3.0.

use std::error::Error;
use std::time::{Duration, Instant};

use subtitle_core::{
    CuePlacement, SpanStyle, StyledText, SubtitleContent, SubtitleEvent, SubtitleLayer,
    SubtitleTextCue, SubtitleTime, SubtitleTimeline, TextSpan, TimelineInsertOutcome,
};
use subtitle_raster::{
    AssRenderOptions, CanvasSize, CueKey, CueSourceId, FontLookupCacheLimits, RasterCacheLimits,
    RasterInputLimits, RasterLimits, RenderRequest, RenderableCue, SubtitleAppearance,
    SubtitleFontLibrary, SubtitleRasterizer, TextLanguagePreference,
};

/// Сколько прогонов в режимах `new cue` и `same cue`.
const WARM_RUNS: usize = 20;
/// Сколько прогонов в режиме `cold` (каждый строит библиотеку шрифтов заново).
const COLD_RUNS: usize = 5;
/// Размер кэша масок (как `RASTER_CUE_CACHE_BYTES` в app-egui).
const CUE_CACHE_BYTES: usize = 32 * 1024 * 1024;
/// Подготовленные лица шрифта (как в app-egui).
const SHAPER_FACES: usize = 16;
/// Кэш поиска глифов (как в app-egui).
const GLYPH_LOOKUP_CACHE: usize = 65_536;

/// Письменность образца и его строки.
struct Script {
    name: &'static str,
    lines: &'static [&'static str],
}

const SCRIPTS: &[Script] = &[
    Script {
        name: "en 1 line",
        lines: &["This blade has a dark past."],
    },
    Script {
        name: "en 2 lines",
        lines: &[
            "You're a fool for traveling alone,",
            "so completely unprepared.",
        ],
    },
    Script {
        name: "ru 2 lines",
        lines: &[
            "Только глупец путешествует в одиночку,",
            "да еще без оружия.",
        ],
    },
    Script {
        name: "ar 1 line (RTL)",
        lines: &["لقد سفكت العديد من دماء الأبرياء."],
    },
    Script {
        name: "ja 1 line (CJK)",
        lines: &["多くの罪なき人の血で汚れている"],
    },
];

/// Холсты: размер картинки внутри окна (для letterbox берётся именно картинка).
const CANVASES: &[(u32, u32)] = &[(1280, 544), (1920, 817), (1920, 1080), (3840, 2160)];

fn limits() -> RasterLimits {
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
            max_ass_events_per_frame: 16,
            max_canvas_side: 8_192,
        },
    }
}

/// Библиотека шрифтов как в приложении: встроенные + системные.
fn build_fonts() -> Result<SubtitleFontLibrary, Box<dyn Error>> {
    let mut fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: GLYPH_LOOKUP_CACHE,
    })?;
    let _report = fonts.load_system_fonts();
    Ok(fonts)
}

/// Реплика и её ключ; каждый вызов даёт новый `CueKey` (новый id события в таймлайне).
fn make_cue(
    timeline: &mut SubtitleTimeline,
    script: &Script,
    ordinal: i64,
) -> Result<(CueKey, SubtitleTextCue), Box<dyn Error>> {
    let mut spans = Vec::new();
    for (index, line) in script.lines.iter().enumerate() {
        if index > 0 {
            spans.push(TextSpan::line_break());
        }
        spans.push(TextSpan::text(*line, SpanStyle::default()));
    }
    let cue = SubtitleTextCue {
        text: StyledText::new(spans),
        placement: CuePlacement::default(),
    };
    let event = SubtitleEvent::new(
        SubtitleTime::from_millis(ordinal * 10),
        SubtitleTime::from_millis(ordinal * 10 + 5),
        SubtitleLayer::DEFAULT,
        SubtitleContent::Text(cue.clone()),
    )?;
    let TimelineInsertOutcome::Inserted(id) = timeline.insert(event) else {
        return Err("event was not inserted".into());
    };
    Ok((CueKey::new(CueSourceId::new(1), id), cue))
}

/// Одна растеризация реплики с замером времени.
fn timed_render(
    rasterizer: &mut SubtitleRasterizer,
    key: CueKey,
    cue: &SubtitleTextCue,
    canvas: CanvasSize,
    appearance: &SubtitleAppearance,
) -> Result<(Duration, usize), Box<dyn Error>> {
    let cues = [RenderableCue::text(key, cue)];
    let started = Instant::now();
    let overlay = rasterizer.render(&RenderRequest {
        moment: subtitle_core::SubtitleTime::ZERO,
        cues: &cues,
        canvas,
        appearance,
        text_language: TextLanguagePreference::unknown(),
        ass: AssRenderOptions::default(),
    });
    let elapsed = started.elapsed();
    // Без битмапов замер ничего не значит (например, не нашлись глифы): падаем громко.
    let bitmaps = overlay.bitmaps().len();
    if bitmaps == 0 {
        return Err("render produced no bitmaps".into());
    }
    Ok((elapsed, bitmaps))
}

/// min / median / max в миллисекундах.
fn summarize(mut samples: Vec<Duration>) -> String {
    samples.sort();
    let to_ms = |value: Duration| value.as_secs_f64() * 1000.0;
    let (Some(min), Some(max)) = (samples.first(), samples.last()) else {
        return "-".to_string();
    };
    let median = samples[samples.len() / 2];
    format!(
        "{:>7.2} {:>7.2} {:>7.2}",
        to_ms(*min),
        to_ms(median),
        to_ms(*max)
    )
}

fn main() -> Result<(), Box<dyn Error>> {
    let fonts_started = Instant::now();
    let _fonts = build_fonts()?;
    println!(
        "font library build (embedded + system): {:.1} ms\n",
        fonts_started.elapsed().as_secs_f64() * 1000.0
    );
    drop(_fonts);

    let appearance = SubtitleAppearance::youtube_like();
    println!(
        "{:<17} {:<10} {:<8} {:>7} {:>7} {:>7}   (ms; min median max)",
        "script", "canvas", "mode", "min", "median", "max"
    );
    for script in SCRIPTS {
        for &(width, height) in CANVASES {
            let canvas = CanvasSize::new(width, height).ok_or("empty canvas")?;
            let label = format!("{width}x{height}");

            // cold: свежие шрифты и растеризатор на каждый прогон.
            let mut cold = Vec::new();
            for run in 0..COLD_RUNS {
                let mut rasterizer = SubtitleRasterizer::new(build_fonts()?, limits());
                let mut timeline = SubtitleTimeline::new();
                let (key, cue) = make_cue(&mut timeline, script, i64::try_from(run)?)?;
                cold.push(timed_render(&mut rasterizer, key, &cue, canvas, &appearance)?.0);
            }
            println!(
                "{:<17} {label:<10} {:<8} {}",
                script.name,
                "cold",
                summarize(cold)
            );

            // new cue / same cue на одном прогретом растеризаторе.
            let mut rasterizer = SubtitleRasterizer::new(build_fonts()?, limits());
            let mut timeline = SubtitleTimeline::new();
            let (warmup_key, warmup_cue) = make_cue(&mut timeline, script, 0)?;
            timed_render(
                &mut rasterizer,
                warmup_key,
                &warmup_cue,
                canvas,
                &appearance,
            )?;
            let mut new_cue = Vec::new();
            let mut same_cue = Vec::new();
            for run in 0..WARM_RUNS {
                let (key, cue) = make_cue(&mut timeline, script, i64::try_from(run)? + 1)?;
                new_cue.push(timed_render(&mut rasterizer, key, &cue, canvas, &appearance)?.0);
                same_cue.push(timed_render(&mut rasterizer, key, &cue, canvas, &appearance)?.0);
            }
            println!(
                "{:<17} {label:<10} {:<8} {}",
                script.name,
                "new cue",
                summarize(new_cue)
            );
            println!(
                "{:<17} {label:<10} {:<8} {}",
                script.name,
                "same cue",
                summarize(same_cue)
            );
        }
    }
    Ok(())
}
