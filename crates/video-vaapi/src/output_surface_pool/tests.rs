//! Unit-тесты пула на fake surfaces: без VA display и железа.

use std::collections::VecDeque;

use super::test_support::{
    FakeSurfaceIdleBehavior, PoolFixture, format_4k_p010, format_1080p, frames, lease,
    pool_with_capacity,
};
use super::*;

#[test]
fn leases_up_to_capacity_then_reports_exhausted_without_allocating() {
    let fixture = pool_with_capacity(3);
    let held: Vec<_> = (0..3).map(|_| lease(&fixture.pool)).collect();

    let outcome = fixture.pool.try_lease();

    assert!(matches!(outcome, OutputSurfaceLeaseOutcome::Exhausted));
    let stats = fixture.pool.stats();
    assert_eq!(stats.surfaces_created, 3, "no surface beyond capacity");
    assert_eq!(stats.live_surfaces, 3);
    assert_eq!(stats.exhausted_lease_attempts, 1);
    assert_eq!(held.len(), 3);
}

#[test]
fn returned_surface_is_leased_again_instead_of_creating_new_one() {
    let fixture = pool_with_capacity(4);
    let first = lease(&fixture.pool);
    let first_id = first.surface().id;
    drop(first);

    let second = lease(&fixture.pool);

    assert_eq!(
        second.surface().id,
        first_id,
        "same surface must be recycled"
    );
    let stats = fixture.pool.stats();
    assert_eq!(stats.surfaces_created, 1);
    assert_eq!(stats.leases_reused, 1);
    assert!(
        fixture.destroyed.borrow().is_empty(),
        "recycling must not destroy the surface"
    );
}

#[test]
fn steady_playback_window_never_creates_surfaces_per_frame() {
    // Модель воспроизведения: в каждый момент держатся 6 кадров (DPB + показ),
    // самый старый отпускается, новый арендуется. 1000 кадров — не 1000 surfaces.
    let fixture = pool_with_capacity(24);
    let mut in_flight = VecDeque::new();
    for _ in 0..1000 {
        in_flight.push_back(lease(&fixture.pool));
        if in_flight.len() > 6 {
            in_flight.pop_front();
        }
    }

    let stats = fixture.pool.stats();
    assert_eq!(stats.surfaces_created, 7, "high-water mark, not per-frame");
    assert_eq!(stats.peak_live_surfaces, 7);
    assert_eq!(stats.leases_reused, 1000 - 7);
    assert!(fixture.destroyed.borrow().is_empty());
}

#[test]
fn driver_allocation_failure_is_reported_separately_from_exhaustion() {
    let fixture = pool_with_capacity(2);
    fixture.fail_allocations.set(true);

    let outcome = fixture.pool.try_lease();

    let OutputSurfaceLeaseOutcome::AllocationFailed(error) = outcome else {
        panic!("allocation failure must not look like an exhausted pool");
    };
    assert_eq!(error.format, format_1080p());
    let stats = fixture.pool.stats();
    assert_eq!(stats.allocation_failures, 1);
    assert_eq!(stats.exhausted_lease_attempts, 0);
    assert_eq!(
        stats.live_surfaces, 0,
        "failed allocation holds no capacity"
    );

    fixture.fail_allocations.set(false);
    assert!(matches!(
        fixture.pool.try_lease(),
        OutputSurfaceLeaseOutcome::Leased(_)
    ));
}

#[test]
fn format_change_destroys_free_surfaces_and_rejects_stale_returns() {
    let fixture = pool_with_capacity(4);
    let returned_before_change = lease(&fixture.pool);
    let held_by_player = lease(&fixture.pool);
    let free_id = returned_before_change.surface().id;
    let held_id = held_by_player.surface().id;
    drop(returned_before_change);

    let report = fixture
        .pool
        .reconfigure(format_4k_p010(), StreamSurfaceMinimum::Unknown);

    assert!(report.format_changed);
    assert_eq!(report.destroyed_free_surfaces, 1);
    assert_eq!(*fixture.destroyed.borrow(), vec![free_id]);

    // Плеер отпускает кадр старого формата: surface уничтожается, а не попадает в пул.
    drop(held_by_player);
    assert_eq!(*fixture.destroyed.borrow(), vec![free_id, held_id]);
    let fresh = lease(&fixture.pool);
    assert_eq!(fresh.surface().format, format_4k_p010());
    assert_eq!(fresh.format(), format_4k_p010());
    assert_eq!(fixture.pool.stats().live_surfaces, 1);
}

#[test]
fn stale_surfaces_still_held_by_player_count_toward_capacity() {
    let fixture = pool_with_capacity(2);
    let old_frames: Vec<_> = (0..2).map(|_| lease(&fixture.pool)).collect();
    fixture
        .pool
        .reconfigure(format_4k_p010(), StreamSurfaceMinimum::Unknown);

    assert!(
        matches!(
            fixture.pool.try_lease(),
            OutputSurfaceLeaseOutcome::Exhausted
        ),
        "live old-format surfaces keep the pool bounded"
    );

    drop(old_frames);
    assert!(matches!(
        fixture.pool.try_lease(),
        OutputSurfaceLeaseOutcome::Leased(_)
    ));
    assert!(fixture.pool.stats().peak_live_surfaces <= 2);
}

