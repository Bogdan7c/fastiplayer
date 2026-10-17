//! Тесты кэшей глифов: идентичность по указателю, негативное кэширование, исключение целой
//! позиции из ключа масок, склейки и вытеснение по лимитам.

use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::bitmap::{BitmapError, CoverageBitmap};
use crate::face_cache::FaceCache;
use crate::glyph_render::{GlyphOutline, GlyphVariant};
use crate::glyph_transform::{
    BorderStrokePlan, QuantizedTransform, RASTERIZER_PRECISION, control_box, quantize_transform,
    scale_and_offset_matrix, unrotated_glyph_matrix,
};
use crate::outline::Outline;
use crate::raster::Rasterizer;
use crate::shaping::shape_text;
use crate::source_text::SourceText;
use crate::span_look::TextDecorations;
use crate::test_support::{embedded_fonts, plain_style, plain_text};

#[test]
fn arc_identity_distinguishes_equal_content_and_follows_clones() {
    let first = Arc::new(String::from("same"));
    let twin = Arc::new(String::from("same"));
    assert_eq!(ArcId::new(&first), ArcId::new(&Arc::clone(&first)));
    assert_ne!(ArcId::new(&first), ArcId::new(&twin));
    // Ключ держит значение: ссылка в ключе - вторая сильная.
    let key = ArcId::new(&first);
    assert_eq!(Arc::strong_count(&first), 2);
    let mut table = HashMap::new();
    table.insert(key, 1);
    assert_eq!(table.get(&ArcId::new(&first)), Some(&1));
    assert_eq!(table.get(&ArcId::new(&twin)), None);
}

/// Контур-квадрат `side` x `side` пикселей (в 26.6) вместе с рамкой.
fn square(side: i32) -> Arc<GlyphOutline> {
    let mut outline = Outline::new();
    outline.add_rect(0, 0, side * 64, side * 64).expect("rect");
    Arc::new(GlyphOutline::from_outline(outline))
}

/// Квантованное преобразование квадрата `scale` px со сдвигом `(x, y)` px.
fn transform_of(outline: &GlyphOutline, scale: f64, x: i32, y: i32) -> QuantizedTransform {
    let mut matrix = scale_and_offset_matrix(
        &unrotated_glyph_matrix(x * 64, y * 64),
        (scale, scale),
        (0.0, 0.0),
    );
    let mut offset = [0.0; 2];
    quantize_transform(&mut matrix, &mut offset, true, &outline.control_box).expect("transform")
}

#[test]
fn bitmap_cache_ignores_the_integer_position_but_not_the_fraction() {
    let mut cache = BitmapCache::new(1 << 20);
    let mut rasterizer = Rasterizer::new(RASTERIZER_PRECISION);
    let shape = square(1);
    let source = BitmapSource::Outline(Arc::clone(&shape));

    let here = transform_of(&shape, 0.1, 10, 20);
    let moved = transform_of(&shape, 0.1, 110, 70);
    assert_ne!(here.position, moved.position);
    assert_eq!((here.offset, here.matrix_x), (moved.offset, moved.matrix_x));

    let first = cache
        .mask(&mut rasterizer, &source, &here)
        .expect("raster")
        .expect("mask");
    let shifted = cache
        .mask(&mut rasterizer, &source, &moved)
        .expect("raster")
        .expect("mask");
    assert!(
        Arc::ptr_eq(&first, &shifted),
        "same mask for a whole-pixel shift"
    );
    let statistics = cache.statistics();
    assert_eq!((statistics.misses, statistics.hits), (1, 1));
    // Дробный сдвиг - другая маска: другой ключ.
    let mut fractional = here;
    fractional.offset[0] = (fractional.offset[0] + 1) % 8;
    let other = cache
        .mask(&mut rasterizer, &source, &fractional)
        .expect("raster")
        .expect("mask");
    assert!(!Arc::ptr_eq(&first, &other));
    assert_eq!(cache.statistics().misses, 2);
    // Идентичность контура входит в ключ: тот же квадрат другим `Arc` - другая запись.
    let twin = BitmapSource::Outline(square(1));
    let from_twin = cache
        .mask(&mut rasterizer, &twin, &here)
        .expect("raster")
        .expect("mask");
    assert!(!Arc::ptr_eq(&first, &from_twin));
    assert_eq!(*first, *from_twin, "but the pixels are equal");
}

