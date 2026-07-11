use super::*;

/// Проверяет pre-submit decision: full reclaim queue должна остановить submit.
#[test]
fn pre_submit_full_reclaim_queue_backpressures_before_packet_submit() {
    let mut suppressed_reclaim_queue = VecDeque::new();
    let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
    let mut frame_pool = reclaim_frame_pool_for_tests();
    let (busy_handle, sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
    suppressed_reclaim_queue.push_back(busy_handle);

    let report = reclaim_ready_suppressed_surfaces_from_queue(
        &mut suppressed_reclaim_queue,
        &mut suppressed_reclaim_counters,
        &mut frame_pool,
        reclaim_capacity_for_tests(1),
    )
    .expect("pre-submit reclaim должен оставить busy handle queued");

    assert!(
        !sync_called.get(),
        "pre-submit full check не должен делать forced sync"
    );
    assert_eq!(report.current_depth, 1);
    assert_eq!(report.max_suppressed_reclaim_frames, 1);
    assert_eq!(report.approximate_available_reclaim_slots, 0);
    assert!(
        is_suppressed_reclaim_queue_full(
            report.current_depth,
            report.max_suppressed_reclaim_frames,
        ),
        "full queue после cheap reclaim должна дать OutputBackpressured до adapter submit"
    );
    assert!(
        !is_suppressed_reclaim_queue_full(1, 2),
        "queue с запасом не должна блокировать submit"
    );
    assert!(
        is_suppressed_reclaim_queue_full(1, 0),
        "нулевой bound нормализуется до 1 и не создаёт unbounded queue"
    );
}

/// Проверяет pre-submit reclaim: ready handles освобождают место до submit.
#[test]
fn pre_submit_reclaim_that_frees_ready_handles_allows_packet_submit() {
    let mut suppressed_reclaim_queue = VecDeque::new();
    let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
    let mut frame_pool = reclaim_frame_pool_for_tests();
    let (ready_handle, sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(true));
    suppressed_reclaim_queue.push_back(ready_handle);

    let report = reclaim_ready_suppressed_surfaces_from_queue(
        &mut suppressed_reclaim_queue,
        &mut suppressed_reclaim_counters,
        &mut frame_pool,
        reclaim_capacity_for_tests(1),
    )
    .expect("pre-submit reclaim должен освободить ready handle");

    assert_eq!(report.current_depth, 0);
    assert_eq!(report.max_suppressed_reclaim_frames, 1);
    assert_eq!(report.approximate_available_reclaim_slots, 1);
    assert!(
        !is_suppressed_reclaim_queue_full(
            report.current_depth,
            report.max_suppressed_reclaim_frames,
        ),
        "после reclaim submit снова разрешён"
    );
    assert!(
        !sync_called.get(),
        "pre-submit reclaim остаётся non-blocking"
    );
}

/// Проверяет suppressed drop: handle ставится в queue и не освобождается сразу.
#[test]
fn suppressed_drop_frame_enqueues_for_reclaim_without_immediate_release() {
    assert_reclaim_enqueue_for_reason("suppress_ready_frame");
}

/// Проверяет обычный reclaim pass: ready surface возвращается в frame pool.
#[test]
fn ready_suppressed_handle_reclaimed_by_nonblocking_pass() {
    let mut suppressed_reclaim_queue = VecDeque::new();
    let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
    let mut frame_pool = reclaim_frame_pool_for_tests();
    let initial_free_frames = frame_pool.num_free();
    let (handle, sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(true));
    suppressed_reclaim_queue.push_back(handle);

    let report = reclaim_ready_suppressed_surfaces_from_queue(
        &mut suppressed_reclaim_queue,
        &mut suppressed_reclaim_counters,
        &mut frame_pool,
        reclaim_capacity_for_tests(4),
    )
    .expect("ready reclaim pass должен пройти");

    assert_eq!(suppressed_reclaim_queue.len(), 0);
    assert_eq!(
        frame_pool.num_free(),
        initial_free_frames + 1,
        "ready handle должен вернуться в frame pool"
    );
    assert!(
        !sync_called.get(),
        "non-blocking reclaim pass не должен вызывать sync"
    );
    assert_eq!(report.current_depth, 0);
    assert_eq!(report.reclaimed_this_pass, 1);
    assert_eq!(suppressed_reclaim_counters.total_reclaimed, 1);
}

/// Проверяет обычный reclaim pass: busy surface остаётся в queue.
#[test]
fn not_ready_suppressed_handle_stays_queued() {
    let mut suppressed_reclaim_queue = VecDeque::new();
    let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
    let mut frame_pool = reclaim_frame_pool_for_tests();
    let initial_free_frames = frame_pool.num_free();
    let (handle, sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
    suppressed_reclaim_queue.push_back(handle);

    let report = reclaim_ready_suppressed_surfaces_from_queue(
        &mut suppressed_reclaim_queue,
        &mut suppressed_reclaim_counters,
        &mut frame_pool,
        reclaim_capacity_for_tests(4),
    )
    .expect("busy reclaim pass должен пройти без blocking sync");

    assert_eq!(suppressed_reclaim_queue.len(), 1);
    assert_eq!(frame_pool.num_free(), initial_free_frames);
    assert!(
        !sync_called.get(),
        "busy reclaim pass не должен вызывать sync"
    );
    assert_eq!(report.current_depth, 1);
    assert_eq!(report.reclaimed_this_pass, 0);
    assert_eq!(suppressed_reclaim_counters.total_reclaimed, 0);
}

/// Проверяет query error: handle не теряется и не считается ready.
#[test]
fn query_error_keeps_suppressed_handle_queued() {
    let mut suppressed_reclaim_queue = VecDeque::new();
    let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
    let mut frame_pool = reclaim_frame_pool_for_tests();
    let initial_free_frames = frame_pool.num_free();
    let (handle, sync_called) =
        fake_decoded_frame_handle(FakeSurfaceReadiness::QueryError("query failed"));
    suppressed_reclaim_queue.push_back(handle);

    let report = reclaim_ready_suppressed_surfaces_from_queue(
        &mut suppressed_reclaim_queue,
        &mut suppressed_reclaim_counters,
        &mut frame_pool,
        reclaim_capacity_for_tests(4),
    )
    .expect("query error должен остаться счетчиком, а не Result error");

    assert_eq!(suppressed_reclaim_queue.len(), 1);
    assert_eq!(frame_pool.num_free(), initial_free_frames);
    assert!(
        !sync_called.get(),
        "query error path не должен делать blocking sync"
    );
    assert_eq!(report.current_depth, 1);
    assert_eq!(report.query_errors_this_pass, 1);
    assert_eq!(suppressed_reclaim_counters.query_errors, 1);
    assert_eq!(suppressed_reclaim_counters.total_reclaimed, 0);
}

/// Проверяет DropIncoming path: older incoming candidate уходит в reclaim queue.
#[test]
fn dropped_incoming_older_candidate_enqueues_for_reclaim() {
    assert_reclaim_enqueue_for_reason("drop_incoming_preroll_fallback_candidate");
}

/// Проверяет retained-candidate drop path: candidate уходит в reclaim queue.
#[test]
fn dropping_retained_candidate_enqueues_for_reclaim() {
    assert_reclaim_enqueue_for_reason("target_or_after_ready_frame");
}

/// Проверяет, что bounded reclaim queue не растёт выше configured bound.
#[test]
fn suppressed_reclaim_queue_respects_bound() {
    let mut suppressed_reclaim_queue = VecDeque::new();
    let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
    let mut frame_pool = reclaim_frame_pool_for_tests();

    for generation in 1..=3 {
        let (handle, _sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
        enqueue_suppressed_frame_for_reclaim_in_queue(
            &mut suppressed_reclaim_queue,
            &mut suppressed_reclaim_counters,
            &mut frame_pool,
            reclaim_capacity_for_tests(2),
            handle,
            "bound_test",
            PrerollFallbackCandidateMetadata {
                pts: Duration::from_millis(generation),
                generation,
            },
        )
        .expect("bounded enqueue должен пройти");
    }

    assert_eq!(suppressed_reclaim_queue.len(), 2);
    assert_eq!(suppressed_reclaim_counters.total_enqueued, 3);
    assert_eq!(suppressed_reclaim_counters.total_reclaimed, 1);
    assert_eq!(suppressed_reclaim_counters.ring_full_count, 1);
    assert_eq!(suppressed_reclaim_counters.forced_sync_count, 1);
}

/// Проверяет classification: forced sync failure должен идти в fatal decoder path.
#[test]
fn surface_lifecycle_sync_error_is_fatal_decoder_error() {
    let error = anyhow::Error::new(VaapiSurfaceLifecycleError::new("synthetic sync failure"));

    assert!(
        is_fatal_decoder_error(&error),
        "surface lifecycle errors нельзя продолжать как non-fatal decode warning"
    );
}

/// Проверяет flush force-drain: lifecycle boundary не оставляет queued handles.
#[test]
fn flush_force_drain_leaves_suppressed_reclaim_queue_empty() {
    let mut suppressed_reclaim_queue = VecDeque::new();
    let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
    let mut frame_pool = reclaim_frame_pool_for_tests();
    let (first_handle, first_sync_called) =
        fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
    let (second_handle, second_sync_called) =
        fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
    suppressed_reclaim_queue.push_back(first_handle);
    suppressed_reclaim_queue.push_back(second_handle);

    force_drain_suppressed_surfaces_from_queue(
        &mut suppressed_reclaim_queue,
        &mut suppressed_reclaim_counters,
        &mut frame_pool,
        "flush",
        reclaim_capacity_for_tests(4),
    )
    .expect("flush force-drain должен очистить queue");

    assert!(suppressed_reclaim_queue.is_empty());
    assert!(first_sync_called.get());
    assert!(second_sync_called.get());
    assert_eq!(suppressed_reclaim_counters.forced_sync_count, 2);
    assert_eq!(suppressed_reclaim_counters.total_reclaimed, 2);
}

/// Проверяет configure_stream cleanup перед adapter replacement.
#[test]
fn configure_stream_force_drain_leaves_no_stale_suppressed_handles() {
    assert_lifecycle_force_drain_clears_candidate_and_queue("configure_stream");
}

/// Проверяет FormatChanged cleanup перед resize/invalidate.
#[test]
fn format_change_force_drain_leaves_no_stale_suppressed_handles() {
    assert_lifecycle_force_drain_clears_candidate_and_queue("format_changed");
}
