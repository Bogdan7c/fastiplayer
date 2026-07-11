use super::*;

/// Проверяет set/clear states без concrete VA handle-а.
#[test]
fn preroll_output_floor_set_repeat_and_clear_preserve_distinct_results() {
    let floor = preroll_floor_policy(7, Duration::from_millis(1_500), true);
    let mut state = PrerollOutputFloorState::default();

    assert_eq!(
        state.set_floor(floor),
        VideoPrerollOutputFloorResult::Applied
    );
    assert_eq!(
        state.set_floor(floor),
        VideoPrerollOutputFloorResult::Unchanged
    );
    assert_eq!(
        state.clear_floor(VideoPrerollOutputFloorClear::MatchingGeneration(8)),
        VideoPrerollOutputFloorResult::Unchanged
    );
    assert_eq!(
        state.clear_floor(VideoPrerollOutputFloorClear::MatchingGeneration(7)),
        VideoPrerollOutputFloorResult::Cleared
    );
    assert_eq!(
        state.clear_floor(VideoPrerollOutputFloorClear::Any),
        VideoPrerollOutputFloorResult::Unchanged
    );
}

/// Проверяет, что pre-floor кадр подавляется и не считается published target-ом.
#[test]
fn preroll_output_floor_suppresses_pre_floor_frame_without_publish_marker() {
    let mut state = PrerollOutputFloorState::default();
    let floor = preroll_floor_policy(11, Duration::from_millis(1_000), true);
    assert_eq!(
        state.set_floor(floor),
        VideoPrerollOutputFloorResult::Applied
    );

    let suppression_floor = state
        .suppression_floor(Duration::from_millis(900), 11)
        .expect("matching pre-floor frame must be suppressed");
    state.record_suppressed_frame();

    assert_eq!(suppression_floor.generation, 11);
    assert_eq!(suppression_floor.floor_pts, Duration::from_millis(1_000));
    assert_eq!(state.counters.suppressed_frame_count, 1);
    assert!(!state.is_target_or_after_for_active_floor(Duration::from_millis(900), 11));
    assert!(state.should_promote_candidate(11));
}

/// Проверяет normal publish path для target-or-after кадра и одноразовый counter.
#[test]
fn preroll_output_floor_target_or_after_publishes_and_closes_fallback_window() {
    let mut state = PrerollOutputFloorState::default();
    let floor = preroll_floor_policy(12, Duration::from_millis(1_000), true);
    assert_eq!(
        state.set_floor(floor),
        VideoPrerollOutputFloorResult::Applied
    );

    assert!(
        state
            .suppression_floor(Duration::from_millis(1_000), 12)
            .is_none()
    );
    assert!(state.is_target_or_after_for_active_floor(Duration::from_millis(1_000), 12));
    assert!(state.record_target_or_after_published(Duration::from_millis(1_000), 12));
    assert!(!state.record_target_or_after_published(Duration::from_millis(1_033), 12));
    assert!(!state.should_promote_candidate(12));
    assert_eq!(state.counters.target_published_after_floor_count, 1);
}

/// Проверяет, что generation mismatch не подавляет кадр и не трогает active fallback window.
#[test]
fn preroll_output_floor_generation_mismatch_publishes_normally() {
    let mut state = PrerollOutputFloorState::default();
    let floor = preroll_floor_policy(21, Duration::from_millis(2_000), true);
    assert_eq!(
        state.set_floor(floor),
        VideoPrerollOutputFloorResult::Applied
    );

    assert!(
        state
            .suppression_floor(Duration::from_millis(1_000), 20)
            .is_none()
    );
    assert!(!state.is_target_or_after_for_active_floor(Duration::from_millis(3_000), 20));
    assert!(!state.record_target_or_after_published(Duration::from_millis(3_000), 20));
    assert!(state.should_promote_candidate(21));
}

/// Проверяет candidate policy: backend хранит только самый поздний pre-floor кадр.
#[test]
fn preroll_fallback_candidate_replaces_only_with_latest_pts() {
    let mut candidate = Some(PrerollFallbackCandidate::new(
        "older",
        Duration::from_millis(700),
        31,
    ));

    let older_incoming = PrerollFallbackCandidateMetadata {
        pts: Duration::from_millis(650),
        generation: 31,
    };
    assert_eq!(
        preroll_fallback_candidate_decision(candidate.as_ref(), older_incoming),
        PrerollFallbackCandidateDecision::DropIncoming
    );

    let newer_candidate = PrerollFallbackCandidate::new("newer", Duration::from_millis(900), 31);
    assert_eq!(
        preroll_fallback_candidate_decision(candidate.as_ref(), newer_candidate.metadata),
        PrerollFallbackCandidateDecision::ReplaceExisting
    );
    let replaced_candidate = candidate
        .replace(newer_candidate)
        .expect("test starts with an older candidate");
    assert_eq!(replaced_candidate.handle, "older");
    assert_eq!(
        candidate.as_ref().map(|candidate| candidate.handle),
        Some("newer")
    );
}

/// Проверяет ReplaceExisting path: replaced candidate уходит в reclaim queue.
#[test]
fn replaced_fallback_candidate_enqueues_for_reclaim() {
    assert_reclaim_enqueue_for_reason("replace_preroll_fallback_candidate");
}

/// Проверяет clear output-floor cleanup при Cleared.
#[test]
fn clear_preroll_output_floor_force_drain_clears_candidate_and_queue() {
    assert_lifecycle_force_drain_clears_candidate_and_queue("clear_preroll_output_floor");
}

/// Проверяет EOF fallback semantics: promotion разрешена ровно один раз.
#[test]
fn preroll_fallback_candidate_promotes_exactly_once_after_eof_without_target() {
    let mut state = PrerollOutputFloorState::default();
    let floor = preroll_floor_policy(41, Duration::from_millis(5_000), true);
    assert_eq!(
        state.set_floor(floor),
        VideoPrerollOutputFloorResult::Applied
    );

    assert!(state.should_promote_candidate(41));
    if state.should_promote_candidate(41) {
        state.record_candidate_promoted(41);
    }

    assert_eq!(state.counters.candidate_promoted_count, 1);
    assert!(!state.should_promote_candidate(41));
    if state.should_promote_candidate(41) {
        state.record_candidate_promoted(41);
    }
    assert_eq!(state.counters.candidate_promoted_count, 1);
}
