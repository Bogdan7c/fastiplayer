use std::sync::Arc;

use subtitle_core::SubtitleColor;

use super::*;
use crate::overlay::SubtitleBitmapKind;

fn image(x: i32, y: i32, width: u32, height: u32, value: u8) -> SubtitleBitmap {
    SubtitleBitmap::new(
        PixelPoint { x, y },
        width,
        height,
        width as usize,
        Arc::from(vec![value; (width * height) as usize]),
        SubtitleColor::opaque(255, 255, 255),
        SubtitleBitmapKind::Glyph,
    )
    .expect("image")
}

/// Маска 4x2 в точке (10, 20) с шагом 6: значения 255, 128, 0, 64 в верхней строке.
fn mask(mode: AssClipMode) -> VectorClipMask {
    let mut coverage = vec![0_u8; 6 * 2];
    coverage[..4].copy_from_slice(&[255, 128, 0, 64]);
    coverage[6..10].copy_from_slice(&[10, 20, 30, 40]);
    VectorClipMask::new(mode, (10, 20), (4, 2), 6, Arc::from(coverage))
}

#[test]
fn regular_clip_multiplies_masks_and_crops_to_the_overlap() {
    let source = image(8, 19, 5, 3, 200);
    let VectorClipEffect::Replaced(clipped) = mask(AssClipMode::Inside).apply(&source) else {
        panic!("есть пересечение");
    };
    // Пересечение: x 10..13, y 20..22 -> 3x2.
    assert_eq!(
        (clipped.origin(), clipped.width(), clipped.height()),
        (PixelPoint { x: 10, y: 20 }, 3, 2)
    );
    // (a*b+255)>>8 для a = 200.
    let expected = |mask_value: u32| ((200 * mask_value + 255) >> 8) as u8;
    assert_eq!(
        clipped.coverage_row(0).expect("row"),
        [expected(255), expected(128), expected(0)]
    );
    assert_eq!(
        clipped.coverage_row(1).expect("row"),
        [expected(10), expected(20), expected(30)]
    );
}

#[test]
fn regular_clip_keeps_the_picture_attributes_and_removes_it_without_overlap() {
    let source =
        image(10, 20, 4, 2, 255).with_placement_group(crate::overlay::OverlayPlacementGroup::Fixed);
    let VectorClipEffect::Replaced(clipped) = mask(AssClipMode::Inside).apply(&source) else {
        panic!("есть пересечение");
    };
    assert_eq!(clipped.placement_group(), source.placement_group());
    assert_eq!(clipped.color(), source.color());
    assert_eq!(clipped.kind(), source.kind());
    // Полная маска 255 на значении 255 даёт 255, а на 128 — 128: `(a*255+255)>>8 == a`.
    assert_eq!(clipped.coverage_row(0).expect("row")[0], 255);

    // Рядом, но без общей площади (касание краем — не пересечение).
    for far in [
        image(14, 20, 3, 2, 255),
        image(10, 22, 4, 2, 255),
        image(0, 0, 5, 5, 255),
    ] {
        assert!(matches!(
            mask(AssClipMode::Inside).apply(&far),
            VectorClipEffect::Removed
        ));
    }
}

#[test]
fn inverse_clip_attenuates_only_the_overlap_and_keeps_the_whole_picture() {
    let source = image(8, 19, 5, 3, 200);
    let VectorClipEffect::Replaced(clipped) = mask(AssClipMode::Outside).apply(&source) else {
        panic!("есть пересечение");
    };
    // Размер и положение прежние.
    assert_eq!(
        (clipped.origin(), clipped.width(), clipped.height()),
        (source.origin(), 5, 3)
    );
    // Вне маски значение не тронуто.
    assert_eq!(clipped.coverage_row(0).expect("row"), [200; 5]);
    // На пересечении: (dst*(255-src)+255)>>8.
    let attenuated = |mask_value: u32| ((200 * (255 - mask_value) + 255) >> 8) as u8;
    assert_eq!(
        clipped.coverage_row(1).expect("row"),
        [200, 200, attenuated(255), attenuated(128), attenuated(0)]
    );
    assert_eq!(
        clipped.coverage_row(2).expect("row"),
        [200, 200, attenuated(10), attenuated(20), attenuated(30)]
    );
}

#[test]
fn inverse_clip_without_overlap_leaves_the_picture_unchanged() {
    assert!(matches!(
        mask(AssClipMode::Outside).apply(&image(0, 0, 5, 5, 255)),
        VectorClipEffect::Unchanged
    ));
}

#[test]
fn empty_mask_hides_everything_for_regular_and_nothing_for_inverse() {
    let source = image(0, 0, 8, 8, 255);
    assert!(matches!(
        VectorClipMask::empty(AssClipMode::Inside, (0, 0)).apply(&source),
        VectorClipEffect::Removed
    ));
    assert!(matches!(
        VectorClipMask::empty(AssClipMode::Outside, (0, 0)).apply(&source),
        VectorClipEffect::Unchanged
    ));
}
