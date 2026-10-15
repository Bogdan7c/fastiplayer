//! Сквозные тесты библиотеки шрифтов: встроенные данные, fallback по глифам, вложенные шрифты,
//! метрики, системные шрифты. Проверяют результат (какое лицо и глиф выбраны), а не вызовы.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "тестовый crate: паника — штатный способ провалить тест"
)]

use std::sync::Arc;

use skrifa::{FontRef, MetadataProvider};
use subtitle_raster::{
    AscentDescentSource, AttachedFontError, AttachedFontLimits, EMBEDDED_FONTS, FaceId, FaceOrigin,
    FamilyMatch, FontLookupCacheLimits, FontRequest, FontStyle, FontWeightClass, GlyphId,
    GlyphLookup, SubtitleFontLibrary, SyntheticBold, SyntheticItalic, SystemFontScanStatus,
};

fn library_with_cache(max_cached_chars: usize) -> SubtitleFontLibrary {
    SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits { max_cached_chars }).unwrap()
}

fn default_request(weight: FontWeightClass) -> FontRequest<'static> {
    FontRequest {
        family: None,
        weight,
        style: FontStyle::Normal,
    }
}

/// Основное лицо простых субтитров (встроенный Noto Sans Regular).
fn primary_face(library: &SubtitleFontLibrary) -> FaceId {
    library
        .resolve_family(&default_request(FontWeightClass::Regular))
        .face
}

/// Семейство лица, в котором найден глиф; тест падает, если глиф не найден.
fn family_of_found_glyph(library: &mut SubtitleFontLibrary, text_char: char) -> String {
    let primary = primary_face(library);
    match library.glyph_for_char(primary, text_char) {
        GlyphLookup::Found { face, glyph } => {
            assert_ne!(glyph, GlyphId::NOTDEF, "{text_char:?}");
            library.describe_face(face).unwrap().family.to_owned()
        }
        other => panic!("{text_char:?}: expected a glyph, got {other:?}"),
    }
}

#[test]
fn every_embedded_font_parses_and_has_a_cmap() {
    // Библиотека создаётся только если разобрались все 22 файла.
    let library = library_with_cache(8);
    assert_eq!(library.statistics().embedded_faces, EMBEDDED_FONTS.len());
    for font in &EMBEDDED_FONTS {
        let parsed = FontRef::from_index(font.data, 0).unwrap();
        let mapped_any_character = (0x20_u32..0x20000)
            .filter_map(char::from_u32)
            .any(|candidate| parsed.charmap().map(candidate).is_some());
        assert!(mapped_any_character, "{} must map characters", font.family);
    }
}

#[test]
fn latin_cyrillic_greek_come_from_primary_noto_sans() {
    let mut library = library_with_cache(64);
    for text in ["Привет", "Hello", "Γειά"] {
        for ch in text.chars() {
            assert_eq!(
                family_of_found_glyph(&mut library, ch),
                "Noto Sans",
                "{ch:?}"
            );
        }
    }
}

#[test]
fn scripts_fall_back_to_matching_embedded_alphabet_fonts() {
    let mut library = library_with_cache(256);
    let cases = [
        ("مرحبا", "Noto Sans Arabic"),
        ("שלום", "Noto Sans Hebrew"),
        ("नमस्ते", "Noto Sans Devanagari"),
        ("ສະບາຍດີ", "Noto Sans Lao"),
        ("ไทย", "Noto Sans Thai"),
        ("ქართული", "Noto Sans Georgian"),
        ("Հայ", "Noto Sans Armenian"),
    ];
    for (text, expected_family) in cases {
        for ch in text.chars() {
            let family = family_of_found_glyph(&mut library, ch);
            // Объединяющие знаки могут лежать и в общем наборе; проверяем, что это не «тофу».
            assert!(
                family == expected_family || family.starts_with("Noto Sans"),
                "{ch:?} -> {family}"
            );
        }
        let first = text.chars().next().unwrap();
        assert_eq!(
            family_of_found_glyph(&mut library, first),
            expected_family,
            "{text}"
        );
    }
}

