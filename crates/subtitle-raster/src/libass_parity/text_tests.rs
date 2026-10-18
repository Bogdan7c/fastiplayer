//! Паритет текстового конвейера с libass 0.17.5: `render_text_event` против эталонов `text_*`.
//!
//! Эталоны (PNG и JSON) лежат рядом с кейсами рисунков и генерируются тем же инструментом
//! `scripts/subtitle-reference`. Сравнение идёт по композиту RGBA, как у рисунков, и по
//! геометрии картинок из JSON (число, порядок, позиция, размер, цвет).

use std::path::PathBuf;

use super::tests::{PixelDiff, diff_rgba, fixtures_dir, load_expected_png};
use super::text_case::{TextCase, load_text_case};
use crate::fonts::{FontLookupCacheLimits, SubtitleFontLibrary};
use crate::overlay::{
    CanvasSize, OverlayChange, OverlayRevision, SubtitleBitmap, SubtitleBitmapKind, SubtitleOverlay,
};
use crate::text_renderer::render_text_event;

/// Допуск кейса: наибольшая разница канала и число отличающихся пикселей.
///
/// Значения — фактические (на 2026-10) с запасом около 30%. Остаточное расхождение даёт
/// разный выбор округления в трёх местах: контуры FreeType и skrifa (разница до 1/64 px на
/// размере 256), продвижения автохинтера skrifa и FreeType (у отдельных арабских глифов
/// расходятся на 1 px размера 256) и позиции меток HarfBuzz и harfrust.
struct Tolerance {
    name: &'static str,
    max_channel_difference: u8,
    differing_pixels: usize,
    /// Сравнивать ли размеры картинок (у арабского кейса ширина объединённой обводки зависит
    /// от выравнивания тайлов и меняется на несколько пикселей при сдвиге глифа на 1/8 px).
    compare_sizes: bool,
}

const TEXT_CASES: [Tolerance; 9] = [
    Tolerance {
        name: "text_plain",
        max_channel_difference: 4,
        differing_pixels: 5,
        compare_sizes: true,
    },
    Tolerance {
        name: "text_outline",
        max_channel_difference: 7,
        differing_pixels: 35,
        compare_sizes: true,
    },
    Tolerance {
        name: "text_shadow",
        max_channel_difference: 28,
        differing_pixels: 36,
        compare_sizes: true,
    },
    Tolerance {
        name: "text_two_lines",
        max_channel_difference: 34,
        differing_pixels: 160,
        compare_sizes: true,
    },
    Tolerance {
        name: "text_wrap",
        max_channel_difference: 34,
        differing_pixels: 560,
        compare_sizes: true,
    },
    Tolerance {
        name: "text_cyrillic",
        max_channel_difference: 7,
        differing_pixels: 115,
        compare_sizes: true,
    },
    Tolerance {
        name: "text_arabic_rtl",
        max_channel_difference: 65,
        differing_pixels: 1300,
        compare_sizes: false,
    },
    Tolerance {
        name: "text_box3",
        max_channel_difference: 4,
        differing_pixels: 5,
        compare_sizes: true,
    },
    Tolerance {
        name: "text_box4",
        max_channel_difference: 12,
        differing_pixels: 35,
        compare_sizes: true,
    },
];

/// Библиотека шрифтов только со встроенными лицами (как fonts-dir у libass).
fn embedded_fonts() -> SubtitleFontLibrary {
    SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 4096,
    })
    .expect("embedded fonts must load")
}

/// Рендерит кейс нашим конвейером.
fn render(name: &str) -> (TextCase, Vec<SubtitleBitmap>) {
    let case = load_text_case(&fixtures_dir().join("cases").join(format!("{name}.ass")));
    let canvas = CanvasSize::new(case.width, case.height).expect("canvas");
    let mut fonts = embedded_fonts();
    let bitmaps = render_text_event(&mut fonts, &case.text, &case.style, canvas)
        .expect("text event must render");
    (case, bitmaps)
}

/// Композит картинок через `SubtitleOverlay`.
fn composite(case: &TextCase, bitmaps: Vec<SubtitleBitmap>) -> Vec<u8> {
    let canvas = CanvasSize::new(case.width, case.height).expect("canvas");
    SubtitleOverlay::new(
        canvas,
        bitmaps,
        OverlayRevision::INITIAL,
        OverlayChange::Replaced,
    )
    .expect("images must fit the canvas")
    .composite_rgba_over_transparent()
}

/// Тип картинки в libass (`ASS_Image.type`); у подложки `BorderStyle = 4` libass оставляет
/// поле неинициализированным, поэтому её тип не сравнивается.
fn libass_image_type(kind: SubtitleBitmapKind) -> Option<i64> {
    match kind {
        SubtitleBitmapKind::Glyph => Some(0),
        SubtitleBitmapKind::Outline => Some(1),
        SubtitleBitmapKind::Shadow => Some(2),
        // В эталоне libass картинок-RGBA нет: тип не сравнивается.
        SubtitleBitmapKind::Background | SubtitleBitmapKind::Picture => None,
    }
}

