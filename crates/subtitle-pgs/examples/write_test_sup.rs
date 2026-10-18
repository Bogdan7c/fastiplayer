//! Пишет маленький детерминированный поток PGS (`.sup`) для фикстуры `pgs-basic.mkv`.
//!
//! Запуск: `cargo run -p subtitle-pgs --example write_test_sup -- <out.sup>`.
//! Содержимое (система координат 64x64, как у видео фикстур) описано в
//! `crates/symphonia-demux/tests/fixtures/subtitles/README.md`.

use std::process::ExitCode;

use subtitle_core::{PictureForcedFlag, SubtitleTime};
use subtitle_pgs::pcs::{CompositionObject, CompositionState};
use subtitle_pgs::stream_builder::{
    DisplaySetSpec, ObjectImageSpec, PaletteColorSpec, PaletteSpec, PgsStreamBuilder,
};
use subtitle_pgs::wds::WindowDefinition;

/// Размер системы координат совпадает с размером видео фикстур.
const REFERENCE_SIZE: u16 = 64;

fn palette(palette_id: u8, colors: [[u8; 4]; 2]) -> PaletteSpec {
    PaletteSpec {
        palette_id,
        version: 0,
        colors: colors
            .iter()
            .enumerate()
            .map(|(position, rgba)| PaletteColorSpec {
                // Индексы 1 и 2: индекс 0 зарезервирован под прозрачный фон.
                index: u8::try_from(position + 1).unwrap_or(1),
                rgba: *rgba,
            })
            .collect(),
    }
}

/// Картинка из двух вертикальных половин: левая — индекс 1, правая — индекс 2.
fn split_image(object_id: u16, width: u16, height: u16) -> ObjectImageSpec {
    let indices = (0..height)
        .flat_map(|_| (0..width).map(move |column| if column < width / 2 { 1 } else { 2 }))
        .collect();
    ObjectImageSpec {
        object_id,
        width,
        height,
        indices,
        max_rle_bytes_per_fragment: None,
    }
}

fn composition_object(
    object_id: u16,
    x: u16,
    y: u16,
    forced: PictureForcedFlag,
) -> CompositionObject {
    CompositionObject {
        object_id,
        window_id: 0,
        forced,
        x,
        y,
        crop: None,
    }
}

fn full_window() -> WindowDefinition {
    WindowDefinition {
        window_id: 0,
        x: 0,
        y: 0,
        width: REFERENCE_SIZE,
        height: REFERENCE_SIZE,
    }
}

fn build_stream() -> PgsStreamBuilder {
    // Обычная реплика 0.5-1.5 с: 16x8, слева красный, справа зелёный, позиция (4, 40).
    let regular = DisplaySetSpec::new(REFERENCE_SIZE, REFERENCE_SIZE, CompositionState::EpochStart)
        .with_window(full_window())
        .with_palette(palette(0, [[255, 0, 0, 255], [0, 255, 0, 255]]))
        .with_image(split_image(1, 16, 8))
        .with_composition_object(composition_object(1, 4, 40, PictureForcedFlag::Regular));
    // Forced-реплика 2.0-2.6 с: 12x6, слева синий, справа белый наполовину прозрачный, (44, 6).
    let forced = DisplaySetSpec::new(REFERENCE_SIZE, REFERENCE_SIZE, CompositionState::EpochStart)
        .with_window(full_window())
        .with_palette(palette(0, [[0, 0, 255, 255], [255, 255, 255, 128]]))
        .with_image(split_image(2, 12, 6))
        .with_composition_object(composition_object(2, 44, 6, PictureForcedFlag::Forced));
    // «Очистка»: композиция без объектов.
    let clear = DisplaySetSpec::new(REFERENCE_SIZE, REFERENCE_SIZE, CompositionState::Normal);

    let mut builder = PgsStreamBuilder::new();
    builder
        .push_display_set(SubtitleTime::from_millis(500), &regular)
        .push_display_set(SubtitleTime::from_millis(1_500), &clear)
        .push_display_set(SubtitleTime::from_millis(2_000), &forced)
        .push_display_set(SubtitleTime::from_millis(2_600), &clear);
    builder
}

fn main() -> ExitCode {
    let Some(output_path) = std::env::args().nth(1) else {
        eprintln!("usage: write_test_sup <out.sup>");
        return ExitCode::FAILURE;
    };
    match std::fs::write(&output_path, build_stream().to_sup_bytes()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cannot write {output_path}: {error}");
            ExitCode::FAILURE
        }
    }
}
