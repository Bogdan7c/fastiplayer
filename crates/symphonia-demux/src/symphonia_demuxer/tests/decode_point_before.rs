//! Decode-point-before seek: retry, rescue, Matroska cues и верификация.

use super::*;

#[test]
fn decode_point_before_seek_rejects_coarse_after_target_position() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 0))]])
        .with_seek_response_policy(FakeSeekResponsePolicy::CoarseAfterTargetAccurateBefore);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "coarse-after-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");
    let target = Duration::from_millis(100);

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .expect("decode-point seek must be completed without error");

    assert!(
        seek_result.actual_position.as_duration() <= target,
        "DecodePointBefore should not accept backend position after target"
    );
}

#[test]
fn decode_point_before_seek_retries_when_first_video_packet_overshoots_target() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_keyframe_packet(1, 11_000))],
            vec![Ok(small_vp9_keyframe_packet(1, 4_000))],
        ])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "packet-after-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");
    let target = Duration::from_secs(10);

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .expect("decode-point seek must retry-lean up packet beforehand target");

    assert!(
        seek_result.actual_position.as_duration() <= target,
        "retry returns packet-level actual position no later than target"
    );
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .as_slice(),
        &[SeekMode::Accurate, SeekMode::Accurate]
    );
}

#[test]
fn decode_point_before_after_target_retry_expands_existing_preroll() {
    let issue = DecodePointBeforeVerificationIssue::FirstVideoAfterTarget {
        packet: DecodePointBeforeVideoPacket {
            pts: Duration::from_millis(29_233),
            track_pts: None,
            keyframe: PacketKeyframe::Keyframe,
        },
    };

    let retry_timestamp = decode_point_before_retry_timestamp_for_issue(
        Duration::from_millis(24_225),
        Duration::from_millis(29_225),
        issue,
        0,
        Duration::from_secs(5),
        Duration::from_secs(10),
    )
    .expect("after-target packet must provide a retry timestamp");

    assert_eq!(
        retry_timestamp,
        Duration::from_millis(19_225),
        "retry should expand pre-roll, Don't just back off on the small one overshoot"
    );
}

#[test]
fn decode_point_before_too_far_rescue_targets_accepted_preroll_window() {
    let issue = DecodePointBeforeVerificationIssue::FirstVideoTooFarBeforeTarget {
        packet: DecodePointBeforeVideoPacket {
            pts: Duration::from_millis(41_224),
            track_pts: None,
            keyframe: PacketKeyframe::NotKeyframe,
        },
    };

    let retry_timestamp = decode_point_before_retry_timestamp_for_issue(
        Duration::from_millis(31_931),
        Duration::from_millis(66_932),
        issue,
        3,
        Duration::from_secs(5),
        Duration::from_secs(10),
    )
    .expect("too-far packet must provide a rescue retry timestamp");

    assert_eq!(
        retry_timestamp,
        Duration::from_millis(56_932),
        "rescue You must jump to the beginning of the acceptable window, rather than target"
    );
}

#[test]
fn decode_point_before_initial_seek_targets_requested_not_far_preroll() {
    // RC1: initial backend seek целится практически в сам target, чтобы stss/cues
    // приземлились на ближайший keyframe ≤ target, а не на GOP за несколько секунд раньше.
    let requested = Duration::from_millis(94_351);
    let initial =
        decode_point_before_initial_timestamp(requested, DECODE_POINT_BEFORE_INITIAL_SEEK_MARGIN);

    assert!(
        initial < requested,
        "The initial seek must remain at the latest target: {initial:?} < {requested:?}"
    );
    assert!(
        requested.saturating_sub(initial) <= Duration::from_millis(2),
        "The initial seek should be almost in target, rather than the entire pre-roll before: {:?}",
        requested.saturating_sub(initial)
    );
}

#[test]
fn decode_point_before_matroska_cue_index_overrides_initial_backend_target() {
    let seek_timestamp_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 8_000))]])
        .with_seek_timestamp_log(Arc::clone(&seek_timestamp_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "matroska-cue-anchor",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");
    demuxer.matroska_cue_index = MatroskaCueIndex::from_track_cues_for_tests(
        TrackId::new(1),
        [Duration::from_secs(2), Duration::from_secs(8)],
    );

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            10,
        )))
        .expect("cue-backed DecodePointBefore must accept keyframe beforehand target");

    assert_eq!(
        seek_timestamp_log
            .lock()
            .expect("seek timestamp log lock")
            .as_slice(),
        &[8_000],
        "The first backend seek should go to the nearest Matroska cue, not the target-1ms."
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        Duration::from_secs(8),
        "actual remains verified decode anchor, public target is stored separately"
    );
    assert_eq!(
        seek_result.requested_position.as_duration(),
        Duration::from_secs(10),
        "requested_position should not be converted into a keyframe-before target"
    );
}

