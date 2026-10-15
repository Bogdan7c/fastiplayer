//! Fallback по шрифтам на уровне растеризатора: непригодное лицо не должно ронять реплику.
//!
//! «Системные» шрифты — временный каталог со шрифтами, которые сканирует `fontdb`; лицо
//! портится ПОСЛЕ скана (как случается в жизни: файл удалили или заменили), поэтому индекс
//! покрытия ещё считает его подходящим, и сбой проявляется только при загрузке для рисования.

use std::sync::Arc;

use super::*;
use crate::fonts::test_fonts::{
    TempFontDir, corrupt_glyph_outline, noto_regular_bytes, remap_letter_a, rename_table,
};
use crate::fonts::{AttachedFontLimits, AttachedFontStatus};

fn youtube() -> SubtitleAppearance {
    SubtitleAppearance::youtube_like()
}

const ATTACHED_LIMITS: AttachedFontLimits = AttachedFontLimits {
    max_font_bytes: 16 * 1024 * 1024,
    max_total_bytes: 64 * 1024 * 1024,
    max_faces: 8,
};

/// Растеризатор, чьи «системные» шрифты — файлы из каталога.
fn rasterizer_over_directory(dir: &TempFontDir) -> SubtitleRasterizer {
    let mut fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 4096,
    })
    .expect("embedded fonts");
    fonts.load_fonts_from_directory(dir.path());
    SubtitleRasterizer::new(
        fonts,
        RasterLimits {
            cache: ROOMY_LIMITS,
            input: ROOMY_INPUT,
        },
    )
}

/// Ширина закрашенной области глифов реплики.
fn glyph_ink_width(overlay: &SubtitleOverlay) -> i32 {
    let (x0, _, x1, _) =
        ink_bounds(of_kind(overlay, SubtitleBitmapKind::Glyph)).expect("glyphs are drawn");
    x1 - x0
}

/// Как испортить системный файл после скана.
#[derive(Clone, Copy, Debug)]
enum Damage {
    /// Файл удалён.
    Removed,
    /// Файл заменён шрифтом без таблицы `head`: разбирается, но метрик нет.
    NoMetrics,
}

fn damage(dir: &TempFontDir, file: &std::path::Path, how: Damage) {
    match how {
        Damage::Removed => std::fs::remove_file(file).expect("remove"),
        Damage::NoMetrics => {
            let mut bytes = noto_regular_bytes();
            remap_letter_a(&mut bytes);
            rename_table(&mut bytes, b"head", b"hxad");
            dir.write("remapped.ttf", &bytes);
        }
    }
}

#[test]
fn unusable_fallback_face_does_not_lose_the_cue_and_is_excluded_afterwards() {
    for how in [Damage::Removed, Damage::NoMetrics] {
        let dir = TempFontDir::new();
        let mut font = noto_regular_bytes();
        let remapped = remap_letter_a(&mut font);
        let file = dir.write("remapped.ttf", &font);
        let mut rasterizer = rasterizer_over_directory(&dir);
        damage(&dir, &file, how);

        let with_fallback_char = format!("AB{remapped}CD");
        let overlay = render_once(
            &mut rasterizer,
            &cues(1, &[with_fallback_char.as_str()]),
            &youtube(),
        );
        let statistics = rasterizer.statistics();
        assert_eq!(statistics.failed_cues, 0, "{how:?}: the cue must be drawn");
        assert_eq!(statistics.rasterized_cues, 1, "{how:?}");
        assert_eq!(statistics.unusable_faces, 1, "{how:?}");
        assert!(!overlay.is_empty(), "{how:?}");
        // Символ рисуется как .notdef основного лица (пустая рамка занимает место): текст
        // шире, чем без него.
        let reference = render_once(&mut rasterizer, &cues(2, &["ABCD"]), &youtube());
        assert!(
            glyph_ink_width(&overlay) > glyph_ink_width(&reference),
            "{how:?}: the .notdef box takes room"
        );

        // Следующая реплика с тем же символом: лицо больше не выбирается и не пересчитывается.
        let again = render_once(
            &mut rasterizer,
            &cues(3, &[with_fallback_char.as_str()]),
            &youtube(),
        );
        assert!(!again.is_empty(), "{how:?}");
        let after = rasterizer.statistics();
        assert_eq!(after.failed_cues, 0, "{how:?}");
        assert_eq!(after.unusable_faces, 1, "{how:?}: no repeated failures");
    }
}