/// Сохраняет тройку «наш / libass / разница» для ручной проверки, если задан каталог в
/// `SUBTITLE_PARITY_DUMP_DIR`.
pub(super) fn dump_comparison(name: &str, width: u32, height: u32, ours: &[u8], theirs: &[u8]) {
    let Some(dir) = std::env::var_os("SUBTITLE_PARITY_DUMP_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("dump dir");
    let write = |suffix: &str, pixels: &[u8]| {
        let file = std::fs::File::create(dir.join(format!("{name}_{suffix}.png"))).expect("file");
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .expect("png header")
            .write_image_data(pixels)
            .expect("png data");
    };
    // На разностной картинке фон тёмно-серый, различия усилены в 16 раз и показаны красным.
    let diff: Vec<u8> = ours
        .as_chunks::<4>()
        .0
        .iter()
        .zip(theirs.as_chunks::<4>().0)
        .flat_map(|(a, e)| {
            let delta = a
                .iter()
                .zip(e)
                .map(|(x, y)| x.abs_diff(*y))
                .max()
                .unwrap_or(0);
            let strength = delta.saturating_mul(16);
            [strength.max(24), 24, 24, 255]
        })
        .collect();
    write("ours", ours);
    write("libass", theirs);
    write("diff", &diff);
}

/// Результат сравнения кейса.
struct CaseReport {
    diff: PixelDiff,
    geometry_mismatches: Vec<String>,
}

/// Сравнивает кейс: геометрия картинок и композит.
fn compare(name: &str, compare_sizes: bool) -> CaseReport {
    let (case, bitmaps) = render(name);
    let json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixtures_dir().join("expected").join(format!("{name}.json")))
            .expect("expected json"),
    )
    .expect("json");
    assert_eq!(json["libass_version"], "0.17.5");
    let expected = json["images"].as_array().expect("images array");
    let mut geometry_mismatches = Vec::new();
    if bitmaps.len() != expected.len() {
        geometry_mismatches.push(format!(
            "image count: ours {} vs libass {}",
            bitmaps.len(),
            expected.len()
        ));
    }
    for (index, (image, reference)) in bitmaps.iter().zip(expected).enumerate() {
        let get = |key: &str| reference[key].as_i64().expect("number");
        let ours = (
            i64::from(image.origin().x),
            i64::from(image.origin().y),
            i64::from(image.width()),
            i64::from(image.height()),
        );
        let theirs = (get("dst_x"), get("dst_y"), get("w"), get("h"));
        let same_geometry = if compare_sizes {
            ours == theirs
        } else {
            (ours.0, ours.1) == (theirs.0, theirs.1)
        };
        if !same_geometry {
            geometry_mismatches.push(format!("image {index}: ours {ours:?} vs libass {theirs:?}"));
        }
        if let Some(kind) = libass_image_type(image.kind())
            && kind != get("type")
        {
            geometry_mismatches.push(format!("image {index}: type {kind} vs {}", get("type")));
        }
        let color: Vec<i64> = reference["color"]
            .as_array()
            .expect("color")
            .iter()
            .map(|v| v.as_i64().expect("channel"))
            .collect();
        let c = image.color();
        let ours_color = [c.red, c.green, c.blue, c.alpha].map(i64::from);
        if ours_color.as_slice() != color.as_slice() {
            geometry_mismatches.push(format!("image {index}: color {ours_color:?} vs {color:?}"));
        }
    }
    let actual = composite(&case, bitmaps);
    let (width, height, reference_pixels) = load_expected_png(name);
    assert_eq!((width, height), (case.width, case.height), "{name}: size");
    let diff = diff_rgba(&actual, &reference_pixels);
    dump_comparison(name, width, height, &actual, &reference_pixels);
    CaseReport {
        diff,
        geometry_mismatches,
    }
}

#[test]
fn text_events_match_libass_within_measured_tolerance() {
    for case in TEXT_CASES {
        let report = compare(case.name, case.compare_sizes);
        eprintln!(
            "{}: max channel diff {}, differing pixels {}",
            case.name, report.diff.max_channel_difference, report.diff.differing_pixels
        );
        assert!(
            report.geometry_mismatches.is_empty(),
            "{}: geometry differs from libass: {:?}",
            case.name,
            report.geometry_mismatches
        );
        assert!(
            report.diff.max_channel_difference <= case.max_channel_difference,
            "{}: max channel difference {} exceeds {}",
            case.name,
            report.diff.max_channel_difference,
            case.max_channel_difference
        );
        assert!(
            report.diff.differing_pixels <= case.differing_pixels,
            "{}: {} pixels differ, allowed {}",
            case.name,
            report.diff.differing_pixels,
            case.differing_pixels
        );
    }
}