#[test]
fn decode_point_before_matroska_retry_uses_previous_cue_before_backoff() {
    let seek_timestamp_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_inter_frame_packet(1, 3_040))],
            vec![
                Ok(small_vp9_keyframe_packet(1, 37)),
                Ok(small_vp9_keyframe_packet(1, 2_039)),
            ],
        ])
        .with_seek_timestamp_log(Arc::clone(&seek_timestamp_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "matroska-previous-cue-retry",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");
    demuxer.matroska_cue_index = MatroskaCueIndex::from_track_cues_for_tests(
        TrackId::new(1),
        [Duration::from_millis(37), Duration::from_millis(2_039)],
    );

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            3,
        )))
        .expect("previous cue retry should find keyframe beforehand target");

    assert_eq!(
        seek_timestamp_log
            .lock()
            .expect("seek timestamp log lock")
            .as_slice(),
        &[2_039, 37],
        "rejected nearest cue should proceed to the preceding cue, not 5s backoff"
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        Duration::from_millis(2_039),
        "The actual must be verified packet from a previous-cue attempt"
    );
}

#[test]
fn decode_point_before_matroska_too_far_uses_rescue_not_previous_cue() {
    let seek_timestamp_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_keyframe_packet(1, 8_000))],
            vec![Ok(small_vp9_keyframe_packet(1, 9_500))],
        ])
        .with_seek_timestamp_log(Arc::clone(&seek_timestamp_log));
    let options = DemuxerOptions::default()
        .with_decode_point_before_preroll(Duration::from_secs(1))
        .with_decode_point_before_max_accepted_preroll(Duration::from_secs(1));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "matroska-too-far-rescue",
        HashMap::new(),
        DemuxSeekability::Seekable,
        options,
    )
    .expect("The fake demuxer must open.");
    demuxer.matroska_cue_index = MatroskaCueIndex::from_track_cues_for_tests(
        TrackId::new(1),
        [Duration::from_secs(2), Duration::from_secs(8)],
    );

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            10,
        )))
        .expect("too-far cue anchor should move on rescue retry nearer target");

    assert_eq!(
        seek_timestamp_log
            .lock()
            .expect("seek timestamp log lock")
            .as_slice(),
        &[8_000, 9_000],
        "The too-far error should use the rescue window, not the previous Matroska cue."
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        Duration::from_millis(9_500),
        "rescue retry must accept keyframe inside pre-roll window"
    );
}

#[test]
fn decode_point_before_seek_fails_when_actual_is_before_but_video_packet_is_after_target() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let scripts = (0..=DECODE_POINT_BEFORE_MAX_RETRIES)
        .map(|_| vec![Ok(small_vp9_keyframe_packet(1, 11_000))])
        .collect::<Vec<_>>();
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(scripts)
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "actual-before-packet-after",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect_err("video packet afterward target should reject DecodePointBefore");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("verification failure must have typed DemuxError");

    assert!(matches!(
        demux_error,
        DemuxError::DecodePointBeforeVerificationFailed {
            reason: "first_video_after_target",
            ..
        }
    ));
    assert!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .len()
            > 1
    );
}

#[test]
fn decode_point_before_seek_success_prebuffers_verified_video_packet() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 400))]]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "packet-before-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("keyframe packet before target must accept DecodePointBefore");
    let packet_event = demuxer
        .next_event()
        .expect("prebuffered packet should be read without error");
    let DemuxReadEvent::Packet(packet) = packet_event else {
        panic!("verification must keep the packet, received {packet_event:?}");
    };

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(400)
    );
    assert_eq!(
        seek_result
            .actual_track_timestamp
            .expect("video packet raw timestamp need to update actual")
            .track_id,
        TrackId::new(1)
    );
    assert_eq!(packet.kind, TrackKind::Video);
    assert_eq!(packet.pts, Duration::from_millis(400));
    assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
}

