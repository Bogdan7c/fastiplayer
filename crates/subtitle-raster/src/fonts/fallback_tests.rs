//! Тесты выбора системного fallback-лица: вес/наклон основного лица и непрерывность по лицам.

use super::face::{FaceId, FaceOrigin, FaceSource, SystemFileKey};
use super::glyph_lookup::GlyphLookup;
use super::language::TextLanguagePreference;
use super::library::SubtitleFontLibrary;
use super::lookup_cache::FontLookupCacheLimits;
use super::style::{FontRequest, FontStyle, FontWeightClass};
use super::test_fonts::{
    TempFontDir, add_meta_dlng, noto_regular_bytes, remap_letter_a, rename_family, set_os2_weight,
};
use subtitle_core::SubtitleLanguage;

/// Японская реплика из живой приёмки.
const JAPANESE_CUE: &str = "多くの罪なき人の血で汚れている";

/// Машинно-локальная диагностика: какое лицо выбрано для каждого символа японской реплики
/// на реальных системных шрифтах. Запуск:
/// `cargo test -p subtitle-raster --lib fallback_tests::print_real_system_choices -- --ignored --nocapture`
#[test]
#[ignore = "зависит от шрифтов машины владельца; печатает выбор лиц"]
fn print_real_system_choices() {
    let mut fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 4096,
    })
    .expect("embedded fonts");
    let report = fonts.load_system_fonts();
    println!("system faces: {}", report.faces_registered);
    let primary = fonts
        .resolve_family(&FontRequest {
            family: None,
            weight: FontWeightClass::Regular,
            style: FontStyle::Normal,
        })
        .face;
    let japanese = TextLanguagePreference::preferred(&SubtitleLanguage::parse("ja").expect("tag"));
    println!("language hint: ja");
    for ch in JAPANESE_CUE.chars() {
        match fonts.glyph_for_char(primary, ch, japanese) {
            GlyphLookup::Found { face, .. } => println!("{ch} {}", describe(&fonts, face)),
            other => println!("{ch} {other:?}"),
        }
    }
}

/// Строка «семейство, вес OS/2, файл#индекс» для диагностики.
fn describe(fonts: &SubtitleFontLibrary, face: FaceId) -> String {
    let Some(description) = fonts.describe_face(face) else {
        return "<unknown face>".to_owned();
    };
    let file = fonts
        .face(face)
        .and_then(|loaded| match &loaded.source {
            FaceSource::System {
                file_key: SystemFileKey::Path(path),
                ..
            } => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            _ => None,
        })
        .unwrap_or_else(|| "-".to_owned());
    format!(
        "{} {:?}/{:?} {}#{}",
        description.family,
        description.weight,
        description.style,
        file,
        description.collection_index
    )
}

/// Библиотека, чьи «системные» лица — файлы каталога. Все файлы — копия Noto Sans Regular
/// с перенесённой `'A'` (общий символ-приманка) и заданным весом OS/2.
fn library_with_weights(files: &[(&str, u16)]) -> (SubtitleFontLibrary, char, TempFontDir) {
    let dir = TempFontDir::new();
    let mut remapped_char = None;
    for (name, weight) in files {
        let mut bytes = noto_regular_bytes();
        remapped_char = Some(remap_letter_a(&mut bytes));
        set_os2_weight(&mut bytes, *weight);
        dir.write(name, &bytes);
    }
    let mut fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 64,
    })
    .expect("embedded fonts");
    fonts.load_fonts_from_directory(dir.path());
    (fonts, remapped_char.expect("at least one font"), dir)
}

fn primary_for(fonts: &SubtitleFontLibrary, weight: FontWeightClass) -> FaceId {
    fonts
        .resolve_family(&FontRequest {
            family: None,
            weight,
            style: FontStyle::Normal,
        })
        .face
}

