use crate::{
    SmoothChunkDuration, SmoothChunkEntry, SmoothChunkRepeat, SmoothChunkStart,
    SmoothDeclaredCountKind, SmoothDeclaredFragmentCount, SmoothManifestError,
    SmoothManifestLimitKind, SmoothManifestTimelineBudget, SmoothManifestVersion,
    SmoothTimelineError, SmoothTimescale,
};

use crate::tests_support::{limits, limits_builder};

#[test]
fn missing_first_start_is_zero_and_later_missing_start_uses_previous_end() {
    let entries = [
        entry(
            SmoothChunkStart::Inferred,
            SmoothChunkDuration::Explicit(10),
            SmoothChunkRepeat::ImplicitSingle,
        ),
        entry(
            SmoothChunkStart::Inferred,
            SmoothChunkDuration::Explicit(15),
            SmoothChunkRepeat::ImplicitSingle,
        ),
    ];
    let timeline = build(&entries, SmoothManifestVersion::V2_0).expect("timeline валиден");

    assert_eq!(timeline.first_start().ticks(), 0);
    assert_eq!(
        timeline.fragment_at(1).expect("fragment 1").start().ticks(),
        10
    );
    assert_eq!(timeline.last_end().ticks(), 25);
}

#[test]
fn missing_duration_is_inferred_only_from_adjacent_explicit_start() {
    let entries = [
        entry(
            SmoothChunkStart::Explicit(100),
            SmoothChunkDuration::InferFromNextExplicitStart,
            SmoothChunkRepeat::Declared(3),
        ),
        entry(
            SmoothChunkStart::Explicit(400),
            SmoothChunkDuration::Explicit(50),
            SmoothChunkRepeat::ImplicitSingle,
        ),
    ];
    let timeline = build(&entries, SmoothManifestVersion::V2_2).expect("timeline валиден");

    assert_eq!(timeline.fragment_count(), 4);
    assert_eq!(
        timeline
            .fragment_at(0)
            .expect("fragment 0")
            .duration_ticks(),
        100
    );
    assert_eq!(
        timeline.fragment_at(2).expect("fragment 2").start().ticks(),
        300
    );
}

#[test]
fn unresolved_or_nondivisible_inferred_duration_is_typed() {
    let final_unresolved = [entry(
        SmoothChunkStart::Inferred,
        SmoothChunkDuration::InferFromNextExplicitStart,
        SmoothChunkRepeat::ImplicitSingle,
    )];
    assert_timeline_error(
        build(&final_unresolved, SmoothManifestVersion::V2_0),
        SmoothTimelineError::MissingAdjacentExplicitStart,
    );

    let adjacent_inferred = [
        entry(
            SmoothChunkStart::Inferred,
            SmoothChunkDuration::InferFromNextExplicitStart,
            SmoothChunkRepeat::ImplicitSingle,
        ),
        entry(
            SmoothChunkStart::Inferred,
            SmoothChunkDuration::Explicit(10),
            SmoothChunkRepeat::ImplicitSingle,
        ),
    ];
    assert_timeline_error(
        build(&adjacent_inferred, SmoothManifestVersion::V2_0),
        SmoothTimelineError::MissingAdjacentExplicitStart,
    );

    let nondivisible = [
        entry(
            SmoothChunkStart::Explicit(0),
            SmoothChunkDuration::InferFromNextExplicitStart,
            SmoothChunkRepeat::Declared(3),
        ),
        entry(
            SmoothChunkStart::Explicit(10),
            SmoothChunkDuration::Explicit(1),
            SmoothChunkRepeat::ImplicitSingle,
        ),
    ];
    assert_timeline_error(
        build(&nondivisible, SmoothManifestVersion::V2_2),
        SmoothTimelineError::NonDivisibleInferredDuration,
    );
}

#[test]
fn repeat_is_one_based_total_count_and_only_version_22_accepts_it() {
    let repeated = [entry(
        SmoothChunkStart::Inferred,
        SmoothChunkDuration::Explicit(10),
        SmoothChunkRepeat::Declared(3),
    )];
    let timeline = build(&repeated, SmoothManifestVersion::V2_2).expect("r=3 валиден");
    assert_eq!(timeline.fragment_count(), 3);
    assert_eq!(timeline.last_end().ticks(), 30);

    assert_timeline_error(
        build(&repeated, SmoothManifestVersion::V2_0),
        SmoothTimelineError::RepeatRequiresVersion22,
    );
    assert_timeline_error(
        build(
            &[entry(
                SmoothChunkStart::Inferred,
                SmoothChunkDuration::Explicit(10),
                SmoothChunkRepeat::Declared(0),
            )],
            SmoothManifestVersion::V2_2,
        ),
        SmoothTimelineError::ZeroRepeat,
    );
}

