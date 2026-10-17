use super::*;
use crate::bitmap::CoverageBitmap;
use crate::fixed_point::Vector26;

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

// --- Повороты, наклон, перспектива (calc_transform_matrix, 3D-путь) ---

/// Положение глифа: 100 px справа от центра поворота, на самом центре по вертикали.
fn placement(rotation: Rotation3d, shear: ShearFactors) -> GlyphPlacement {
    GlyphPlacement {
        rotation,
        shear,
        scale: (1.0, 1.0),
        shift: Vector26 { x: 0, y: 0 },
        ascent: 0,
        frame_position: Vector26 {
            x: 500 * 64,
            y: 300 * 64,
        },
        pixel_aspect_x: 1.0,
    }
}

/// Куда матрица переводит точку контура (x, y) в 26.6: образ после деления на z.
fn project(matrix: &Matrix3, x: f64, y: f64) -> (f64, f64, f64) {
    let z = matrix[2][0] * x + matrix[2][1] * y + matrix[2][2];
    (
        (matrix[0][0] * x + matrix[0][1] * y + matrix[0][2]) / z,
        (matrix[1][0] * x + matrix[1][1] * y + matrix[1][2]) / z,
        z,
    )
}

#[test]
fn without_rotation_or_shear_the_matrix_is_the_plain_translation() {
    let matrix = glyph_transform_matrix(&placement(Rotation3d::default(), ShearFactors::default()));
    assert_eq!(matrix, unrotated_glyph_matrix(500 * 64, 300 * 64));
}

#[test]
fn frz_rotates_a_point_counter_clockwise_around_the_centre() {
    // Центр поворота в кадре — frame_position (shift = 0): точка (+64, 0) при 90 градусах
    // уходит вверх на экране (y уменьшается), как у libass.
    let rotation = Rotation3d {
        z_degrees: 90.0,
        ..Rotation3d::default()
    };
    let matrix = glyph_transform_matrix(&placement(rotation, ShearFactors::default()));
    let (x, y, _) = project(&matrix, 64.0, 0.0);
    assert!((x - 500.0 * 64.0).abs() < 1e-6, "x {x}");
    assert!((y - (300.0 * 64.0 - 64.0)).abs() < 1e-6, "y {y}");
    assert!(!matrix_has_perspective(&matrix), "frz не даёт перспективы");
}

/// Есть ли у матрицы перспектива (строка z зависит от x или y).
fn matrix_has_perspective(matrix: &Matrix3) -> bool {
    matrix[2][0] != 0.0 || matrix[2][1] != 0.0
}

#[test]
fn frx_and_fry_make_the_z_row_depend_on_the_point() {
    let frx = Rotation3d {
        x_degrees: 45.0,
        ..Rotation3d::default()
    };
    let fry = Rotation3d {
        y_degrees: 45.0,
        ..Rotation3d::default()
    };
    // frx наклоняет по оси y контура, fry — по оси x.
    assert!(glyph_transform_matrix(&placement(frx, ShearFactors::default()))[2][1] != 0.0);
    assert!(glyph_transform_matrix(&placement(fry, ShearFactors::default()))[2][0] != 0.0);
}

#[test]
fn fax_slides_points_in_proportion_to_height_and_anchors_at_the_baseline() {
    let mut pose = placement(
        Rotation3d::default(),
        ShearFactors {
            horizontal: 0.5,
            vertical: 0.0,
        },
    );
    pose.ascent = 40 * 64;
    let matrix = glyph_transform_matrix(&pose);
    // Опора наклона — базовая линия (на `ascent` ниже верха ячейки): её точка сдвинута на
    // `ascent * fax`. Ось y контура направлена вниз, поэтому точка y = -64 лежит выше базовой
    // линии на 64 единицы и сдвигается ещё на `fax * 64` в сторону, противоположную нулевой.
    let (on_baseline, _, _) = project(&matrix, 0.0, 0.0);
    let (above, _, _) = project(&matrix, 0.0, -64.0);
    assert!((on_baseline - (500.0 * 64.0 + 0.5 * 40.0 * 64.0)).abs() < 1e-6);
    assert!(
        ((above - on_baseline) - (0.5 * -64.0)).abs() < 1e-6,
        "{above}"
    );
}

#[test]
fn point_far_from_the_centre_goes_behind_the_camera() {
    // Глиф в 400 px от центра поворота (больше расстояния камеры 312.5 px), повёрнутый на
    // 90 градусов вокруг оси X, оказывается за камерой: ключа нет, паники нет.
    let (_, cbox) = unit_box();
    let mut pose = placement(
        Rotation3d {
            x_degrees: 90.0,
            ..Rotation3d::default()
        },
        ShearFactors::default(),
    );
    pose.shift = Vector26 { x: 0, y: 400 * 64 };
    let mut matrix =
        scale_and_offset_matrix(&glyph_transform_matrix(&pose), (1.0, 1.0), (0.0, 0.0));
    let mut offset = [0.0; 2];
    assert!(quantize_transform(&mut matrix, &mut offset, true, &cbox).is_none());
}

#[test]
fn perspective_rotation_quantizes_a_z_row_and_builds_a_narrower_bitmap() {
    let (outline, cbox) = unit_box();
    let empty = Outline::new();
    let build = |rotation: Rotation3d| {
        let pose = placement(rotation, ShearFactors::default());
        let mut matrix =
            scale_and_offset_matrix(&glyph_transform_matrix(&pose), (200.0, 200.0), (0.0, 0.0));
        let mut offset = [0.0; 2];
        let key = quantize_transform(&mut matrix, &mut offset, true, &cbox).expect("key");
        let mut rasterizer = Rasterizer::new(RASTERIZER_PRECISION);
        let bitmap = construct_bitmap(&mut rasterizer, &key, &cbox, [&outline, &empty])
            .expect("raster")
            .expect("non-empty");
        (key, bitmap)
    };
    let (flat_key, flat) = build(Rotation3d::default());
    assert!(!flat_key.has_perspective());
    let (turned_key, turned) = build(Rotation3d {
        y_degrees: 60.0,
        ..Rotation3d::default()
    });
    assert!(turned_key.has_perspective());
    // Квадрат 200x200 px, повернутый на 60 градусов вокруг вертикали, вдвое уже (cos 60 = 0.5).
    assert!(
        turned.width * 10 < flat.width * 7,
        "{} vs {}",
        turned.width,
        flat.width
    );
    // Высота при повороте вокруг вертикали остаётся прежней (с запасом на края перспективы).
    assert!((turned.height - flat.height).abs() <= 2);
    assert!(turned.buffer.iter().any(|value| *value != 0));
}

#[test]
fn restored_matrix_of_an_affine_key_has_the_identity_z_row() {
    let (_, cbox) = unit_box();
    let mut matrix = scale_and_offset_matrix(&unrotated_glyph_matrix(0, 0), (3.0, 3.0), (0.0, 0.0));
    let mut offset = [0.0; 2];
    let key = quantize_transform(&mut matrix, &mut offset, true, &cbox).expect("key");
    assert_eq!(key.matrix_z, [0, 0]);
    let restored = restore_transform(&key, &cbox);
    assert_eq!(restored[2], [0.0, 0.0, 1.0]);
}
