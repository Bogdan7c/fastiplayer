use super::*;

fn apply(rows: [[f32; 3]; 2], u: f32, v: f32) -> [f32; 2] {
    let dot = |row: [f32; 3]| row[0] * u + row[1] * v + row[2];
    [dot(rows[0]), dot(rows[1])]
}

#[test]
fn quarter_turns_map_display_corners_to_rotated_source_corners() {
    // Верхний левый угол экрана после поворота кадра на 90° по часовой показывает нижний левый
    // угол кадра декодера: source = (v, 1 − u).
    assert_eq!(
        apply(QuarterTurn::Identity.display_to_source_rows(), 0.0, 0.0),
        [0.0, 0.0]
    );
    assert_eq!(
        apply(QuarterTurn::Clockwise90.display_to_source_rows(), 0.0, 0.0),
        [0.0, 1.0]
    );
    assert_eq!(
        apply(QuarterTurn::Half.display_to_source_rows(), 0.0, 0.0),
        [1.0, 1.0]
    );
    assert_eq!(
        apply(QuarterTurn::Clockwise270.display_to_source_rows(), 0.0, 0.0),
        [1.0, 0.0]
    );
}

#[test]
fn y_axis_is_flipped_for_gl_window_coordinates() {
    let rect = PixelRect {
        x: 10,
        y: 20,
        width: 30,
        height: 40,
    };
    assert_eq!(
        to_gl_window_rect(rect, 100),
        Ok(GlWindowRect {
            x: 10,
            y: 40,
            width: 30,
            height: 40
        })
    );
}

#[test]
fn rectangle_below_the_target_gets_negative_gl_bottom() {
    let rect = PixelRect {
        x: 0,
        y: 90,
        width: 10,
        height: 20,
    };
    assert_eq!(to_gl_window_rect(rect, 100).map(|gl| gl.y), Ok(-10));
}

#[test]
fn huge_rectangle_is_a_typed_error() {
    let rect = PixelRect {
        x: u32::MAX,
        y: 0,
        width: 1,
        height: 1,
    };
    assert_eq!(
        to_gl_window_rect(rect, 100),
        Err(RectOutOfRange {
            rect,
            target_height: 100
        })
    );
}

#[test]
fn uncovered_parts_leave_only_what_no_rect_covers() {
    let target = PixelRect {
        x: 0,
        y: 0,
        width: 100,
        height: 50,
    };
    assert_eq!(uncovered_parts(target, &[target]), Vec::new());
    let sidebar_free = PixelRect {
        x: 30,
        y: 0,
        width: 70,
        height: 50,
    };
    assert_eq!(
        uncovered_parts(target, &[sidebar_free]),
        vec![PixelRect {
            x: 0,
            y: 0,
            width: 30,
            height: 50
        }]
    );
    // Две части рядом накрывают всё.
    let top = PixelRect {
        x: 0,
        y: 0,
        width: 100,
        height: 20,
    };
    let bottom = PixelRect {
        x: 0,
        y: 20,
        width: 100,
        height: 30,
    };
    assert_eq!(uncovered_parts(target, &[top, bottom]), Vec::new());
    // Прямоугольник за пределами цели ничего не накрывает.
    let outside = PixelRect {
        x: 200,
        y: 200,
        width: 5,
        height: 5,
    };
    assert_eq!(uncovered_parts(target, &[outside]), vec![target]);
}

#[test]
fn intersection_handles_touching_and_huge_rects() {
    let left = PixelRect {
        x: 0,
        y: 0,
        width: 10,
        height: 10,
    };
    let touching = PixelRect {
        x: 10,
        y: 0,
        width: 10,
        height: 10,
    };
    assert_eq!(left.intersection(touching), None);
    let huge = PixelRect {
        x: 5,
        y: 5,
        width: u32::MAX,
        height: u32::MAX,
    };
    assert_eq!(
        left.intersection(huge),
        Some(PixelRect {
            x: 5,
            y: 5,
            width: 5,
            height: 5
        })
    );
}

