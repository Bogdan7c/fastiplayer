use std::sync::Arc;

use subtitle_core::SubtitleColor;

use super::*;
use crate::overlay::{PixelPoint, SubtitleBitmapKind};

fn canvas() -> CanvasSize {
    CanvasSize::new(640, 360).expect("canvas")
}

/// Картинка 50x40 в точке (100, 100); значение пикселя кодирует его координаты в картинке,
/// чтобы по куску можно было проверить, ОТКУДА он вырезан.
fn gradient_image() -> SubtitleBitmap {
    let (width, height) = (50_u32, 40_u32);
    let coverage: Vec<u8> = (0..height)
        .flat_map(|row| (0..width).map(move |column| (row * 5 + column) as u8 + 1))
        .collect();
    SubtitleBitmap::new(
        PixelPoint { x: 100, y: 100 },
        width,
        height,
        width as usize,
        Arc::from(coverage),
        SubtitleColor::opaque(255, 255, 255),
        SubtitleBitmapKind::Glyph,
    )
    .expect("image")
}

fn parts_of(clip: &ScreenRectClip) -> Vec<SubtitleBitmap> {
    let mut parts = Vec::new();
    clip.push_visible_parts(&gradient_image(), ImagePaint::AsStored, &mut parts);
    parts
}

fn geometry(part: &SubtitleBitmap) -> (i32, i32, u32, u32) {
    (
        part.origin().x,
        part.origin().y,
        part.width(),
        part.height(),
    )
}

fn clip(mode: AssClipMode, x0: i32, y0: i32, x1: i32, y1: i32) -> ScreenRectClip {
    ScreenRectClip::new(mode, [x0, y0, x1, y1], canvas())
}

#[test]
fn inside_clip_keeps_the_intersection_with_the_original_pixels() {
    let parts = parts_of(&clip(AssClipMode::Inside, 110, 120, 130, 135));
    let [part] = parts.as_slice() else {
        panic!("одна часть, а не {}", parts.len());
    };
    assert_eq!(geometry(part), (110, 120, 20, 15));
    // Пиксель (0,0) куска — это пиксель (10,20) исходной картинки.
    assert_eq!(
        part.coverage_row(0).expect("row")[0],
        (20 * 5 + 10) as u8 + 1
    );
    assert_eq!(
        part.coverage_row(14).expect("row")[19],
        (34 * 5 + 29) as u8 + 1
    );
    assert_eq!(part.color(), gradient_image().color());
}

#[test]
fn inside_clip_without_overlap_or_with_flipped_corners_removes_the_image() {
    for corners in [
        (0, 0, 90, 90),       // целиком левее и выше
        (200, 200, 300, 300), // целиком правее и ниже
        (130, 120, 110, 135), // углы перевёрнуты: пустой прямоугольник
        (110, 120, 110, 135), // нулевая ширина
    ] {
        let (x0, y0, x1, y1) = corners;
        assert!(
            parts_of(&clip(AssClipMode::Inside, x0, y0, x1, y1)).is_empty(),
            "{corners:?}"
        );
    }
}

#[test]
fn inside_clip_is_limited_by_the_canvas() {
    // Клип шире холста не может вернуть то, чего на холсте нет: картинка и так внутри.
    let parts = parts_of(&clip(AssClipMode::Inside, -1000, -1000, 5000, 5000));
    assert_eq!(
        parts.iter().map(geometry).collect::<Vec<_>>(),
        vec![(100, 100, 50, 40)]
    );
}

#[test]
fn inverse_clip_around_a_hole_gives_four_parts_left_top_bottom_right() {
    let parts = parts_of(&clip(AssClipMode::Outside, 110, 120, 130, 135));
    assert_eq!(
        parts.iter().map(geometry).collect::<Vec<_>>(),
        vec![
            (100, 100, 10, 40), // слева
            (110, 100, 20, 20), // над дырой
            (110, 135, 20, 5),  // под дырой
            (130, 100, 20, 40), // справа
        ]
    );
    // Куски вырезаны из тех же мест исходной картинки.
    assert_eq!(parts[3].coverage_row(0).expect("row")[0], 30 + 1);
    assert_eq!(
        parts[2].coverage_row(0).expect("row")[0],
        (35 * 5 + 10) as u8 + 1
    );
}

#[test]
fn inverse_clip_hole_touching_edges_drops_the_empty_parts() {
    // Дыра прижата к левому краю и верху картинки: слева и сверху кусков нет.
    let parts = parts_of(&clip(AssClipMode::Outside, 0, 0, 120, 120));
    assert_eq!(
        parts.iter().map(geometry).collect::<Vec<_>>(),
        vec![(100, 120, 20, 20), (120, 100, 30, 40)]
    );
}

#[test]
fn inverse_clip_that_misses_the_image_keeps_it_whole_and_covering_one_hides_it() {
    let miss = parts_of(&clip(AssClipMode::Outside, 300, 300, 400, 340));
    assert_eq!(
        miss.iter().map(geometry).collect::<Vec<_>>(),
        vec![(100, 100, 50, 40)]
    );
    assert_eq!(miss[0].coverage(), gradient_image().coverage());

    assert!(parts_of(&clip(AssClipMode::Outside, 0, 0, 640, 360)).is_empty());
}

#[test]
fn inverse_clip_with_flipped_corners_keeps_libass_overlapping_parts() {
    // Перевёрнутая «дыра»: по формулам `render_glyph_i` левый и правый куски заходят друг на
    // друга (общая полоса рисуется дважды), верхний и нижний отбрасываются как пустые.
    let parts = parts_of(&clip(AssClipMode::Outside, 130, 135, 110, 120));
    assert_eq!(
        parts.iter().map(geometry).collect::<Vec<_>>(),
        vec![(100, 100, 30, 40), (110, 100, 40, 40)]
    );
}
