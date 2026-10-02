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
    .expect("fake demuxer должен открыться");
    let target = Duration::from_millis(100);

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .expect("decode-point seek должен завершиться без ошибки");

    assert!(
        seek_result.actual_position.as_duration() <= target,
        "DecodePointBefore не должен принимать backend-позицию после target"
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
    .expect("fake demuxer должен открыться");
    let target = Duration::from_secs(10);

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .expect("decode-point seek должен retry-нуться до packet перед target");

    assert!(
        seek_result.actual_position.as_duration() <= target,
        "retry должен вернуть packet-level actual position не позже target"
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
    .expect("after-target packet должен дать retry timestamp");

    assert_eq!(
        retry_timestamp,
        Duration::from_millis(19_225),
        "retry должен расширить pre-roll, а не отступить только на маленький overshoot"
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
    .expect("too-far packet должен дать rescue retry timestamp");

    assert_eq!(
        retry_timestamp,
        Duration::from_millis(56_932),
        "rescue должен прыгать к началу допустимого окна, а не в сам target"
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
        "initial seek должен оставаться не позже target: {initial:?} < {requested:?}"
    );
    assert!(
        requested.saturating_sub(initial) <= Duration::from_millis(2),
        "initial seek должен быть почти в target, а не на целый pre-roll раньше: {:?}",
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
    .expect("fake demuxer должен открыться");
    demuxer.matroska_cue_index = MatroskaCueIndex::from_track_cues_for_tests(
        TrackId::new(1),
        [Duration::from_secs(2), Duration::from_secs(8)],
    );

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            10,
        )))
        .expect("cue-backed DecodePointBefore должен принять keyframe перед target");

    assert_eq!(
        seek_timestamp_log
            .lock()
            .expect("seek timestamp log lock")
            .as_slice(),
        &[8_000],
        "первый backend seek должен идти к ближайшему Matroska cue, а не к target-1ms"
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        Duration::from_secs(8),
        "actual остаётся verified decode anchor, public target хранится отдельно"
    );
    assert_eq!(
        seek_result.requested_position.as_duration(),
        Duration::from_secs(10),
        "requested_position не должен превращаться в keyframe-before target"
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
    .expect("fake demuxer должен открыться");
    demuxer.matroska_cue_index = MatroskaCueIndex::from_track_cues_for_tests(
        TrackId::new(1),
        [Duration::from_millis(37), Duration::from_millis(2_039)],
    );

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            3,
        )))
        .expect("previous cue retry должен найти keyframe перед target");

    assert_eq!(
        seek_timestamp_log
            .lock()
            .expect("seek timestamp log lock")
            .as_slice(),
        &[2_039, 37],
        "rejected nearest cue должен перейти к предыдущему cue, а не к 5s backoff"
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        Duration::from_millis(2_039),
        "actual должен стать verified packet из previous-cue attempt"
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
    .expect("fake demuxer должен открыться");
    demuxer.matroska_cue_index = MatroskaCueIndex::from_track_cues_for_tests(
        TrackId::new(1),
        [Duration::from_secs(2), Duration::from_secs(8)],
    );

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            10,
        )))
        .expect("too-far cue anchor должен перейти в rescue retry ближе к target");

    assert_eq!(
        seek_timestamp_log
            .lock()
            .expect("seek timestamp log lock")
            .as_slice(),
        &[8_000, 9_000],
        "too-far ошибка должна использовать rescue window, а не предыдущий Matroska cue"
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        Duration::from_millis(9_500),
        "rescue retry должен принять keyframe внутри разрешённого pre-roll окна"
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
    .expect("fake demuxer должен открыться");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect_err("video packet после target должен отклонить DecodePointBefore");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("verification failure должен быть typed DemuxError");

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
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("keyframe packet до target должен принять DecodePointBefore");
    let packet_event = demuxer
        .next_event()
        .expect("prebuffered packet должен читаться без ошибки");
    let DemuxReadEvent::Packet(packet) = packet_event else {
        panic!("verification должна сохранить packet, получено {packet_event:?}");
    };

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(400)
    );
    assert_eq!(
        seek_result
            .actual_track_timestamp
            .expect("video packet raw timestamp должен обновить actual")
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
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::ZERO))
        .expect("стартовый keyframe сразу после zero seek должен приниматься");
    let packet_event = demuxer
        .next_event()
        .expect("startup packet должен остаться в prebuffer");
    let DemuxReadEvent::Packet(packet) = packet_event else {
        panic!("verification должна сохранить startup packet, получено {packet_event:?}");
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
    .expect("fake demuxer должен открыться");
    let restored_position = Duration::from_nanos(10_417);

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(restored_position))
        .expect("микросекундный restore drift должен остаться стартом media");
    let packet_event = demuxer
        .next_event()
        .expect("принятый startup packet должен дойти до playback pipeline");
    let DemuxReadEvent::Packet(packet) = packet_event else {
        panic!("verification должна сохранить startup packet, получено {packet_event:?}");
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
    .expect("fake demuxer должен открыться");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(2),
        ))
        .expect_err("обычный seek не должен принимать keyframe после target");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("verification failure должен быть typed DemuxError");

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
    .expect("fake demuxer должен открыться");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::ZERO))
        .expect_err("слишком поздний startup keyframe не должен считаться началом");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("verification failure должен быть typed DemuxError");

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
    .expect("fake demuxer должен открыться");

    demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("verification должен принять selected video packet");

    let first_event = demuxer
        .next_event()
        .expect("первый buffered event должен читаться");
    let second_event = demuxer
        .next_event()
        .expect("второй buffered event должен читаться");

    match first_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(2));
            assert_eq!(packet.kind, TrackKind::Audio);
            assert_eq!(packet.pts, Duration::from_millis(100));
        }
        unexpected_event => panic!("ожидали audio packet, получили {unexpected_event:?}"),
    }
    match second_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(1));
            assert_eq!(packet.kind, TrackKind::Video);
            assert_eq!(packet.pts, Duration::from_millis(400));
        }
        unexpected_event => panic!("ожидали video packet, получили {unexpected_event:?}"),
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
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("unselected video packet не должен решать verification selected track-а");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(400)
    );
    assert_eq!(
        seek_result
            .actual_track_timestamp
            .expect("verified actual должен быть timestamp selected video track-а")
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
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("unknown keyframe до target не должен блокировать seek полностью");
    let packet_event = demuxer
        .next_event()
        .expect("prebuffered packet должен читаться");
    let DemuxReadEvent::Packet(packet) = packet_event else {
        panic!("packet должен вернуться pipeline, получено {packet_event:?}");
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
    .expect("fake demuxer должен открыться");

    demuxer
        .seek_with_request(DemuxSeekRequest::preview(Duration::from_millis(500)))
        .expect("preview-mode seek должен завершиться до post-seek read");

    let reset_event = demuxer
        .next_event()
        .expect("ResetRequired после seek должен стать lifecycle event");
    let packet_event = demuxer
        .next_event()
        .expect("packet после TracksChanged должен остаться доступным");

    match reset_event {
        DemuxReadEvent::TracksChanged(track_update) => {
            assert_eq!(track_update.tracks[0].id, TrackId::new(4));
        }
        unexpected_event => panic!("ожидали TracksChanged, получили {unexpected_event:?}"),
    }
    match packet_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(4));
            assert_eq!(packet.pts, Duration::from_millis(600));
        }
        unexpected_event => {
            panic!("ожидали packet нового track-а, получили {unexpected_event:?}")
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
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("verification должен пережить ResetRequired и принять новый video track");
    let reset_event = demuxer
        .next_event()
        .expect("TracksChanged должен вернуться перед verified packet-ом");
    let packet_event = demuxer
        .next_event()
        .expect("verified packet должен вернуться после TracksChanged");

    assert_eq!(
        seek_result
            .actual_track_timestamp
            .expect("verified actual должен принадлежать новому video track-у")
            .track_id,
        TrackId::new(4)
    );
    assert!(matches!(reset_event, DemuxReadEvent::TracksChanged(_)));
    match packet_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(4));
            assert_eq!(packet.pts, Duration::from_millis(400));
        }
        unexpected_event => panic!("ожидали verified packet, получили {unexpected_event:?}"),
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
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect("retry должен найти keyframe после ResetRequired");
    let reset_event = demuxer
        .next_event()
        .expect("TracksChanged из rejected attempt должен сохраниться");
    let packet_event = demuxer
        .next_event()
        .expect("verified packet успешной retry-попытки должен остаться доступен");

    match reset_event {
        DemuxReadEvent::TracksChanged(track_update) => {
            assert!(
                track_update
                    .tracks
                    .iter()
                    .any(|track| track.kind == TrackKind::Video && track.id == TrackId::new(4))
            );
        }
        unexpected_event => panic!("ожидали TracksChanged, получили {unexpected_event:?}"),
    }
    match packet_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(4));
            assert_eq!(packet.pts, Duration::from_millis(300));
            assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
        }
        unexpected_event => panic!("ожидали verified packet, получили {unexpected_event:?}"),
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
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect("retry должен найти keyframe до target");

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
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(116_449),
        ))
        .expect("verification должен принять keyframe внутри bounded prefix-а");
    let first_event = demuxer
        .next_event()
        .expect("prebuffered inter-frame должен читаться");
    let DemuxReadEvent::Packet(first_packet) = first_event else {
        panic!("verification prefix должен сохранить первый packet, получено {first_event:?}");
    };
    let accepted_event = demuxer
        .next_event()
        .expect("prebuffered keyframe должен читаться");
    let DemuxReadEvent::Packet(accepted_packet) = accepted_event else {
        panic!(
            "verification prefix должен сохранить accepted keyframe, получено {accepted_event:?}"
        );
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
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(69_143),
        ))
        .expect("default verification limit должен дойти до keyframe текущего GOP");

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
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect("after-target packet до keyframe должен вызвать retry");

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
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(96_784),
        ))
        .expect("too-far decode point должен retry-нуться ближе к target");

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
    .expect("fake demuxer должен открыться");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(96_784),
        ))
        .expect_err("seek в середину файла не должен принимать packet с начала файла");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("too-far verification failure должен быть typed DemuxError");

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
    .expect("fake demuxer должен открыться");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect_err("ранний audio actual не должен маскировать video packet после target");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("video verification failure должен быть typed DemuxError");

    assert!(matches!(
        demux_error,
        DemuxError::DecodePointBeforeVerificationFailed {
            reason: "first_video_after_target",
            ..
        }
    ));
}