fn chosen_weight(fonts: &mut SubtitleFontLibrary, primary: FaceId, ch: char) -> u16 {
    match fonts.glyph_for_char(primary, ch, TextLanguagePreference::unknown()) {
        GlyphLookup::Found { face, .. } => {
            let description = fonts.describe_face(face).expect("face");
            assert_eq!(description.origin, FaceOrigin::System);
            description.os2_weight
        }
        other => panic!("glyph must be found in a system face, got {other:?}"),
    }
}

#[test]
fn system_fallback_picks_the_face_closest_to_the_requested_weight() {
    // Имена файлов упорядочены так, чтобы «первый по индексу» был самым жирным.
    let (mut fonts, ch, _dir) = library_with_weights(&[
        ("a-black.ttf", 900),
        ("b-light.ttf", 300),
        ("c-regular.ttf", 400),
        ("d-bold.ttf", 700),
    ]);
    let regular = primary_for(&fonts, FontWeightClass::Regular);
    let bold = primary_for(&fonts, FontWeightClass::Bold);
    assert_eq!(chosen_weight(&mut fonts, regular, ch), 400);
    assert_eq!(chosen_weight(&mut fonts, bold, ch), 700);
}

#[test]
fn system_fallback_takes_the_nearest_weight_when_the_exact_one_is_missing() {
    let (mut fonts, ch, _dir) =
        library_with_weights(&[("a.ttf", 100), ("b.ttf", 500), ("c.ttf", 800)]);
    let regular = primary_for(&fonts, FontWeightClass::Regular);
    let bold = primary_for(&fonts, FontWeightClass::Bold);
    assert_eq!(chosen_weight(&mut fonts, regular, ch), 500);
    assert_eq!(chosen_weight(&mut fonts, bold, ch), 800);
}

#[test]
fn system_fallback_tie_is_broken_by_file_path_not_scan_order() {
    let (mut fonts, ch, _dir) = library_with_weights(&[("b-copy.ttf", 400), ("a-copy.ttf", 400)]);
    let regular = primary_for(&fonts, FontWeightClass::Regular);
    let GlyphLookup::Found { face, .. } =
        fonts.glyph_for_char(regular, ch, TextLanguagePreference::unknown())
    else {
        panic!("glyph must be found");
    };
    let file = fonts.face(face).and_then(|loaded| match &loaded.source {
        FaceSource::System {
            file_key: SystemFileKey::Path(path),
            ..
        } => path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
        _ => None,
    });
    assert_eq!(file.as_deref(), Some("a-copy.ttf"));
}

#[test]
fn used_system_face_is_reused_even_when_another_face_ranks_better() {
    let (mut fonts, ch, _dir) = library_with_weights(&[("a-black.ttf", 900), ("b.ttf", 400)]);
    let regular = primary_for(&fonts, FontWeightClass::Regular);
    let GlyphLookup::Found { face: best, glyph } =
        fonts.glyph_for_char(regular, ch, TextLanguagePreference::unknown())
    else {
        panic!("glyph must be found");
    };
    assert_eq!(fonts.describe_face(best).expect("face").os2_weight, 400);
    // Лицо Black уже использовалось в реплике: символ остаётся в нём (libass `font->faces`).
    let black = fonts
        .system_face_ids
        .iter()
        .copied()
        .find(|face| {
            fonts
                .describe_face(*face)
                .is_some_and(|d| d.os2_weight == 900)
        })
        .expect("black face");
    let reused = fonts.glyph_in_used_system_faces(&[black], ch);
    assert_eq!(reused, Some((black, glyph)));
    // Пустой список, не системное лицо и символ, которого в лице нет: ничего не подставляем.
    assert_eq!(fonts.glyph_in_used_system_faces(&[], ch), None);
    assert_eq!(fonts.glyph_in_used_system_faces(&[regular], 'A'), None);
    assert_eq!(fonts.glyph_in_used_system_faces(&[black], 'あ'), None);
}

