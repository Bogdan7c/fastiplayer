//! Тесты выходного контракта: инварианты картинки, детектор изменений, композиция.

use super::*;

const WHITE: SubtitleColor = SubtitleColor::opaque(255, 255, 255);

fn solid_bitmap(
    x: i32,
    y: i32,
    side: u32,
    color: SubtitleColor,
    mask: &Arc<[u8]>,
) -> SubtitleBitmap {
    SubtitleBitmap::new(
        PixelPoint { x, y },
        side,
        side,
        side as usize,
        Arc::clone(mask),
        color,
        SubtitleBitmapKind::Glyph,
    )
    .unwrap()
}

fn full_mask(side: u32) -> Arc<[u8]> {
    vec![255_u8; (side * side) as usize].into()
}

#[test]
fn canvas_size_rejects_zero_sides() {
    assert!(CanvasSize::new(0, 10).is_none());
    assert!(CanvasSize::new(10, 0).is_none());
    let canvas = CanvasSize::new(1920, 1080).unwrap();
    assert_eq!((canvas.width(), canvas.height()), (1920, 1080));
}

#[test]
fn bitmap_constructor_rejects_broken_layouts() {
    let origin = PixelPoint { x: 0, y: 0 };
    let mask: Arc<[u8]> = vec![0_u8; 10].into();
    let build = |width, height, stride| {
        SubtitleBitmap::new(
            origin,
            width,
            height,
            stride,
            Arc::clone(&mask),
            WHITE,
            SubtitleBitmapKind::Glyph,
        )
    };
    assert_eq!(
        build(0, 2, 5).unwrap_err(),
        SubtitleBitmapError::EmptyArea {
            width: 0,
            height: 2
        }
    );
    assert_eq!(
        build(5, 2, 4).unwrap_err(),
        SubtitleBitmapError::StrideSmallerThanWidth {
            stride: 4,
            width: 5
        }
    );
    // 2 строки по шагу 8 и ширине 5 требуют 8 + 5 = 13 байт, а есть 10.
    assert_eq!(
        build(5, 2, 8).unwrap_err(),
        SubtitleBitmapError::CoverageTooShort {
            actual: 10,
            required: 13
        }
    );
    // Хвост шага у последней строки не обязателен: 5 + 5 = 10 байт хватает.
    assert!(build(5, 2, 5).is_ok());
}

#[test]
fn coverage_row_returns_exactly_width_bytes_per_stride() {
    let mask: Arc<[u8]> = vec![1, 2, 9, 3, 4, 9].into();
    let bitmap = SubtitleBitmap::new(
        PixelPoint { x: 0, y: 0 },
        2,
        2,
        3,
        mask,
        WHITE,
        SubtitleBitmapKind::Outline,
    )
    .unwrap();
    assert_eq!(bitmap.coverage_row(0), Some(&[1_u8, 2][..]));
    assert_eq!(bitmap.coverage_row(1), Some(&[3_u8, 4][..]));
    assert_eq!(bitmap.coverage_row(2), None);
}

#[test]
fn overlay_rejects_bitmaps_that_leave_the_canvas() {
    let canvas = CanvasSize::new(10, 10).unwrap();
    let mask = full_mask(4);
    let inside = solid_bitmap(6, 6, 4, WHITE, &mask);
    assert!(
        SubtitleOverlay::new(
            canvas,
            vec![inside],
            OverlayRevision::INITIAL,
            OverlayChange::Replaced
        )
        .is_ok()
    );
    for (x, y) in [(7, 6), (6, 7), (-1, 0), (0, -1)] {
        let outside = solid_bitmap(x, y, 4, WHITE, &mask);
        let error = SubtitleOverlay::new(
            canvas,
            vec![outside],
            OverlayRevision::INITIAL,
            OverlayChange::Replaced,
        )
        .unwrap_err();
        assert_eq!(error.bitmap_index, 0, "({x},{y})");
    }
}

#[test]
fn revision_grows_and_empty_overlay_reports_empty() {
    let next = OverlayRevision::INITIAL.next();
    assert_eq!(next.value(), 1);
    assert!(next > OverlayRevision::INITIAL);
    let overlay = SubtitleOverlay::new(
        CanvasSize::new(4, 4).unwrap(),
        Vec::new(),
        next,
        OverlayChange::Unchanged,
    )
    .unwrap();
    assert!(overlay.is_empty());
    assert_eq!(overlay.composite_rgba_over_transparent(), vec![0_u8; 64]);
}