#[test]
fn stream_minimum_raises_capacity_only_when_setting_is_smaller() {
    let fixture = pool_with_capacity(4);

    let raised = fixture
        .pool
        .reconfigure(format_1080p(), StreamSurfaceMinimum::Frames(frames(20)));
    assert_eq!(raised.capacity.get(), 20);
    assert!(raised.raised_to_stream_minimum);
    assert_eq!(fixture.pool.stats().capacity, 20);

    let kept = fixture
        .pool
        .reconfigure(format_1080p(), StreamSurfaceMinimum::Frames(frames(3)));
    assert_eq!(
        kept.capacity.get(),
        4,
        "user setting is the ceiling above the minimum"
    );
    assert!(!kept.raised_to_stream_minimum);

    let unknown = fixture
        .pool
        .reconfigure(format_1080p(), StreamSurfaceMinimum::Unknown);
    assert_eq!(unknown.capacity.get(), 4);
}

#[test]
fn shrinking_capacity_destroys_surplus_surfaces_instead_of_pooling_them() {
    let fixture = pool_with_capacity(2);
    fixture
        .pool
        .reconfigure(format_1080p(), StreamSurfaceMinimum::Frames(frames(4)));
    let held: Vec<_> = (0..4).map(|_| lease(&fixture.pool)).collect();
    let mut held = held.into_iter();
    // Две surfaces вернулись до сжатия ёмкости.
    drop(held.next());
    drop(held.next());

    let report = fixture
        .pool
        .reconfigure(format_1080p(), StreamSurfaceMinimum::Unknown);

    assert!(!report.format_changed);
    assert_eq!(report.destroyed_free_surfaces, 2);
    assert_eq!(fixture.pool.stats().live_surfaces, 2);
    // Оставшиеся две уже помещаются в новую ёмкость и возвращаются в пул.
    drop(held.next());
    drop(held.next());
    let stats = fixture.pool.stats();
    assert_eq!(stats.live_surfaces, 2);
    assert_eq!(stats.free_surfaces, 2);
    assert_eq!(fixture.destroyed.borrow().len(), 2);
}

#[test]
fn surface_returned_while_pool_is_over_capacity_is_destroyed() {
    let fixture = pool_with_capacity(1);
    fixture
        .pool
        .reconfigure(format_1080p(), StreamSurfaceMinimum::Frames(frames(3)));
    let held: Vec<_> = (0..3).map(|_| lease(&fixture.pool)).collect();
    fixture
        .pool
        .reconfigure(format_1080p(), StreamSurfaceMinimum::Unknown);

    drop(held);

    let stats = fixture.pool.stats();
    assert_eq!(stats.live_surfaces, 1, "pool shrinks back to its capacity");
    assert_eq!(stats.free_surfaces, 1);
    assert_eq!(fixture.destroyed.borrow().len(), 2);
}

#[test]
fn same_format_reconfigure_keeps_free_surfaces_for_reuse() {
    let fixture = pool_with_capacity(4);
    drop(lease(&fixture.pool));

    let report = fixture
        .pool
        .reconfigure(format_1080p(), StreamSurfaceMinimum::Unknown);

    assert!(!report.format_changed);
    assert_eq!(report.destroyed_free_surfaces, 0);
    assert_eq!(fixture.pool.stats().free_surfaces, 1);
    let reused = lease(&fixture.pool);
    assert_eq!(reused.surface().id, 1);
    assert_eq!(fixture.pool.stats().surfaces_created, 1);
}

#[test]
fn lease_outliving_the_pool_destroys_its_surface() {
    let fixture = pool_with_capacity(2);
    let lease = lease(&fixture.pool);
    let surface_id = lease.surface().id;
    let PoolFixture {
        pool, destroyed, ..
    } = fixture;

    drop(pool);
    assert!(destroyed.borrow().is_empty(), "leased surface still alive");
    drop(lease);

    assert_eq!(*destroyed.borrow(), vec![surface_id]);
}

#[test]
fn dropping_the_pool_destroys_free_surfaces() {
    let fixture = pool_with_capacity(2);
    drop(lease(&fixture.pool));
    let PoolFixture {
        pool, destroyed, ..
    } = fixture;

    drop(pool);

    assert_eq!(*destroyed.borrow(), vec![1]);
}

#[test]
fn detached_surface_leaves_pool_accounting() {
    let fixture = pool_with_capacity(1);
    let detached = lease(&fixture.pool).into_detached_surface();

    assert!(detached.is_some());
    let stats = fixture.pool.stats();
    assert_eq!(stats.live_surfaces, 0);
    assert_eq!(stats.surfaces_detached, 1, "detach is accounted as detach");
    assert_eq!(
        stats.surfaces_destroyed, 0,
        "a detached surface is still alive"
    );
    assert!(
        matches!(
            fixture.pool.try_lease(),
            OutputSurfaceLeaseOutcome::Leased(_)
        ),
        "capacity is free again after detach"
    );
}