#[test]
fn unmapped_character_is_missing_counted_and_uses_primary_notdef() {
    let mut library = library_with_cache(8);
    let primary = primary_face(&library);
    assert_eq!(library.statistics().missing_glyph_lookups, 0);
    // U+10FFFD — private use plane 16: нет ни в одном встроенном шрифте.
    let lookup = library.glyph_for_char(primary, '\u{10FFFD}');
    assert_eq!(
        lookup,
        GlyphLookup::Missing {
            face: primary,
            notdef: GlyphId::NOTDEF
        }
    );
    assert_eq!(library.statistics().missing_glyph_lookups, 1);
    // Повтор (теперь из кэша) тоже считается пропуском и даёт тот же результат.
    assert_eq!(library.glyph_for_char(primary, '\u{10FFFD}'), lookup);
    assert_eq!(library.statistics().missing_glyph_lookups, 2);
}

#[test]
fn control_characters_resolve_to_primary_without_counting_as_missing() {
    let mut library = library_with_cache(8);
    let primary = primary_face(&library);
    for ch in ['\u{0}', '\n', '\u{1F}'] {
        assert_eq!(
            library.glyph_for_char(primary, ch),
            GlyphLookup::ControlCharacter { face: primary }
        );
    }
    let stats = library.statistics();
    assert_eq!((stats.missing_glyph_lookups, stats.cached_lookups), (0, 0));
}

#[test]
fn repeated_lookup_hits_cache_and_returns_same_result() {
    let mut library = library_with_cache(16);
    let primary = primary_face(&library);
    let first = library.glyph_for_char(primary, 'ש');
    let before = library.statistics();
    let second = library.glyph_for_char(primary, 'ש');
    let after = library.statistics();
    assert_eq!(first, second);
    assert!(matches!(first, GlyphLookup::Found { .. }));
    assert_eq!(after.lookup_cache_hits, before.lookup_cache_hits + 1);
    assert_eq!(after.lookup_cache_misses, before.lookup_cache_misses);
}

#[test]
fn cache_never_exceeds_its_limit_and_results_stay_correct() {
    let mut library = library_with_cache(3);
    let primary = primary_face(&library);
    let expected: Vec<GlyphLookup> = "abcdefgh"
        .chars()
        .map(|ch| library.glyph_for_char(primary, ch))
        .collect();
    assert!(library.statistics().cached_lookups <= 3);
    // После вытеснения результаты те же.
    let again: Vec<GlyphLookup> = "abcdefgh"
        .chars()
        .map(|ch| library.glyph_for_char(primary, ch))
        .collect();
    assert_eq!(expected, again);
    assert!(
        expected
            .iter()
            .all(|lookup| matches!(lookup, GlyphLookup::Found { .. }))
    );
    assert_eq!(library.statistics().cached_lookups, 3);
}

#[test]
fn default_request_resolves_to_noto_sans_with_requested_weight() {
    let library = library_with_cache(1);
    let regular = library.resolve_family(&default_request(FontWeightClass::Regular));
    let bold = library.resolve_family(&default_request(FontWeightClass::Bold));
    assert_eq!(
        library.describe_face(regular.face).unwrap().weight,
        FontWeightClass::Regular
    );
    assert_eq!(
        library.describe_face(bold.face).unwrap().weight,
        FontWeightClass::Bold
    );
    assert_ne!(regular.face, bold.face);
    for found in [regular, bold] {
        assert_eq!(found.origin, FaceOrigin::Embedded);
        assert_eq!(found.family_match, FamilyMatch::DefaultFallback);
        assert_eq!(found.synthetic_bold, SyntheticBold::NotNeeded);
        assert_eq!(found.synthetic_italic, SyntheticItalic::NotNeeded);
    }
}

#[test]
fn italic_request_needs_synthetic_italic_and_unknown_family_falls_back() {
    let library = library_with_cache(1);
    let request = FontRequest {
        family: Some("No Such Family 42"),
        weight: FontWeightClass::Regular,
        style: FontStyle::Italic,
    };
    let found = library.resolve_family(&request);
    assert_eq!(found.family_match, FamilyMatch::DefaultFallback);
    assert_eq!(found.synthetic_italic, SyntheticItalic::Needed);
}