#[test]
fn detect_change_differing_lengths_is_replaced() {
    let mask = full_mask(2);
    let one = vec![solid_bitmap(0, 0, 2, WHITE, &mask)];
    assert_eq!(detect_overlay_change(&[], &one), OverlayChange::Replaced);
    assert_eq!(detect_overlay_change(&one, &[]), OverlayChange::Replaced);
}

#[test]
fn detect_change_two_empty_lists_are_unchanged() {
    assert_eq!(detect_overlay_change(&[], &[]), OverlayChange::Unchanged);
}

#[test]
fn detect_change_same_masks_and_positions_are_unchanged() {
    let mask = full_mask(2);
    let list = vec![solid_bitmap(1, 1, 2, WHITE, &mask)];
    assert_eq!(
        detect_overlay_change(&list, &list.clone()),
        OverlayChange::Unchanged
    );
}

#[test]
fn detect_change_only_position_is_moved() {
    let mask = full_mask(2);
    let before = vec![
        solid_bitmap(1, 1, 2, WHITE, &mask),
        solid_bitmap(5, 5, 2, WHITE, &mask),
    ];
    let after = vec![
        solid_bitmap(1, 1, 2, WHITE, &mask),
        solid_bitmap(6, 5, 2, WHITE, &mask),
    ];
    assert_eq!(detect_overlay_change(&before, &after), OverlayChange::Moved);
}

#[test]
fn detect_change_replaced_wins_over_moved_in_later_pair() {
    let mask = full_mask(2);
    let other_mask = full_mask(2);
    let before = vec![
        solid_bitmap(1, 1, 2, WHITE, &mask),
        solid_bitmap(5, 5, 2, WHITE, &mask),
    ];
    // Первая пара только сдвинута, вторая — другая маска: итог Replaced.
    let after = vec![
        solid_bitmap(2, 1, 2, WHITE, &mask),
        solid_bitmap(5, 5, 2, WHITE, &other_mask),
    ];
    assert_eq!(
        detect_overlay_change(&before, &after),
        OverlayChange::Replaced
    );
}

#[test]
fn detect_change_equal_bytes_in_different_arc_is_replaced() {
    // Как в libass: сравнивается указатель, а не содержимое.
    let before = vec![solid_bitmap(0, 0, 2, WHITE, &full_mask(2))];
    let after = vec![solid_bitmap(0, 0, 2, WHITE, &full_mask(2))];
    assert_eq!(
        detect_overlay_change(&before, &after),
        OverlayChange::Replaced
    );
}

#[test]
fn detect_change_color_size_stride_or_kind_is_replaced() {
    let mask = full_mask(4);
    let base = solid_bitmap(0, 0, 2, WHITE, &mask);
    let red = solid_bitmap(0, 0, 2, SubtitleColor::opaque(255, 0, 0), &mask);
    assert_eq!(
        detect_overlay_change(std::slice::from_ref(&base), &[red]),
        OverlayChange::Replaced
    );

    let bigger = solid_bitmap(0, 0, 3, WHITE, &full_mask(3));
    assert_eq!(
        detect_overlay_change(std::slice::from_ref(&base), &[bigger]),
        OverlayChange::Replaced
    );

    let wider_stride = SubtitleBitmap::new(
        PixelPoint { x: 0, y: 0 },
        2,
        2,
        4,
        Arc::clone(&mask),
        WHITE,
        SubtitleBitmapKind::Glyph,
    )
    .unwrap();
    assert_eq!(
        detect_overlay_change(std::slice::from_ref(&base), &[wider_stride]),
        OverlayChange::Replaced
    );

    let other_kind = SubtitleBitmap::new(
        PixelPoint { x: 0, y: 0 },
        2,
        2,
        2,
        Arc::clone(base.shared_coverage()),
        WHITE,
        SubtitleBitmapKind::Shadow,
    )
    .unwrap();
    assert_eq!(
        detect_overlay_change(&[base], &[other_kind]),
        OverlayChange::Replaced
    );
}

fn pixel_at(rgba: &[u8], canvas_width: usize, x: usize, y: usize) -> [u8; 4] {
    let offset = (y * canvas_width + x) * 4;
    [
        rgba[offset],
        rgba[offset + 1],
        rgba[offset + 2],
        rgba[offset + 3],
    ]
}

