//! Устойчивость: нет глифа, битая реплика, письменности без системных шрифтов, лимит лиц.

use super::*;
use crate::overlay::OverlayChange;
use cue_render::{CueRenderError, RasterizedCue};

/// Символ из частной области плоскости 16: его нет ни в одном шрифте.
const GLYPHLESS: char = '\u{10FFFD}';

fn youtube() -> SubtitleAppearance {
    SubtitleAppearance::youtube_like()
}

#[test]
fn character_missing_from_every_font_does_not_break_other_cues() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let broken_text = format!("плохой {GLYPHLESS} символ");
    let test_cues = cues(1, &["Нормальная реплика", broken_text.as_str()]);
    let overlay = render_once(&mut rasterizer, &test_cues, &youtube());

    let plates = of_kind(&overlay, SubtitleBitmapKind::Background);
    assert_eq!(plates.len(), 2, "both cues are drawn");
    assert_eq!(rasterizer.statistics().failed_cues, 0);
    assert!(rasterizer.statistics().missing_glyph_lookups > 0);
    // Вторая реплика содержит .notdef-квадрат, но остальные буквы нарисованы.
    let second_glyphs = ink_bounds(
        of_kind(&overlay, SubtitleBitmapKind::Glyph)
            .into_iter()
            .filter(|bitmap| {
                bitmap.origin().y < plates[1].origin().y + plates[1].height() as i32
                    && bitmap.origin().y >= plates[1].origin().y
            }),
    )
    .expect("glyphs of the second cue");
    assert!(second_glyphs.2 - second_glyphs.0 > 150);
}

/// Рисовальщик, который «ломается» на репликах с текстом `BROKEN`, остальные рисует честно.
fn painter_failing_on_broken(
    fonts: &mut SubtitleFontLibrary,
    faces: &mut FaceCache,
    cue: &SubtitleTextCue,
    appearance: &SubtitleAppearance,
    canvas: CanvasSize,
    language: crate::fonts::TextLanguagePreference,
) -> Result<RasterizedCue, CueRenderError> {
    if cue.text.plain_text().contains("BROKEN") {
        return Err(CueRenderError::Text(
            crate::text_renderer::TextRenderError::InvalidDecorationSize,
        ));
    }
    rasterize_cue(fonts, faces, cue, appearance, canvas, language)
}

#[test]
fn failing_cue_is_skipped_counted_and_the_rest_stays() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    rasterizer.painter = painter_failing_on_broken;
    let test_cues = cues(1, &["Первая хорошая", "BROKEN", "Третья хорошая"]);
    let overlay = render_once(&mut rasterizer, &test_cues, &youtube());

    assert_eq!(rasterizer.statistics().failed_cues, 1);
    assert_eq!(rasterizer.statistics().rasterized_cues, 2);
    let plates = of_kind(&overlay, SubtitleBitmapKind::Background);
    assert_eq!(plates.len(), 2, "the two good cues are on screen");
    // Хорошие реплики не пересекаются: битая не занимает места.
    let lower = ink_bounds([plates[0]]).expect("ink");
    let upper = ink_bounds([plates[1]]).expect("ink");
    assert!(upper.3 <= lower.1);

    // Сбой не кэшируется как «пустая реплика»: следующий кадр пробует снова.
    let again = render_once(&mut rasterizer, &cues(9, &["BROKEN"]), &youtube());
    assert!(again.is_empty());
    assert_eq!(rasterizer.statistics().failed_cues, 2);
    assert_eq!(again.change(), OverlayChange::Replaced);
}

#[test]
fn scripts_without_system_fonts_use_embedded_fonts_and_draw_no_notdef() {
    let texts = [
        "مرحبا بالعالم، كيف حالك؟",
        "שלום עולם, מה שלומך?",
        "नमस्ते दुनिया, आप कैसे हैं?",
        "გამარჯობა მსოფლიო, როგორ ხარ?",
        "Բարեւ աշխարհ, ինչպես ես?",
        "สวัสดีชาวโลก",
        "Γειά σου Κόσμε",
    ];
    for text in texts {
        // Библиотека без `load_system_fonts`: только встроенные шрифты.
        let mut rasterizer = rasterizer(ROOMY_LIMITS);
        let overlay = render_once(&mut rasterizer, &cues(1, &[text]), &youtube());
        assert!(!overlay.is_empty(), "{text}");
        assert_eq!(
            rasterizer.statistics().missing_glyph_lookups,
            0,
            "{text}: some characters have no glyph in embedded fonts"
        );
        let glyphs = ink_bounds(of_kind(&overlay, SubtitleBitmapKind::Glyph)).expect("ink");
        assert!(
            glyphs.2 - glyphs.0 > 60,
            "{text}: text must have some width"
        );
    }
}

#[test]
fn face_cache_is_trimmed_to_the_limit_between_frames() {
    let limits = RasterCacheLimits {
        cue_bitmaps_bytes: 64 * 1024 * 1024,
        shaper_faces: 1,
    };
    let mut rasterizer = rasterizer(limits);
    // Три письменности — три разных лица.
    for (index, text) in ["Hello", "שלום", "مرحبا", "გამარჯობა"].iter().enumerate()
    {
        render_once(&mut rasterizer, &cues(index as u64, &[text]), &youtube());
    }
    let statistics = rasterizer.statistics().face_cache;
    assert!(statistics.evictions > 0, "{statistics:?}");
    // Лица, которых нет в кэше, были подготовлены; часть из них вытеснена.
    assert_eq!(
        statistics.cached_faces as u64,
        statistics.misses - statistics.evictions
    );
    assert!(statistics.cached_faces < 6, "{statistics:?}");
    // Следующий кадр начинается с подрезки до лимита (1 лицо) и добавляет лишь лицо
    // латинского текста.
    render_once(&mut rasterizer, &cues(9, &["Hello"]), &youtube());
    assert!(rasterizer.statistics().face_cache.cached_faces <= 2);
}

#[test]
fn face_cache_serves_repeated_text_from_cache_when_the_limit_allows() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    render_once(&mut rasterizer, &cues(1, &["Первая"]), &youtube());
    let after_first = rasterizer.statistics().face_cache;
    render_once(&mut rasterizer, &cues(2, &["Вторая"]), &youtube());
    let after_second = rasterizer.statistics().face_cache;
    assert_eq!(
        after_second.misses, after_first.misses,
        "the face is prepared only once"
    );
    assert!(after_second.hits > after_first.hits);
    assert_eq!(after_second.evictions, 0);
}
