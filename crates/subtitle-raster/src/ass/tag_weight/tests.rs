use subtitle_core::{AssRgb, AssTransformTiming};

use super::*;

fn range_accel(t1_ms: i32, t2_ms: i32, accel: f64) -> AssTransformTiming {
    AssTransformTiming::RangeAccel {
        t1_ms,
        t2_ms,
        accel,
    }
}

fn weight(timing: &AssTransformTiming, elapsed_ms: i32, duration_ms: i32) -> f64 {
    transform_weight_at(timing, elapsed_ms, duration_ms).value()
}

#[test]
fn accel_two_squares_the_progress_and_accel_half_takes_the_root() {
    // Формула libass: k = pow((t - t1) / (t2 - t1), accel). Числа посчитаны вручную.
    let slow = range_accel(0, 1000, 2.0);
    assert_eq!(weight(&slow, 500, 5000), 0.25);
    assert_eq!(weight(&slow, 250, 5000), 0.0625);
    assert_eq!(weight(&slow, 750, 5000), 0.5625);
    let fast = range_accel(0, 1000, 0.5);
    assert_eq!(weight(&fast, 250, 5000), 0.5);
    assert_eq!(weight(&fast, 640, 5000), 0.8);
}

#[test]
fn weight_is_zero_before_t1_and_one_from_t2() {
    let timing = range_accel(200, 600, 1.0);
    assert_eq!(weight(&timing, -50, 5000), 0.0);
    assert_eq!(weight(&timing, 199, 5000), 0.0);
    // Ровно на t1 прогресс 0: pow(0, 1) = 0.
    assert_eq!(weight(&timing, 200, 5000), 0.0);
    assert_eq!(weight(&timing, 400, 5000), 0.5);
    assert_eq!(weight(&timing, 599, 5000), 399.0 / 400.0);
    // На t2 и позже — полный вес (проверка `t >= t2`).
    assert_eq!(weight(&timing, 600, 5000), 1.0);
    assert_eq!(weight(&timing, 9999, 5000), 1.0);
}

#[test]
fn zero_accel_jumps_to_full_weight_at_t1() {
    // pow(0, 0) = 1 в C: при accel = 0 вес на самом t1 уже полный, до t1 — нулевой.
    let timing = range_accel(300, 900, 0.0);
    assert_eq!(weight(&timing, 299, 5000), 0.0);
    assert_eq!(weight(&timing, 300, 5000), 1.0);
}

#[test]
fn zero_t2_means_the_event_duration() {
    // `\t(500,0,...)`: t2 == 0 заменяется длительностью реплики 2500 мс -> отрезок 500..2500.
    let range = AssTransformTiming::Range {
        t1_ms: 500,
        t2_ms: 0,
    };
    assert_eq!(weight(&range, 1500, 2500), 0.5);
    let squared = range_accel(500, 0, 2.0);
    assert_eq!(weight(&squared, 1500, 2500), 0.25);
    // Формы без времён: весь отрезок 0..длительность.
    assert_eq!(weight(&AssTransformTiming::Default, 1000, 4000), 0.25);
    assert_eq!(weight(&AssTransformTiming::Accel(2.0), 1000, 4000), 0.0625);
}

#[test]
fn negative_t2_is_not_replaced_and_gives_full_weight() {
    // Заменяется только ровно 0: при t2 < 0 и t >= t1 > t2 вес полный (t >= t2).
    let timing = AssTransformTiming::Range {
        t1_ms: 100,
        t2_ms: -5,
    };
    assert_eq!(weight(&timing, 50, 4000), 0.0);
    assert_eq!(weight(&timing, 100, 4000), 1.0);
}

#[test]
fn uint32_wraparound_matches_libass_for_extreme_times() {
    // t1 = -2e9, t2 = 2e9, t = 0. В C: delta = (int32)((uint32)t2 - (uint32)t1) = 4e9 - 2^32
    // = -294_967_296, числитель (int32)((uint32)0 - (uint32)t1) = 2_000_000_000, то есть
    // k = 2e9 / -294_967_296 < 0 (отрицательный вес — не ошибка, а арифметика libass).
    let timing = range_accel(-2_000_000_000, 2_000_000_000, 1.0);
    let expected = 2_000_000_000.0 / -294_967_296.0;
    assert_eq!(weight(&timing, 0, 5000), expected);
    assert!(expected < -6.7 && expected > -6.8);
    // Дробное ускорение от отрицательного основания в C даёт NaN, и здесь тоже.
    let fractional = range_accel(-2_000_000_000, 2_000_000_000, 0.5);
    assert!(weight(&fractional, 0, 5000).is_nan());
}

