//! Тесты растеризатора: аналитические формы, окно заливки, ошибки и устройство сегментов.

use super::{FillWindow, OutlineLayer, RasterError, Rasterizer};
use crate::outline::{Outline, OutlineError, Point, SegmentKind};

/// 26.6 из пикселей.
fn d6(pixels: f64) -> i32 {
    (pixels * 64.0).round() as i32
}

/// Многоугольник (замкнутая ломаная) по вершинам в пикселях.
fn polygon(vertices: &[(f64, f64)]) -> Outline {
    let mut outline = Outline::new();
    for (x, y) in vertices {
        outline.push_point(Point::new(d6(*x), d6(*y))).unwrap();
        outline.push_segment(SegmentKind::Line);
    }
    assert!(outline.close_contour());
    outline
}

/// Несколько замкнутых контуров в одном `Outline` (одна группа намотки).
fn polygons(contours: &[&[(f64, f64)]]) -> Outline {
    let mut outline = Outline::new();
    for vertices in contours {
        for (x, y) in *vertices {
            outline.push_point(Point::new(d6(*x), d6(*y))).unwrap();
            outline.push_segment(SegmentKind::Line);
        }
        assert!(outline.close_contour());
    }
    outline
}

/// Растеризует контур(ы) в окно `width x height` с началом в (0, 0); stride равен ширине.
fn rasterize(
    primary: &Outline,
    additional: Option<&Outline>,
    width: usize,
    height: usize,
) -> Vec<u8> {
    let mut rasterizer = Rasterizer::new(16);
    rasterizer
        .set_outline(primary, OutlineLayer::Primary)
        .unwrap();
    if let Some(extra) = additional {
        rasterizer
            .set_outline(extra, OutlineLayer::Additional)
            .unwrap();
    }
    let mut buffer = vec![0xAB; width * height];
    rasterizer
        .fill(
            &mut buffer,
            FillWindow {
                x0: 0,
                y0: 0,
                width,
                height,
                stride: width,
            },
        )
        .unwrap();
    buffer
}

fn pixel(buffer: &[u8], width: usize, x: usize, y: usize) -> u8 {
    buffer[y * width + x]
}

/// Сумма покрытия в «площади» пикселей.
fn covered_area(buffer: &[u8]) -> f64 {
    buffer.iter().map(|&value| f64::from(value)).sum::<f64>() / 255.0
}

#[test]
fn integer_rectangle_is_opaque_inside_and_empty_outside_across_tiles() {
    let outline = polygon(&[(5.0, 3.0), (45.0, 3.0), (45.0, 28.0), (5.0, 28.0)]);
    let buffer = rasterize(&outline, None, 64, 48);
    for y in 0..48 {
        for x in 0..64 {
            let deep_inside = (6..44).contains(&x) && (4..27).contains(&y);
            let far_outside = !(4..46).contains(&x) || !(2..29).contains(&y);
            let value = pixel(&buffer, 64, x, y);
            if deep_inside {
                assert_eq!(value, 255, "pixel ({x}, {y}) is inside");
            } else if far_outside {
                assert_eq!(value, 0, "pixel ({x}, {y}) is outside");
            }
        }
    }
    // Горизонтальные границы на целых координатах точны.
    assert_eq!(pixel(&buffer, 64, 20, 3), 255);
    assert_eq!(pixel(&buffer, 64, 20, 2), 0);
    assert_eq!(pixel(&buffer, 64, 20, 27), 255);
    assert_eq!(pixel(&buffer, 64, 20, 28), 0);
}

