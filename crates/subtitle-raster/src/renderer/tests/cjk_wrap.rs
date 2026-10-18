//! Перенос CJK-текста без пробелов через публичный `render`: простые форматы переносят,
//! ASS остаётся как libass.

use super::ass::{glyph_ink, render_ass, single};
use super::*;

/// Число строк текста: объединённые по вертикали рамки картинок букв.
fn text_line_count(overlay: &SubtitleOverlay) -> usize {
    let mut spans: Vec<(i32, i32)> = of_kind(overlay, SubtitleBitmapKind::Glyph)
        .into_iter()
        .filter_map(|bitmap| ink_bounds([bitmap]))
        .map(|(_, top, _, bottom)| (top, bottom))
        .collect();
    spans.sort_unstable();
    let mut lines = 0;
    let mut covered_to = i32::MIN;
    for (top, bottom) in spans {
        if top >= covered_to {
            lines += 1;
        }
        covered_to = covered_to.max(bottom);
    }
    lines
}

fn cjk_sentence() -> String {
    "这是一个很长的中文字幕，没有任何空格。".repeat(6) // 108+ символов
}

#[test]
fn srt_style_cjk_cue_wraps_into_lines_that_fit_the_canvas() {
    let text = cjk_sentence();
    assert!(text.chars().count() >= 108);
    let overlay = render_once(
        &mut rasterizer(ROOMY_LIMITS),
        &cues(1, &[&text]),
        &SubtitleAppearance::youtube_like(),
    );
    assert!(
        text_line_count(&overlay) >= 2,
        "lines: {}",
        text_line_count(&overlay)
    );
    // Раньше это была одна строка, обрезанная краями холста (ink от x = 0 до x = 1280).
    let (left, _, right, bottom) = glyph_ink(&overlay);
    assert!(
        left > 0 && right < i32::try_from(WIDTH).expect("width"),
        "ink {left}..{right}"
    );
    assert!(bottom <= i32::try_from(HEIGHT).expect("height"));
}

#[test]
fn ass_cue_with_the_same_cjk_text_stays_on_one_line() {
    let overlay = render_ass(&mut rasterizer(ROOMY_LIMITS), &single(&cjk_sentence()));
    assert_eq!(text_line_count(&overlay), 1);
}

#[test]
fn latin_srt_cue_with_spaces_still_wraps_at_spaces() {
    let text = "Wrapping at spaces must keep working for ordinary text. ".repeat(4);
    let overlay = render_once(
        &mut rasterizer(ROOMY_LIMITS),
        &cues(2, &[&text]),
        &SubtitleAppearance::youtube_like(),
    );
    assert!(text_line_count(&overlay) >= 2);
}