#[test]
fn bold_request_for_regular_only_family_needs_synthetic_bold() {
    let library = library_with_cache(1);
    // Имя семейства сравнивается без учёта регистра; у Arabic есть только Regular.
    let request = FontRequest {
        family: Some("noto sans arabic"),
        weight: FontWeightClass::Bold,
        style: FontStyle::Normal,
    };
    let found = library.resolve_family(&request);
    assert_eq!(found.family_match, FamilyMatch::Requested);
    assert_eq!(
        library.describe_face(found.face).unwrap().family,
        "Noto Sans Arabic"
    );
    assert_eq!(found.synthetic_bold, SyntheticBold::Needed);
    // А у Noto Sans Bold есть: синтетика не нужна.
    let request = FontRequest {
        family: Some("Noto Sans"),
        weight: FontWeightClass::Bold,
        style: FontStyle::Normal,
    };
    let found = library.resolve_family(&request);
    assert_eq!(
        library.describe_face(found.face).unwrap().weight,
        FontWeightClass::Bold
    );
    assert_eq!(found.synthetic_bold, SyntheticBold::NotNeeded);
}

fn noto_regular_bytes() -> Arc<[u8]> {
    Arc::from(EMBEDDED_FONTS[0].data)
}

const LIMITS: AttachedFontLimits = AttachedFontLimits {
    max_font_bytes: 10 * 1024 * 1024,
    max_total_bytes: 64 * 1024 * 1024,
    max_faces: 64,
};

#[test]
fn attached_font_too_large_is_rejected_and_library_is_unchanged() {
    let mut library = library_with_cache(1);
    let bytes = noto_regular_bytes();
    let tiny_limit = AttachedFontLimits {
        max_font_bytes: bytes.len() - 1,
        ..LIMITS
    };
    let error = library
        .register_attached_font(Arc::clone(&bytes), &tiny_limit)
        .unwrap_err();
    assert_eq!(
        error,
        AttachedFontError::TooLarge {
            actual_bytes: bytes.len(),
            max_bytes: bytes.len() - 1
        }
    );
    assert_eq!(library.statistics().attached_faces, 0);
    // Ровно по лимиту — проходит.
    let exact = AttachedFontLimits {
        max_font_bytes: bytes.len(),
        ..LIMITS
    };
    assert!(library.register_attached_font(bytes, &exact).is_ok());
}

#[test]
fn attached_garbage_is_unparseable_and_library_is_unchanged() {
    let mut library = library_with_cache(1);
    let garbage: Arc<[u8]> = Arc::from(&b"this is definitely not a font file"[..]);
    assert_eq!(
        library
            .register_attached_font(garbage, &LIMITS)
            .unwrap_err(),
        AttachedFontError::Unparseable
    );
    assert_eq!(
        library
            .register_attached_font(Arc::from(&b""[..]), &LIMITS)
            .unwrap_err(),
        AttachedFontError::Unparseable
    );
    assert_eq!(library.statistics().attached_faces, 0);
}

#[test]
fn attached_font_wins_over_embedded_by_family_name() {
    let mut library = library_with_cache(1);
    let registration = library
        .register_attached_font(noto_regular_bytes(), &LIMITS)
        .unwrap();
    assert_eq!(registration.faces.len(), 1);
    assert_eq!(library.statistics().attached_faces, 1);
    let request = FontRequest {
        family: Some("Noto Sans"),
        weight: FontWeightClass::Regular,
        style: FontStyle::Normal,
    };
    let found = library.resolve_family(&request);
    assert_eq!(found.origin, FaceOrigin::Attached);
    assert_eq!(found.face, registration.faces[0]);
    assert_eq!(found.family_match, FamilyMatch::Requested);
    // Лицо реально читается: метрики и байты доступны.
    assert!(library.face_metrics(found.face).is_some());
    assert_eq!(
        library.face_bytes(found.face).unwrap().as_slice(),
        &*noto_regular_bytes()
    );
    // Запрос без имени по-прежнему даёт встроенное, а не вложенное.
    assert_eq!(
        library
            .resolve_family(&default_request(FontWeightClass::Regular))
            .origin,
        FaceOrigin::Embedded
    );
}

/// Читает uint16 big-endian по смещению.
fn be_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
}

/// Независимо от skrifa находит таблицу по тегу в каталоге sfnt: (смещение, длина).
fn find_table(font: &[u8], tag: &[u8; 4]) -> (usize, usize) {
    let table_count = usize::from(be_u16(font, 4));
    for index in 0..table_count {
        let record = 12 + index * 16;
        if &font[record..record + 4] == tag {
            let offset = u32::from_be_bytes(font[record + 8..record + 12].try_into().unwrap());
            let length = u32::from_be_bytes(font[record + 12..record + 16].try_into().unwrap());
            return (offset as usize, length as usize);
        }
    }
    panic!("table {tag:?} not found");
}