#[test]
fn vertical_integer_edges_leak_like_the_libass_c_rasterizer() {
    // 16-битная арифметика заливки тайла даёт на целых вертикальных границах не ровно 0/255:
    // значения сняты с C-растеризатора libass 0.17.5 (ass_rasterizer.c, C-движок) на тех же данных.
    let outline = polygon(&[(5.0, 3.0), (45.0, 3.0), (45.0, 28.0), (5.0, 28.0)]);
    let buffer = rasterize(&outline, None, 64, 48);
    for y in 3..28 {
        assert_eq!(pixel(&buffer, 64, 4, y), 3, "row {y}");
        assert_eq!(pixel(&buffer, 64, 5, y), 254, "row {y}");
        assert_eq!(pixel(&buffer, 64, 44, y), 252, "row {y}");
        assert_eq!(pixel(&buffer, 64, 45, y), 1, "row {y}");
    }
}

#[test]
fn half_pixel_edges_give_half_coverage() {
    // Левый край на x = 2.5, верхний на y = 3.5; правый и нижний — целые.
    let outline = polygon(&[(2.5, 3.5), (20.0, 3.5), (20.0, 12.0), (2.5, 12.0)]);
    let buffer = rasterize(&outline, None, 32, 16);
    let near = |actual: u8, expected: i32| (i32::from(actual) - expected).abs() <= 1;
    assert!(
        near(pixel(&buffer, 32, 2, 8), 128),
        "left edge: {}",
        pixel(&buffer, 32, 2, 8)
    );
    assert!(
        near(pixel(&buffer, 32, 10, 3), 128),
        "top edge: {}",
        pixel(&buffer, 32, 10, 3)
    );
    // Угол, где пересекаются два полупикселя: четверть покрытия (C-растеризатор даёт 62).
    assert_eq!(pixel(&buffer, 32, 2, 3), 62, "corner");
    assert_eq!(pixel(&buffer, 32, 2, 8), 127);
    assert_eq!(pixel(&buffer, 32, 1, 8), 0);
    assert_eq!(pixel(&buffer, 32, 3, 8), 255);
    assert_eq!(pixel(&buffer, 32, 10, 8), 255);
    assert_eq!(pixel(&buffer, 32, 10, 2), 0);
}

#[test]
fn quarter_pixel_edge_gives_proportional_coverage() {
    let outline = polygon(&[(2.25, 1.0), (10.0, 1.0), (10.0, 10.0), (2.25, 10.0)]);
    let buffer = rasterize(&outline, None, 16, 16);
    // Левый столбец покрыт на 75%.
    let value = i32::from(pixel(&buffer, 16, 2, 5));
    assert!(
        (value - 191).abs() <= 2,
        "75% coverage expected, got {value}"
    );
}

#[test]
fn diagonal_edge_covers_half_of_the_boundary_pixels() {
    // Прямоугольный треугольник с гипотенузой x + y = 24 внутри окна с запасом.
    let outline = polygon(&[(4.0, 4.0), (20.0, 4.0), (4.0, 20.0)]);
    let buffer = rasterize(&outline, None, 32, 32);
    let area = covered_area(&buffer);
    assert!((area - 128.0).abs() < 1.5, "area {area}");
    // Пиксель (11, 12) разрезан гипотенузой ровно пополам.
    let diagonal = i32::from(pixel(&buffer, 32, 11, 12));
    assert!((diagonal - 128).abs() <= 3, "diagonal pixel {diagonal}");
    assert_eq!(pixel(&buffer, 32, 6, 6), 255);
    assert_eq!(pixel(&buffer, 32, 18, 18), 0);
}

#[test]
fn shallow_diagonal_edge_values_match_the_libass_c_rasterizer() {
    // Треугольник с пологой гипотенузой в окне 128x64: большинство тайлов вдоль гипотенузы
    // содержат один отрезок (заливка полуплоскостью), угловые — общий случай. Значения сняты
    // с C-растеризатора libass 0.17.5 (C-движок, тайл 16) на тех же 26.6-данных.
    let outline = polygon(&[(4.0, 4.0), (110.0, 4.0), (30.0, 60.0)]);
    let buffer = rasterize(&outline, None, 128, 64);
    let golden = [
        (100, 10, 166),
        (101, 10, 16),
        (102, 10, 0),
        (86, 20, 115),
        (87, 20, 0),
        (88, 20, 0),
        (71, 30, 227),
        (72, 30, 64),
        (73, 30, 0),
        (57, 40, 192),
        (58, 40, 28),
        (59, 40, 0),
        (43, 50, 140),
        (44, 50, 3),
        (45, 50, 0),
    ];
    for (x, y, expected) in golden {
        assert_eq!(pixel(&buffer, 128, x, y), expected, "pixel ({x}, {y})");
    }
}