#[test]
fn blend_is_old_times_one_minus_weight_plus_new_times_weight() {
    let quarter = TagWeight::new(0.25);
    assert_eq!(quarter.blend(40.0, 80.0), 50.0);
    assert_eq!(TagWeight::FULL.blend(40.0, 80.0), 80.0);
    assert_eq!(TagWeight::NONE.blend(40.0, 80.0), 40.0);
    // Вес вне [0, 1] допустим: экстраполяция.
    assert_eq!(TagWeight::new(2.0).blend(100.0, 200.0), 300.0);
}

#[test]
fn relative_font_size_scales_the_old_size() {
    // `\fs+5` при весе 0.5: 40 * (1 + 0.5 * 5 / 10) = 50; `\fs-10` при весе 0.25: 40 * 0.75.
    assert_eq!(TagWeight::new(0.5).scale_relative(40.0, 5.0), 50.0);
    assert_eq!(TagWeight::new(0.25).scale_relative(40.0, -10.0), 30.0);
    assert_eq!(TagWeight::FULL.scale_relative(40.0, 5.0), 60.0);
}

#[test]
fn rect_coordinates_truncate_toward_zero() {
    assert_eq!(TagWeight::new(0.75).blend_truncated(100, 0), 25);
    // 0 * 0.75 + (-10) * 0.25 = -2.5 -> -2, а не -3.
    assert_eq!(TagWeight::new(0.25).blend_truncated(0, -10), -2);
    assert_eq!(TagWeight::new(0.5).blend_truncated(1, 2), 1);
}

#[test]
fn colors_blend_per_channel_with_truncation() {
    let old = AssRgb::new(255, 0, 100);
    let new = AssRgb::new(0, 255, 200);
    // R: 127.5 -> 127, G: 127.5 -> 127, B: 150.
    assert_eq!(
        TagWeight::new(0.5).blend_rgb(old, new),
        AssRgb::new(127, 127, 150)
    );
    // R: 0.75 * 255 = 191.25 -> 191; G: 0.25 * 255 = 63.75 -> 63; B: 0.75 * 100 + 0.25 * 200 = 125.
    assert_eq!(
        TagWeight::new(0.25).blend_rgb(old, new),
        AssRgb::new(191, 63, 125)
    );
    assert_eq!(TagWeight::NONE.blend_rgb(old, new), old);
    assert_eq!(TagWeight::FULL.blend_rgb(old, new), new);
}

#[test]
fn color_channels_wrap_like_the_c_mask_when_weight_leaves_the_range() {
    // Вес 2: 100 -> 200 даёт 300; маска канала оставляет младший байт: 300 & 0xff = 44.
    let old = AssRgb::new(100, 100, 100);
    let new = AssRgb::new(200, 200, 200);
    assert_eq!(
        TagWeight::new(2.0).blend_rgb(old, new),
        AssRgb::new(44, 44, 44)
    );
}

#[test]
fn alpha_blend_keeps_the_low_byte_of_the_truncated_mix() {
    assert_eq!(TagWeight::new(0.5).blend_alpha(0x00, 0xFF), 127);
    assert_eq!(TagWeight::new(0.25).blend_alpha(0xFF, 0x00), 191);
    assert_eq!(TagWeight::NONE.blend_alpha(0x12, 0xFF), 0x12);
    assert_eq!(TagWeight::FULL.blend_alpha(0x12, 0xFF), 0xFF);
    // Вес 2: 100 -> 200 = 300 -> 44 (приведение к uint8_t).
    assert_eq!(TagWeight::new(2.0).blend_alpha(100, 200), 44);
}

#[test]
fn edge_blur_rounds_after_blending_and_dtoi32_maps_overflow_to_min() {
    // be = 2 -> 5 при весе 0.5: 3.5 + 0.5 = 4.0 -> 4; при весе 0.25: 2.75 + 0.5 = 3.25 -> 3.
    assert_eq!(TagWeight::new(0.5).blend_edge_blur_rounded(2, 5.0), 4);
    assert_eq!(TagWeight::new(0.25).blend_edge_blur_rounded(2, 5.0), 3);
    assert_eq!(
        TagWeight::FULL.blend_edge_blur_rounded(7, f64::INFINITY),
        i32::MIN
    );
    assert_eq!(dtoi32(f64::NAN), i32::MIN);
    assert_eq!(dtoi32(-2.9), -2);
}