#[test]
fn bitmap_cache_remembers_empty_and_oversized_results() {
    let mut cache = BitmapCache::new(1 << 20);
    // Пустой контур: маски нет, и повторный запрос - попадание, а не новая растеризация.
    let empty = Arc::new(GlyphOutline::from_outline(Outline::new()));
    let transform = transform_of(&square(1), 0.1, 5, 5);
    let mut rasterizer = Rasterizer::new(RASTERIZER_PRECISION);
    let source = BitmapSource::Outline(empty);
    assert!(
        cache
            .mask(&mut rasterizer, &source, &transform)
            .expect("raster")
            .is_none()
    );
    assert!(
        cache
            .mask(&mut rasterizer, &source, &transform)
            .expect("raster")
            .is_none()
    );
    assert_eq!((cache.statistics().misses, cache.statistics().hits), (1, 1));

    // Предел стороны: отказ кэшируется и возвращается той же ошибкой.
    let big = square(1);
    let big_transform = transform_of(&big, 200.0, 0, 0);
    let mut small_limit = Rasterizer::new(RASTERIZER_PRECISION).with_max_bitmap_side(64);
    let source = BitmapSource::Outline(big);
    let first = cache.mask(&mut small_limit, &source, &big_transform);
    let second = cache.mask(&mut small_limit, &source, &big_transform);
    assert!(matches!(
        first,
        Err(BitmapError::ExceedsSideLimit { limit: 64, .. })
    ));
    assert_eq!(first, second);
    assert_eq!(cache.statistics().hits, 2);
    // Другой предел стороны - другая запись: ответ зависит от предела.
    let mut roomy = Rasterizer::new(RASTERIZER_PRECISION).with_max_bitmap_side(4096);
    assert!(
        cache
            .mask(&mut roomy, &source, &big_transform)
            .expect("fits")
            .is_some()
    );
}

#[test]
fn outline_cache_shares_glyph_outlines_and_clear_forgets_them() {
    let mut fonts = embedded_fonts();
    let style = plain_style();
    let source = SourceText::from_styled(&plain_text("AA"), &style);
    let mut faces = FaceCache::new(crate::test_support::SHAPING_BUDGET_BYTES);
    let clusters = shape_text(
        &mut fonts,
        &mut faces,
        &source,
        &style,
        crate::fonts::TextLanguagePreference::unknown(),
    )
    .expect("shaping");
    let glyph = clusters[0].root.clone();
    let mut cache = OutlineCache::new(100);
    let first = cache
        .glyph(&faces, glyph.face, glyph.glyph, GlyphVariant::PLAIN)
        .expect("face")
        .expect("outline");
    let again = cache
        .glyph(&faces, glyph.face, glyph.glyph, GlyphVariant::PLAIN)
        .expect("face")
        .expect("outline");
    assert!(Arc::ptr_eq(&first, &again));
    assert_eq!(
        (
            cache.statistics().misses,
            cache.statistics().hits,
            cache.statistics().entries
        ),
        (1, 1, 1)
    );
    // Другой вариант контура (подчёркивание) - другая запись с другим контуром.
    let underlined = GlyphVariant {
        decorations: TextDecorations {
            underline: true,
            strike_out: false,
        },
        ..GlyphVariant::PLAIN
    };
    let with_line = cache
        .glyph(&faces, glyph.face, glyph.glyph, underlined)
        .expect("face")
        .expect("outline");
    assert!(!Arc::ptr_eq(&first, &with_line));
    assert_ne!(
        first.outline, with_line.outline,
        "the underline adds geometry"
    );
    assert_eq!(cache.statistics().entries, 2);
    cache.clear();
    assert_eq!(cache.statistics().entries, 0);
    let rebuilt = cache
        .glyph(&faces, glyph.face, glyph.glyph, GlyphVariant::PLAIN)
        .expect("face")
        .expect("outline");
    assert!(
        !Arc::ptr_eq(&first, &rebuilt),
        "cleared cache builds a new outline"
    );
    assert_eq!(*first, *rebuilt);
}