#[test]
fn diagonal_edge_across_many_tiles_matches_the_libass_c_rasterizer() {
    // Гипотенуза x + y = 104 проходит через много тайлов; все её пиксели — полупокрытие.
    let outline = polygon(&[(4.0, 4.0), (100.0, 4.0), (4.0, 100.0)]);
    let buffer = rasterize(&outline, None, 128, 128);
    for (x, y, expected) in [
        (60, 43, 126),
        (61, 43, 0),
        (40, 63, 126),
        (41, 63, 0),
        (30, 73, 126),
        (31, 73, 0),
        (50, 53, 127),
        (51, 53, 0),
        (70, 33, 127),
        (71, 33, 0),
        (10, 93, 126),
        (11, 93, 0),
        (95, 8, 126),
        (96, 8, 0),
        (90, 13, 126),
        (91, 13, 0),
        (20, 83, 127),
        (21, 83, 0),
    ] {
        assert_eq!(pixel(&buffer, 128, x, y), expected, "pixel ({x}, {y})");
    }
    assert!((covered_area(&buffer) - 4608.25).abs() < 0.01);
}

#[test]
fn non_zero_winding_fills_the_center_of_a_pentagram() {
    // Пентаграмма одним контуром: центр обходится дважды (намотка 2), при even-odd был бы дырой.
    let outline = polygon(&[
        (25.0, 0.0),
        (41.0, 48.0),
        (0.0, 18.0),
        (50.0, 18.0),
        (9.0, 48.0),
    ]);
    let buffer = rasterize(&outline, None, 64, 64);
    assert_eq!(pixel(&buffer, 64, 25, 24), 255, "center of the star");
    assert_eq!(pixel(&buffer, 64, 25, 5), 255, "top arm");
    assert_eq!(pixel(&buffer, 64, 2, 40), 0, "outside");
}

#[test]
fn contours_of_one_layer_follow_non_zero_winding() {
    let first = [(4.0, 4.0), (24.0, 4.0), (24.0, 24.0), (4.0, 24.0)];
    let same = [(12.0, 12.0), (32.0, 12.0), (32.0, 32.0), (12.0, 32.0)];
    let opposite = [(12.0, 12.0), (12.0, 32.0), (32.0, 32.0), (32.0, 12.0)];

    let aligned = rasterize(&polygons(&[&first, &same]), None, 48, 48);
    assert_eq!(
        pixel(&aligned, 48, 16, 16),
        255,
        "overlap of equal orientation stays filled"
    );
    assert_eq!(pixel(&aligned, 48, 6, 6), 255);
    assert_eq!(pixel(&aligned, 48, 30, 30), 255);
    assert_eq!(pixel(&aligned, 48, 30, 6), 0);

    let crossed = rasterize(&polygons(&[&first, &opposite]), None, 48, 48);
    assert_eq!(
        pixel(&crossed, 48, 16, 16),
        0,
        "windings cancel in the overlap"
    );
    assert_eq!(pixel(&crossed, 48, 6, 6), 255);
    assert_eq!(pixel(&crossed, 48, 30, 30), 255);
}

#[test]
fn primary_and_additional_layers_are_united_not_summed() {
    // Две границы обводки рисуются как объединение: у каждого слоя своя намотка,
    // противоположная ориентация второго слоя дыру в пересечении не даёт.
    let first = polygon(&[(4.0, 4.0), (24.0, 4.0), (24.0, 24.0), (4.0, 24.0)]);
    let opposite = polygon(&[(12.0, 12.0), (12.0, 32.0), (32.0, 32.0), (32.0, 12.0)]);
    let buffer = rasterize(&first, Some(&opposite), 48, 48);
    assert_eq!(pixel(&buffer, 48, 16, 16), 255);
    assert_eq!(pixel(&buffer, 48, 6, 6), 255);
    assert_eq!(pixel(&buffer, 48, 30, 30), 255);
    assert_eq!(pixel(&buffer, 48, 30, 6), 0);
}

