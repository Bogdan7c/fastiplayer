//! Writer limits, cancellation, representability и inspection passthrough.

use std::cell::Cell;

use super::super::error::{
    FragmentMediaBoxType, FragmentReconstructionError, FragmentWriteCancellationPhase,
    FragmentWriteError,
};
use super::super::plan::{
    WRITE_CANCELLATION_INTERVAL, checked_box_size, checked_data_offset, plan_media_fragment,
};
use super::super::write::write_media_fragment;
use super::super::{FragmentMediaKind, FragmentWriteLimits};
use super::support::{
    SyntheticRun, VIDEO_HIGH_FIRST, atom, insert_traf_child, inspect, inspection_limits,
    never_cancel, reconstruct, reconstruct_with, synthetic_fragment, write_limits,
};
use crate::{
    FragmentDrmEvidence, FragmentInspectionError, FragmentPrivateExtension,
    FragmentStructureContext,
};

#[test]
fn malformed_drm_private_and_live_are_exact_inspection_causes() {
    let malformed = &VIDEO_HIGH_FIRST[..VIDEO_HIGH_FIRST.len() - 1];
    assert!(matches!(
        reconstruct(
            malformed,
            0,
            FragmentMediaKind::VideoWithRequiredProvenRandomAccess
        ),
        Err(FragmentReconstructionError::Inspection(
            FragmentInspectionError::StructuralTruncation {
                context: FragmentStructureContext::TopLevel
                    | FragmentStructureContext::TrackFragmentRun
            }
        ))
    ));

    let mut drm = VIDEO_HIGH_FIRST.to_vec();
    insert_traf_child(&mut drm, &atom(*b"pssh", &[0; 4]));
    assert!(matches!(
        reconstruct(
            &drm,
            0,
            FragmentMediaKind::VideoWithRequiredProvenRandomAccess
        ),
        Err(FragmentReconstructionError::Inspection(
            FragmentInspectionError::DrmProtected {
                evidence: FragmentDrmEvidence::Box(box_type)
            }
        )) if box_type == *b"pssh"
    ));

    let mut private = VIDEO_HIGH_FIRST.to_vec();
    insert_traf_child(&mut private, &atom(*b"uuid", &[0x11; 16]));
    assert!(matches!(
        reconstruct(
            &private,
            0,
            FragmentMediaKind::VideoWithRequiredProvenRandomAccess
        ),
        Err(FragmentReconstructionError::Inspection(
            FragmentInspectionError::PrivateExtension {
                extension: FragmentPrivateExtension::UnknownUuid
            }
        ))
    ));

    let tfrf_uuid = [
        0xd4, 0x80, 0x7e, 0xf2, 0xca, 0x39, 0x46, 0x95, 0x8e, 0x54, 0x26, 0xcb, 0x9e, 0x46, 0xa7,
        0x9f,
    ];
    let mut live = VIDEO_HIGH_FIRST.to_vec();
    insert_traf_child(&mut live, &atom(*b"uuid", &tfrf_uuid));
    assert!(matches!(
        reconstruct(
            &live,
            0,
            FragmentMediaKind::VideoWithRequiredProvenRandomAccess
        ),
        Err(FragmentReconstructionError::Inspection(
            FragmentInspectionError::LiveMetadata
        ))
    ));
}

#[test]
fn mandatory_output_limit_and_preinspection_cancellation_are_typed() {
    assert!(FragmentWriteLimits::try_new(0).is_err());
    let limits = inspection_limits();
    let tiny_output = FragmentWriteLimits::try_new(16).expect("ненулевой tiny budget");
    assert!(matches!(
        reconstruct_with(
            VIDEO_HIGH_FIRST,
            0,
            FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
            &limits,
            tiny_output,
            &never_cancel
        ),
        Err(FragmentReconstructionError::Writing(
            FragmentWriteError::OutputLimitExceeded { limit: 16, .. }
        ))
    ));
    assert!(matches!(
        reconstruct_with(
            VIDEO_HIGH_FIRST,
            0,
            FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
            &limits,
            write_limits(),
            &|| true
        ),
        Err(FragmentReconstructionError::Inspection(
            FragmentInspectionError::Cancelled
        ))
    ));
}

#[test]
fn allocation_failure_variant_has_stable_secret_safe_display() {
    let error = FragmentWriteError::AllocationFailed { requested: 4_096 };
    assert_eq!(
        error.to_string(),
        "canonical fragment media allocation failed"
    );
    assert_eq!(
        error,
        FragmentWriteError::AllocationFailed { requested: 4_096 }
    );
}

