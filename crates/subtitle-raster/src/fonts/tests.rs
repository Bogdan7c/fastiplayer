//! Тесты цепочки fallback, индекса покрытия системных шрифтов и общих байт файлов.
//!
//! «Системные» шрифты здесь — временный каталог с файлами, который сканирует `fontdb`, так что
//! весь путь (скан -> индекс -> поиск -> материализация) настоящий, а не заглушка.

use std::sync::Arc;

use skrifa::{FontRef, GlyphId, MetadataProvider};

use super::face::{FaceBytes, FaceId};
use super::glyph_lookup::GlyphLookup;
use super::language::TextLanguagePreference;
use super::library::SubtitleFontLibrary;
use super::lookup_cache::FontLookupCacheLimits;
use super::style::{FontRequest, FontStyle, FontWeightClass};
use super::test_fonts::{
    TempFontDir, as_collection, noto_regular_bytes, remap_letter_a, rename_table,
};
use super::{AttachedFontLimits, AttachedFontStatus, FaceOrigin};

/// Символ, которого нет ни в одном встроенном шрифте.
const ABSENT_EVERYWHERE: char = '\u{10FFFD}';

const ATTACHED_LIMITS: AttachedFontLimits = AttachedFontLimits {
    max_font_bytes: 16 * 1024 * 1024,
    max_total_bytes: 64 * 1024 * 1024,
    max_faces: 64,
};

fn library() -> SubtitleFontLibrary {
    SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 64,
    })
    .expect("embedded fonts")
}

fn primary(library: &SubtitleFontLibrary) -> FaceId {
    library
        .resolve_family(&FontRequest {
            family: None,
            weight: FontWeightClass::Regular,
            style: FontStyle::Normal,
        })
        .face
}

/// Шрифт, у которого бывшая `'A'` живёт под другим кодпоинтом; возвращает этот символ.
fn font_with_remapped_letter() -> (Vec<u8>, char) {
    let mut bytes = noto_regular_bytes();
    let remapped = remap_letter_a(&mut bytes);
    (bytes, remapped)
}

/// Номер глифа бывшей `'A'` в исходном Noto: ожидаемый ответ поиска.
fn glyph_of_letter_a() -> GlyphId {
    let bytes = noto_regular_bytes();
    let font = FontRef::new(&bytes).expect("noto");
    font.charmap().map('A').expect("A is mapped")
}

fn found_face(lookup: GlyphLookup) -> (FaceId, GlyphId) {
    match lookup {
        GlyphLookup::Found { face, glyph } => (face, glyph),
        other => panic!("glyph must be found, got {other:?}"),
    }
}

#[test]
fn system_lookup_reads_no_files_except_the_selected_face() {
    let dir = TempFontDir::new();
    let (remapped_font, remapped) = font_with_remapped_letter();
    dir.write("plain-regular.ttf", &noto_regular_bytes());
    dir.write("plain-regular-copy.ttf", &noto_regular_bytes());
    dir.write("remapped.ttf", &remapped_font);
    let mut fonts = library();
    let report = fonts.load_fonts_from_directory(dir.path());
    assert_eq!(report.faces_registered, 3);
    assert!(report.coverage_index_bytes > 0);

    let primary = primary(&fonts);
    let opens_after_scan = fonts.statistics().system_font_file_opens;
    assert!(opens_after_scan >= 3, "the scan itself reads every file");

    // Символ есть только в одном из трёх файлов: открыт должен быть ровно он.
    let (face, glyph) =
        found_face(fonts.glyph_for_char(primary, remapped, TextLanguagePreference::unknown()));
    assert_eq!(
        fonts.describe_face(face).expect("face").origin,
        FaceOrigin::System
    );
    assert_eq!(
        glyph,
        glyph_of_letter_a(),
        "glyph id comes from the selected face"
    );
    assert_eq!(
        fonts.statistics().system_font_file_opens - opens_after_scan,
        1,
        "only the selected face is opened, the two non-covering ones are rejected in memory"
    );

    // Повтор идёт из кэша поиска: файлы не открываются вовсе.
    let opens_after_hit = fonts.statistics().system_font_file_opens;
    found_face(fonts.glyph_for_char(primary, remapped, TextLanguagePreference::unknown()));
    assert_eq!(fonts.statistics().system_font_file_opens, opens_after_hit);

    // Символ, которого нет нигде: полный перебор по индексу, ни одного открытия файла.
    assert!(matches!(
        fonts.glyph_for_char(
            primary,
            ABSENT_EVERYWHERE,
            TextLanguagePreference::unknown()
        ),
        GlyphLookup::Missing { .. }
    ));
    assert_eq!(fonts.statistics().system_font_file_opens, opens_after_hit);
}

