//! Тесты битмапов покрытия: рамка и выравнивание, сдвиг, fix_outline, сложение, тень.

use super::{
    BitmapError, CoverageBitmap, PixelBounds, PlacedBitmap, RasterRegion, combine_bitmaps,
    combine_bitmaps_with_padding, fix_outline, outline_to_bitmap, outline_to_bitmap_within,
    shadow_bitmap, shift_bitmap,
};
use crate::outline::{OUTLINE_MAX, Outline, Point, SegmentKind};
use crate::raster::Rasterizer;

fn d6(pixels: f64) -> i32 {
    (pixels * 64.0).round() as i32
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Outline {
    let mut outline = Outline::new();
    outline.add_rect(d6(x0), d6(y0), d6(x1), d6(y1)).unwrap();
    outline
}

/// Битмап с заданными значениями в левом верхнем углу (остальное — нули).
fn bitmap_with(width: i32, height: i32, left: i32, top: i32, rows: &[&[u8]]) -> CoverageBitmap {
    let mut bitmap = CoverageBitmap::zeroed(width, height).unwrap();
    bitmap.left = left;
    bitmap.top = top;
    for (y, row) in rows.iter().enumerate() {
        let start = y * bitmap.stride;
        bitmap.buffer[start..start + row.len()].copy_from_slice(row);
    }
    bitmap
}

fn row_prefix(bitmap: &CoverageBitmap, row: i32, length: usize) -> Vec<u8> {
    bitmap.row(row).unwrap()[..length].to_vec()
}

#[test]
fn zeroed_bitmap_rounds_the_stride_up_to_sixteen() {
    let bitmap = CoverageBitmap::zeroed(17, 3).unwrap();
    assert_eq!((bitmap.width, bitmap.height, bitmap.stride), (17, 3, 32));
    assert_eq!(bitmap.buffer.len(), 32 * 3);
    assert!(bitmap.buffer.iter().all(|&value| value == 0));
    assert_eq!(CoverageBitmap::zeroed(16, 1).unwrap().stride, 16);
    assert_eq!(CoverageBitmap::zeroed(0, 5).unwrap().buffer.len(), 0);
}

#[test]
fn zeroed_bitmap_rejects_negative_and_gigantic_sizes() {
    assert!(matches!(
        CoverageBitmap::zeroed(-1, 5),
        Err(BitmapError::TooLarge { .. })
    ));
    assert!(matches!(
        CoverageBitmap::zeroed(5, -1),
        Err(BitmapError::TooLarge { .. })
    ));
    assert!(matches!(
        CoverageBitmap::zeroed(50_000, 50_000),
        Err(BitmapError::TooLarge { .. })
    ));
}

#[test]
fn outline_to_bitmap_pads_the_box_and_aligns_to_tiles() {
    let mut rasterizer = Rasterizer::new(16);
    let bitmap = outline_to_bitmap(&mut rasterizer, Some(&rect(5.0, 3.0, 45.0, 28.0)), None)
        .unwrap()
        .expect("rectangle is not empty");
    // x: (320 - 1) >> 6 = 4 .. (2880 + 127) >> 6 = 46 -> 42 px -> 48 по тайлам;
    // y: (192 - 1) >> 6 = 2 .. (1792 + 127) >> 6 = 29 -> 27 px -> 32 по тайлам.
    assert_eq!((bitmap.left, bitmap.top), (4, 2));
    assert_eq!((bitmap.width, bitmap.height, bitmap.stride), (48, 32, 48));
    assert_eq!(bitmap.buffer.len(), 48 * 32);
    // Внутри прямоугольника покрытие полное, у самой границы — нет.
    assert_eq!(
        bitmap.row(10 - bitmap.top).unwrap()[(20 - bitmap.left) as usize],
        255
    );
    // Первый столбец (x = 4) вне прямоугольника: только «просачивание» 16-битной арифметики
    // libass на целой вертикальной границе (в C-растеризаторе там тоже 2..3).
    assert!(bitmap.row(10 - bitmap.top).unwrap()[0] <= 4);
    // Не заливаются пиксели вне фигуры: нижний тайловый хвост пуст.
    assert!(bitmap.row(31).unwrap().iter().all(|&value| value == 0));
}

#[test]
fn outline_to_bitmap_accepts_either_outline_alone_and_unites_both() {
    let left = rect(2.0, 2.0, 10.0, 10.0);
    let right = rect(30.0, 2.0, 38.0, 10.0);
    let mut rasterizer = Rasterizer::new(16);

    let only_additional = outline_to_bitmap(&mut rasterizer, None, Some(&left))
        .unwrap()
        .unwrap();
    assert_eq!(
        only_additional.row(5 - only_additional.top).unwrap()[(5 - only_additional.left) as usize],
        255
    );

    let both = outline_to_bitmap(&mut rasterizer, Some(&left), Some(&right))
        .unwrap()
        .unwrap();
    let at = |x: i32, y: i32| both.row(y - both.top).unwrap()[(x - both.left) as usize];
    assert_eq!(at(5, 5), 255);
    assert_eq!(at(33, 5), 255);
    assert_eq!(at(20, 5), 0);
    assert_eq!((both.left, both.top), (1, 1));
}

#[test]
fn outline_to_bitmap_reports_nothing_to_draw_without_an_error() {
    let mut rasterizer = Rasterizer::new(16);
    assert!(
        outline_to_bitmap(&mut rasterizer, None, None)
            .unwrap()
            .is_none()
    );
    assert!(
        outline_to_bitmap(
            &mut rasterizer,
            Some(&Outline::new()),
            Some(&Outline::new())
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn outline_to_bitmap_refuses_gigantic_glyph_boxes() {
    let mut huge = Outline::new();
    huge.add_rect(-OUTLINE_MAX, -OUTLINE_MAX, OUTLINE_MAX, OUTLINE_MAX)
        .unwrap();
    let mut rasterizer = Rasterizer::new(16);
    assert!(matches!(
        outline_to_bitmap(&mut rasterizer, Some(&huge), None),
        Err(BitmapError::TooLarge { .. })
    ));
}

#[test]
fn outline_to_bitmap_reports_malformed_outlines() {
    let mut malformed = Outline::new();
    malformed.push_point(Point::new(0, 0)).unwrap();
    malformed.push_segment(SegmentKind::Cubic);
    let mut rasterizer = Rasterizer::new(16);
    assert!(matches!(
        outline_to_bitmap(&mut rasterizer, Some(&malformed), None),
        Err(BitmapError::Raster(_))
    ));
}

#[test]
fn shift_moves_a_fraction_of_each_pixel_to_the_right_neighbour_without_cascading() {
    // value * shift >> 6 уходит вправо; обход справа налево исключает каскад.
    let mut bitmap = bitmap_with(4, 1, 0, 0, &[&[200, 0, 0, 0]]);
    shift_bitmap(&mut bitmap, 32, 0).unwrap();
    assert_eq!(row_prefix(&bitmap, 0, 4), [100, 100, 0, 0]);

    let mut bitmap = bitmap_with(4, 1, 0, 0, &[&[200, 100, 0, 0]]);
    shift_bitmap(&mut bitmap, 16, 0).unwrap();
    // x=2: 100*16>>6 = 25 -> [.., 75, 25]; x=1: 200*16>>6 = 50 -> [150, 125, 25, 0].
    assert_eq!(row_prefix(&bitmap, 0, 4), [150, 125, 25, 0]);
}

#[test]
fn shift_moves_down_in_the_vertical_direction() {
    let mut bitmap = bitmap_with(1, 3, 0, 0, &[&[200], &[100], &[0]]);
    shift_bitmap(&mut bitmap, 0, 32).unwrap();
    // y=2: 100*32>>6 = 50 -> [200, 50, 50]; y=1: 200*32>>6 = 100 -> [100, 150, 50].
    let column: Vec<u8> = (0..3).map(|y| bitmap.row(y).unwrap()[0]).collect();
    assert_eq!(column, [100, 150, 50]);
}

#[test]
fn shift_adds_modulo_256_like_the_c_uint8_arithmetic() {
    let mut bitmap = bitmap_with(2, 1, 0, 0, &[&[200, 200]]);
    shift_bitmap(&mut bitmap, 63, 0).unwrap();
    // 200*63>>6 = 196: левый 200 - 196 = 4; правый 200 + 196 = 396 -> 140 (переполнение uint8).
    assert_eq!(row_prefix(&bitmap, 0, 2), [4, 140]);
}

#[test]
fn shift_leaves_stride_padding_and_zero_shifts_alone() {
    let mut bitmap = bitmap_with(3, 1, 0, 0, &[&[90, 90, 90]]);
    let before = bitmap.clone();
    shift_bitmap(&mut bitmap, 0, 0).unwrap();
    assert_eq!(bitmap, before);
    bitmap.buffer[5] = 77; // padding после видимой ширины
    shift_bitmap(&mut bitmap, 63, 0).unwrap();
    assert_eq!(bitmap.buffer[5], 77);
    assert_eq!(bitmap.buffer[3], 0);
}

#[test]
fn shift_rejects_fractions_outside_one_pixel_without_changing_the_bitmap() {
    let mut bitmap = bitmap_with(2, 2, 0, 0, &[&[1, 2], &[3, 4]]);
    let before = bitmap.clone();
    for (shift_x, shift_y) in [(64, 0), (-1, 0), (0, 64), (0, -5)] {
        assert_eq!(
            shift_bitmap(&mut bitmap, shift_x, shift_y),
            Err(BitmapError::InvalidShift { shift_x, shift_y })
        );
        assert_eq!(bitmap, before);
    }
}

#[test]
fn shift_rejects_inconsistent_geometry() {
    let mut bitmap = bitmap_with(4, 2, 0, 0, &[&[1, 2, 3, 4]]);
    bitmap.buffer.truncate(20); // нужно 2 * 16
    assert_eq!(
        shift_bitmap(&mut bitmap, 8, 8),
        Err(BitmapError::InconsistentGeometry)
    );
}

#[test]
fn fix_outline_subtracts_half_of_the_glyph_where_the_outline_is_stronger() {
    let glyph = bitmap_with(4, 1, 0, 0, &[&[0, 100, 255, 20]]);
    let mut outline = bitmap_with(4, 1, 0, 0, &[&[50, 100, 200, 255]]);
    fix_outline(&glyph, &mut outline).unwrap();
    // 50 > 0 -> 50; 100 > 100 нет -> 0; 200 > 255 нет -> 0; 255 > 20 -> 255 - 10.
    assert_eq!(row_prefix(&outline, 0, 4), [50, 0, 0, 245]);

    let glyph = bitmap_with(2, 1, 0, 0, &[&[10, 20]]);
    let mut outline = bitmap_with(2, 1, 0, 0, &[&[100, 100]]);
    fix_outline(&glyph, &mut outline).unwrap();
    assert_eq!(row_prefix(&outline, 0, 2), [95, 90]);
}

#[test]
fn fix_outline_touches_only_the_intersection_of_the_bitmaps() {
    // Глиф смещён вправо на 2 и вниз на 1 относительно обводки.
    let glyph = bitmap_with(4, 2, 2, 1, &[&[100, 100], &[100, 100]]);
    let mut outline = bitmap_with(4, 3, 0, 0, &[&[200; 4], &[200; 4], &[200; 4]]);
    let untouched = outline.clone();
    fix_outline(&glyph, &mut outline).unwrap();
    // Строка 0 вне глифа (top = 1) не меняется; в строках 1..3 с колонки 2 вычитается 50.
    assert_eq!(row_prefix(&outline, 0, 4), row_prefix(&untouched, 0, 4));
    assert_eq!(row_prefix(&outline, 1, 4), [200, 200, 150, 150]);
    assert_eq!(row_prefix(&outline, 2, 4), [200, 200, 150, 150]);
}

#[test]
fn fix_outline_ignores_disjoint_bitmaps_and_rejects_broken_ones() {
    let glyph = bitmap_with(4, 1, 100, 100, &[&[255; 4]]);
    let mut outline = bitmap_with(4, 1, 0, 0, &[&[9; 4]]);
    let before = outline.clone();
    fix_outline(&glyph, &mut outline).unwrap();
    assert_eq!(outline, before);

    let mut broken = glyph.clone();
    broken.buffer.truncate(3);
    assert_eq!(
        fix_outline(&broken, &mut outline),
        Err(BitmapError::InconsistentGeometry)
    );
    assert_eq!(outline, before, "failed call leaves the outline intact");
    let mut broken_outline = before.clone();
    broken_outline.buffer.clear();
    assert_eq!(
        fix_outline(&glyph, &mut broken_outline),
        Err(BitmapError::InconsistentGeometry)
    );
}

#[test]
fn combine_adds_with_saturation_into_the_union_rectangle() {
    let first = bitmap_with(4, 1, 0, 0, &[&[200, 200, 50, 0]]);
    let second = bitmap_with(4, 1, 0, 0, &[&[0, 100, 100, 100]]);
    let combined = combine_bitmaps(&[
        PlacedBitmap {
            bitmap: &first,
            anchor: (10, 20),
        },
        PlacedBitmap {
            bitmap: &second,
            anchor: (12, 20),
        },
    ])
    .unwrap()
    .unwrap();
    assert_eq!(
        (combined.left, combined.top, combined.width, combined.height),
        (10, 20, 6, 1)
    );
    // [200 200 50 0 . .] + [. . 0 100 100 100] со сдвигом на 2: сумма с насыщением до 255.
    assert_eq!(row_prefix(&combined, 0, 6), [200, 200, 50, 100, 100, 100]);

    let overlapping = combine_bitmaps(&[
        PlacedBitmap {
            bitmap: &first,
            anchor: (0, 0),
        },
        PlacedBitmap {
            bitmap: &first,
            anchor: (0, 0),
        },
    ])
    .unwrap()
    .unwrap();
    assert_eq!(row_prefix(&overlapping, 0, 4), [255, 255, 100, 0]);
}

#[test]
fn combine_of_a_single_bitmap_copies_it_with_the_anchor_applied() {
    let only = bitmap_with(4, 2, -3, 5, &[&[1, 2, 3, 4], &[5, 6, 7, 8]]);
    let copy = combine_bitmaps(&[PlacedBitmap {
        bitmap: &only,
        anchor: (100, 200),
    }])
    .unwrap()
    .unwrap();
    assert_eq!((copy.left, copy.top), (97, 205));
    assert_eq!(copy.buffer, only.buffer);
    assert_eq!(only.left, -3, "source stays untouched");
}

#[test]
fn combine_of_nothing_is_none_and_vertical_union_keeps_both_rows() {
    assert!(combine_bitmaps(&[]).unwrap().is_none());
    let upper = bitmap_with(2, 1, 0, 0, &[&[10, 20]]);
    let lower = bitmap_with(2, 1, 0, 0, &[&[30, 40]]);
    let combined = combine_bitmaps(&[
        PlacedBitmap {
            bitmap: &upper,
            anchor: (0, 0),
        },
        PlacedBitmap {
            bitmap: &lower,
            anchor: (0, 3),
        },
    ])
    .unwrap()
    .unwrap();
    assert_eq!(combined.height, 4);
    assert_eq!(row_prefix(&combined, 0, 2), [10, 20]);
    assert_eq!(row_prefix(&combined, 1, 2), [0, 0]);
    assert_eq!(row_prefix(&combined, 3, 2), [30, 40]);
}

#[test]
fn shadow_splits_the_offset_into_whole_pixels_and_a_fraction() {
    let source = bitmap_with(8, 1, 7, 9, &[&[0, 0, 0, 200, 0, 0, 0, 0]]);
    // 96 = 1 px + 32/64: сдвиг на пиксель вправо и половина значения в соседний.
    let shadow = shadow_bitmap(&source, 96, 0).unwrap();
    assert_eq!((shadow.left, shadow.top), (8, 9));
    assert_eq!(row_prefix(&shadow, 0, 6), [0, 0, 0, 100, 100, 0]);
    assert_eq!(source.left, 7, "the source bitmap is not modified");
    assert_eq!(source.buffer[3], 200);
}

#[test]
fn shadow_rounds_negative_offsets_toward_minus_infinity() {
    let source = bitmap_with(2, 2, 0, 0, &[&[0, 0], &[200, 0]]);
    // -70 = -2 px + 58/64: целая часть -2 (>> 6), дробная 58 (& 63).
    let shadow = shadow_bitmap(&source, -70, -70).unwrap();
    assert_eq!((shadow.left, shadow.top), (-2, -2));
    // 200 * 58 >> 6 = 181 уходит вправо (в колонке 1 той же строки) и вниз.
    assert_eq!(row_prefix(&shadow, 1, 2), [19, 181]);
}

#[test]
fn padded_combine_grows_every_side_and_never_takes_the_single_bitmap_shortcut() {
    let only = bitmap_with(2, 1, 0, 0, &[&[7, 9]]);
    let padded = combine_bitmaps_with_padding(
        &[PlacedBitmap {
            bitmap: &only,
            anchor: (10, 20),
        }],
        3,
    )
    .unwrap()
    .unwrap();
    // Запас 3 пикселя с каждой стороны: углы сдвинуты, размер вырос на 6 по каждой оси.
    assert_eq!(
        (padded.left, padded.top, padded.width, padded.height),
        (7, 17, 8, 7)
    );
    // Сами значения лежат на прежнем месте внутри запаса, всё остальное нули.
    assert_eq!(row_prefix(&padded, 3, 8), [0, 0, 0, 7, 9, 0, 0, 0]);
    assert!(
        (0..7)
            .filter(|row| *row != 3)
            .all(|row| row_prefix(&padded, row, 8).iter().all(|value| *value == 0))
    );
    // Пустой список и с запасом даёт `None`.
    assert!(combine_bitmaps_with_padding(&[], 3).unwrap().is_none());
}

/// Косой пятиугольник 300 x 220 пикселей с нецелыми вершинами: в нём есть и сплошные тайлы, и
/// граничные с разными наклонами.
fn skewed_pentagon() -> Outline {
    let mut outline = Outline::new();
    let vertices = [
        (3.3, 7.9),
        (301.6, 20.25),
        (290.1, 221.7),
        (140.45, 160.2),
        (10.75, 229.9),
    ];
    for (x, y) in vertices {
        outline.push_point(Point::new(d6(x), d6(y))).unwrap();
        outline.push_segment(SegmentKind::Line);
    }
    assert!(outline.close_contour());
    outline
}

fn rasterized(region: RasterRegion) -> Option<CoverageBitmap> {
    let mut rasterizer = Rasterizer::new(16);
    outline_to_bitmap_within(&mut rasterizer, Some(&skewed_pentagon()), None, region).unwrap()
}

/// Значение пикселя битмапа в координатах контура (0 вне битмапа).
fn value_at(bitmap: &CoverageBitmap, x: i32, y: i32) -> u8 {
    let (column, row) = (x - bitmap.left, y - bitmap.top);
    if column < 0 || row < 0 || column >= bitmap.width || row >= bitmap.height {
        return 0;
    }
    bitmap.buffer[row as usize * bitmap.stride + column as usize]
}

#[test]
fn region_bitmap_has_exactly_the_pixels_of_the_whole_bitmap_inside_the_region() {
    let whole = rasterized(RasterRegion::Everything).expect("whole");
    // Регион не по сетке тайлов, режет фигуру со всех сторон.
    let region = PixelBounds {
        left: 37,
        top: 5,
        right: 219,
        bottom: 190,
    };
    let part = rasterized(RasterRegion::Within(region)).expect("part");
    // Память не растёт вместе с фигурой: битмап чуть больше региона (не более тайла с каждой
    // стороны), а не 304 x 240 целиком.
    assert!(
        part.buffer.len() < whole.buffer.len() * 3 / 4,
        "{}",
        part.buffer.len()
    );
    for y in region.top..region.bottom {
        for x in region.left..region.right {
            assert_eq!(value_at(&part, x, y), value_at(&whole, x, y), "({x}, {y})");
        }
    }
    // Были и заполненные, и частичные пиксели: сравнение не вырождено.
    let values: Vec<u8> = (region.top..region.bottom)
        .flat_map(|y| (region.left..region.right).map(move |x| (x, y)))
        .map(|(x, y)| value_at(&whole, x, y))
        .collect();
    assert!(values.contains(&255));
    assert!(values.iter().filter(|v| **v > 0 && **v < 255).count() > 100);
}

#[test]
fn region_bitmap_ends_where_libass_bitmap_ends_not_at_the_tile_after_it() {
    let whole = rasterized(RasterRegion::Everything).expect("whole");
    // Регион шире фигуры: правый и нижний край битмапа те же, что у целого.
    let part = rasterized(RasterRegion::Within(PixelBounds {
        left: -1000,
        top: -1000,
        right: 1000,
        bottom: 1000,
    }))
    .expect("part");
    assert_eq!(
        (part.left, part.top, part.width, part.height),
        (whole.left, whole.top, whole.width, whole.height)
    );
    assert_eq!(part.buffer, whole.buffer);
}

#[test]
fn region_without_overlap_gives_no_bitmap_and_does_not_touch_state() {
    let mut rasterizer = Rasterizer::new(16);
    let nowhere = RasterRegion::Within(PixelBounds {
        left: 5000,
        top: 5000,
        right: 6000,
        bottom: 6000,
    });
    let result = outline_to_bitmap_within(&mut rasterizer, Some(&skewed_pentagon()), None, nowhere);
    assert_eq!(result, Ok(None));
    // Растеризатор остался пригоден: следующий вызов на всё работает.
    let again = outline_to_bitmap_within(
        &mut rasterizer,
        Some(&skewed_pentagon()),
        None,
        RasterRegion::Everything,
    )
    .unwrap()
    .expect("bitmap");
    assert!(again.buffer.contains(&255));
}