#[test]
fn drawings_are_cached_including_rejected_ones() {
    let mut cache = OutlineCache::new(100);
    let parsed = |entry: DrawingEntry| match entry {
        DrawingEntry::Parsed(drawing) => drawing,
        DrawingEntry::Rejected => panic!("drawing must parse"),
    };
    let first = parsed(cache.drawing("m 0 0 l 10 0 10 10 0 10"));
    let again = parsed(cache.drawing("m 0 0 l 10 0 10 10 0 10"));
    assert!(Arc::ptr_eq(&first, &again));
    assert_eq!((first.width, first.height), (10 * 64, 10 * 64));
    let other = parsed(cache.drawing("m 0 0 l 20 0 20 20 0 20"));
    assert!(!Arc::ptr_eq(&first, &other));

    // Координата вне диапазона: отказ тоже запись, повторный разбор не нужен.
    assert!(matches!(
        cache.drawing("m 0 0 l 99999999999 0"),
        DrawingEntry::Rejected
    ));
    let misses = cache.statistics().misses;
    assert!(matches!(
        cache.drawing("m 0 0 l 99999999999 0"),
        DrawingEntry::Rejected
    ));
    assert_eq!(cache.statistics().misses, misses);
}

#[test]
fn strokes_are_cached_per_outline_and_parameters_including_failures() {
    let mut cache = OutlineCache::new(100);
    let shape = square(10);
    let plan = |border: i32| BorderStrokePlan {
        scale_order_x: 0,
        scale_order_y: 0,
        border_x: border,
        border_y: border,
    };
    let first = cache.stroked(&shape, &plan(2)).expect("stroke");
    let again = cache.stroked(&shape, &plan(2)).expect("stroke");
    assert!(Arc::ptr_eq(&first, &again));
    let thicker = cache.stroked(&shape, &plan(3)).expect("stroke");
    assert!(!Arc::ptr_eq(&first, &thicker));
    let twin = square(10);
    let for_twin = cache.stroked(&twin, &plan(2)).expect("stroke");
    assert!(
        !Arc::ptr_eq(&first, &for_twin),
        "identity includes the source outline"
    );
    // Рамка обводки считается по обеим границам и больше рамки исходного контура.
    assert_eq!(
        first.control_box,
        control_box(&[&first.positive, &first.negative])
    );
    assert!(first.control_box.x_min < shape.control_box.x_min);

    // Неудача обводки (толщина вне диапазона обводчика) кэшируется как `None`.
    assert!(cache.stroked(&shape, &plan(1 << 26)).is_none());
    let misses = cache.statistics().misses;
    assert!(cache.stroked(&shape, &plan(1 << 26)).is_none());
    assert_eq!(cache.statistics().misses, misses);
}

#[test]
fn unit_box_is_one_per_cache_and_a_unit_square() {
    let mut cache = OutlineCache::new(10);
    let first = cache.unit_box().expect("box");
    assert!(Arc::ptr_eq(&first, &cache.unit_box().expect("box")));
    assert_eq!((first.control_box.x_max, first.control_box.y_max), (64, 64));
}

#[test]
fn shared_mask_keeps_exactly_the_bytes_an_image_needs() {
    let mut bitmap = CoverageBitmap::zeroed(20, 3).expect("bitmap");
    bitmap.left = -4;
    bitmap.top = 7;
    for (index, byte) in bitmap.buffer.iter_mut().enumerate() {
        *byte = u8::try_from(index % 251).unwrap_or(0) | 1;
    }
    let stride = bitmap.stride;
    let expected: Vec<u8> = bitmap.buffer[..2 * stride + 20].to_vec();
    let mask = SharedMask::freeze(bitmap);
    assert_eq!(
        (mask.left, mask.top, mask.width, mask.height, mask.stride),
        (-4, 7, 20, 3, 32)
    );
    // Хвост шага последней строки не хранится: ровно `(h - 1) * stride + w`.
    assert_eq!(&*mask.pixels, expected.as_slice());
    let placed = PlacedMask::new(Arc::new(mask), (100, 200));
    assert_eq!((placed.left(), placed.top()), (96, 207));
}