#[test]
fn writer_cancellation_phases_are_polled_at_owned_boundaries() {
    let limits = inspection_limits();
    let normalized = inspect(
        VIDEO_HIGH_FIRST,
        0,
        FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
        &limits,
    );
    assert!(matches!(
        plan_media_fragment(
            &normalized,
            FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
            write_limits(),
            &|| true
        ),
        Err(FragmentWriteError::Cancelled {
            phase: FragmentWriteCancellationPhase::Planning
        })
    ));

    let layout = plan_media_fragment(
        &normalized,
        FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
        write_limits(),
        &never_cancel,
    )
    .expect("writer plan");
    assert!(matches!(
        write_media_fragment(&normalized, layout, &|| true),
        Err(FragmentWriteError::Cancelled {
            phase: FragmentWriteCancellationPhase::SampleTable
        })
    ));

    let table_polls = normalized
        .samples()
        .len()
        .div_ceil(WRITE_CANCELLATION_INTERVAL);
    assert_cancelled_after_polls(
        &normalized,
        layout,
        table_polls,
        FragmentWriteCancellationPhase::BeforeMediaPayload,
    );
    assert_cancelled_after_polls(
        &normalized,
        layout,
        table_polls + 1,
        FragmentWriteCancellationPhase::BeforePublication,
    );
}

#[test]
fn missing_video_flags_and_mixed_unrepresentable_cto_fail_in_writer() {
    let limits = inspection_limits();
    let without_flags = synthetic_fragment(
        0,
        &[SyntheticRun {
            version: 0,
            offsets: vec![0],
            include_flags: false,
        }],
    );
    let audio_plan = inspect(
        &without_flags,
        0,
        FragmentMediaKind::AudioWithoutRandomAccessRequirement,
        &limits,
    );
    assert!(matches!(
        plan_media_fragment(
            &audio_plan,
            FragmentMediaKind::VideoWithRequiredProvenRandomAccess,
            write_limits(),
            &never_cancel
        ),
        Err(FragmentWriteError::MissingVideoSampleFlags { sample_index: 0 })
    ));

    let mixed = synthetic_fragment(
        1,
        &[
            SyntheticRun {
                version: 1,
                offsets: vec![-1],
                include_flags: false,
            },
            SyntheticRun {
                version: 0,
                offsets: vec![i64::from(i32::MAX) + 1],
                include_flags: false,
            },
        ],
    );
    let mixed_plan = inspect(
        &mixed,
        1,
        FragmentMediaKind::AudioWithoutRandomAccessRequirement,
        &limits,
    );
    assert!(matches!(
        plan_media_fragment(
            &mixed_plan,
            FragmentMediaKind::AudioWithoutRandomAccessRequirement,
            write_limits(),
            &never_cancel
        ),
        Err(FragmentWriteError::CompositionOffsetUnrepresentable {
            sample_index: 1,
            offset
        }) if offset == i64::from(i32::MAX) + 1
    ));
}

#[test]
fn cto_and_tfdt_versions_follow_exact_representability() {
    for (base_decode_time, source_version, offset, expected_tfdt, expected_trun) in [
        (0, 0, i64::from(i32::MAX) + 1, 0, 0),
        (1, 1, -1, 0, 1),
        (u64::from(u32::MAX) + 1, 0, 0, 1, 0),
    ] {
        let source = synthetic_fragment(
            base_decode_time,
            &[SyntheticRun {
                version: source_version,
                offsets: vec![offset],
                include_flags: false,
            }],
        );
        let output = reconstruct(
            &source,
            base_decode_time,
            FragmentMediaKind::AudioWithoutRandomAccessRequirement,
        )
        .expect("representable canonical fragment");
        assert_eq!(output.as_bytes()[56], expected_tfdt);
        let tfdt_size = if expected_tfdt == 0 { 16 } else { 20 };
        let trun_start = 48 + tfdt_size;
        assert_eq!(output.as_bytes()[trun_start + 8], expected_trun);
    }
}

#[test]
fn box_size_and_data_offset_arithmetic_are_typed() {
    assert!(matches!(
        checked_box_size(
            FragmentMediaBoxType::TrackFragmentRun,
            u64::from(u32::MAX) + 1
        ),
        Err(FragmentWriteError::BoxSizeUnrepresentable {
            box_type: FragmentMediaBoxType::TrackFragmentRun,
            ..
        })
    ));
    assert!(matches!(
        checked_data_offset(u64::try_from(i64::from(i32::MAX) + 1).expect("positive")),
        Err(FragmentWriteError::DataOffsetUnrepresentable { .. })
    ));
}

fn assert_cancelled_after_polls(
    normalized: &super::super::super::model::NormalizedFragmentPlan<'_>,
    layout: super::super::plan::PlannedMediaFragment,
    successful_polls: usize,
    expected_phase: FragmentWriteCancellationPhase,
) {
    let polls = Cell::new(0_usize);
    let cancellation = || {
        let current = polls.get();
        polls.set(current + 1);
        current >= successful_polls
    };
    assert!(matches!(
        write_media_fragment(normalized, layout, &cancellation),
        Err(FragmentWriteError::Cancelled { phase }) if phase == expected_phase
    ));
}
