//! Каждый mandatory budget и все cancellation checkpoints.

use std::cell::Cell;

use super::super::error::{FragmentInspectionError, FragmentInspectionLimitKind};
use super::super::limits::{
    FragmentInspectionLimitBuildError, FragmentInspectionLimits, FragmentInspectionLimitsBuilder,
};
use super::super::model::FragmentSampleDefaults;
use super::support::{
    VIDEO_HIGH_FIRST, duplicate_traf, duplicate_trun, inspect_with, video_expectation,
};

#[test]
fn limits_builder_requires_every_nonzero_budget() {
    assert_eq!(
        FragmentInspectionLimits::builder().build(),
        Err(FragmentInspectionLimitBuildError::Missing {
            kind: FragmentInspectionLimitKind::InputBytes,
        })
    );
    assert_eq!(
        complete_builder().max_input_bytes(0).build(),
        Err(FragmentInspectionLimitBuildError::Zero {
            kind: FragmentInspectionLimitKind::InputBytes,
        })
    );
}

#[test]
fn input_box_count_and_depth_limits_stop_before_deeper_work() {
    assert_limit(
        complete_builder()
            .max_input_bytes(VIDEO_HIGH_FIRST.len() - 1)
            .build()
            .expect("complete limits"),
        VIDEO_HIGH_FIRST,
        FragmentInspectionLimitKind::InputBytes,
    );
    assert_limit(
        complete_builder()
            .max_box_count(5)
            .build()
            .expect("complete limits"),
        VIDEO_HIGH_FIRST,
        FragmentInspectionLimitKind::BoxCount,
    );
    assert_limit(
        complete_builder()
            .max_box_depth(2)
            .build()
            .expect("complete limits"),
        VIDEO_HIGH_FIRST,
        FragmentInspectionLimitKind::BoxDepth,
    );
}

#[test]
fn traf_trun_and_sample_limits_stop_before_unbounded_loops() {
    let mut two_traf = VIDEO_HIGH_FIRST.to_vec();
    duplicate_traf(&mut two_traf);
    assert_limit(
        complete_builder()
            .max_traf_count(1)
            .build()
            .expect("complete limits"),
        &two_traf,
        FragmentInspectionLimitKind::TrackFragments,
    );

    let mut two_trun = VIDEO_HIGH_FIRST.to_vec();
    duplicate_trun(&mut two_trun);
    assert_limit(
        complete_builder()
            .max_trun_count(1)
            .build()
            .expect("complete limits"),
        &two_trun,
        FragmentInspectionLimitKind::TrackRuns,
    );
    assert_limit(
        complete_builder()
            .max_samples(95)
            .build()
            .expect("complete limits"),
        VIDEO_HIGH_FIRST,
        FragmentInspectionLimitKind::Samples,
    );
}

#[test]
fn sample_table_and_box_payload_limits_guard_allocations() {
    assert_limit(
        complete_builder()
            .max_sample_table_bytes(1)
            .build()
            .expect("complete limits"),
        VIDEO_HIGH_FIRST,
        FragmentInspectionLimitKind::SampleTableBytes,
    );
    assert_limit(
        complete_builder()
            .max_box_payload_bytes(50_000)
            .build()
            .expect("complete limits"),
        VIDEO_HIGH_FIRST,
        FragmentInspectionLimitKind::BoxPayloadBytes,
    );
}

#[test]
fn cancellation_is_polled_before_parse_periodically_and_before_return() {
    let limits = complete_builder().build().expect("complete limits");
    let expectation = video_expectation(0, FragmentSampleDefaults::absent());
    assert!(matches!(
        inspect_with(VIDEO_HIGH_FIRST, expectation, &limits, &|| true),
        Err(FragmentInspectionError::Cancelled)
    ));

    let total_polls = Cell::new(0usize);
    inspect_with(VIDEO_HIGH_FIRST, expectation, &limits, &|| {
        total_polls.set(total_polls.get() + 1);
        false
    })
    .expect("measurement inspection succeeds");
    assert!(total_polls.get() >= 3);

    let periodic_target = total_polls.get() - 1;
    let periodic_polls = Cell::new(0usize);
    assert!(matches!(
        inspect_with(VIDEO_HIGH_FIRST, expectation, &limits, &|| {
            periodic_polls.set(periodic_polls.get() + 1);
            periodic_polls.get() == periodic_target
        }),
        Err(FragmentInspectionError::Cancelled)
    ));

    let before_return_target = total_polls.get();
    let return_polls = Cell::new(0usize);
    assert!(matches!(
        inspect_with(VIDEO_HIGH_FIRST, expectation, &limits, &|| {
            return_polls.set(return_polls.get() + 1);
            return_polls.get() == before_return_target
        }),
        Err(FragmentInspectionError::Cancelled)
    ));
}

/// Собирает полный test builder, который отдельный тест затем точечно сужает.
fn complete_builder() -> FragmentInspectionLimitsBuilder {
    FragmentInspectionLimits::builder()
        .max_input_bytes(250_000)
        .max_box_count(64)
        .max_box_depth(3)
        .max_traf_count(4)
        .max_trun_count(8)
        .max_samples(512)
        .max_sample_table_bytes(100_000)
        .max_box_payload_bytes(250_000)
}

/// Проверяет exact typed limit kind.
fn assert_limit(
    limits: FragmentInspectionLimits,
    input: &[u8],
    expected_kind: FragmentInspectionLimitKind,
) {
    assert!(matches!(
        inspect_with(
            input,
            video_expectation(0, FragmentSampleDefaults::absent()),
            &limits,
            &|| false,
        ),
        Err(FragmentInspectionError::LimitExceeded { kind, .. }) if kind == expected_kind
    ));
}