#[test]
fn faces_of_one_collection_share_bytes_and_memory_is_released_after_eviction() {
    let dir = TempFontDir::new();
    let (remapped_font, _) = font_with_remapped_letter();
    dir.write("pair.ttc", &as_collection(&remapped_font, 2));
    let mut fonts = library();
    assert_eq!(
        fonts.load_fonts_from_directory(dir.path()).faces_registered,
        2
    );
    assert_eq!(
        fonts.statistics().resident_system_files,
        0,
        "scan keeps no file bytes"
    );

    let (first, second) = (
        FaceId(fonts.embedded_face_count_for_test()),
        FaceId(fonts.embedded_face_count_for_test() + 1),
    );
    let (Some(FaceBytes::Shared(first_bytes)), Some(FaceBytes::Shared(second_bytes))) =
        (fonts.face_bytes(first), fonts.face_bytes(second))
    else {
        panic!("system faces must expose shared bytes");
    };
    assert!(
        Arc::ptr_eq(&first_bytes, &second_bytes),
        "one file, one allocation"
    );
    let statistics = fonts.statistics();
    assert_eq!(statistics.resident_system_files, 1);
    assert_eq!(statistics.resident_system_file_bytes, first_bytes.len());

    // «Вытеснение из FaceCache» = отпустить все сильные ссылки: библиотека память не держит.
    drop((first_bytes, second_bytes));
    assert_eq!(fonts.statistics().resident_system_files, 0);
    assert_eq!(fonts.statistics().resident_system_file_bytes, 0);

    // При повторном использовании файл читается заново.
    let opens_before = fonts.statistics().system_font_file_opens;
    let reloaded = fonts.face_bytes(first);
    assert!(reloaded.is_some());
    assert_eq!(fonts.statistics().system_font_file_opens - opens_before, 1);
    assert_eq!(fonts.statistics().resident_system_files, 1);
    drop(reloaded);
}

#[test]
fn unreadable_candidate_is_marked_unusable_and_never_selected_again() {
    let dir = TempFontDir::new();
    let (remapped_font, remapped) = font_with_remapped_letter();
    let file = dir.write("remapped.ttf", &remapped_font);
    let mut fonts = library();
    fonts.load_fonts_from_directory(dir.path());
    let primary = primary(&fonts);
    // Файл портится уже после скана: индекс покрытия ещё считает лицо подходящим.
    std::fs::write(&file, b"no longer a font").expect("overwrite");

    assert!(matches!(
        fonts.glyph_for_char(primary, remapped, TextLanguagePreference::unknown()),
        GlyphLookup::Missing { .. }
    ));
    let statistics = fonts.statistics();
    assert_eq!(statistics.unusable_faces, 1);
    assert_eq!(
        statistics.unreadable_faces, 1,
        "the parse failure is counted"
    );

    // Второй поиск лицо не трогает: файл не открывается, счётчики не растут.
    let opens = statistics.system_font_file_opens;
    assert!(matches!(
        fonts.glyph_for_char(primary, '\u{10F001}', TextLanguagePreference::unknown()),
        GlyphLookup::Missing { .. }
    ));
    assert_eq!(fonts.statistics().system_font_file_opens, opens);
    assert_eq!(fonts.statistics().unusable_faces, 1);
}

#[test]
fn scan_skips_faces_without_metrics_instead_of_registering_them() {
    let dir = TempFontDir::new();
    let mut headless = noto_regular_bytes();
    rename_table(&mut headless, b"head", b"hxad");
    dir.write("headless.ttf", &headless);
    dir.write("good.ttf", &noto_regular_bytes());
    let mut fonts = library();
    let report = fonts.load_fonts_from_directory(dir.path());
    // fontdb находит файл без `head`, но метрик у лица нет: оно пропущено и посчитано.
    assert_eq!(report.faces_registered, 1);
    assert_eq!(report.faces_skipped_unusable, 1);
    assert_eq!(fonts.statistics().unusable_faces, 1);
    let family = fonts
        .describe_face(FaceId(fonts.embedded_face_count_for_test()))
        .expect("face")
        .family;
    assert_eq!(family, "Noto Sans");
}

#[test]
fn glyph_chain_prefers_attached_over_system_and_embedded_over_both() {
    let dir = TempFontDir::new();
    let (remapped_font, remapped) = font_with_remapped_letter();
    dir.write("remapped.ttf", &remapped_font);
    let mut fonts = library();
    fonts.load_fonts_from_directory(dir.path());
    let primary = primary(&fonts);

    // Пока вложенного нет, символ берётся из системного шрифта.
    let (system_face, _) =
        found_face(fonts.glyph_for_char(primary, remapped, TextLanguagePreference::unknown()));
    assert_eq!(
        fonts.describe_face(system_face).expect("face").origin,
        FaceOrigin::System
    );

    // Вложенный шрифт с тем же символом выигрывает у системного (кэш поиска при этом сброшен).
    let registration = fonts
        .register_attached_font(Arc::from(remapped_font), &ATTACHED_LIMITS)
        .expect("valid font");
    assert_eq!(registration.status, AttachedFontStatus::Registered);
    let (face, glyph) =
        found_face(fonts.glyph_for_char(primary, remapped, TextLanguagePreference::unknown()));
    assert_eq!(
        fonts.describe_face(face).expect("face").origin,
        FaceOrigin::Attached
    );
    assert!(registration.faces.contains(&face));
    assert_eq!(glyph, glyph_of_letter_a());

    // Латиница по-прежнему из встроенного, даже если вложенный шрифт тоже мог бы её дать.
    let (latin_face, _) =
        found_face(fonts.glyph_for_char(primary, 'B', TextLanguagePreference::unknown()));
    assert_eq!(
        fonts.describe_face(latin_face).expect("face").origin,
        FaceOrigin::Embedded
    );
}