#[test]
fn decode_point_before_seek_accepts_startup_keyframe_after_zero() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 33))]]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "startup-keyframe-after-zero",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::ZERO))
        .expect("The starting keyframe should be accepted immediately after zero seek.");
    let packet_event = demuxer
        .next_event()
        .expect("startup packet should stay prebuffer");
    let DemuxReadEvent::Packet(packet) = packet_event else {
        panic!("verification must save startup packet received {packet_event:?}");
    };

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(33)
    );
    assert_eq!(packet.kind, TrackKind::Video);
    assert_eq!(packet.pts, Duration::from_millis(33));
    assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
}

#[test]
fn decode_point_before_seek_accepts_startup_keyframe_after_near_zero_restore() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 33))]]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "startup-keyframe-after-near-zero-restore",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");
    let restored_position = Duration::from_nanos(10_417);

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(restored_position))
        .expect("The microsecond restore drift should remain the start of media.");
    let packet_event = demuxer
        .next_event()
        .expect("accepted startup packet should reach playback pipeline");
    let DemuxReadEvent::Packet(packet) = packet_event else {
        panic!("verification must save startup packet received {packet_event:?}");
    };

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(33)
    );
    assert_eq!(packet.kind, TrackKind::Video);
    assert_eq!(packet.pts, Duration::from_millis(33));
    assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
}

#[test]
fn decode_point_before_seek_does_not_treat_regular_early_seek_as_startup_drift() {
    let scripts = (0..=DECODE_POINT_BEFORE_MAX_RETRIES)
        .map(|_| vec![Ok(small_vp9_keyframe_packet(1, 33))])
        .collect::<Vec<_>>();
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(scripts);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "regular-early-seek-is-not-startup-drift",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(2),
        ))
        .expect_err("A regular seek will not accept a keyframe after the target");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("verification failure must have typed DemuxError");

    assert!(matches!(
        demux_error,
        DemuxError::DecodePointBeforeVerificationFailed {
            reason: "first_video_after_target",
            ..
        }
    ));
}

#[test]
fn decode_point_before_seek_rejects_startup_keyframe_beyond_lead_window() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 500))]]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "startup-keyframe-too-late",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::ZERO))
        .expect_err("Too late startup keyframe should not be considered the beginning.");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("verification failure must have typed DemuxError");

    assert!(matches!(
        demux_error,
        DemuxError::DecodePointBeforeVerificationFailed {
            reason: "first_video_after_target",
            ..
        }
    ));
}

#[test]
fn decode_point_before_returns_verification_events_in_read_order() {
    let reader = FakeFormatReader::new(
        vec![
            vp9_video_track(1),
            aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000)),
        ],
        Vec::new(),
    )
    .with_seek_packet_scripts(vec![vec![
        Ok(fake_packet(2, 100, b"audio".to_vec())),
        Ok(small_vp9_keyframe_packet(1, 400)),
    ]]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "verification-order",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("verification must accept selected video packet");

    let first_event = demuxer
        .next_event()
        .expect("The first buffered event should read");
    let second_event = demuxer
        .next_event()
        .expect("second buffered event should read");

    match first_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(2));
            assert_eq!(packet.kind, TrackKind::Audio);
            assert_eq!(packet.pts, Duration::from_millis(100));
        }
        unexpected_event => panic!("expected audio packet, received {unexpected_event:?}"),
    }
    match second_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(1));
            assert_eq!(packet.kind, TrackKind::Video);
            assert_eq!(packet.pts, Duration::from_millis(400));
        }
        unexpected_event => panic!("expected video packet, received {unexpected_event:?}"),
    }
}

#[test]
fn decode_point_before_verifies_selected_video_track_not_any_video_track() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1), vp9_video_track(2)], Vec::new())
        .with_seek_packet_scripts(vec![vec![
            Ok(small_vp9_keyframe_packet(2, 11_000)),
            Ok(small_vp9_keyframe_packet(1, 400)),
        ]]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "selected-video-verification",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("unselected video packet does not have to solve verification selected track");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(400)
    );
    assert_eq!(
        seek_result
            .actual_track_timestamp
            .expect("verified actual must have timestamp selected video track-?")
            .track_id,
        TrackId::new(1)
    );
}