/// Картинка ли пиксель `index` области длиной `length` по формуле шейдера сессий 05–12:
/// `display = (index + 0.5) / length × scale + offset` в `[0, 1]`.
fn shader_would_paint(index: u32, length: u32, scale: f32, offset: f32) -> bool {
    let display =
        (f64::from(index) + 0.5) / f64::from(length) * f64::from(scale) + f64::from(offset);
    (0.0..=1.0).contains(&display)
}

#[test]
fn picture_rect_of_1080p_on_t410_panel_skips_45_rows_of_bars_on_each_side() {
    // Кадр 16:9 во всю ширину панели 1440×900 (render-core `PictureLayout::fit`): картинка 810
    // строк, полосы по 45 строк сверху и снизу.
    let viewport = PixelRect {
        x: 0,
        y: 0,
        width: 1440,
        height: 900,
    };
    let scale_y = (1920.0_f32 / 1080.0) / (1440.0_f32 / 900.0);
    let picture = PictureMapping {
        uv_scale: [1.0, scale_y],
        uv_offset: [0.0, (1.0 - scale_y) * 0.5],
    };
    assert_eq!(
        picture_pixel_rect(viewport, picture),
        Some(PixelRect {
            x: 0,
            y: 45,
            width: 1440,
            height: 810,
        })
    );
}

#[test]
fn picture_rect_matches_the_former_shader_test_pixel_by_pixel() {
    // Смещённая область, полосы слева и справа, дробные края: каждый пиксель внутри
    // прямоугольника — ровно те, что раньше красил шейдер, и только они.
    let viewport = PixelRect {
        x: 37,
        y: 11,
        width: 333,
        height: 201,
    };
    let picture = PictureMapping {
        uv_scale: [1.37, 1.0],
        uv_offset: [-0.185, 0.0],
    };
    let rect = picture_pixel_rect(viewport, picture).expect("picture is inside the area");
    for column in 0..viewport.width {
        let inside = (rect.x..rect.x + rect.width).contains(&(viewport.x + column));
        assert_eq!(
            inside,
            shader_would_paint(column, viewport.width, 1.37, -0.185),
            "column {column}"
        );
    }
    assert_eq!((rect.y, rect.height), (viewport.y, viewport.height));
}

#[test]
fn picture_rect_includes_a_pixel_whose_center_is_exactly_on_the_edge() {
    // Картинка начинается ровно в центре пикселя 2 из 8 (display(2.5 / 8) = 0) и кончается ровно
    // в центре пикселя 6 (display(6.5 / 8) = 1): оба внутри — границы включительно, как
    // `x < 0.0 || x > 1.0` в прежнем шейдере.
    let viewport = PixelRect {
        x: 0,
        y: 0,
        width: 8,
        height: 1,
    };
    let picture = PictureMapping {
        uv_scale: [2.0, 1.0],
        uv_offset: [-0.625, 0.0],
    };
    let rect = picture_pixel_rect(viewport, picture).expect("picture is inside the area");
    assert_eq!((rect.x, rect.width), (2, 5));
}

#[test]
fn picture_rect_is_absent_for_a_picture_outside_the_area_or_a_broken_mapping() {
    let viewport = PixelRect {
        x: 0,
        y: 0,
        width: 64,
        height: 64,
    };
    let shifted_away = PictureMapping {
        uv_scale: [1.0, 1.0],
        uv_offset: [2.0, 0.0],
    };
    assert_eq!(picture_pixel_rect(viewport, shifted_away), None);
    let not_a_number = PictureMapping {
        uv_scale: [f32::NAN, 1.0],
        uv_offset: [0.0, 0.0],
    };
    assert_eq!(picture_pixel_rect(viewport, not_a_number), None);
    assert_eq!(
        picture_pixel_rect(
            PixelRect {
                width: 0,
                ..viewport
            },
            PictureMapping::FILL
        ),
        None
    );
    // Картинка на всю область — вся область.
    assert_eq!(
        picture_pixel_rect(viewport, PictureMapping::FILL),
        Some(viewport)
    );
}