#[test]
fn cubic_circle_has_the_area_of_a_circle() {
    // Четыре кубических дуги (k = 0.5522847) радиуса 20 px вокруг (32, 32).
    let (cx, cy, r) = (32.0, 32.0, 20.0);
    let k = 0.552_284_7 * r;
    let mut outline = Outline::new();
    let points = [
        (cx + r, cy),
        (cx + r, cy + k),
        (cx + k, cy + r),
        (cx, cy + r),
        (cx - k, cy + r),
        (cx - r, cy + k),
        (cx - r, cy),
        (cx - r, cy - k),
        (cx - k, cy - r),
        (cx, cy - r),
        (cx + k, cy - r),
        (cx + r, cy - k),
    ];
    for chunk in points.chunks(3) {
        for (x, y) in chunk {
            outline.push_point(Point::new(d6(*x), d6(*y))).unwrap();
        }
        outline.push_segment(SegmentKind::Cubic);
    }
    assert!(outline.close_contour());
    let buffer = rasterize(&outline, None, 64, 64);
    let area = covered_area(&buffer);
    let exact = std::f64::consts::PI * r * r;
    assert!(
        (area - exact).abs() / exact < 0.01,
        "area {area} vs {exact}"
    );
    assert_eq!(pixel(&buffer, 64, 32, 32), 255);
    assert_eq!(
        pixel(&buffer, 64, 14, 14),
        0,
        "corner of the bounding square is outside"
    );
    assert_eq!(
        pixel(&buffer, 64, 50, 32),
        255,
        "inside near the right extreme"
    );
    assert_eq!(
        pixel(&buffer, 64, 53, 32),
        0,
        "outside right of the extreme"
    );
}

#[test]
fn quadratic_segment_area_matches_the_parabola_formula() {
    // Параболический сегмент: хорда 40 px, стрелка прогиба 30 px, площадь (2/3) * 40 * 30 = 800.
    let mut outline = Outline::new();
    for (x, y) in [(10.0, 40.0), (30.0, -20.0)] {
        outline.push_point(Point::new(d6(x), d6(y + 20.0))).unwrap();
    }
    outline.push_segment(SegmentKind::Quadratic);
    outline.push_point(Point::new(d6(50.0), d6(60.0))).unwrap();
    outline.push_segment(SegmentKind::Line);
    assert!(outline.close_contour());
    let buffer = rasterize(&outline, None, 64, 64);
    let area = covered_area(&buffer);
    assert!((area - 800.0).abs() < 8.0, "area {area}");
}

#[test]
fn bounding_box_follows_primary_and_additional_layers() {
    let mut rasterizer = Rasterizer::new(16);
    assert!(rasterizer.bounding_box().is_empty());
    let first = polygon(&[(1.0, 2.0), (3.0, 2.0), (3.0, 4.0), (1.0, 4.0)]);
    let second = polygon(&[(10.0, 11.0), (12.0, 11.0), (12.0, 13.0), (10.0, 13.0)]);
    rasterizer
        .set_outline(&first, OutlineLayer::Primary)
        .unwrap();
    let bbox = rasterizer.bounding_box();
    assert_eq!(
        (bbox.x_min, bbox.y_min, bbox.x_max, bbox.y_max),
        (64, 128, 192, 256)
    );
    rasterizer
        .set_outline(&second, OutlineLayer::Additional)
        .unwrap();
    let bbox = rasterizer.bounding_box();
    assert_eq!(
        (bbox.x_min, bbox.y_min, bbox.x_max, bbox.y_max),
        (64, 128, 768, 832)
    );
    // Как в libass, рамка только растёт до следующего основного контура, даже если
    // дополнительный контур заменили (ломаная заменяется, `bbox` — нет).
    rasterizer
        .set_outline(&first, OutlineLayer::Additional)
        .unwrap();
    assert_eq!(rasterizer.bounding_box().x_max, 768);
    // Основной контур сбрасывает всё.
    rasterizer
        .set_outline(&second, OutlineLayer::Primary)
        .unwrap();
    let bbox = rasterizer.bounding_box();
    assert_eq!((bbox.x_min, bbox.x_max), (640, 768));
}