#[test]
fn decode_point_before_seek_accepts_unknown_keyframe_before_target() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(fake_packet(1, 400, b"\x00".to_vec()))]]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "unknown-keyframe-before-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("unknown keyframe before target should not be blocked seek fully");
    let packet_event = demuxer
        .next_event()
        .expect("The prebuffered packet should read");
    let DemuxReadEvent::Packet(packet) = packet_event else {
        panic!("Packet to return pipeline, received {packet_event:?}");
    };

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(400)
    );
    assert_eq!(packet.keyframe, PacketKeyframe::Unknown);
}

#[test]
fn reset_required_after_preview_seek_is_returned_as_tracks_changed_event() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![
            Err(SymphoniaError::ResetRequired),
            Ok(small_vp9_keyframe_packet(4, 600)),
        ]])
        .with_reset_track_update(vec![vp9_video_track(4)]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "post-seek-reset",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    demuxer
        .seek_with_request(DemuxSeekRequest::preview(Duration::from_millis(500)))
        .expect("preview-mode seek must be completed before post-seek read");

    let reset_event = demuxer
        .next_event()
        .expect("ResetRequired after seek should be lifecycle event");
    let packet_event = demuxer
        .next_event()
        .expect("packet afterward TracksChanged should remain available");

    match reset_event {
        DemuxReadEvent::TracksChanged(track_update) => {
            assert_eq!(track_update.tracks[0].id, TrackId::new(4));
        }
        unexpected_event => panic!("expected TracksChanged, received {unexpected_event:?}"),
    }
    match packet_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(4));
            assert_eq!(packet.pts, Duration::from_millis(600));
        }
        unexpected_event => {
            panic!("Expected packet new track, received {unexpected_event:?}")
        }
    }
}

#[test]
fn reset_required_during_decode_point_verification_is_buffered_before_packet() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![
            Err(SymphoniaError::ResetRequired),
            Ok(small_vp9_keyframe_packet(4, 400)),
        ]])
        .with_reset_track_update(vec![vp9_video_track(4)]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "verification-reset",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("verification must survive ResetRequired and accept a new video track");
    let reset_event = demuxer
        .next_event()
        .expect("TracksChanged must return before verified packet");
    let packet_event = demuxer
        .next_event()
        .expect("verified packet should come back after TracksChanged");

    assert_eq!(
        seek_result
            .actual_track_timestamp
            .expect("The verified actual must belong to the new video track.")
            .track_id,
        TrackId::new(4)
    );
    assert!(matches!(reset_event, DemuxReadEvent::TracksChanged(_)));
    match packet_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(4));
            assert_eq!(packet.pts, Duration::from_millis(400));
        }
        unexpected_event => panic!("expected verified packet, received {unexpected_event:?}"),
    }
}

#[test]
fn tracks_changed_from_failed_decode_point_verification_survives_retry() {
    let seek_track_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![
                Err(SymphoniaError::ResetRequired),
                Ok(small_vp9_inter_frame_packet(4, 400)),
            ],
            vec![Ok(small_vp9_keyframe_packet(4, 300))],
        ])
        .with_reset_track_update(vec![
            aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000)),
            vp9_video_track(4),
        ])
        .with_seek_track_log(Arc::clone(&seek_track_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "verification-reset-before-retry",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect("retry should find keyframe afterward ResetRequired");
    let reset_event = demuxer
        .next_event()
        .expect("TracksChanged from rejected attempt should be retained");
    let packet_event = demuxer
        .next_event()
        .expect("The verified packet of a successful retry attempt must remain available.");

    match reset_event {
        DemuxReadEvent::TracksChanged(track_update) => {
            assert!(
                track_update
                    .tracks
                    .iter()
                    .any(|track| track.kind == TrackKind::Video && track.id == TrackId::new(4))
            );
        }
        unexpected_event => panic!("expected TracksChanged, received {unexpected_event:?}"),
    }
    match packet_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(4));
            assert_eq!(packet.pts, Duration::from_millis(300));
            assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
        }
        unexpected_event => panic!("expected verified packet, received {unexpected_event:?}"),
    }
    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(300)
    );
    assert_eq!(
        seek_track_log
            .lock()
            .expect("seek track log mutex should not be poisoned")
            .as_slice(),
        &[1, 4]
    );
}

#[test]
fn decode_point_before_seek_retries_when_first_video_packet_is_not_keyframe() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_inter_frame_packet(1, 400))],
            vec![Ok(small_vp9_keyframe_packet(1, 300))],
        ])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "inter-frame-before-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect("retry should find keyframe before target");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(300)
    );
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .len(),
        2
    );
}

