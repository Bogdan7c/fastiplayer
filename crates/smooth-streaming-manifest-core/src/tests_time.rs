use crate::{SmoothTime, SmoothTimeError, SmoothTimescale};

#[test]
fn cross_timescale_equality_is_exact_for_ten_million_and_forty_eight_thousand() {
    let manifest_clock = SmoothTimescale::new(10_000_000).expect("timescale валиден");
    let audio_clock = SmoothTimescale::new(48_000).expect("timescale валиден");

    assert_eq!(
        SmoothTime::new(5_000_000, manifest_clock),
        SmoothTime::new(24_000, audio_clock)
    );
}

#[test]
fn cross_timescale_order_never_uses_lossy_rescale() {
    let manifest_clock = SmoothTimescale::new(10_000_000).expect("timescale валиден");
    let audio_clock = SmoothTimescale::new(48_000).expect("timescale валиден");
    let earlier = SmoothTime::new(4_999_999, manifest_clock);
    let exact_half_second = SmoothTime::new(24_000, audio_clock);
    let later = SmoothTime::new(5_000_001, manifest_clock);

    assert!(earlier < exact_half_second);
    assert!(later > exact_half_second);
}

#[test]
fn maximum_tick_values_compare_without_overflow() {
    let fastest = SmoothTimescale::new(u64::MAX).expect("timescale валиден");
    let unit = SmoothTimescale::new(1).expect("timescale валиден");

    assert_eq!(SmoothTime::new(u64::MAX, fastest), SmoothTime::new(1, unit));
}

#[test]
fn zero_timescale_is_impossible() {
    assert_eq!(SmoothTimescale::new(0), Err(SmoothTimeError::ZeroTimescale));
}