#[test]
fn composite_single_layer_applies_coverage_times_alpha() {
    let canvas = CanvasSize::new(4, 4).unwrap();
    // Полупрозрачный красный (alpha 128) с покрытием 255 и 0 в двух пикселях.
    let mask: Arc<[u8]> = vec![255_u8, 0].into();
    let color = SubtitleColor {
        red: 200,
        green: 10,
        blue: 20,
        alpha: 128,
    };
    let bitmap = SubtitleBitmap::new(
        PixelPoint { x: 1, y: 2 },
        2,
        1,
        2,
        mask,
        color,
        SubtitleBitmapKind::Glyph,
    )
    .unwrap();
    let overlay = SubtitleOverlay::new(
        canvas,
        vec![bitmap],
        OverlayRevision::INITIAL,
        OverlayChange::Replaced,
    )
    .unwrap();
    let rgba = overlay.composite_rgba_over_transparent();
    assert_eq!(rgba.len(), 64);
    // Покрытие 255 -> альфа слоя 128, цвет прямой (не умноженный на альфу).
    assert_eq!(pixel_at(&rgba, 4, 1, 2), [200, 10, 20, 128]);
    // Покрытие 0 и все остальные пиксели остаются прозрачными.
    assert_eq!(pixel_at(&rgba, 4, 2, 2), [0, 0, 0, 0]);
    assert_eq!(pixel_at(&rgba, 4, 0, 0), [0, 0, 0, 0]);
}

#[test]
fn composite_overlapping_layers_follow_list_order() {
    let canvas = CanvasSize::new(3, 1).unwrap();
    let full: Arc<[u8]> = vec![255_u8; 2].into();
    // Нижний: непрозрачный синий в x=0..2. Верхний: красный alpha 128 в x=1..3.
    let bottom = SubtitleBitmap::new(
        PixelPoint { x: 0, y: 0 },
        2,
        1,
        2,
        Arc::clone(&full),
        SubtitleColor::opaque(0, 0, 255),
        SubtitleBitmapKind::Background,
    )
    .unwrap();
    let top = SubtitleBitmap::new(
        PixelPoint { x: 1, y: 0 },
        2,
        1,
        2,
        full,
        SubtitleColor {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 128,
        },
        SubtitleBitmapKind::Glyph,
    )
    .unwrap();
    let overlay = SubtitleOverlay::new(
        canvas,
        vec![bottom, top],
        OverlayRevision::INITIAL,
        OverlayChange::Replaced,
    )
    .unwrap();
    let rgba = overlay.composite_rgba_over_transparent();
    // x=0: только синий.
    assert_eq!(pixel_at(&rgba, 3, 0, 0), [0, 0, 255, 255]);
    // x=1: красный (a=128) над непрозрачным синим: a_out=255,
    // c = (255*128*255 + 0*255*127)/(255*255) = 128 для красного; синий: (0 + 255*255*127)/(255*255) = 127.
    assert_eq!(pixel_at(&rgba, 3, 1, 0), [128, 0, 127, 255]);
    // x=2: красный над прозрачным: цвет остаётся прямым, альфа 128.
    assert_eq!(pixel_at(&rgba, 3, 2, 0), [255, 0, 0, 128]);
}

#[test]
fn composite_order_matters() {
    let canvas = CanvasSize::new(1, 1).unwrap();
    let mask: Arc<[u8]> = vec![255_u8].into();
    let make = |color| {
        SubtitleBitmap::new(
            PixelPoint { x: 0, y: 0 },
            1,
            1,
            1,
            Arc::clone(&mask),
            color,
            SubtitleBitmapKind::Glyph,
        )
        .unwrap()
    };
    let green = make(SubtitleColor::opaque(0, 255, 0));
    let white = make(SubtitleColor::opaque(255, 255, 255));
    let top_white = SubtitleOverlay::new(
        canvas,
        vec![green.clone(), white.clone()],
        OverlayRevision::INITIAL,
        OverlayChange::Replaced,
    )
    .unwrap();
    let top_green = SubtitleOverlay::new(
        canvas,
        vec![white, green],
        OverlayRevision::INITIAL,
        OverlayChange::Replaced,
    )
    .unwrap();
    assert_eq!(
        top_white.composite_rgba_over_transparent(),
        vec![255, 255, 255, 255]
    );
    assert_eq!(
        top_green.composite_rgba_over_transparent(),
        vec![0, 255, 0, 255]
    );
}

