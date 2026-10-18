//! Прогрев шрифтов: системное лицо готово ДО первой реплики, а лишнего не грузится.

use super::*;
use crate::fonts::TextLanguagePreference;
use crate::fonts::test_fonts::{TempFontDir, noto_regular_bytes, remap_letter_a};

fn youtube() -> SubtitleAppearance {
    SubtitleAppearance::youtube_like()
}

/// Растеризатор с одним «системным» лицом, покрывающим символ-приманку (в встроенных шрифтах
/// его нет). Возвращает растеризатор, приманку и каталог (должен жить до конца теста).
fn rasterizer_with_decoy_face(
    limits: RasterCacheLimits,
) -> (SubtitleRasterizer, String, TempFontDir) {
    let directory = TempFontDir::new();
    let mut font = noto_regular_bytes();
    let decoy = remap_letter_a(&mut font);
    directory.write("decoy.ttf", &font);
    let mut fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 4096,
    })
    .expect("embedded fonts");
    fonts.load_fonts_from_directory(directory.path());
    let rasterizer = SubtitleRasterizer::new(
        fonts,
        RasterLimits {
            cache: limits,
            input: ROOMY_INPUT,
        },
    );
    (rasterizer, decoy.to_string(), directory)
}

#[test]
fn warm_up_prepares_the_system_face_so_the_first_cue_does_not_load_it() {
    let (mut cold, decoy, _cold_dir) = rasterizer_with_decoy_face(ROOMY_LIMITS);
    let (mut warmed, _, _warmed_dir) = rasterizer_with_decoy_face(ROOMY_LIMITS);

    let outcome = warmed.warm_up_fonts(&decoy, TextLanguagePreference::unknown());
    assert_eq!(outcome, FontWarmUp::Warmed { faces_loaded: 1 });
    let after_warm_up = warmed.statistics().face_cache;
    assert_eq!(after_warm_up.misses, 1, "warm-up loads exactly one face");
    assert_eq!(after_warm_up.cached_faces, 1);

    let test_cues = cues(1, &[decoy.as_str()]);
    let warmed_overlay = render_once(&mut warmed, &test_cues, &youtube());
    let cold_overlay = render_once(&mut cold, &test_cues, &youtube());

    // Без прогрева реплика грузит и основное лицо, и системное; с прогревом — только основное.
    assert_eq!(cold.statistics().face_cache.misses, 2);
    assert_eq!(warmed.statistics().face_cache.misses, 2);
    assert_eq!(
        warmed.statistics().face_cache.misses - after_warm_up.misses,
        1,
        "the system face is already prepared, only the default face is new"
    );
    // Прогрев невидим для результата: картинки те же, что без него.
    assert!(!warmed_overlay.is_empty());
    assert_eq!(warmed_overlay.bitmaps().len(), cold_overlay.bitmaps().len());
}

#[test]
fn warm_up_of_text_covered_by_builtin_fonts_loads_nothing() {
    let (mut rasterizer, _, _dir) = rasterizer_with_decoy_face(ROOMY_LIMITS);
    let outcome = rasterizer.warm_up_fonts("Hello, мир! \u{1}", TextLanguagePreference::unknown());
    assert_eq!(outcome, FontWarmUp::NothingToWarm);
    let face_cache = rasterizer.statistics().face_cache;
    assert_eq!(face_cache.misses, 0);
    assert_eq!(face_cache.cached_faces, 0);
}

#[test]
fn warm_up_of_empty_or_uncovered_text_loads_nothing() {
    let (mut rasterizer, _, _dir) = rasterizer_with_decoy_face(ROOMY_LIMITS);
    assert_eq!(
        rasterizer.warm_up_fonts("", TextLanguagePreference::unknown()),
        FontWarmUp::NothingToWarm
    );
    // Символ из области, которой нет ни в одном шрифте.
    assert_eq!(
        rasterizer.warm_up_fonts("\u{10FFFD}", TextLanguagePreference::unknown()),
        FontWarmUp::NothingToWarm
    );
    assert_eq!(rasterizer.statistics().face_cache.cached_faces, 0);
}

#[test]
fn repeated_warm_up_does_not_reload_the_face() {
    let (mut rasterizer, decoy, _dir) = rasterizer_with_decoy_face(ROOMY_LIMITS);
    let language = TextLanguagePreference::unknown();
    assert_eq!(
        rasterizer.warm_up_fonts(&decoy, language),
        FontWarmUp::Warmed { faces_loaded: 1 }
    );
    assert_eq!(
        rasterizer.warm_up_fonts(&decoy, language),
        FontWarmUp::NothingToWarm
    );
    assert_eq!(rasterizer.statistics().face_cache.misses, 1);
}

#[test]
fn warm_up_respects_half_of_the_face_cache_limit() {
    // Лимит 1 лицо -> бюджет прогрева 0: прогрев не должен занимать место в кэше лиц.
    let limits = RasterCacheLimits {
        shaper_faces: 1,
        ..ROOMY_LIMITS
    };
    let (mut rasterizer, decoy, _dir) = rasterizer_with_decoy_face(limits);
    let outcome = rasterizer.warm_up_fonts(&decoy, TextLanguagePreference::unknown());
    assert_eq!(outcome, FontWarmUp::StoppedAtFaceBudget { faces_loaded: 0 });
    assert_eq!(rasterizer.statistics().face_cache.cached_faces, 0);
}