#[test]
fn zero_duration_backward_overlap_and_discontinuity_are_distinct() {
    assert_timeline_error(
        build(
            &[entry(
                SmoothChunkStart::Inferred,
                SmoothChunkDuration::Explicit(0),
                SmoothChunkRepeat::ImplicitSingle,
            )],
            SmoothManifestVersion::V2_0,
        ),
        SmoothTimelineError::ZeroDuration,
    );

    let overlap = [explicit_entry(100, 20), explicit_entry(110, 10)];
    assert_timeline_error(
        build(&overlap, SmoothManifestVersion::V2_0),
        SmoothTimelineError::Overlap,
    );

    let backward = [explicit_entry(100, 20), explicit_entry(90, 10)];
    assert_timeline_error(
        build(&backward, SmoothManifestVersion::V2_0),
        SmoothTimelineError::BackwardStart,
    );

    let discontinuity = [explicit_entry(100, 20), explicit_entry(121, 10)];
    assert_timeline_error(
        build(&discontinuity, SmoothManifestVersion::V2_0),
        SmoothTimelineError::Discontinuity,
    );
}

#[test]
fn cancellation_is_polled_inside_timeline_normalization_before_repeat_accounting() {
    let configured_limits = limits();
    let timescale = SmoothTimescale::new(10).expect("timescale валиден");
    let entries = [entry(
        SmoothChunkStart::Inferred,
        SmoothChunkDuration::Explicit(10),
        SmoothChunkRepeat::Declared(u64::MAX),
    )];
    let mut poll_count = 0usize;
    let mut cancel_on_entry = || {
        poll_count += 1;
        poll_count == 2
    };
    let mut budget = SmoothManifestTimelineBudget::new(&configured_limits);
    assert_eq!(
        budget.build_stream_timeline_cancellable(
            SmoothManifestVersion::V2_2,
            timescale,
            &entries,
            SmoothDeclaredFragmentCount::Unspecified,
            &mut cancel_on_entry,
        ),
        Err(SmoothManifestError::Cancelled)
    );
}

#[test]
fn checked_arithmetic_rejects_run_end_overflow() {
    let overflow = [entry(
        SmoothChunkStart::Explicit(u64::MAX),
        SmoothChunkDuration::Explicit(1),
        SmoothChunkRepeat::ImplicitSingle,
    )];
    assert_timeline_error(
        build(&overflow, SmoothManifestVersion::V2_0),
        SmoothTimelineError::ArithmeticOverflow,
    );
}

#[test]
fn declared_count_mismatch_is_not_collapsed_into_timeline_text() {
    let configured_limits = limits();
    let mut budget = SmoothManifestTimelineBudget::new(&configured_limits);
    let error = budget
        .build_stream_timeline(
            SmoothManifestVersion::V2_0,
            SmoothTimescale::new(10_000_000).expect("timescale валиден"),
            &[explicit_entry(0, 10)],
            SmoothDeclaredFragmentCount::Exact(2),
        )
        .expect_err("declared count должен совпасть");

    assert_eq!(
        error,
        SmoothManifestError::DeclaredCountMismatch {
            kind: SmoothDeclaredCountKind::FragmentCount,
            declared: 2,
            actual: 1,
        }
    );
}

#[test]
fn per_stream_and_total_limits_apply_before_budget_commit() {
    let configured_limits = limits_builder()
        .maximum_fragments_per_stream(3)
        .maximum_total_fragments(4)
        .build()
        .expect("test limits валидны");
    let mut budget = SmoothManifestTimelineBudget::new(&configured_limits);
    let three = [entry(
        SmoothChunkStart::Inferred,
        SmoothChunkDuration::Explicit(1),
        SmoothChunkRepeat::Declared(3),
    )];
    budget
        .build_stream_timeline(
            SmoothManifestVersion::V2_2,
            SmoothTimescale::new(1).expect("timescale валиден"),
            &three,
            SmoothDeclaredFragmentCount::Unspecified,
        )
        .expect("первые три fragments проходят");
    let total_error = budget
        .build_stream_timeline(
            SmoothManifestVersion::V2_0,
            SmoothTimescale::new(1).expect("timescale валиден"),
            &[explicit_entry(0, 1), explicit_entry(1, 1)],
            SmoothDeclaredFragmentCount::Unspecified,
        )
        .expect_err("total budget должен сработать");
    assert_eq!(
        total_error,
        SmoothManifestError::LimitExceeded {
            limit: SmoothManifestLimitKind::TotalFragments,
            maximum: 4,
        }
    );
    assert_eq!(budget.accepted_fragments(), 3);
    assert_eq!(budget.accepted_timeline_entries(), 1);

    let per_stream_error = budget
        .build_stream_timeline(
            SmoothManifestVersion::V2_2,
            SmoothTimescale::new(1).expect("timescale валиден"),
            &[entry(
                SmoothChunkStart::Inferred,
                SmoothChunkDuration::Explicit(1),
                SmoothChunkRepeat::Declared(4),
            )],
            SmoothDeclaredFragmentCount::Unspecified,
        )
        .expect_err("per-stream budget должен сработать");
    assert_eq!(
        per_stream_error,
        SmoothManifestError::LimitExceeded {
            limit: SmoothManifestLimitKind::FragmentsPerStream,
            maximum: 3,
        }
    );
    assert_eq!(budget.accepted_fragments(), 3);
}