#[test]
fn content_bounds_is_none_without_bitmaps() {
    let canvas = CanvasSize::new(100, 50).unwrap();
    let overlay = SubtitleOverlay::empty(canvas, OverlayRevision::INITIAL, OverlayChange::Replaced);
    assert_eq!(overlay.content_bounds(), None);
}

#[test]
fn content_bounds_is_union_of_all_bitmaps() {
    let canvas = CanvasSize::new(100, 50).unwrap();
    let mask = full_mask(10);
    let bitmaps = vec![
        solid_bitmap(5, 30, 10, WHITE, &mask),
        solid_bitmap(60, 20, 10, WHITE, &mask),
        solid_bitmap(40, 38, 10, WHITE, &mask),
    ];
    let overlay = SubtitleOverlay::new(
        canvas,
        bitmaps,
        OverlayRevision::INITIAL,
        OverlayChange::Replaced,
    )
    .unwrap();
    assert_eq!(
        overlay.content_bounds(),
        Some(CanvasRect {
            x: 5,
            y: 20,
            width: 65,
            height: 28
        })
    );
}

#[test]
fn content_bounds_of_single_bitmap_equals_its_rect() {
    let canvas = CanvasSize::new(100, 50).unwrap();
    let mask = full_mask(10);
    let overlay = SubtitleOverlay::new(
        canvas,
        vec![solid_bitmap(90, 40, 10, WHITE, &mask)],
        OverlayRevision::INITIAL,
        OverlayChange::Replaced,
    )
    .unwrap();
    assert_eq!(
        overlay.content_bounds(),
        Some(CanvasRect {
            x: 90,
            y: 40,
            width: 10,
            height: 10
        })
    );
}

/// Картинка 3x4 с различимыми строками: значение пикселя = номер строки + 1.
fn striped_bitmap(y: i32) -> SubtitleBitmap {
    let rows: Vec<u8> = (0..4_u8).flat_map(|row| [row + 1; 3]).collect();
    SubtitleBitmap::new(
        PixelPoint { x: 5, y },
        3,
        4,
        3,
        rows.into(),
        WHITE,
        SubtitleBitmapKind::Glyph,
    )
    .unwrap()
}

#[test]
fn clipped_shift_inside_the_canvas_shares_the_mask() {
    let bitmap = striped_bitmap(10);
    let moved = bitmap.shifted_vertically_clipped(-4, 100).unwrap();
    assert_eq!(moved.origin(), PixelPoint { x: 5, y: 6 });
    assert_eq!(moved.height(), 4);
    assert!(Arc::ptr_eq(
        moved.shared_coverage(),
        bitmap.shared_coverage()
    ));
}

#[test]
fn clipped_shift_crops_rows_above_the_canvas() {
    let moved = striped_bitmap(1)
        .shifted_vertically_clipped(-3, 100)
        .unwrap();
    // Строки 0 и 1 (y = -2 и -1) обрезаны: остались строки со значениями 3 и 4 у верхнего края.
    assert_eq!(moved.origin(), PixelPoint { x: 5, y: 0 });
    assert_eq!(moved.height(), 2);
    assert_eq!(moved.coverage_row(0).unwrap(), &[3, 3, 3]);
    assert_eq!(moved.coverage_row(1).unwrap(), &[4, 4, 4]);
}

#[test]
fn clipped_shift_crops_rows_below_the_canvas() {
    let moved = striped_bitmap(90)
        .shifted_vertically_clipped(8, 100)
        .unwrap();
    // Низ картинки на 102: две последние строки вышли за холст высотой 100.
    assert_eq!(moved.origin(), PixelPoint { x: 5, y: 98 });
    assert_eq!(moved.height(), 2);
    assert_eq!(moved.coverage_row(0).unwrap(), &[1, 1, 1]);
    // Обрезанные внизу строки не хранятся в маске (две видимые строки по 3 байта).
    assert_eq!(moved.coverage(), &[1, 1, 1, 2, 2, 2]);
}

#[test]
fn clipped_shift_outside_the_canvas_leaves_nothing() {
    assert!(
        striped_bitmap(10)
            .shifted_vertically_clipped(-20, 100)
            .is_none()
    );
    assert!(
        striped_bitmap(10)
            .shifted_vertically_clipped(200, 100)
            .is_none()
    );
}
