//! Язык текста доходит до выбора системного лица: регион CJK-шрифта выбирает язык, а не алфавит.

use subtitle_core::SubtitleLanguage;

use super::*;
use crate::fonts::TextLanguagePreference;
use crate::fonts::test_fonts::{
    TempFontDir, noto_bold_bytes, noto_regular_bytes, remap_letter_a, rename_family, set_os2_weight,
};
use crate::overlay::OverlayChange;

fn language(tag: &str) -> TextLanguagePreference {
    TextLanguagePreference::preferred(&SubtitleLanguage::parse(tag).expect("tag"))
}

/// Растеризатор с двумя «системными» лицами, у которых общий символ-приманка и РАЗНЫЕ контуры:
/// `JP` — обычное начертание, `HK` — жирное (вес OS/2 у обоих 400, чтобы решал только язык).
/// Файл HK назван так, что по алфавиту идёт первым, как у настоящих Noto Sans CJK.
fn rasterizer_with_regional_faces() -> (SubtitleRasterizer, String, TempFontDir) {
    let directory = TempFontDir::new();
    let mut regular = noto_regular_bytes();
    let decoy = remap_letter_a(&mut regular);
    rename_family(&mut regular, "Noto Sans", "Noto A JP");
    set_os2_weight(&mut regular, 400);
    directory.write("b-jp.ttf", &regular);
    let mut bold = noto_bold_bytes();
    remap_letter_a(&mut bold);
    rename_family(&mut bold, "Noto Sans", "Noto A HK");
    set_os2_weight(&mut bold, 400);
    directory.write("a-hk.ttf", &bold);

    let mut fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 4096,
    })
    .expect("embedded fonts");
    fonts.load_fonts_from_directory(directory.path());
    let rasterizer = SubtitleRasterizer::new(
        fonts,
        RasterLimits {
            cache: ROOMY_LIMITS,
            input: ROOMY_INPUT,
        },
    );
    (rasterizer, decoy.to_string(), directory)
}

fn masks(overlay: &SubtitleOverlay) -> Vec<Vec<u8>> {
    overlay
        .bitmaps()
        .iter()
        .filter(|bitmap| bitmap.kind() == SubtitleBitmapKind::Glyph)
        .map(|bitmap| bitmap.coverage().to_vec())
        .collect()
}

#[test]
fn language_hint_changes_which_regional_face_is_drawn() {
    let (mut rasterizer, decoy, _dir) = rasterizer_with_regional_faces();
    let test_cues = cues(1, &[&decoy]);
    let appearance = SubtitleAppearance::youtube_like();
    let draw = |rasterizer: &mut SubtitleRasterizer, preference| {
        masks(&render_in_language(
            rasterizer,
            &test_cues,
            &appearance,
            canvas(),
            preference,
        ))
    };

    let japanese = draw(&mut rasterizer, language("ja"));
    let hong_kong = draw(&mut rasterizer, language("zh-HK"));
    let japanese_again = draw(&mut rasterizer, language("ja"));
    let unknown = draw(&mut rasterizer, TextLanguagePreference::unknown());

    assert!(!japanese.is_empty());
    assert_ne!(japanese, hong_kong, "the JP face and the HK face differ");
    assert_eq!(
        japanese, japanese_again,
        "ja is stable after another language"
    );
    // Без языка прежний порядок: алфавитно первое семейство (HK).
    assert_eq!(unknown, hong_kong);
}

#[test]
fn changing_language_redraws_but_the_same_language_reuses_the_frame() {
    let (mut rasterizer, decoy, _dir) = rasterizer_with_regional_faces();
    let test_cues = cues(1, &[&decoy]);
    let appearance = SubtitleAppearance::youtube_like();
    let mut render = |preference| {
        render_in_language(
            &mut rasterizer,
            &test_cues,
            &appearance,
            canvas(),
            preference,
        )
    };

    render(language("ja"));
    let same = render(language("ja"));
    assert_eq!(same.change(), OverlayChange::Unchanged);
    let other = render(language("zh-HK"));
    assert_ne!(other.change(), OverlayChange::Unchanged);
    let statistics = rasterizer.statistics();
    assert_eq!(statistics.unchanged_frames, 1);
    assert_eq!(statistics.rendered_frames, 2);
    assert_eq!(
        statistics.rasterized_cues, 2,
        "the cue cache must not serve a ja mask to zh-HK"
    );
}