#[test]
fn noto_sans_metrics_match_independently_read_os2_win_values() {
    let library = library_with_cache(1);
    for (font_index, font) in EMBEDDED_FONTS.iter().take(2).enumerate() {
        let (os2_offset, _) = find_table(font.data, b"OS/2");
        // usWinAscent — смещение 74, usWinDescent — 76 (OpenType OS/2).
        let win_ascent = i32::from(be_u16(font.data, os2_offset + 74) as i16);
        let win_descent = i32::from(be_u16(font.data, os2_offset + 76) as i16);
        assert_ne!(
            win_ascent + win_descent,
            0,
            "тест имеет смысл только для ненулевых Win-метрик"
        );
        let (head_offset, _) = find_table(font.data, b"head");
        let units_per_em = be_u16(font.data, head_offset + 18);

        let weight = if font_index == 0 {
            FontWeightClass::Regular
        } else {
            FontWeightClass::Bold
        };
        let face = library.resolve_family(&default_request(weight)).face;
        let metrics = library.face_metrics(face).unwrap();
        assert_eq!(metrics.source, AscentDescentSource::WindowsOs2);
        assert_eq!(metrics.units_per_em, units_per_em);
        assert_eq!(metrics.ascender, win_ascent);
        assert_eq!(metrics.descender, -win_descent);
        assert_eq!(metrics.height, win_ascent + win_descent);
    }
}

#[test]
fn metrics_of_unknown_face_are_absent() {
    // Лицо из другой библиотеки, где вложенный шрифт есть; в этой его нет.
    let mut other = library_with_cache(1);
    let foreign = other
        .register_attached_font(noto_regular_bytes(), &LIMITS)
        .unwrap()
        .faces[0];
    let library = library_with_cache(1);
    assert!(library.face_metrics(foreign).is_none());
    assert!(library.describe_face(foreign).is_none());
    assert!(library.face_bytes(foreign).is_none());
}

#[test]
fn system_font_scan_is_consistent_and_idempotent() {
    let mut library = library_with_cache(8);
    let first = library.load_system_fonts();
    assert_eq!(first.status, SystemFontScanStatus::Scanned);
    assert_eq!(first.faces_registered, library.statistics().system_faces);
    assert!(first.family_names <= first.faces_registered);
    assert_eq!(first.faces_registered == 0, first.family_names == 0);

    let second = library.load_system_fonts();
    assert_eq!(second.status, SystemFontScanStatus::AlreadyScanned);
    assert_eq!(second.faces_registered, first.faces_registered);
    assert_eq!(second.family_names, first.family_names);

    // Системные шрифты не ломают поиск: латиница по-прежнему из основного Noto Sans.
    assert_eq!(family_of_found_glyph(&mut library, 'A'), "Noto Sans");
    // А «ничего не найдено» остаётся Missing независимо от машины.
    let primary = primary_face(&library);
    assert!(matches!(
        library.glyph_for_char(primary, '\u{10FFFD}'),
        GlyphLookup::Missing { .. }
    ));
}

#[test]
fn system_face_found_by_family_name_is_readable_when_machine_has_fonts() {
    let mut library = library_with_cache(8);
    let report = library.load_system_fonts();
    if report.faces_registered == 0 {
        return; // Машина без шрифтов: проверять нечего, и это допустимо.
    }
    // Берём любое системное лицо через fallback по CJK-иероглифу, если он есть в системе.
    let primary = primary_face(&library);
    if let GlyphLookup::Found { face, glyph } = library.glyph_for_char(primary, '漢') {
        let description = library.describe_face(face).unwrap();
        assert_eq!(description.origin, FaceOrigin::System);
        assert_ne!(glyph, GlyphId::NOTDEF);
        assert!(library.face_bytes(face).is_some());
        assert!(library.face_metrics(face).is_some());
        let request = FontRequest {
            family: Some(description.family),
            weight: FontWeightClass::Regular,
            style: FontStyle::Normal,
        };
        let resolved = library.resolve_family(&request);
        assert_eq!(resolved.origin, FaceOrigin::System);
        assert_eq!(resolved.family_match, FamilyMatch::Requested);
    }
}
