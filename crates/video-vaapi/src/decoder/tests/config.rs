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
        "default pool 24 не должен снова закреплять bound в диапазоне 8..12 без замеров"
    );

    let capacity = SuppressedReclaimCapacity::new(
        DEFAULT_DECODER_SURFACE_POOL_FRAMES,
        DEFAULT_DECODER_READY_QUEUE_FRAMES,
        default_bound,
    );
    assert_eq!(
        capacity.approximate_available_reclaim_slots(0),
        default_bound,
        "диагностика должна показывать стартовую ёмкость reclaim queue"
    );
    assert_eq!(
        capacity.approximate_reserved_surface_headroom_frames(),
        SUPPRESSED_RECLAIM_REFERENCE_HEADROOM_FRAMES
            + SUPPRESSED_RECLAIM_RENDER_HELD_HEADROOM_FRAMES
            + SUPPRESSED_RECLAIM_READY_PUBLISH_HEADROOM_FRAMES
            + SUPPRESSED_RECLAIM_MARGIN_FRAMES,
        "диагностика должна показывать surface headroom вне reclaim queue"
    );
}
