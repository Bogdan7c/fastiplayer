//! Ручная приёмка растеризатора: рисует PNG нескольких реплик в обоих стилях.
//!
//! Запуск: `cargo run -p subtitle-raster --example render_samples -- <каталог>`.
//! Каталог создаётся при необходимости; в репозиторий ничего не пишется.
//!
//! Каждая картинка — серый градиентный «кадр» 1280x720 с нарисованными поверх субтитрами,
//! чтобы была видна прозрачность плашки. Системные шрифты подключаются явно: так CJK и
//! эмодзи берутся из системы (если шрифт там есть), остальные письменности — из встроенных.

use std::error::Error;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use subtitle_core::{
    CuePlacement, SpanStyle, StyledText, SubtitleContent, SubtitleEvent, SubtitleLayer,
    SubtitleTextCue, SubtitleTime, SubtitleTimeline, TextSpan, TimelineInsertOutcome,
};
use subtitle_raster::{
    AssRenderOptions, CanvasSize, CueKey, CueSourceId, FontLookupCacheLimits, RasterCacheLimits,
    RasterInputLimits, RasterLimits, RenderRequest, RenderableCue, SubtitleAppearance,
    SubtitleFontLibrary, SubtitleOverlay, SubtitleRasterizer, TextLanguagePreference,
};

/// Ширина кадра приёмки.
const FRAME_WIDTH: u32 = 1280;
/// Высота кадра приёмки.
const FRAME_HEIGHT: u32 = 720;
/// Лимит кэша масок реплик для примера: 32 МиБ.
const CUE_CACHE_BYTES: usize = 32 * 1024 * 1024;
/// Сколько подготовленных лиц шрифта держать для примера.
const SHAPER_FACES: usize = 16;
/// Сколько пар (лицо, символ) запоминает поиск глифов для примера.
const GLYPH_LOOKUP_CACHE: usize = 4096;
/// Максимум символов в одной реплике.
const MAX_CHARS_PER_CUE: usize = 2000;
/// Максимум реплик в кадре.
const MAX_CUES_PER_FRAME: usize = 16;
/// Максимальная сторона холста.
const MAX_CANVAS_SIDE: u32 = 8192;

/// Образец: имя файла и строки текста.
struct Sample {
    file_stem: &'static str,
    lines: &'static [&'static str],
}

const SAMPLES: &[Sample] = &[
    Sample {
        file_stem: "latin",
        lines: &["The quick brown fox jumps over the lazy dog"],
    },
    Sample {
        file_stem: "cyrillic",
        lines: &["Привет, как дела?"],
    },
    Sample {
        file_stem: "two_lines",
        lines: &["Первая строка чуть длиннее", "Вторая"],
    },
    Sample {
        file_stem: "arabic",
        lines: &["مرحبا بالعالم، كيف حالك؟"],
    },
    Sample {
        file_stem: "hebrew",
        lines: &["שלום עולם, מה שלומך?"],
    },
    Sample {
        file_stem: "hindi",
        lines: &["नमस्ते दुनिया, आप कैसे हैं?"],
    },
    Sample {
        file_stem: "cjk",
        lines: &["你好，世界。こんにちは世界。안녕하세요"],
    },
    Sample {
        file_stem: "emoji_mixed",
        lines: &["Mixed text with emoji \u{1F600} and symbols \u{2605}"],
    },
];