#[test]
fn attached_font_supplies_the_glyph_and_duplicate_registration_keeps_the_caches() {
    let mut font = noto_regular_bytes();
    let remapped = remap_letter_a(&mut font);
    let text = format!("x{remapped}y");

    // Без вложенного шрифта символ нигде не найден.
    let mut bare = rasterizer(ROOMY_LIMITS);
    render_once(&mut bare, &cues(1, &[text.as_str()]), &youtube());
    assert!(bare.statistics().missing_glyph_lookups > 0);

    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let registration = rasterizer
        .register_attached_font(Arc::from(font.clone()), &ATTACHED_LIMITS)
        .expect("valid font");
    assert_eq!(registration.status, AttachedFontStatus::Registered);
    let test_cues = cues(1, &[text.as_str()]);
    let first = render_once(&mut rasterizer, &test_cues, &youtube());
    assert_eq!(
        rasterizer.statistics().missing_glyph_lookups,
        0,
        "the attached font covers the character"
    );
    let reference = render_once(&mut rasterizer, &cues(2, &["xAy"]), &youtube());
    assert_eq!(
        glyph_ink_width(&first),
        glyph_ink_width(&reference),
        "the attached glyph is the letter A drawn by the same outlines"
    );

    // Повтор тех же байт: кэши реплик и прошлый кадр на месте.
    let entries_before = rasterizer.statistics().cue_cache.entries;
    let duplicate = rasterizer
        .register_attached_font(Arc::from(font), &ATTACHED_LIMITS)
        .expect("duplicate");
    assert_eq!(duplicate.status, AttachedFontStatus::AlreadyRegistered);
    assert_eq!(rasterizer.statistics().cue_cache.entries, entries_before);
    let repeat = render_once(&mut rasterizer, &cues(2, &["xAy"]), &youtube());
    assert_eq!(repeat.change(), OverlayChange::Unchanged);
    assert_eq!(repeat.revision(), reference.revision());
}

#[test]
fn glyph_with_a_broken_outline_is_skipped_counted_and_the_rest_of_the_cue_is_drawn() {
    let mut font = noto_regular_bytes();
    let remapped = remap_letter_a(&mut font);
    // Глиф бывшей `A` повреждён: он найден по cmap, но контур не читается.
    let glyph_a = {
        let reference = noto_regular_bytes();
        let parsed = skrifa::FontRef::new(&reference).expect("noto");
        skrifa::MetadataProvider::charmap(&parsed)
            .map('A')
            .expect("A")
    };
    corrupt_glyph_outline(&mut font, glyph_a.to_u32() as u16);

    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    rasterizer
        .register_attached_font(Arc::from(font), &ATTACHED_LIMITS)
        .expect("the font parses and has metrics");
    let text = format!("xx{remapped}yy");
    let overlay = render_once(&mut rasterizer, &cues(1, &[text.as_str()]), &youtube());

    let statistics = rasterizer.statistics();
    assert_eq!(statistics.failed_cues, 0);
    assert_eq!(
        statistics.skipped_glyphs, 1,
        "exactly the broken glyph is skipped"
    );
    assert!(!overlay.is_empty());
    // Остальные четыре буквы нарисованы.
    let reference = render_once(&mut rasterizer, &cues(2, &["xxyy"]), &youtube());
    assert!(glyph_ink_width(&overlay) >= glyph_ink_width(&reference));

    // Повторная выдача из кэша счётчик не увеличивает: считается растеризация, а не показ.
    render_once(&mut rasterizer, &cues(1, &[text.as_str()]), &youtube());
    assert_eq!(rasterizer.statistics().skipped_glyphs, 1);
}

#[test]
fn system_font_bytes_live_only_while_the_face_cache_holds_the_face() {
    let dir = TempFontDir::new();
    let mut font = noto_regular_bytes();
    let remapped = remap_letter_a(&mut font);
    dir.write("remapped.ttf", &font);
    let mut fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 4096,
    })
    .expect("embedded fonts");
    fonts.load_fonts_from_directory(dir.path());
    // Лимит лиц 0: при сборке каждого следующего кадра кэш подготовленных лиц опустошается.
    let mut rasterizer = SubtitleRasterizer::new(
        fonts,
        RasterLimits {
            cache: RasterCacheLimits {
                cue_bitmaps_bytes: 64 * 1024 * 1024,
                shaper_faces: 0,
            },
            input: ROOMY_INPUT,
        },
    );
    assert_eq!(rasterizer.fonts().statistics().resident_system_files, 0);

    let text = format!("x{remapped}");
    render_once(&mut rasterizer, &cues(1, &[text.as_str()]), &youtube());
    let resident = rasterizer.fonts().statistics();
    assert_eq!(
        resident.resident_system_files, 1,
        "the face cache holds the file"
    );
    assert_eq!(resident.resident_system_file_bytes, font.len());

    // Следующий кадр сначала вытесняет лица; ему системный шрифт не нужен.
    render_once(&mut rasterizer, &cues(2, &["plain"]), &youtube());
    let released = rasterizer.fonts().statistics();
    assert_eq!(
        released.resident_system_files, 0,
        "the library kept no strong reference"
    );
    assert_eq!(released.resident_system_file_bytes, 0);

    // Вернулись к реплике с системным символом: файл читается заново, реплика рисуется.
    let opens_before = released.system_font_file_opens;
    let again = render_once(&mut rasterizer, &cues(3, &[text.as_str()]), &youtube());
    assert!(!again.is_empty());
    assert!(rasterizer.fonts().statistics().system_font_file_opens > opens_before);
    assert_eq!(rasterizer.statistics().failed_cues, 0);
}