#[test]
fn pool_teardown_accounts_free_surfaces_and_leases_that_outlive_it() {
    let fixture = pool_with_capacity(3);
    let held = lease(&fixture.pool);
    drop(lease(&fixture.pool));
    let PoolFixture { pool, counters, .. } = fixture;
    assert_eq!(counters.snapshot().live_surfaces, 2);

    drop(pool);
    let after_pool_drop = counters.snapshot();
    assert_eq!(
        after_pool_drop.live_surfaces, 1,
        "only the leased surface is alive"
    );
    assert_eq!(after_pool_drop.free_surfaces, 0);
    assert_eq!(after_pool_drop.surfaces_destroyed, 1);

    drop(held);
    let after_lease_drop = counters.snapshot();
    assert_eq!(
        after_lease_drop.live_surfaces, 0,
        "a dead decoder holds no surfaces"
    );
    assert_eq!(after_lease_drop.surfaces_destroyed, 2);
}

#[test]
fn busy_surface_is_waited_for_before_it_is_leased_again() {
    let fixture = pool_with_capacity(2);
    let first = lease(&fixture.pool);
    let first_id = first.surface().id;
    fixture
        .idle_behavior
        .set(FakeSurfaceIdleBehavior::BusyThenIdle);

    drop(first);
    let stats = fixture.pool.stats();
    assert_eq!(
        stats.surfaces_synced_on_return, 1,
        "pool waited for the driver"
    );
    assert_eq!(
        stats.free_surfaces, 1,
        "waited surface is pooled, not destroyed"
    );

    fixture.idle_behavior.set(FakeSurfaceIdleBehavior::Idle);
    let reused = lease(&fixture.pool);
    assert_eq!(reused.surface().id, first_id, "the same surface is reused");
    assert_eq!(fixture.pool.stats().surfaces_created, 1);
}

#[test]
fn surface_with_unknown_driver_state_is_destroyed_instead_of_pooled() {
    let fixture = pool_with_capacity(2);
    let first = lease(&fixture.pool);
    let first_id = first.surface().id;
    fixture
        .idle_behavior
        .set(FakeSurfaceIdleBehavior::QueryFails);

    drop(first);
    assert_eq!(*fixture.destroyed.borrow(), vec![first_id]);
    let stats = fixture.pool.stats();
    assert_eq!(
        stats.free_surfaces, 0,
        "an unverifiable surface never reaches the free list"
    );
    assert_eq!(stats.live_surfaces, 0);
    assert_eq!(stats.surfaces_destroyed, 1);
    assert_eq!(stats.surfaces_destroyed_not_idle, 1);
    assert_eq!(stats.surfaces_synced_on_return, 0);

    fixture.idle_behavior.set(FakeSurfaceIdleBehavior::Idle);
    let replacement = lease(&fixture.pool);
    assert_ne!(
        replacement.surface().id,
        first_id,
        "capacity is reused by a fresh surface"
    );
    assert_eq!(fixture.pool.stats().surfaces_created, 2);
}

#[test]
fn stale_format_surface_is_destroyed_without_consulting_the_driver() {
    let fixture = pool_with_capacity(2);
    let old_format = lease(&fixture.pool);
    fixture
        .pool
        .reconfigure(format_4k_p010(), StreamSurfaceMinimum::Unknown);
    fixture
        .idle_behavior
        .set(FakeSurfaceIdleBehavior::QueryFails);

    drop(old_format);
    let stats = fixture.pool.stats();
    assert_eq!(stats.surfaces_destroyed, 1);
    assert_eq!(
        stats.surfaces_destroyed_not_idle, 0,
        "stale-format path does not query"
    );
}

#[test]
fn peak_capacity_keeps_the_largest_capacity_after_a_shrink() {
    let fixture = pool_with_capacity(4);
    let raised = fixture
        .pool
        .reconfigure(format_1080p(), StreamSurfaceMinimum::Frames(frames(20)));
    assert_eq!(raised.capacity.get(), 20);
    let held: Vec<_> = (0..20).map(|_| lease(&fixture.pool)).collect();

    let shrunk = fixture
        .pool
        .reconfigure(format_1080p(), StreamSurfaceMinimum::Frames(frames(8)));
    let stats = fixture.pool.stats();
    assert_eq!(shrunk.capacity.get(), 8);
    assert_eq!(stats.capacity, 8);
    assert_eq!(stats.peak_capacity, 20);
    assert_eq!(stats.peak_live_surfaces, 20);
    assert!(
        stats.peak_live_surfaces <= stats.peak_capacity,
        "live surfaces never exceeded the capacity that was in force when they were created"
    );
    drop(held);
}

#[test]
fn counters_are_readable_through_shared_handle() {
    let fixture = pool_with_capacity(3);
    let _held = lease(&fixture.pool);

    let snapshot = fixture.counters.snapshot();

    assert_eq!(snapshot, fixture.pool.stats());
    assert_eq!(snapshot.capacity, 3);
    assert_eq!(snapshot.surfaces_created, 1);
}