fn main() -> Result<(), Box<dyn Error>> {
    let output_dir: PathBuf = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: render_samples <output directory>")?;
    std::fs::create_dir_all(&output_dir)?;

    let mut fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: GLYPH_LOOKUP_CACHE,
    })?;
    let scan = fonts.load_system_fonts();
    println!("system fonts: {scan:?}");
    let mut rasterizer = SubtitleRasterizer::new(
        fonts,
        RasterLimits {
            cache: RasterCacheLimits {
                cue_bitmaps_bytes: CUE_CACHE_BYTES,
                shaper_faces: SHAPER_FACES,
            },
            input: RasterInputLimits {
                max_chars_per_cue: MAX_CHARS_PER_CUE,
                max_cues_per_frame: MAX_CUES_PER_FRAME,
                max_ass_events_per_frame: MAX_CUES_PER_FRAME,
                max_canvas_side: MAX_CANVAS_SIDE,
            },
        },
    );
    let canvas = CanvasSize::new(FRAME_WIDTH, FRAME_HEIGHT).ok_or("canvas must not be empty")?;

    let styles = [
        ("youtube", SubtitleAppearance::youtube_like()),
        ("outline", SubtitleAppearance::outline_and_shadow()),
    ];
    let mut timeline = SubtitleTimeline::new();
    for (index, sample) in SAMPLES.iter().enumerate() {
        let (key, cue) = build_cue(&mut timeline, index, sample)?;
        for (style_name, appearance) in &styles {
            let cues = [RenderableCue::text(key, &cue)];
            let overlay = rasterizer.render(&RenderRequest {
                moment: subtitle_core::SubtitleTime::ZERO,
                cues: &cues,
                canvas,
                appearance,
                text_language: TextLanguagePreference::unknown(),
                ass: AssRenderOptions::default(),
            });
            let path = output_dir.join(format!("{}_{style_name}.png", sample.file_stem));
            write_over_gradient(&path, overlay)?;
            println!("wrote {}", path.display());
        }
    }
    println!("statistics: {:?}", rasterizer.statistics());
    Ok(())
}

/// Строит реплику образца и её ключ (идентификатор события выдаёт только таймлайн).
fn build_cue(
    timeline: &mut SubtitleTimeline,
    index: usize,
    sample: &Sample,
) -> Result<(CueKey, SubtitleTextCue), Box<dyn Error>> {
    let mut spans = Vec::new();
    for (line_index, line) in sample.lines.iter().enumerate() {
        if line_index > 0 {
            spans.push(TextSpan::line_break());
        }
        spans.push(TextSpan::text(*line, SpanStyle::default()));
    }
    let cue = SubtitleTextCue {
        text: StyledText::new(spans),
        placement: CuePlacement::default(),
    };
    let start = SubtitleTime::from_millis(i64::try_from(index)? * 10);
    let event = SubtitleEvent::new(
        start,
        SubtitleTime::from_millis(i64::try_from(index)? * 10 + 5),
        SubtitleLayer::DEFAULT,
        SubtitleContent::Text(cue.clone()),
    )?;
    let TimelineInsertOutcome::Inserted(id) = timeline.insert(event) else {
        return Err("sample event was not inserted".into());
    };
    Ok((CueKey::new(CueSourceId::new(1), id), cue))
}

/// Кладёт субтитры на серый градиентный кадр и сохраняет PNG.
fn write_over_gradient(path: &Path, overlay: &SubtitleOverlay) -> Result<(), Box<dyn Error>> {
    let width = overlay.canvas().width();
    let height = overlay.canvas().height();
    let subtitles = overlay.composite_rgba_over_transparent();
    let mut frame = Vec::with_capacity(subtitles.len());
    for (pixel_index, subtitle) in subtitles.as_chunks::<4>().0.iter().enumerate() {
        let row = u32::try_from(pixel_index)? / width;
        let column = u32::try_from(pixel_index)? % width;
        // Фон: диагональный градиент от тёмно-серого к светло-серому с шахматными полосами,
        // чтобы на нём были видны и светлые, и тёмные части субтитров.
        let base = 60 + (column * 120 / width + row * 60 / height);
        let stripe = if (row / 90 + column / 160).is_multiple_of(2) {
            0
        } else {
            18
        };
        let background = u8::try_from(base + stripe)?;
        let alpha = u32::from(subtitle.get(3).copied().unwrap_or(0));
        for channel in 0..3 {
            let color = u32::from(subtitle.get(channel).copied().unwrap_or(0));
            let mixed = (color * alpha + u32::from(background) * (255 - alpha) + 127) / 255;
            frame.push(u8::try_from(mixed)?);
        }
        frame.push(255);
    }
    let mut encoder = png::Encoder::new(BufWriter::new(File::create(path)?), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&frame)?;
    Ok(())
}
