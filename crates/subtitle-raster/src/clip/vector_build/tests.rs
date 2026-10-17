use super::*;

fn canvas() -> CanvasSize {
    CanvasSize::new(4096, 4096).expect("canvas")
}

fn plan(mode: AssClipMode, drawing: &str, scale: f64) -> VectorClipPlan {
    VectorClipPlan {
        mode,
        drawing: drawing.to_owned(),
        scale_x: scale,
        scale_y: scale,
    }
}

/// Покрытие маски в точке холста (0 вне маски).
fn mask_value(mask: &VectorClipMask, x: i32, y: i32) -> u8 {
    let image = crate::overlay::SubtitleBitmap::new(
        crate::overlay::PixelPoint { x, y },
        1,
        1,
        1,
        std::sync::Arc::from([255_u8]),
        subtitle_core::SubtitleColor::opaque(255, 255, 255),
        crate::overlay::SubtitleBitmapKind::Glyph,
    )
    .expect("pixel");
    // Применение инверсного клипа к пикселю 255 даёт 255 - значение маски (с округлением libass).
    match mask.apply(&image) {
        super::super::vector::VectorClipEffect::Replaced(result) => result.coverage()[0],
        super::super::vector::VectorClipEffect::Removed => 0,
        super::super::vector::VectorClipEffect::Unchanged => 0,
    }
}

fn ready(plan: &VectorClipPlan) -> VectorClipMask {
    match build_vector_clip_mask(plan, canvas()).expect("rasterizer") {
        VectorClipOutcome::Ready(mask) => mask,
        other => panic!("маска должна быть готова: {other:?}"),
    }
}

#[test]
fn square_drawing_becomes_a_solid_mask_at_the_scaled_position() {
    // Квадрат 20x30 в точке (100, 50), масштаб кадра 2: на экране (200, 100) .. (240, 160).
    let mask = ready(&plan(
        AssClipMode::Inside,
        "m 100 50 l 120 50 120 80 100 80",
        2.0,
    ));
    // Внутри квадрата картинка из 255 проходит как есть, вне — удаляется.
    assert_eq!(mask_value(&mask, 210, 110), 255);
    // Край фигуры квантуется матрицей libass (около 0,06 %): крайний пиксель почти полный.
    assert!(mask_value(&mask, 239, 159) > 240);
    assert!(mask_value(&mask, 199, 110) < 8);
    assert!(mask_value(&mask, 240, 110) < 8);
    assert!(mask_value(&mask, 210, 99) < 8);
    assert!(mask_value(&mask, 210, 160) < 8);
}

#[test]
fn halving_the_matrix_halves_the_shape() {
    let full = ready(&plan(AssClipMode::Inside, "m 0 0 l 64 0 64 64 0 64", 1.0));
    let half = ready(&plan(AssClipMode::Inside, "m 0 0 l 64 0 64 64 0 64", 0.5));
    assert_eq!(mask_value(&full, 50, 50), 255);
    assert_eq!(mask_value(&half, 50, 50), 0);
    assert_eq!(mask_value(&half, 30, 30), 255);
}

#[test]
fn drawing_without_outline_gives_an_empty_but_valid_mask() {
    let mask = ready(&plan(AssClipMode::Inside, "m 5 5", 1.0));
    assert_eq!(mask.mask_bytes(), 0);
}

#[test]
fn rejected_drawing_is_ignored_like_an_invalid_outline_in_libass() {
    let outcome = build_vector_clip_mask(
        &plan(AssClipMode::Inside, "m 0 0 l 99999999999 0", 1.0),
        canvas(),
    )
    .expect("no rasterizer error");
    assert_eq!(
        outcome,
        VectorClipOutcome::Ignored(IgnoredVectorClip::InvalidDrawing)
    );
}

#[test]
fn zero_scale_matrix_degenerates_into_an_empty_mask() {
    // `\clip(0, ...)`: libass обнуляет множитель (`1 << 31` отрицательно) -> нулевая матрица.
    let outcome = build_vector_clip_mask(
        &plan(AssClipMode::Inside, "m 0 0 l 64 0 64 64 0 64", 0.0),
        canvas(),
    )
    .expect("no rasterizer error");
    match outcome {
        VectorClipOutcome::Ready(mask) => assert_eq!(mask_value(&mask, 10, 10), 0),
        VectorClipOutcome::Ignored(_) => {}
    }
}

#[test]
fn huge_shape_is_rasterized_only_where_the_canvas_is_and_keeps_clipping() {
    // Фигура 100000x50100 px, углом далеко за холстом: нижний край на y = 100. Раньше маска
    // шире предела стороны отбрасывалась, и клип переставал клипать (всё оставалось видно).
    let huge = ready(&plan(
        AssClipMode::Inside,
        "m -50000 -50000 l 50000 -50000 50000 100 -50000 100",
        1.0,
    ));
    assert_eq!(mask_value(&huge, 50, 50), 255, "внутри фигуры");
    assert!(mask_value(&huge, 2000, 92) > 250, "внутри у нижнего края");
    assert!(mask_value(&huge, 50, 108) < 5, "под нижним краем");
    assert_eq!(mask_value(&huge, 2000, 3000), 0, "далеко под фигурой");
    // Память маски ограничена холстом (4096 x 4096 + тайл), а не фигурой в сто тысяч пикселей.
    assert!(huge.mask_bytes() <= 4112 * 4112, "{}", huge.mask_bytes());
}

#[test]
fn shape_entirely_outside_the_canvas_gives_an_empty_mask() {
    let far = ready(&plan(
        AssClipMode::Inside,
        "m 90000 90000 l 91000 90000 91000 91000 90000 91000",
        1.0,
    ));
    assert_eq!(far.mask_bytes(), 0);
    assert_eq!(mask_value(&far, 10, 10), 0);
}

#[test]
fn restricted_mask_has_the_same_pixels_as_the_unrestricted_one_inside_the_canvas() {
    // Контур с косой стороной и кривой: сравниваем каждый пиксель холста с растеризацией
    // целиком на очень большом холсте (регион покрывает всю фигуру).
    let drawing = "m 10 10 l 300 40 b 380 200 320 260 200 300 l 20 280";
    let small_canvas = CanvasSize::new(256, 256).expect("canvas");
    let restricted =
        match build_vector_clip_mask(&plan(AssClipMode::Inside, drawing, 1.0), small_canvas)
            .expect("rasterizer")
        {
            VectorClipOutcome::Ready(mask) => mask,
            other => panic!("{other:?}"),
        };
    let whole = ready(&plan(AssClipMode::Inside, drawing, 1.0));
    for y in 0..256 {
        for x in 0..256 {
            assert_eq!(
                mask_value(&restricted, x, y),
                mask_value(&whole, x, y),
                "({x}, {y})"
            );
        }
    }
}
