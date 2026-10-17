//! Функциональные тесты публичного растеризатора: реплики на входе — картинки на выходе.

mod ass;
mod ass_cache;
mod ass_placement_group;
mod ass_robustness;
mod ass_text_style;
mod attached_fonts;
mod caching;
mod font_fallback;
mod geometry;
mod language;
mod limits;
mod robustness;

use subtitle_core::{
    CuePlacement, SpanStyle, StyledText, SubtitleContent, SubtitleEvent, SubtitleLayer,
    SubtitleTextCue, SubtitleTime, SubtitleTimeline, TextSpan, TimelineInsertOutcome,
};

use super::*;
use crate::fonts::FontLookupCacheLimits;
use crate::overlay::{SubtitleBitmapKind, SubtitleOverlay};

pub(super) const WIDTH: u32 = 1280;
pub(super) const HEIGHT: u32 = 720;

/// Лимиты, с которыми кэши не мешают тестам.
pub(super) const ROOMY_LIMITS: RasterCacheLimits = RasterCacheLimits {
    cue_bitmaps_bytes: 64 * 1024 * 1024,
    shaper_faces: 16,
};

/// Лимиты входа, с которыми они не мешают тестам.
pub(super) const ROOMY_INPUT: RasterInputLimits = RasterInputLimits {
    max_chars_per_cue: 10_000,
    max_cues_per_frame: 64,
    max_ass_events_per_frame: 256,
    max_canvas_side: 16_384,
};

pub(super) fn canvas() -> CanvasSize {
    CanvasSize::new(WIDTH, HEIGHT).expect("canvas")
}

/// Растеризатор только со встроенными шрифтами.
pub(super) fn rasterizer(limits: RasterCacheLimits) -> SubtitleRasterizer {
    rasterizer_with_limits(RasterLimits {
        cache: limits,
        input: ROOMY_INPUT,
    })
}

/// Растеризатор со встроенными шрифтами и полным набором лимитов.
pub(super) fn rasterizer_with_limits(limits: RasterLimits) -> SubtitleRasterizer {
    let fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 4096,
    })
    .expect("embedded fonts");
    SubtitleRasterizer::new(fonts, limits)
}

/// Реплика теста: ключ и содержимое.
pub(super) struct TestCue {
    pub(super) key: CueKey,
    pub(super) cue: SubtitleTextCue,
}

/// Строит реплики из текстов (строки внутри текста разделяются `\n`) с настоящими
/// `SubtitleEventId`: их выдаёт только `SubtitleTimeline`.
pub(super) fn cues(source: u64, texts: &[&str]) -> Vec<TestCue> {
    let mut timeline = SubtitleTimeline::new();
    texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let cue = SubtitleTextCue {
                text: styled(text),
                placement: CuePlacement::default(),
            };
            let start = SubtitleTime::from_millis(index as i64 * 10);
            let event = SubtitleEvent::new(
                start,
                SubtitleTime::from_millis(index as i64 * 10 + 5),
                SubtitleLayer::DEFAULT,
                SubtitleContent::Text(cue.clone()),
            )
            .expect("interval");
            let TimelineInsertOutcome::Inserted(id) = timeline.insert(event) else {
                panic!("event must be inserted");
            };
            TestCue {
                key: CueKey::new(CueSourceId::new(source), id),
                cue,
            }
        })
        .collect()
}

/// Текст со спанами по строкам.
pub(super) fn styled(text: &str) -> StyledText {
    let mut spans = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            spans.push(TextSpan::line_break());
        }
        spans.push(TextSpan::text(line, SpanStyle::default()));
    }
    StyledText::new(spans)
}

/// Запрос из реплик теста.
pub(super) fn renderables(test_cues: &[TestCue]) -> Vec<RenderableCue<'_>> {
    test_cues
        .iter()
        .map(|test_cue| RenderableCue::text(test_cue.key, &test_cue.cue))
        .collect()
}

/// Рисует реплики на холсте по умолчанию и возвращает копию результата.
pub(super) fn render_once(
    rasterizer: &mut SubtitleRasterizer,
    test_cues: &[TestCue],
    appearance: &SubtitleAppearance,
) -> SubtitleOverlay {
    render_on(rasterizer, test_cues, appearance, canvas())
}

pub(super) fn render_on(
    rasterizer: &mut SubtitleRasterizer,
    test_cues: &[TestCue],
    appearance: &SubtitleAppearance,
    canvas: CanvasSize,
) -> SubtitleOverlay {
    render_in_language(
        rasterizer,
        test_cues,
        appearance,
        canvas,
        crate::fonts::TextLanguagePreference::unknown(),
    )
}

/// Рисует реплики с заданным языковым предпочтением текста.
pub(super) fn render_in_language(
    rasterizer: &mut SubtitleRasterizer,
    test_cues: &[TestCue],
    appearance: &SubtitleAppearance,
    canvas: CanvasSize,
    text_language: crate::fonts::TextLanguagePreference,
) -> SubtitleOverlay {
    let list = renderables(test_cues);
    rasterizer
        .render(&RenderRequest {
            cues: &list,
            canvas,
            appearance,
            text_language,
            ass: crate::ass::AssRenderOptions::default(),
        })
        .clone()
}

/// Рамка закрашенных пикселей картинок: `(x0, y0, x1, y1)`, правый и нижний края не включены.
pub(super) fn ink_bounds<'a>(
    bitmaps: impl IntoIterator<Item = &'a SubtitleBitmap>,
) -> Option<(i32, i32, i32, i32)> {
    let mut bounds: Option<(i32, i32, i32, i32)> = None;
    for bitmap in bitmaps {
        for row in 0..bitmap.height() {
            let cells = bitmap.coverage_row(row)?;
            for (column, value) in cells.iter().enumerate() {
                if *value == 0 {
                    continue;
                }
                let x = bitmap.origin().x + column as i32;
                let y = bitmap.origin().y + row as i32;
                bounds = Some(match bounds {
                    None => (x, y, x + 1, y + 1),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
                });
            }
        }
    }
    bounds
}

/// Картинки одного вида.
pub(super) fn of_kind(overlay: &SubtitleOverlay, kind: SubtitleBitmapKind) -> Vec<&SubtitleBitmap> {
    overlay
        .bitmaps()
        .iter()
        .filter(|bitmap| bitmap.kind() == kind)
        .collect()
}

/// Значение маски в точке холста (0 вне картинки).
pub(super) fn coverage_at(bitmap: &SubtitleBitmap, x: i32, y: i32) -> u8 {
    let (Ok(column), Ok(row)) = (
        u32::try_from(x - bitmap.origin().x),
        u32::try_from(y - bitmap.origin().y),
    ) else {
        return 0;
    };
    bitmap
        .coverage_row(row)
        .and_then(|cells| cells.get(column as usize).copied())
        .unwrap_or(0)
}