/// Библиотека, чьи «системные» лица — копии Noto Sans с общей `'A'`-приманкой, заданным именем
/// семейства (ровно 9 символов), весом OS/2 и, при желании, `meta`/`dlng`.
fn library_with_languages(files: &[FakeFace<'_>]) -> (SubtitleFontLibrary, char, TempFontDir) {
    let dir = TempFontDir::new();
    let mut decoy = None;
    for face in files {
        let mut bytes = noto_regular_bytes();
        decoy = Some(remap_letter_a(&mut bytes));
        rename_family(&mut bytes, "Noto Sans", face.family);
        set_os2_weight(&mut bytes, face.weight);
        let bytes = match face.meta_languages {
            Some(languages) => add_meta_dlng(&bytes, languages),
            None => bytes,
        };
        dir.write(face.file, &bytes);
    }
    let mut fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 64,
    })
    .expect("embedded fonts");
    fonts.load_fonts_from_directory(dir.path());
    (fonts, decoy.expect("at least one font"), dir)
}

/// Описание поддельного системного лица для языковых тестов.
struct FakeFace<'a> {
    file: &'a str,
    /// Семейство из ровно 9 символов (длина «Noto Sans»).
    family: &'a str,
    weight: u16,
    meta_languages: Option<&'a str>,
}

fn named(file: &'static str, family: &'static str) -> FakeFace<'static> {
    FakeFace {
        file,
        family,
        weight: 400,
        meta_languages: None,
    }
}

fn language_of(tag: &str) -> TextLanguagePreference {
    TextLanguagePreference::preferred(&SubtitleLanguage::parse(tag).expect("tag"))
}

/// Семейство выбранного системного лица.
fn chosen_family(
    fonts: &mut SubtitleFontLibrary,
    ch: char,
    weight: FontWeightClass,
    language: TextLanguagePreference,
) -> String {
    let primary = primary_for(fonts, weight);
    match fonts.glyph_for_char(primary, ch, language) {
        GlyphLookup::Found { face, .. } => {
            fonts.describe_face(face).expect("face").family.to_owned()
        }
        other => panic!("glyph must be found in a system face, got {other:?}"),
    }
}

fn regional_noto_faces() -> Vec<FakeFace<'static>> {
    vec![
        named("a-hk.ttf", "Noto A HK"),
        named("b-jp.ttf", "Noto A JP"),
        named("c-kr.ttf", "Noto A KR"),
        named("d-sc.ttf", "Noto A SC"),
        named("e-tc.ttf", "Noto A TC"),
    ]
}

#[test]
fn language_hint_picks_the_regional_face_even_when_another_sorts_first() {
    let (mut fonts, ch, _dir) = library_with_languages(&regional_noto_faces());
    let regular = FontWeightClass::Regular;
    let pick = |fonts: &mut SubtitleFontLibrary, tag: &str| {
        chosen_family(fonts, ch, regular, language_of(tag))
    };
    assert_eq!(pick(&mut fonts, "ja"), "Noto A JP");
    assert_eq!(pick(&mut fonts, "ja-JP"), "Noto A JP");
    assert_eq!(pick(&mut fonts, "ko"), "Noto A KR");
    assert_eq!(pick(&mut fonts, "zh-Hans"), "Noto A SC");
    assert_eq!(pick(&mut fonts, "zh"), "Noto A SC");
    assert_eq!(pick(&mut fonts, "zh-TW"), "Noto A TC");
    assert_eq!(pick(&mut fonts, "zh-Hant"), "Noto A TC");
    assert_eq!(pick(&mut fonts, "zh-HK"), "Noto A HK");
}

#[test]
fn unknown_or_non_cjk_language_keeps_the_previous_deterministic_order() {
    let (mut fonts, ch, _dir) = library_with_languages(&regional_noto_faces());
    let regular = FontWeightClass::Regular;
    let unknown = chosen_family(&mut fonts, ch, regular, TextLanguagePreference::unknown());
    assert_eq!(unknown, "Noto A HK", "alphabetical family, as before");
    let english = chosen_family(&mut fonts, ch, regular, language_of("en"));
    assert_eq!(english, unknown, "a non-CJK language prefers nothing");
}

