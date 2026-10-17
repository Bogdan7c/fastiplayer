//! Тесты описания фильтров участка: размытие, флаги и смещение тени из вида символа.

use super::*;
use crate::event_style::AxisPair;
use crate::source_text::SourceText;
use crate::span_look::SpanBlur;
use crate::test_support::{plain_style, plain_text};

/// Нейтральный вид с обводкой и тенью, чтобы тень участвовала в фильтрах.
fn look_with(blur: SpanBlur, shadow: f64) -> SpanLook {
    let source = SourceText::from_styled(&plain_text("a"), &plain_style());
    let mut look = source.looks[0].clone();
    look.border = AxisPair::uniform(2.0);
    look.shadow = AxisPair::uniform(shadow);
    look.blur = blur;
    look
}

fn blur(radius: f64, scale: f64, edge_passes: i32) -> SpanBlur {
    SpanBlur {
        radius,
        scale: AxisPair::uniform(scale),
        edge_passes,
    }
}

#[test]
fn no_blur_means_no_variance_and_the_fine_shadow_mask() {
    let filter = RunFilter::for_look(&look_with(SpanBlur::NONE, 3.3), EventFade::NONE);
    assert_eq!(filter.blur.gaussian.x, 0.0);
    assert_eq!(filter.blur.gaussian.y, 0.0);
    assert_eq!(filter.blur.box_passes.count(), 0);
    // 3.3 px = 211.2 в 26.6; маска 7 (шаг 1/8 px): (211 + 3) & !7 = 208.
    assert_eq!(filter.shadow_offset, (208, 208));
}

#[test]
fn blur_radius_is_scaled_by_the_frame_and_restored_from_the_quantized_level() {
    let plain = RunFilter::for_look(&look_with(blur(10.0, 1.0, 0), 0.0), EventFade::NONE);
    let doubled_frame = RunFilter::for_look(&look_with(blur(10.0, 2.0, 0), 0.0), EventFade::NONE);
    // Радиус в пикселях = blur * scale * 2/sqrt(ln 256); дисперсия ~ квадрат радиуса.
    let radius = 10.0 * blur_radius_scale();
    assert!((plain.blur.gaussian.x - radius * radius).abs() / (radius * radius) < 0.03);
    assert!(
        (doubled_frame.blur.gaussian.x / plain.blur.gaussian.x - 4.0).abs() < 0.2,
        "вдвое больший кадр — вчетверо большая дисперсия"
    );
    assert_eq!(plain.blur.gaussian.x, plain.blur.gaussian.y);
}

#[test]
fn shadow_offset_gets_coarser_as_the_blur_grows() {
    let sharp = RunFilter::for_look(&look_with(SpanBlur::NONE, 3.3), EventFade::NONE);
    let soft = RunFilter::for_look(&look_with(blur(100.0, 3.0, 0), 3.3), EventFade::NONE);
    // Сильное размытие допускает грубое смещение: младшие биты сброшены маской.
    assert_eq!(sharp.shadow_offset.0 % 8, 0);
    assert_ne!(soft.shadow_offset, sharp.shadow_offset);
    assert_eq!(soft.shadow_offset.0 % 64, 0, "{:?}", soft.shadow_offset);
}

#[test]
fn edge_blur_passes_are_clamped_and_set_the_padding() {
    let filter = RunFilter::for_look(&look_with(blur(0.0, 1.0, 5), 0.0), EventFade::NONE);
    assert_eq!(filter.blur.box_passes.count(), 5);
    assert_eq!(filter.blur.box_passes.padding_px(), 4);
    let huge = RunFilter::for_look(&look_with(blur(0.0, 1.0, 10_000), 0.0), EventFade::NONE);
    assert_eq!(huge.blur.box_passes.count(), 127);
}

#[test]
fn no_shadow_means_zero_offset_even_with_blur() {
    let filter = RunFilter::for_look(&look_with(blur(5.0, 1.0, 0), 0.0), EventFade::NONE);
    assert_eq!(filter.shadow_offset, (0, 0));
}
