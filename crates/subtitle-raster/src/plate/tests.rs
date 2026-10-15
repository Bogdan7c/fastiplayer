use subtitle_core::SubtitleColor;

use super::*;
use crate::overlay::{OverlayChange, OverlayRevision, SubtitleOverlay};

const PLATE_COLOR: SubtitleColor = SubtitleColor {
    red: 8,
    green: 8,
    blue: 8,
    alpha: 191,
};

fn canvas() -> CanvasSize {
    CanvasSize::new(400, 200).expect("canvas")
}

fn style(radius: f64) -> PlateStyle {
    PlateStyle {
        color: PLATE_COLOR,
        horizontal_padding: 0.0,
        vertical_padding: 0.0,
        corner_radius: radius,
    }
}

fn line(left: f64, top: f64, right: f64, bottom: f64) -> LineBox {
    LineBox {
        left,
        right,
        top,
        bottom,
    }
}

fn pixel(bitmap: &SubtitleBitmap, x: i32, y: i32) -> u8 {
    let column = (x - bitmap.origin().x) as usize;
    let row = (y - bitmap.origin().y) as u32;
    bitmap.coverage_row(row).expect("row")[column]
}

#[test]
fn rounded_corners_are_cut_while_side_middles_are_solid() {
    let plate = build_plate(&[line(50.0, 50.0, 150.0, 90.0)], &style(12.0), canvas())
        .expect("build")
        .expect("plate");
    assert_eq!(plate.kind(), SubtitleBitmapKind::Background);
    assert_eq!(plate.color(), PLATE_COLOR);
    // Угловой пиксель вне скругления, середины сторон и центр залиты полностью.
    assert!(
        pixel(&plate, 50, 50) < 20,
        "corner {}",
        pixel(&plate, 50, 50)
    );
    // Крайние пиксели у выровненных по сетке сторон: растеризатор libass (как и в оригинале)
    // даёт на них 250..255 из 255 из-за расширения рамки на 1/64 пикселя.
    assert!(pixel(&plate, 50, 70) >= 254);
    assert!(pixel(&plate, 100, 50) >= 254);
    assert!(pixel(&plate, 149, 70) >= 250);
    assert!(pixel(&plate, 100, 89) >= 254);
    assert_eq!(pixel(&plate, 100, 70), 255);
    assert_eq!(pixel(&plate, 51, 70), 255);
    // Вне прямоугольника почти ничего нет (до 3/255 от расширения рамки на 1/64 px).
    assert!(pixel(&plate, 49, 70) <= 3);
}

#[test]
fn zero_radius_gives_a_sharp_rectangle() {
    let plate = build_plate(&[line(50.0, 50.0, 150.0, 90.0)], &style(0.0), canvas())
        .expect("build")
        .expect("plate");
    assert!(pixel(&plate, 50, 50) >= 254);
    assert!(pixel(&plate, 149, 89) >= 254);
}

#[test]
fn overlapping_lines_are_one_mask_without_double_darkening() {
    // Две строки перекрываются на 10 строк пикселей (y 40..50).
    let lines = [line(50.0, 20.0, 200.0, 50.0), line(80.0, 40.0, 170.0, 70.0)];
    let plate = build_plate(&lines, &style(0.0), canvas())
        .expect("build")
        .expect("plate");
    let overlay = SubtitleOverlay::new(
        canvas(),
        vec![plate.clone()],
        OverlayRevision::INITIAL,
        OverlayChange::Replaced,
    )
    .expect("overlay");
    let rgba = overlay.composite_rgba_over_transparent();
    let alpha_at = |x: usize, y: usize| rgba[(y * 400 + x) * 4 + 3];
    // В зоне перекрытия альфа плашки та же, что вне его: 191 из 255.
    assert_eq!(alpha_at(100, 45), 191);
    assert_eq!(alpha_at(100, 30), 191);
    assert_eq!(alpha_at(100, 60), 191);
    // Узкая вторая строка не расширяет первую: правее неё внизу пусто.
    assert_eq!(alpha_at(190, 60), 0);
}

#[test]
fn padding_expands_each_side_and_clipping_keeps_plate_inside_canvas() {
    let padded = PlateStyle {
        horizontal_padding: 10.0,
        vertical_padding: 4.0,
        ..style(0.0)
    };
    let plate = build_plate(&[line(50.0, 50.0, 150.0, 90.0)], &padded, canvas())
        .expect("build")
        .expect("plate");
    assert!(pixel(&plate, 40, 46) >= 254);
    assert!(pixel(&plate, 159, 93) >= 250);
    assert!(pixel(&plate, 39, 70) <= 3);
    assert!(pixel(&plate, 160, 70) <= 3);

    // Плашка, торчащая за левый верхний угол холста, обрезается без ошибки.
    let clipped = build_plate(&[line(-30.0, -10.0, 60.0, 30.0)], &style(0.0), canvas())
        .expect("build")
        .expect("plate");
    assert_eq!((clipped.origin().x, clipped.origin().y), (0, 0));
    // Ink: x 0..60, y 0..30 (+ до 1 px от расширения рамки растеризатора).
    assert!((60..=62).contains(&clipped.width()), "{}", clipped.width());
    assert!(
        (30..=32).contains(&clipped.height()),
        "{}",
        clipped.height()
    );
}

#[test]
fn nothing_to_draw_gives_no_plate() {
    assert!(
        build_plate(&[], &style(5.0), canvas())
            .expect("build")
            .is_none()
    );
    let transparent = PlateStyle {
        color: SubtitleColor::TRANSPARENT,
        ..style(5.0)
    };
    assert!(
        build_plate(&[line(1.0, 1.0, 50.0, 20.0)], &transparent, canvas())
            .expect("build")
            .is_none()
    );
    // Целиком за холстом.
    assert!(
        build_plate(&[line(500.0, 10.0, 600.0, 30.0)], &style(0.0), canvas())
            .expect("build")
            .is_none()
    );
}
