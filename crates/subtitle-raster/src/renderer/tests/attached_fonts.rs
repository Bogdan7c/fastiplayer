//! Вложенные шрифты через публичный растеризатор: подготовка в фоне, установка, смена media.
//!
//! У реплики нет собственного семейства шрифта (стиль пользователя), поэтому вложенный шрифт
//! влияет на картинку через fallback по глифу: символ вне встроенных шрифтов рисуется только
//! при установленном вложении. Сравниваем реальные картинки, а не счётчики вызовов.

use std::sync::Arc;

use super::*;
use crate::fonts::test_fonts::{noto_regular_bytes, remap_letter_a};
use crate::fonts::{
    AttachedFontError, AttachedFontLimits, AttachedFontsClearOutcome, PreparedAttachedFont,
    PreparedAttachedFonts,
};

const LIMITS: AttachedFontLimits = AttachedFontLimits {
    max_font_bytes: 16 * 1024 * 1024,
    max_total_bytes: 64 * 1024 * 1024,
    max_faces: 8,
};

/// Шрифт с символом вне встроенных и сам символ.
fn font_with_private_char() -> (Vec<u8>, char) {
    let mut bytes = noto_regular_bytes();
    let remapped = remap_letter_a(&mut bytes);
    (bytes, remapped)
}

/// Содержимое картинок кадра: положение, размер и маска каждой.
fn pixels_of(overlay: &SubtitleOverlay) -> Vec<(i32, i32, u32, u32, Vec<u8>)> {
    overlay
        .bitmaps()
        .iter()
        .map(|bitmap| {
            (
                bitmap.origin().x,
                bitmap.origin().y,
                bitmap.width(),
                bitmap.height(),
                bitmap.coverage().to_vec(),
            )
        })
        .collect()
}

fn youtube() -> SubtitleAppearance {
    SubtitleAppearance::youtube_like()
}

#[test]
fn prepared_font_installed_off_thread_changes_the_picture_and_clear_restores_it() {
    let (font, private_char) = font_with_private_char();
    let text = format!("x{private_char}y");
    let test_cues = cues(1, &[text.as_str()]);

    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let bare = pixels_of(&render_once(&mut rasterizer, &test_cues, &youtube()));

    // Тяжёлая фаза — в другом потоке, установка — в «UI».
    let bytes: Arc<[u8]> = Arc::from(font);
    let prepared = std::thread::spawn(move || PreparedAttachedFonts::prepare([bytes], &LIMITS))
        .join()
        .expect("worker thread");
    let report = rasterizer.install_prepared_attached_fonts(prepared, &LIMITS);
    assert_eq!(report.registered, 1);
    assert!(report.rejected.is_empty());

    // Тот же ключ реплики: картинка обязана пересобраться (кэш сброшен), и в ней есть глиф.
    let attached = pixels_of(&render_once(&mut rasterizer, &test_cues, &youtube()));
    assert_ne!(attached, bare, "the attached glyph must change the picture");

    let cleared = rasterizer.clear_attached_fonts();
    assert!(matches!(
        cleared,
        AttachedFontsClearOutcome::Cleared { faces: 1, .. }
    ));
    let after_clear = pixels_of(&render_once(&mut rasterizer, &test_cues, &youtube()));
    assert_eq!(
        after_clear, bare,
        "clear must fall back to the bare picture"
    );
}

#[test]
fn clear_with_nothing_attached_keeps_caches_and_the_previous_frame() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let test_cues = cues(1, &["hello"]);
    let first = render_once(&mut rasterizer, &test_cues, &youtube());
    let entries = rasterizer.statistics().cue_cache.entries;

    assert_eq!(
        rasterizer.clear_attached_fonts(),
        AttachedFontsClearOutcome::NothingAttached
    );
    assert_eq!(rasterizer.statistics().cue_cache.entries, entries);
    let repeat = render_once(&mut rasterizer, &test_cues, &youtube());
    assert_eq!(repeat.change(), OverlayChange::Unchanged);
    assert_eq!(repeat.revision(), first.revision());
}

#[test]
fn install_of_rejected_or_duplicate_fonts_keeps_caches() {
    let (font, _) = font_with_private_char();
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    rasterizer
        .register_attached_font(Arc::from(font.clone()), &LIMITS)
        .expect("valid");
    let test_cues = cues(1, &["hello"]);
    let first = render_once(&mut rasterizer, &test_cues, &youtube());

    // Дубликат: библиотека и кэши не меняются.
    let prepared = PreparedAttachedFont::prepare(Arc::from(font), &LIMITS).expect("valid");
    rasterizer
        .install_prepared_attached_font(prepared, &LIMITS)
        .expect("duplicate");
    // Мусор отвергается ещё при подготовке.
    let garbage = PreparedAttachedFonts::prepare([Arc::<[u8]>::from(vec![3_u8; 512])], &LIMITS);
    assert_eq!(garbage.rejected()[0].error, AttachedFontError::Unparseable);
    let report = rasterizer.install_prepared_attached_fonts(garbage, &LIMITS);
    assert_eq!(report.registered, 0);
    assert_eq!(report.rejected.len(), 1);

    let repeat = render_once(&mut rasterizer, &test_cues, &youtube());
    assert_eq!(repeat.change(), OverlayChange::Unchanged);
    assert_eq!(repeat.revision(), first.revision());
}