#[test]
fn weight_still_decides_within_the_same_language() {
    let (mut fonts, ch, _dir) = library_with_languages(&[
        FakeFace {
            weight: 300,
            ..named("a-jp-light.ttf", "Noto A JP")
        },
        FakeFace {
            weight: 700,
            ..named("b-jp-bold.ttf", "Noto A JP")
        },
        FakeFace {
            weight: 400,
            ..named("c-hk-regular.ttf", "Noto A HK")
        },
    ]);
    let japanese = language_of("ja");
    let regular = primary_for(&fonts, FontWeightClass::Regular);
    let bold = primary_for(&fonts, FontWeightClass::Bold);
    let weight_of = |fonts: &mut SubtitleFontLibrary, primary| match fonts
        .glyph_for_char(primary, ch, japanese)
    {
        GlyphLookup::Found { face, .. } => {
            let description = fonts.describe_face(face).expect("face");
            (description.family.to_owned(), description.os2_weight)
        }
        other => panic!("glyph must be found, got {other:?}"),
    };
    // Язык выше веса: HK 400 ближе к Regular, но японское лицо всё равно выигрывает.
    assert_eq!(
        weight_of(&mut fonts, regular),
        ("Noto A JP".to_owned(), 300)
    );
    assert_eq!(weight_of(&mut fonts, bold), ("Noto A JP".to_owned(), 700));
}

#[test]
fn undeclared_face_beats_a_face_of_a_different_cjk_language() {
    let (mut fonts, ch, _dir) = library_with_languages(&[
        named("a-jp.ttf", "Noto A JP"),
        named("b-none.ttf", "Noto A Xx"),
    ]);
    let regular = FontWeightClass::Regular;
    assert_eq!(
        chosen_family(&mut fonts, ch, regular, language_of("zh-Hans")),
        "Noto A Xx"
    );
    assert_eq!(
        chosen_family(&mut fonts, ch, regular, language_of("ja")),
        "Noto A JP"
    );
}

#[test]
fn meta_dlng_is_authoritative_over_the_family_name() {
    let (mut fonts, ch, _dir) = library_with_languages(&[
        // Имя говорит «HK», но шрифт сам объявляет японский.
        FakeFace {
            meta_languages: Some("Jpan"),
            ..named("a.ttf", "Noto A HK")
        },
        FakeFace {
            meta_languages: Some("zh-Hant-HK"),
            ..named("b.ttf", "Noto A Aa")
        },
        FakeFace {
            meta_languages: Some("Hans"),
            ..named("c.ttf", "Noto A Bb")
        },
    ]);
    let regular = FontWeightClass::Regular;
    assert_eq!(
        chosen_family(&mut fonts, ch, regular, language_of("ja")),
        "Noto A HK"
    );
    assert_eq!(
        chosen_family(&mut fonts, ch, regular, language_of("zh-HK")),
        "Noto A Aa"
    );
    assert_eq!(
        chosen_family(&mut fonts, ch, regular, language_of("zh-CN")),
        "Noto A Bb"
    );
}

#[test]
fn lookup_cache_does_not_leak_a_pick_between_languages() {
    let (mut fonts, ch, _dir) = library_with_languages(&regional_noto_faces());
    let regular = FontWeightClass::Regular;
    let japanese = chosen_family(&mut fonts, ch, regular, language_of("ja"));
    let hong_kong = chosen_family(&mut fonts, ch, regular, language_of("zh-HK"));
    let japanese_again = chosen_family(&mut fonts, ch, regular, language_of("ja"));
    assert_eq!(japanese, "Noto A JP");
    assert_eq!(hong_kong, "Noto A HK");
    assert_eq!(japanese_again, japanese);
    let statistics = fonts.statistics();
    assert_eq!(
        statistics.lookup_cache_misses, 2,
        "ja and zh-HK are separate keys"
    );
    assert_eq!(
        statistics.lookup_cache_hits, 1,
        "the second ja comes from the cache"
    );
}