#[test]
fn window_offset_and_clipping_by_the_window_borders() {
    // Окно сдвинуто на (10, 20); прямоугольник частично выходит за окно со всех сторон.
    let outline = polygon(&[(5.0, 15.0), (30.0, 15.0), (30.0, 100.0), (5.0, 100.0)]);
    let mut rasterizer = Rasterizer::new(16);
    rasterizer
        .set_outline(&outline, OutlineLayer::Primary)
        .unwrap();
    let mut buffer = vec![0; 32 * 32];
    rasterizer
        .fill(
            &mut buffer,
            FillWindow {
                x0: 10,
                y0: 20,
                width: 32,
                height: 32,
                stride: 32,
            },
        )
        .unwrap();
    for y in 0..32 {
        for x in 0..32 {
            // Окно покрывает x в [10, 42) и y в [20, 52): внутри прямоугольника x < 30.
            let expected = if x < 20 { 255 } else { 0 };
            assert_eq!(pixel(&buffer, 32, x, y), expected, "pixel ({x}, {y})");
        }
    }
}

#[test]
fn stride_larger_than_width_leaves_padding_untouched() {
    let outline = polygon(&[(0.0, 0.0), (16.0, 0.0), (16.0, 16.0), (0.0, 16.0)]);
    let mut rasterizer = Rasterizer::new(16);
    rasterizer
        .set_outline(&outline, OutlineLayer::Primary)
        .unwrap();
    let mut buffer = vec![7_u8; 32 * 16];
    rasterizer
        .fill(
            &mut buffer,
            FillWindow {
                x0: 0,
                y0: 0,
                width: 16,
                height: 16,
                stride: 32,
            },
        )
        .unwrap();
    for row in buffer.chunks(32) {
        assert!(row[..16].iter().all(|&value| value == 255));
        assert!(
            row[16..].iter().all(|&value| value == 7),
            "padding is not owned by the window"
        );
    }
}

#[test]
fn polyline_is_consumed_by_fill() {
    let outline = polygon(&[(0.0, 0.0), (16.0, 0.0), (16.0, 16.0), (0.0, 16.0)]);
    let mut rasterizer = Rasterizer::new(16);
    rasterizer
        .set_outline(&outline, OutlineLayer::Primary)
        .unwrap();
    let window = FillWindow {
        x0: 0,
        y0: 0,
        width: 16,
        height: 16,
        stride: 16,
    };
    let mut first = vec![0; 256];
    rasterizer.fill(&mut first, window).unwrap();
    assert!(first.iter().all(|&value| value == 255));
    let mut second = vec![9; 256];
    rasterizer.fill(&mut second, window).unwrap();
    assert!(
        second.iter().all(|&value| value == 0),
        "no outline is loaded any more"
    );
}

