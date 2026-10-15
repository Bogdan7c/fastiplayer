use super::*;
use crate::bitmap::CoverageBitmap;

fn unit_box() -> (Outline, ControlBox) {
    let mut outline = Outline::new();
    outline.add_rect(0, 0, 64, 64).expect("rect");
    let cbox = control_box(&[&outline]);
    (outline, cbox)
}

#[test]
fn position_is_quantized_to_eighths_of_a_pixel_around_the_box_center() {
    let (_, cbox) = unit_box();
    // Глиф в точке (10 px, 20 px); центр рамки 0.5 px -> 10.5 px = 84/8, 20.5 px = 164/8.
    let mut matrix = scale_and_offset_matrix(
        &unrotated_glyph_matrix(10 * 64, 20 * 64),
        (1.0, 1.0),
        (0.0, 0.0),
    );
    let mut offset = [0.0; 2];
    let key = quantize_transform(&mut matrix, &mut offset, true, &cbox).expect("key");
    assert_eq!((key.position.x, key.position.y), (10, 20));
    assert_eq!(key.offset, [4, 4], "half a pixel = 4/8");
}

#[test]
fn glyph_behind_the_camera_has_no_key() {
    let (_, cbox) = unit_box();
    let mut matrix = unrotated_glyph_matrix(0, 0);
    matrix[2][2] = -1.0;
    let mut offset = [0.0; 2];
    assert!(quantize_transform(&mut matrix, &mut offset, true, &cbox).is_none());
}

#[test]
fn constructed_bitmap_covers_the_transformed_box() {
    let (outline, cbox) = unit_box();
    let empty = Outline::new();
    // Единичный квадрат 64 -> масштаб 10 px: ровно квадрат 10x10 пикселей.
    let mut matrix = scale_and_offset_matrix(
        &unrotated_glyph_matrix(100 * 64, 50 * 64),
        (10.0, 10.0),
        (0.0, 0.0),
    );
    let mut offset = [0.0; 2];
    let key = quantize_transform(&mut matrix, &mut offset, true, &cbox).expect("key");
    let mut rasterizer = Rasterizer::new(RASTERIZER_PRECISION);
    let bitmap: CoverageBitmap = construct_bitmap(&mut rasterizer, &key, &cbox, [&outline, &empty])
        .expect("raster")
        .expect("non-empty");
    let covered: u32 = bitmap.buffer.iter().map(|v| u32::from(*v)).sum();
    // Площадь 100 px^2 при покрытии 255 (допуск на округление краёв).
    assert!(
        (covered as i64 - 100 * 255).abs() < 400,
        "coverage {covered}"
    );
    // Первый закрашенный столбец — ровно x = 100 (битмап начинается на пиксель левее).
    let stride = bitmap.stride;
    let first_column = (0..stride)
        .find(|column| bitmap.buffer.chunks(stride).any(|row| row[*column] != 0))
        .expect("ink");
    assert_eq!(bitmap.left + key.position.x + first_column as i32, 100);
}

#[test]
fn border_plan_scales_with_matrix_and_rejects_overflow() {
    let (_, cbox) = unit_box();
    let mut matrix =
        scale_and_offset_matrix(&unrotated_glyph_matrix(0, 0), (0.1875, 0.1875), (0.0, 0.0));
    let mut offset = [0.0; 2];
    quantize_transform(&mut matrix, &mut offset, true, &cbox).expect("key");
    // Обводка 2 px при масштабе 0.1875: 64*2/0.1875 единиц контура.
    let plan = plan_border_stroke(&matrix, &cbox, (64.0 * 2.0 / 0.1875, 64.0 * 2.0 / 0.1875))
        .expect("plan");
    assert!(plan.border_x > 0 && plan.border_y > 0);
    assert_eq!(plan.border_x, plan.border_y);
    assert!(plan_border_stroke(&matrix, &cbox, (1e30, 1.0)).is_none());
}