#[test]
fn decode_point_before_accepts_keyframe_after_initial_inter_frame_in_prefix() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![
            Ok(small_vp9_inter_frame_packet(1, 111_445)),
            Ok(small_vp9_keyframe_packet(1, 112_145)),
        ]])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "inter-frame-then-keyframe-before-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(116_449),
        ))
        .expect("verification must accept keyframe inside bounded prefix-?");
    let first_event = demuxer
        .next_event()
        .expect("prebuffered inter-frame should read");
    let DemuxReadEvent::Packet(first_packet) = first_event else {
        panic!("verification prefix should keep the first packet, got {first_event:?}");
    };
    let accepted_event = demuxer
        .next_event()
        .expect("The prebuffered keyframe should read");
    let DemuxReadEvent::Packet(accepted_packet) = accepted_event else {
        panic!("verification prefix should be preserved accepted keyframe, got {accepted_event:?}");
    };

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(112_145)
    );
    assert_eq!(first_packet.keyframe, PacketKeyframe::NotKeyframe);
    assert_eq!(accepted_packet.keyframe, PacketKeyframe::Keyframe);
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .len(),
        1
    );
}

#[test]
fn decode_point_before_default_prefix_limit_reaches_later_keyframe() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let mut long_prefix = Vec::new();
    for frame_index in 0..180 {
        long_prefix.push(Ok(small_vp9_inter_frame_packet(
            1,
            65_181 + frame_index * 16,
        )));
    }
    long_prefix.push(Ok(small_vp9_keyframe_packet(1, 68_061)));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![long_prefix])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "long-prefix-keyframe-before-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(69_143),
        ))
        .expect("default verification limit should reach keyframe current GOP");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(68_061)
    );
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .len(),
        1
    );
}

#[test]
fn decode_point_before_retries_when_prefix_reaches_target_before_keyframe() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![
                Ok(small_vp9_inter_frame_packet(1, 9_900)),
                Ok(small_vp9_inter_frame_packet(1, 10_100)),
            ],
            vec![Ok(small_vp9_keyframe_packet(1, 9_000))],
        ])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "inter-frame-crosses-target-before-keyframe",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect("after-target packet before keyframe must be called retry");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(9_000)
    );
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .len(),
        2
    );
}

#[test]
fn decode_point_before_retries_when_first_video_packet_is_too_far_before_target() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_keyframe_packet(1, 0))],
            vec![Ok(small_vp9_keyframe_packet(1, 96_000))],
        ])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "too-far-before-target-rescue",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(96_784),
        ))
        .expect("too-far decode point must retry-lean in target");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(96_000)
    );
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .len(),
        2
    );
}

#[test]
fn decode_point_before_fails_instead_of_accepting_start_of_file_for_middle_seek() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_keyframe_packet(1, 0))],
            vec![Ok(small_vp9_keyframe_packet(1, 0))],
        ]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "too-far-before-target-failure",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(96_784),
        ))
        .expect_err("A seek in the middle of a file must not accept a packet from the beginning of the file.");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("too-far verification failure must have typed DemuxError");

    assert!(matches!(
        demux_error,
        DemuxError::DecodePointBeforeVerificationFailed {
            reason: "first_video_too_far_before_target",
            ..
        }
    ));
}

#[test]
fn decode_point_before_uses_video_packet_when_audio_actual_is_earlier() {
    let scripts = (0..=DECODE_POINT_BEFORE_MAX_RETRIES)
        .map(|_| {
            vec![
                Ok(fake_packet(1, 100, b"audio".to_vec())),
                Ok(small_vp9_keyframe_packet(2, 11_000)),
            ]
        })
        .collect::<Vec<_>>();
    let reader = FakeFormatReader::new(
        vec![
            aac_audio_track_with_timing(1, SymphoniaDuration::new(30_000)),
            vp9_video_track(2),
        ],
        Vec::new(),
    )
    .with_seek_packet_scripts(scripts);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "audio-actual-video-after",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("The fake demuxer must open.");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect_err("Early audio actual should not mask video packet after target");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("video verification failure must have typed DemuxError");

    assert!(matches!(
        demux_error,
        DemuxError::DecodePointBeforeVerificationFailed {
            reason: "first_video_after_target",
            ..
        }
    ));
}