#[test]
fn invalid_windows_and_buffers_are_reported_without_touching_the_buffer() {
    let outline = polygon(&[(0.0, 0.0), (16.0, 0.0), (16.0, 16.0), (0.0, 16.0)]);
    let try_fill = |window: FillWindow, buffer_len: usize| {
        let mut rasterizer = Rasterizer::new(16);
        rasterizer
            .set_outline(&outline, OutlineLayer::Primary)
            .unwrap();
        let mut buffer = vec![5_u8; buffer_len];
        let result = rasterizer.fill(&mut buffer, window);
        assert!(
            buffer.iter().all(|&value| value == 5),
            "buffer untouched on error"
        );
        result
    };
    let window = |width, height, stride| FillWindow {
        x0: 0,
        y0: 0,
        width,
        height,
        stride,
    };
    assert_eq!(
        try_fill(window(15, 16, 16), 256),
        Err(RasterError::InvalidWindow)
    );
    assert_eq!(
        try_fill(window(16, 0, 16), 256),
        Err(RasterError::InvalidWindow)
    );
    assert_eq!(
        try_fill(window(0, 16, 16), 256),
        Err(RasterError::InvalidWindow)
    );
    assert_eq!(
        try_fill(window(32, 16, 16), 512),
        Err(RasterError::InvalidWindow)
    );
    assert_eq!(
        try_fill(window(16, 16, 16), 255),
        Err(RasterError::BufferTooSmall)
    );
}

#[test]
fn malformed_outlines_are_rejected_by_set_outline() {
    let mut malformed = Outline::new();
    malformed.push_point(Point::new(0, 0)).unwrap();
    malformed.push_segment(SegmentKind::Cubic);
    let mut rasterizer = Rasterizer::new(16);
    assert_eq!(
        rasterizer.set_outline(&malformed, OutlineLayer::Primary),
        Err(RasterError::Outline(OutlineError::Malformed))
    );
}

#[test]
fn degenerate_outlines_produce_an_empty_bounding_box_and_empty_mask() {
    // Все точки совпадают: отрезков нет.
    let point = polygon(&[(3.0, 3.0), (3.0, 3.0), (3.0, 3.0)]);
    let mut rasterizer = Rasterizer::new(16);
    rasterizer
        .set_outline(&point, OutlineLayer::Primary)
        .unwrap();
    assert!(rasterizer.bounding_box().is_empty());
    let buffer = rasterize(&point, None, 16, 16);
    assert!(buffer.iter().all(|&value| value == 0));
}

#[test]
fn fill_tiles_rejects_a_visible_rect_outside_the_window_and_matches_the_whole_fill() {
    use super::TileRect;
    let shape = polygon(&[(2.5, 3.5), (60.25, 9.0), (50.0, 58.75), (5.0, 40.0)]);
    let load = || {
        let mut rasterizer = Rasterizer::new(16);
        rasterizer
            .set_outline(&shape, OutlineLayer::Primary)
            .unwrap();
        rasterizer
    };
    let window = |stride| FillWindow {
        x0: 0,
        y0: 0,
        width: 64,
        height: 64,
        stride,
    };
    // Прямоугольник тайлов за пределами окна и нулевого размера - ошибка окна, буфер цел.
    for bad in [
        TileRect {
            tile_x: 3,
            tile_y: 0,
            tiles_wide: 2,
            tiles_high: 1,
        },
        TileRect {
            tile_x: 0,
            tile_y: 0,
            tiles_wide: 0,
            tiles_high: 1,
        },
    ] {
        let mut buffer = vec![0xAB_u8; 64 * 64];
        assert_eq!(
            load().fill_tiles(&mut buffer, window(64), bad),
            Err(RasterError::InvalidWindow)
        );
        assert!(buffer.iter().all(|v| *v == 0xAB));
    }
    // Тайлы (1..3, 1..2) в узком буфере равны тем же пикселям при заливке всего окна.
    let mut whole = vec![0_u8; 64 * 64];
    load().fill(&mut whole, window(64)).unwrap();
    let visible = TileRect {
        tile_x: 1,
        tile_y: 1,
        tiles_wide: 2,
        tiles_high: 1,
    };
    let mut part = vec![0_u8; 32 * 16];
    load().fill_tiles(&mut part, window(32), visible).unwrap();
    for row in 0..16 {
        assert_eq!(
            part[row * 32..(row + 1) * 32],
            whole[(16 + row) * 64 + 16..(16 + row) * 64 + 48],
            "строка {row}"
        );
    }
    assert!(part.iter().any(|v| *v != 0));
}
