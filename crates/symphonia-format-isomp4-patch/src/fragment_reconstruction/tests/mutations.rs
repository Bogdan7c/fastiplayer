//! Structural mutations exact fixtures для timing/range/security failures.

use std::num::NonZeroU32;

use super::super::error::{
    FragmentArithmeticOperation, FragmentBoxKind, FragmentDrmEvidence, FragmentInspectionError,
    FragmentPrivateExtension, FragmentTimingEvidence, FragmentUnsupportedLayout,
};
use super::super::model::FragmentSampleDefaults;
use super::support::{
    AUDIO_FIRST, AUDIO_SECOND, VIDEO_HIGH_FIRST, VIDEO_HIGH_SECOND,
    add_tfhd_duration_and_size_defaults, adjust_box_size, atom, audio_expectation, box_range,
    duplicate_traf, duplicate_trun, insert_tfdt, insert_traf_child, inspect, read_u32,
    repair_first_trun_data_offset, single_sample_video, strip_first_sample_flags,
    strip_tfhd_default_flags, strip_trun_sample_field, video_expectation, write_u32,
};

#[test]
fn manifest_duration_is_not_generic_fragment_inspection_policy() {
    let plan = inspect(
        AUDIO_FIRST,
        audio_expectation(0, FragmentSampleDefaults::absent()),
    )
    .expect("coded samples remain authoritative inside generic inspection");

    assert_eq!(plan.coded_coverage().duration(), 40_106_666);
    assert_ne!(plan.coded_coverage().duration(), 39_680_000);
}

#[test]
fn recognized_styp_and_optional_matching_tfdt_are_accepted() {
    let mut with_styp = atom(*b"styp", b"msdh\0\0\0\0msdh");
    with_styp.extend_from_slice(VIDEO_HIGH_FIRST);
    let styp_plan = inspect(
        &with_styp,
        video_expectation(0, FragmentSampleDefaults::absent()),
    )
    .expect("recognized styp is accepted");
    assert_eq!(styp_plan.coded_coverage().end_exclusive(), 40_000_000);

    let mut with_tfdt = VIDEO_HIGH_SECOND.to_vec();
    insert_tfdt(&mut with_tfdt, 40_000_000);
    let tfdt_plan = inspect(
        &with_tfdt,
        video_expectation(40_000_000, FragmentSampleDefaults::absent()),
    )
    .expect("matching tfdt is accepted");
    assert_eq!(tfdt_plan.base_decode_time(), 40_000_000);
}

