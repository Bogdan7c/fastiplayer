//! Сравнение порта с эталонами libass 0.17.5 (композит PNG и геометрия JSON).

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use super::ass_case::load_case;
use super::pipeline::{RenderedImage, render_case};
use crate::overlay::{
    CanvasSize, OverlayChange, OverlayRevision, PixelPoint, SubtitleBitmap, SubtitleBitmapKind,
    SubtitleOverlay,
};

/// Каталог фикстур libass.
pub(super) fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/libass")
}

/// Имена кейсов-рисунков (без расширения) в алфавитном порядке; текстовые кейсы (`text_*`)
/// проверяет `text_tests`.
fn case_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir().join("cases"))
        .expect("cases directory must exist")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "ass").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .filter(|name| !name.starts_with("text_"))
        .collect();
    names.sort();
    names
}

/// Рендерит кейс кодом порта.
fn render(name: &str) -> (super::ass_case::DrawingCase, Vec<RenderedImage>) {
    let case = load_case(&fixtures_dir().join("cases").join(format!("{name}.ass")));
    let images = render_case(&case);
    (case, images)
}

/// Собирает `SubtitleOverlay` из картинок и композитит его в RGBA.
fn composite(case: &super::ass_case::DrawingCase, images: &[RenderedImage]) -> Vec<u8> {
    let bitmaps = images
        .iter()
        .map(|image| {
            SubtitleBitmap::new(
                PixelPoint {
                    x: image.x,
                    y: image.y,
                },
                image.width as u32,
                image.height as u32,
                image.stride,
                Arc::from(image.mask.as_slice()),
                image.color,
                image.kind,
            )
            .expect("bitmap layout must be valid")
        })
        .collect();
    let canvas = CanvasSize::new(case.width as u32, case.height as u32).expect("canvas size");
    SubtitleOverlay::new(
        canvas,
        bitmaps,
        OverlayRevision::INITIAL,
        OverlayChange::Replaced,
    )
    .expect("images must fit the canvas")
    .composite_rgba_over_transparent()
}

/// Читает эталонный PNG (RGBA 8 бит).
pub(super) fn load_expected_png(name: &str) -> (u32, u32, Vec<u8>) {
    let bytes = std::fs::read(fixtures_dir().join("expected").join(format!("{name}.png")))
        .expect("expected PNG must exist");
    let mut reader = png::Decoder::new(Cursor::new(bytes))
        .read_info()
        .expect("PNG header must be valid");
    let mut pixels = vec![0; reader.output_buffer_size().expect("PNG size")];
    let info = reader
        .next_frame(&mut pixels)
        .expect("PNG data must be valid");
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    pixels.truncate(info.buffer_size());
    (info.width, info.height, pixels)
}

/// Сводка расхождения двух RGBA-картинок.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct PixelDiff {
    pub(super) max_channel_difference: u8,
    pub(super) differing_pixels: usize,
}

pub(super) fn diff_rgba(actual: &[u8], expected: &[u8]) -> PixelDiff {
    assert_eq!(actual.len(), expected.len());
    let mut diff = PixelDiff::default();
    for (a, e) in actual
        .as_chunks::<4>()
        .0
        .iter()
        .zip(expected.as_chunks::<4>().0)
    {
        let pixel_max = a
            .iter()
            .zip(e)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        if pixel_max > 0 {
            diff.differing_pixels += 1;
            diff.max_channel_difference = diff.max_channel_difference.max(pixel_max);
        }
    }
    diff
}

/// Тип картинки в libass (`ASS_Image.type`).
fn libass_image_type(kind: SubtitleBitmapKind) -> i64 {
    match kind {
        SubtitleBitmapKind::Glyph => 0,
        SubtitleBitmapKind::Outline => 1,
        SubtitleBitmapKind::Shadow => 2,
        SubtitleBitmapKind::Background => panic!("libass reference has no background images"),
    }
}

#[test]
fn reference_cases_are_present() {
    let names = case_names();
    assert!(
        names.len() >= 8,
        "expected the full reference set, found {names:?}"
    );
    for name in names {
        assert!(
            fixtures_dir()
                .join("expected")
                .join(format!("{name}.png"))
                .exists()
        );
        assert!(
            fixtures_dir()
                .join("expected")
                .join(format!("{name}.json"))
                .exists()
        );
    }
}

#[test]
fn image_geometry_matches_libass_for_every_case() {
    for name in case_names() {
        let (_, images) = render(&name);
        let json: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixtures_dir().join("expected").join(format!("{name}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(json["libass_version"], "0.17.5");
        let expected = json["images"].as_array().expect("images array");
        assert_eq!(images.len(), expected.len(), "{name}: image count");
        for (index, (image, reference)) in images.iter().zip(expected).enumerate() {
            let label = format!("{name}[{index}]");
            assert_eq!(
                libass_image_type(image.kind),
                reference["type"].as_i64().unwrap(),
                "{label}: type"
            );
            assert_eq!(
                i64::from(image.x),
                reference["dst_x"].as_i64().unwrap(),
                "{label}: x"
            );
            assert_eq!(
                i64::from(image.y),
                reference["dst_y"].as_i64().unwrap(),
                "{label}: y"
            );
            assert_eq!(
                i64::from(image.width),
                reference["w"].as_i64().unwrap(),
                "{label}: w"
            );
            assert_eq!(
                i64::from(image.height),
                reference["h"].as_i64().unwrap(),
                "{label}: h"
            );
            // Шаг строки зависит от SIMD-движка libass (AVX2 выравнивает по 32, C-движок — по 16);
            // содержимое от него не зависит, поэтому сверяем только согласованность с шириной.
            assert!(
                image.stride >= image.width as usize,
                "{label}: stride covers the width"
            );
            assert_eq!(
                image.stride % 16,
                0,
                "{label}: stride is 16-aligned like the C engine"
            );
            assert!(
                reference["stride"].as_i64().unwrap() >= i64::from(image.width),
                "{label}"
            );
            let color: Vec<i64> = reference["color"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap())
                .collect();
            let actual = [
                image.color.red,
                image.color.green,
                image.color.blue,
                image.color.alpha,
            ]
            .map(i64::from);
            assert_eq!(actual.as_slice(), color.as_slice(), "{label}: color");
        }
    }
}

/// Допуск композита по каналу для кейса. Цель — 0; ненулевое значение требует объяснения.
fn allowed_channel_difference(_case_name: &str) -> u8 {
    0
}

#[test]
fn composite_matches_libass_png_for_every_case() {
    let mut report = Vec::new();
    for name in case_names() {
        let (case, images) = render(&name);
        let actual = composite(&case, &images);
        let (width, height, expected) = load_expected_png(&name);
        assert_eq!(
            (width, height),
            (case.width as u32, case.height as u32),
            "{name}: size"
        );
        // Эталон не пустой: иначе равенство ничего не доказывает.
        assert!(
            expected
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] != 0),
            "{name}: reference PNG must contain visible pixels"
        );
        let diff = diff_rgba(&actual, &expected);
        report.push(format!(
            "{name}: max channel diff {}, differing pixels {}",
            diff.max_channel_difference, diff.differing_pixels
        ));
        assert!(
            diff.max_channel_difference <= allowed_channel_difference(&name),
            "{name}: composite differs from libass: {diff:?}"
        );
    }
    eprintln!("{}", report.join("\n"));
}