#[test]
fn composite_cache_is_keyed_by_bitmap_identity_and_relative_positions() {
    let mut cache = CompositeCache::new(1 << 20);
    let glyph = Arc::new(CoverageBitmap::zeroed(16, 16).expect("bitmap"));
    let filter = FilterKey {
        flags: 0b1_0001,
        gaussian_bits: [0, 0],
        box_passes: 0,
        shadow_offset: (0, 0),
        blur_side_limit: 0,
    };
    let key_at = |position: (i32, i32), bitmap: &Arc<CoverageBitmap>| CompositeKey {
        filter,
        parts: vec![PartKey {
            fill: Some(ArcId::new(bitmap)),
            outline: None,
            fill_position: position,
            outline_position: position,
        }],
    };
    assert!(cache.get(&key_at((0, 0), &glyph)).is_none());
    let entry = CompositeEntry {
        fill: Some(Arc::new(SharedMask::freeze(
            CoverageBitmap::zeroed(16, 16).expect("bitmap"),
        ))),
        unblurred_oversized_layers: 2,
        ..CompositeEntry::default()
    };
    let stored = cache.insert(key_at((0, 0), &glyph), entry);
    let hit = cache.get(&key_at((0, 0), &glyph)).expect("hit");
    assert!(Arc::ptr_eq(&stored, &hit));
    // Счётчики пропущенного живут в записи и видны при каждом попадании.
    assert_eq!(hit.unblurred_oversized_layers, 2);
    // Другая относительная позиция или другой `Arc` маски - другой ключ.
    assert!(cache.get(&key_at((1, 0), &glyph)).is_none());
    assert!(
        cache
            .get(&key_at(
                (0, 0),
                &Arc::new(CoverageBitmap::zeroed(16, 16).expect("bitmap"))
            ))
            .is_none()
    );
    let statistics = cache.statistics();
    assert_eq!(statistics.entries, 1);
    assert!(statistics.current_bytes >= 16 * 16);
}

#[test]
fn cut_to_limits_trims_oldest_entries_of_every_cache_and_clear_empties_them() {
    let mut caches = GlyphCaches::new(GlyphCacheLimits {
        outline_items: 2,
        bitmap_bytes: 1 << 20,
        composite_bytes: 1 << 20,
    });
    for text in [
        "m 0 0 l 1 0 1 1",
        "m 0 0 l 2 0 2 2",
        "m 0 0 l 3 0 3 3",
        "m 0 0 l 4 0 4 4",
    ] {
        let _ = caches.outlines.drawing(text);
    }
    // Внутри кадра вытеснения нет.
    assert_eq!(caches.statistics().outlines.entries, 4);
    caches.cut_to_limits();
    let statistics = caches.statistics().outlines;
    assert_eq!((statistics.entries, statistics.evictions), (2, 2));
    // Остались самые свежие: последний рисунок - попадание, первый - промах.
    let hits = statistics.hits;
    let _ = caches.outlines.drawing("m 0 0 l 4 0 4 4");
    assert_eq!(caches.statistics().outlines.hits, hits + 1);
    let misses = caches.statistics().outlines.misses;
    let _ = caches.outlines.drawing("m 0 0 l 1 0 1 1");
    assert_eq!(caches.statistics().outlines.misses, misses + 1);
    caches.clear();
    let cleared = caches.statistics();
    assert_eq!(
        (
            cleared.outlines.entries,
            cleared.bitmaps.entries,
            cleared.composites.entries
        ),
        (0, 0, 0)
    );
}