#[test]
fn unknown_styp_and_conflicting_tfdt_fail_closed() {
    let mut unknown_styp = atom(*b"styp", b"evil\0\0\0\0evil");
    unknown_styp.extend_from_slice(VIDEO_HIGH_FIRST);
    assert!(matches!(
        inspect(
            &unknown_styp,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::UnsupportedLayout {
            reason: FragmentUnsupportedLayout::UnrecognizedSegmentType
        })
    ));

    let mut conflicting_tfdt = VIDEO_HIGH_SECOND.to_vec();
    insert_tfdt(&mut conflicting_tfdt, 40_000_001);
    assert!(matches!(
        inspect(
            &conflicting_tfdt,
            video_expectation(40_000_000, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::TimingConflict {
            expected_base_decode_time: 40_000_000,
            actual_base_decode_time: 40_000_001,
        })
    ));
}

#[test]
fn trun_explicit_values_and_first_flags_win_over_defaults() {
    let fixture = single_sample_video();
    let defaults = FragmentSampleDefaults::absent()
        .with_sample_duration(NonZeroU32::new(1).expect("non-zero"))
        .with_sample_size(NonZeroU32::new(1).expect("non-zero"))
        .with_sample_flags(0x0101_0000);
    let plan = inspect(&fixture, video_expectation(0, defaults))
        .expect("explicit trun values must override caller defaults");
    let sample = &plan.samples()[0];

    assert_ne!(sample.duration(), 1);
    assert_ne!(sample.payload_range().len(), 1);
    assert_eq!(sample.flags(), Some(0x0240_0040));
}

#[test]
fn tfhd_defaults_fill_absent_trun_duration_and_size() {
    let mut fixture = single_sample_video();
    let trun = box_range(&fixture, *b"trun");
    let duration = read_u32(&fixture, trun.start + 24);
    let size = read_u32(&fixture, trun.start + 28);
    add_tfhd_duration_and_size_defaults(&mut fixture, duration, size);
    strip_trun_sample_field(&mut fixture, 0x000100);
    strip_trun_sample_field(&mut fixture, 0x000200);

    let plan = inspect(
        &fixture,
        video_expectation(0, FragmentSampleDefaults::absent()),
    )
    .expect("tfhd defaults provide exact missing fields");
    assert_eq!(plan.samples()[0].duration(), duration);
    assert_eq!(plan.samples()[0].payload_range().len(), size as usize);
}

#[test]
fn missing_duration_size_and_flags_have_distinct_evidence_errors() {
    let mut missing_duration = single_sample_video();
    strip_trun_sample_field(&mut missing_duration, 0x000100);
    assert!(matches!(
        inspect(
            &missing_duration,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::TimingEvidenceMissing {
            evidence: FragmentTimingEvidence::Duration,
            sample_index: 0,
        })
    ));

    let mut missing_size = single_sample_video();
    strip_trun_sample_field(&mut missing_size, 0x000200);
    assert!(matches!(
        inspect(
            &missing_size,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::TimingEvidenceMissing {
            evidence: FragmentTimingEvidence::Size,
            sample_index: 0,
        })
    ));

    let mut missing_flags = single_sample_video();
    strip_first_sample_flags(&mut missing_flags);
    strip_tfhd_default_flags(&mut missing_flags);
    assert!(matches!(
        inspect(
            &missing_flags,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::TimingEvidenceMissing {
            evidence: FragmentTimingEvidence::Flags,
            sample_index: 0,
        })
    ));
}

#[test]
fn synthetic_trun_v1_negative_cto_is_preserved_without_retiming() {
    // Это документированная mutation exact fixture, а не captured dialect evidence.
    let mut fixture = VIDEO_HIGH_SECOND.to_vec();
    let trun = box_range(&fixture, *b"trun");
    fixture[trun.start + 8] = 1;
    write_u32(&mut fixture, trun.start + 32, u32::MAX);
    let plan = inspect(
        &fixture,
        video_expectation(40_000_000, FragmentSampleDefaults::absent()),
    )
    .expect("standard trun v1 signed CTO is supported");

    assert_eq!(plan.samples()[0].composition_offset(), -1);
    assert_eq!(plan.samples()[0].pts(), 39_999_999);
}

#[test]
fn negative_pts_and_decode_time_overflow_are_rejected() {
    let mut negative_pts = VIDEO_HIGH_FIRST.to_vec();
    let trun = box_range(&negative_pts, *b"trun");
    negative_pts[trun.start + 8] = 1;
    write_u32(&mut negative_pts, trun.start + 32, u32::MAX);
    assert!(matches!(
        inspect(
            &negative_pts,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::ArithmeticOverflow {
            operation: FragmentArithmeticOperation::PresentationTime
        })
    ));

    assert!(matches!(
        inspect(
            AUDIO_FIRST,
            audio_expectation(u64::MAX, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::ArithmeticOverflow {
            operation: FragmentArithmeticOperation::DecodeTime
        })
    ));
}

#[test]
fn offsets_outside_skip_overlap_and_underflow_are_distinct() {
    let mut outside = single_sample_video();
    let moof = box_range(&outside, *b"moof");
    let trun = box_range(&outside, *b"trun");
    write_u32(&mut outside, trun.start + 16, (moof.end + 7) as u32);
    assert!(matches!(
        inspect(
            &outside,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::SampleRangeOutsideMdat { .. })
    ));

    let mut skipped = single_sample_video();
    let moof = box_range(&skipped, *b"moof");
    let trun = box_range(&skipped, *b"trun");
    write_u32(&mut skipped, trun.start + 16, (moof.end + 9) as u32);
    assert!(matches!(
        inspect(
            &skipped,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::PayloadMismatch { .. })
    ));

    let mut overlap = VIDEO_HIGH_FIRST.to_vec();
    duplicate_trun(&mut overlap);
    assert!(matches!(
        inspect(
            &overlap,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::SampleRangeOverlap { .. })
    ));

    let mut underflow = single_sample_video();
    let trun = box_range(&underflow, *b"trun");
    write_u32(&mut underflow, trun.start + 16, u32::MAX);
    assert!(matches!(
        inspect(
            &underflow,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::OffsetOverflow)
    ));
}

#[test]
fn sample_size_outside_and_incomplete_payload_are_rejected() {
    let mut outside = single_sample_video();
    let trun = box_range(&outside, *b"trun");
    let size = read_u32(&outside, trun.start + 28);
    write_u32(&mut outside, trun.start + 28, size + 1);
    assert!(matches!(
        inspect(
            &outside,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::SampleRangeOutsideMdat { .. })
    ));

    let mut incomplete = single_sample_video();
    let trun = box_range(&incomplete, *b"trun");
    let size = read_u32(&incomplete, trun.start + 28);
    write_u32(&mut incomplete, trun.start + 28, size - 1);
    assert!(matches!(
        inspect(
            &incomplete,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::PayloadMismatch { .. })
    ));
}

#[test]
fn non_rap_and_wrong_track_are_rejected() {
    let mut non_rap = single_sample_video();
    let trun = box_range(&non_rap, *b"trun");
    write_u32(&mut non_rap, trun.start + 20, 0x0101_0000);
    assert!(matches!(
        inspect(
            &non_rap,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::RapFailure)
    ));

    let mut wrong_track = single_sample_video();
    let tfhd = box_range(&wrong_track, *b"tfhd");
    write_u32(&mut wrong_track, tfhd.start + 12, 2);
    assert!(matches!(
        inspect(
            &wrong_track,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::MultiTrack {
            expected_track_id: 1,
            actual_track_id: 2,
        })
    ));
}

#[test]
fn truncation_trailing_bytes_and_multiple_traf_fail_closed() {
    let mut truncated = VIDEO_HIGH_FIRST.to_vec();
    truncated.pop();
    assert!(matches!(
        inspect(
            &truncated,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::StructuralTruncation { .. })
    ));

    let mut trailing = VIDEO_HIGH_FIRST.to_vec();
    trailing.push(1);
    assert!(matches!(
        inspect(
            &trailing,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::UnsupportedLayout {
            reason: FragmentUnsupportedLayout::TrailingNonPadding
        })
    ));

    let mut multiple_traf = VIDEO_HIGH_FIRST.to_vec();
    duplicate_traf(&mut multiple_traf);
    assert!(matches!(
        inspect(
            &multiple_traf,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::LimitExceeded { .. })
            | Err(FragmentInspectionError::DuplicateBox {
                kind: FragmentBoxKind::TrackFragment
            })
    ));
}

#[test]
fn missing_and_duplicate_structural_boxes_are_typed() {
    let mut missing_mfhd = VIDEO_HIGH_FIRST.to_vec();
    remove_child(&mut missing_mfhd, *b"mfhd", *b"moof");
    assert!(matches!(
        inspect(
            &missing_mfhd,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::MissingBox {
            kind: FragmentBoxKind::MovieFragmentHeader
        })
    ));

    let mut duplicate_tfhd = VIDEO_HIGH_FIRST.to_vec();
    let tfhd = box_range(&duplicate_tfhd, *b"tfhd");
    let copy = duplicate_tfhd[tfhd].to_vec();
    insert_traf_child(&mut duplicate_tfhd, &copy);
    assert!(matches!(
        inspect(
            &duplicate_tfhd,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::DuplicateBox {
            kind: FragmentBoxKind::TrackFragmentHeader
        })
    ));

    let mut duplicate_tfdt = VIDEO_HIGH_FIRST.to_vec();
    insert_tfdt(&mut duplicate_tfdt, 0);
    insert_tfdt(&mut duplicate_tfdt, 0);
    assert!(matches!(
        inspect(
            &duplicate_tfdt,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::DuplicateBox {
            kind: FragmentBoxKind::TrackFragmentDecodeTime
        })
    ));

    let mut missing_trun = VIDEO_HIGH_FIRST.to_vec();
    remove_child(&mut missing_trun, *b"trun", *b"traf");
    assert!(matches!(
        inspect(
            &missing_trun,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::MissingBox {
            kind: FragmentBoxKind::TrackFragmentRun
        })
    ));
}

#[test]
fn unknown_uuid_drm_and_live_metadata_fail_closed() {
    let mut unknown_uuid = VIDEO_HIGH_FIRST.to_vec();
    insert_traf_child(&mut unknown_uuid, &atom(*b"uuid", &[0x11; 16]));
    assert!(matches!(
        inspect(
            &unknown_uuid,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::PrivateExtension {
            extension: FragmentPrivateExtension::UnknownUuid
        })
    ));

    let mut drm = VIDEO_HIGH_FIRST.to_vec();
    insert_traf_child(&mut drm, &atom(*b"pssh", &[0; 4]));
    assert!(matches!(
        inspect(
            &drm,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::DrmProtected {
            evidence: FragmentDrmEvidence::Box(code)
        }) if code == *b"pssh"
    ));

    let mut live = VIDEO_HIGH_FIRST.to_vec();
    let tfrf_uuid = [
        0xd4, 0x80, 0x7e, 0xf2, 0xca, 0x39, 0x46, 0x95, 0x8e, 0x54, 0x26, 0xcb, 0x9e, 0x46, 0xa7,
        0x9f,
    ];
    insert_traf_child(&mut live, &atom(*b"uuid", &tfrf_uuid));
    assert!(matches!(
        inspect(
            &live,
            video_expectation(0, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::LiveMetadata)
    ));
}

#[test]
fn captured_tfxd_is_ignored_but_malformed_envelope_is_rejected() {
    let plan = inspect(
        AUDIO_SECOND,
        audio_expectation(39_680_000, FragmentSampleDefaults::absent()),
    )
    .expect("captured exact tfxd envelope is recognized but not applied");
    assert_eq!(plan.coded_coverage().start(), 39_680_000);

    let mut malformed = AUDIO_SECOND.to_vec();
    let uuid = box_range(&malformed, *b"uuid");
    malformed[uuid.start + 24] = 2;
    assert!(matches!(
        inspect(
            &malformed,
            audio_expectation(39_680_000, FragmentSampleDefaults::absent())
        ),
        Err(FragmentInspectionError::StructuralTruncation { .. })
    ));
}

/// Удаляет child и обновляет parent/ancestor sizes для targeted missing-box mutation.
fn remove_child(bytes: &mut Vec<u8>, child_type: [u8; 4], parent_type: [u8; 4]) {
    let moof = box_range(bytes, *b"moof");
    let parent = box_range(bytes, parent_type);
    let child = box_range(bytes, child_type);
    let removed = child.len();
    bytes.drain(child);
    adjust_box_size(bytes, parent.start, -(removed as isize));
    if parent_type != *b"moof" {
        adjust_box_size(bytes, moof.start, -(removed as isize));
    }
    if child_type != *b"trun" {
        repair_first_trun_data_offset(bytes);
    }
}