#[test]
fn raw_timeline_entry_limits_are_per_stream_total_and_transactional() {
    let configured_limits = limits_builder()
        .maximum_timeline_entries_per_stream(1)
        .maximum_total_timeline_entries(1)
        .build()
        .expect("test limits валидны");
    let mut budget = SmoothManifestTimelineBudget::new(&configured_limits);
    let per_stream_error = budget
        .build_stream_timeline(
            SmoothManifestVersion::V2_0,
            SmoothTimescale::new(1).expect("timescale валиден"),
            &[explicit_entry(0, 1), explicit_entry(1, 1)],
            SmoothDeclaredFragmentCount::Unspecified,
        )
        .expect_err("raw per-stream entry budget должен сработать");
    assert_eq!(
        per_stream_error,
        SmoothManifestError::LimitExceeded {
            limit: SmoothManifestLimitKind::TimelineEntriesPerStream,
            maximum: 1,
        }
    );
    assert_eq!(budget.accepted_timeline_entries(), 0);

    budget
        .build_stream_timeline(
            SmoothManifestVersion::V2_0,
            SmoothTimescale::new(1).expect("timescale валиден"),
            &[explicit_entry(0, 1)],
            SmoothDeclaredFragmentCount::Unspecified,
        )
        .expect("первая timeline проходит");
    let total_error = budget
        .build_stream_timeline(
            SmoothManifestVersion::V2_0,
            SmoothTimescale::new(1).expect("timescale валиден"),
            &[explicit_entry(0, 1)],
            SmoothDeclaredFragmentCount::Unspecified,
        )
        .expect_err("raw total entry budget должен сработать");
    assert_eq!(
        total_error,
        SmoothManifestError::LimitExceeded {
            limit: SmoothManifestLimitKind::TotalTimelineEntries,
            maximum: 1,
        }
    );
    assert_eq!(budget.accepted_timeline_entries(), 1);
}

#[test]
fn large_allowed_repeat_stays_one_run_and_supports_random_access() {
    let entries = [entry(
        SmoothChunkStart::Inferred,
        SmoothChunkDuration::Explicit(2),
        SmoothChunkRepeat::Declared(1_000_000),
    )];
    let timeline = build(&entries, SmoothManifestVersion::V2_2).expect("large repeat валиден");

    assert_eq!(timeline.run_count(), 1);
    assert_eq!(timeline.fragment_count(), 1_000_000);
    let last = timeline.fragment_at(999_999).expect("последний fragment");
    assert_eq!(last.start().ticks(), 1_999_998);
    assert_eq!(timeline.last_end().ticks(), 2_000_000);
}

#[test]
fn iterator_and_random_access_preserve_exact_fragment_sequence() {
    let entries = [
        explicit_entry(5, 5),
        entry(
            SmoothChunkStart::Inferred,
            SmoothChunkDuration::Explicit(10),
            SmoothChunkRepeat::Declared(2),
        ),
    ];
    let timeline = build(&entries, SmoothManifestVersion::V2_2).expect("timeline валиден");
    let starts = timeline
        .iter_fragments()
        .map(|fragment| fragment.start().ticks())
        .collect::<Vec<_>>();

    assert_eq!(starts, [5, 10, 20]);
    assert_timeline_error(
        timeline.fragment_at(3),
        SmoothTimelineError::FragmentIndexOutOfRange,
    );
}

fn build(
    entries: &[SmoothChunkEntry],
    version: SmoothManifestVersion,
) -> Result<crate::SmoothChunkTimeline, SmoothManifestError> {
    let configured_limits = limits();
    let mut budget = SmoothManifestTimelineBudget::new(&configured_limits);
    budget.build_stream_timeline(
        version,
        SmoothTimescale::new(10_000_000).expect("timescale валиден"),
        entries,
        SmoothDeclaredFragmentCount::Unspecified,
    )
}

fn entry(
    start: SmoothChunkStart,
    duration: SmoothChunkDuration,
    repeat: SmoothChunkRepeat,
) -> SmoothChunkEntry {
    SmoothChunkEntry::new(start, duration, repeat)
}

fn explicit_entry(start_ticks: u64, duration_ticks: u64) -> SmoothChunkEntry {
    entry(
        SmoothChunkStart::Explicit(start_ticks),
        SmoothChunkDuration::Explicit(duration_ticks),
        SmoothChunkRepeat::ImplicitSingle,
    )
}

fn assert_timeline_error<T: std::fmt::Debug>(
    result: Result<T, SmoothManifestError>,
    expected: SmoothTimelineError,
) {
    assert_eq!(
        result.expect_err("timeline fixture должна завершиться ошибкой"),
        SmoothManifestError::InvalidTimeline { reason: expected }
    );
}