#[test]
fn duplicate_attached_font_changes_nothing_and_keeps_lookup_cache() {
    let (remapped_font, remapped) = font_with_remapped_letter();
    let bytes: Arc<[u8]> = Arc::from(remapped_font);
    let mut fonts = library();
    let primary = primary(&fonts);
    let first = fonts
        .register_attached_font(Arc::clone(&bytes), &ATTACHED_LIMITS)
        .expect("valid");
    found_face(fonts.glyph_for_char(primary, remapped, TextLanguagePreference::unknown()));
    let before = fonts.statistics();
    assert_eq!(before.cached_lookups, 1);

    // Те же байты (даже другим Arc): повтор, ничего не добавлено и кэш поиска цел.
    let again = fonts
        .register_attached_font(Arc::from(bytes.to_vec()), &ATTACHED_LIMITS)
        .expect("duplicate is not an error");
    assert_eq!(again.status, AttachedFontStatus::AlreadyRegistered);
    assert_eq!(again.faces, first.faces);
    let after = fonts.statistics();
    assert_eq!(after.attached_faces, before.attached_faces);
    assert_eq!(after.attached_font_bytes, before.attached_font_bytes);
    assert_eq!(
        after.cached_lookups, 1,
        "lookup cache is not reset by a duplicate"
    );

    // Новый (другой) шрифт кэш поиска сбрасывает: набор шрифтов изменился.
    let mut other = noto_regular_bytes();
    rename_table(&mut other, b"OS/2", b"OS/3");
    let added = fonts
        .register_attached_font(Arc::from(other), &ATTACHED_LIMITS)
        .expect("valid");
    assert_eq!(added.status, AttachedFontStatus::Registered);
    assert_eq!(fonts.statistics().cached_lookups, 0);
}

#[test]
fn attached_totals_are_limited_in_bytes_and_in_faces() {
    let regular = noto_regular_bytes();
    let mut second = noto_regular_bytes();
    rename_table(&mut second, b"OS/2", b"OS/3");
    let first_len = regular.len();

    // Суммарные байты: второй файл не помещается, библиотека остаётся как была.
    let mut fonts = library();
    let byte_limits = AttachedFontLimits {
        max_total_bytes: first_len + second.len() - 1,
        ..ATTACHED_LIMITS
    };
    fonts
        .register_attached_font(Arc::from(regular.clone()), &byte_limits)
        .expect("fits");
    let error = fonts
        .register_attached_font(Arc::from(second.clone()), &byte_limits)
        .expect_err("over the total");
    assert_eq!(
        error,
        super::AttachedFontError::TotalBytesExceeded {
            current_bytes: first_len,
            adding_bytes: second.len(),
            max_bytes: first_len + second.len() - 1,
        }
    );
    assert_eq!(fonts.statistics().attached_faces, 1);
    assert_eq!(fonts.statistics().attached_font_bytes, first_len);

    // Лица: коллекция из двух лиц не помещается в лимит 1.
    let mut fonts = library();
    let face_limits = AttachedFontLimits {
        max_faces: 1,
        ..ATTACHED_LIMITS
    };
    let error = fonts
        .register_attached_font(Arc::from(as_collection(&regular, 2)), &face_limits)
        .expect_err("two faces over the limit of one");
    assert_eq!(
        error,
        super::AttachedFontError::TooManyFaces {
            current_faces: 0,
            adding_faces: 2,
            max_faces: 1
        }
    );
    assert_eq!(fonts.statistics().attached_faces, 0);
}

#[test]
fn unusable_face_is_skipped_by_family_resolution() {
    let mut fonts = library();
    let registration = fonts
        .register_attached_font(Arc::from(noto_regular_bytes()), &ATTACHED_LIMITS)
        .expect("valid");
    let attached = registration.faces[0];
    let request = FontRequest {
        family: Some("Noto Sans"),
        weight: FontWeightClass::Regular,
        style: FontStyle::Normal,
    };
    assert_eq!(
        fonts.resolve_family(&request).face,
        attached,
        "attached wins by name"
    );

    assert!(fonts.mark_face_unusable(attached));
    assert!(
        !fonts.mark_face_unusable(attached),
        "second mark is a no-op"
    );
    assert_ne!(fonts.resolve_family(&request).face, attached);
    assert_eq!(fonts.statistics().unusable_faces, 1);
    // Встроенные лица отключать нельзя: у цепочки должен быть надёжный конец.
    assert!(!fonts.mark_face_unusable(primary(&fonts)));
}
