use super::*;

/// Проверяет, что default bound выводится из surface accounting, а не из малого magic limit.
#[test]
fn suppressed_reclaim_default_bound_uses_surface_accounting() {
    let default_bound = default_max_suppressed_reclaim_frames(
        DEFAULT_DECODER_SURFACE_POOL_FRAMES,
        DEFAULT_DECODER_READY_QUEUE_FRAMES,
    );

    assert_eq!(
        default_bound,
        DEFAULT_DECODER_SURFACE_POOL_FRAMES
            - SUPPRESSED_RECLAIM_REFERENCE_HEADROOM_FRAMES
            - SUPPRESSED_RECLAIM_RENDER_HELD_HEADROOM_FRAMES
            - SUPPRESSED_RECLAIM_READY_PUBLISH_HEADROOM_FRAMES
            - SUPPRESSED_RECLAIM_MARGIN_FRAMES
    );
    assert!(
        default_bound > 12,
        "default pool 24 must not fix the bound again in the range 8..12 without measurement."
    );

    let capacity = SuppressedReclaimCapacity::new(
        DEFAULT_DECODER_SURFACE_POOL_FRAMES,
        DEFAULT_DECODER_READY_QUEUE_FRAMES,
        default_bound,
    );
    assert_eq!(
        capacity.approximate_available_reclaim_slots(0),
        default_bound,
        "Diagnostics should show the starting capacity reclaim queue"
    );
    assert_eq!(
        capacity.approximate_reserved_surface_headroom_frames(),
        SUPPRESSED_RECLAIM_REFERENCE_HEADROOM_FRAMES
            + SUPPRESSED_RECLAIM_RENDER_HELD_HEADROOM_FRAMES
            + SUPPRESSED_RECLAIM_READY_PUBLISH_HEADROOM_FRAMES
            + SUPPRESSED_RECLAIM_MARGIN_FRAMES,
        "The screen must show the surface headroom outside of reclaim queue."
    );
}

/// Правило «ноль → один» живёт в одном месте и используется всеми выводами из конфига.
#[test]
fn zero_surface_pool_setting_is_normalized_to_one_everywhere() {
    assert_eq!(non_zero_or_one(0).get(), 1);
    assert_eq!(non_zero_or_one(7).get(), 7);

    let from_accounting = VaapiDecoderRuntimeConfig::from_surface_accounting(0, 0);
    assert_eq!(from_accounting.surface_pool_frames, 1);
    assert_eq!(from_accounting.ready_queue_frames, 1);
    assert_eq!(from_accounting.surface_pool_frames_non_zero().get(), 1);

    let normalized = VaapiDecoderRuntimeConfig {
        surface_pool_frames: 0,
        ready_queue_frames: 0,
        max_suppressed_reclaim_frames: 0,
    }
    .normalized();
    assert_eq!(normalized.surface_pool_frames, 1);
    assert_eq!(normalized.ready_queue_frames, 1);
    assert_eq!(normalized.max_suppressed_reclaim_frames, 1);
}

/// Бюджеты от поднятой ёмкости: reclaim bound считается заново от реального числа surfaces.
#[test]
fn effective_surface_pool_frames_rederive_the_reclaim_bound() {
    let configured = VaapiDecoderRuntimeConfig::from_surface_accounting(2, 8).normalized();
    assert_eq!(
        configured.max_suppressed_reclaim_frames, 1,
        "tiny pool → bound 1"
    );

    let raised =
        configured.with_effective_surface_pool_frames(NonZeroUsize::new(25).expect("non-zero"));

    assert_eq!(raised.surface_pool_frames, 25);
    assert_eq!(
        raised.ready_queue_frames, 8,
        "ready queue bound is not touched"
    );
    assert_eq!(
        raised.max_suppressed_reclaim_frames,
        default_max_suppressed_reclaim_frames(25, 8),
        "bound follows the effective capacity"
    );
    assert!(raised.max_suppressed_reclaim_frames > configured.max_suppressed_reclaim_frames);
}
