//! Восстановление видео-бэклога: scan, лимиты, keyframe и отмена.

use super::*;

#[test]
fn video_backlog_recovery_scan_requires_selected_video_and_proven_keyframe() {
    let mut pipeline = PlaybackPipeline::default();
    let selected_track_id = TrackId::new(10);
    let generation = pipeline.seek_generation();

    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::NoSelectedVideo
    );
    assert!(!pipeline.video_backlog_recovery_scan_allows_demux());

    pipeline.select_video_track(
        selected_track_id,
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::NoProvenKeyframeObserved
    );
    assert!(!pipeline.video_backlog_recovery_scan_allows_demux());

    assert_eq!(
        pipeline.route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(10),
            generation,
            Bytes::from_static(b"unknown-selected-video"),
            PacketKeyframe::Unknown,
        )),
        VideoBacklogRecoveryRouteOutcome::QueuedNormally
    );
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::NoProvenKeyframeObserved
    );

    assert_eq!(
        pipeline.route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
            TrackId::new(11),
            Duration::from_millis(20),
            generation,
            Bytes::from_static(b"foreign-track-keyframe"),
            PacketKeyframe::Keyframe,
        )),
        VideoBacklogRecoveryRouteOutcome::QueuedNormally
    );
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::NoProvenKeyframeObserved
    );

    assert_eq!(
        pipeline.route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(30),
            generation,
            Bytes::from_static(b"selected-track-keyframe"),
            PacketKeyframe::Keyframe,
        )),
        VideoBacklogRecoveryRouteOutcome::QueuedNormally
    );
    let preserved_pending_packets = pipeline.pending_video_packet_len();

    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::DecoderAwaitingKeyframe
    );
    assert!(!pipeline.video_backlog_recovery_scan_allows_demux());

    pipeline.mark_video_decoder_bootstrapped();
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::Started
    );
    assert!(pipeline.video_backlog_recovery_scan_allows_demux());
    assert_eq!(
        pipeline.pending_video_packet_len(),
        preserved_pending_packets
    );
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::AlreadyScanning
    );
    assert!(pipeline.video_backlog_recovery_scan_allows_demux());
    assert_eq!(
        pipeline.pending_video_packet_len(),
        preserved_pending_packets
    );
}

#[test]
fn video_backlog_recovery_scan_stages_unproven_foreign_and_stale_packets_without_touching_backlog()
{
    let mut pipeline = PlaybackPipeline::default();
    let selected_track_id = TrackId::new(10);
    let generation = pipeline.seek_generation();
    pipeline.select_video_track(
        selected_track_id,
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );

    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        selected_track_id,
        Duration::from_millis(10),
        generation,
        Bytes::from_static(b"proof-keyframe"),
        PacketKeyframe::Keyframe,
    ));
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        selected_track_id,
        Duration::from_millis(20),
        generation,
        Bytes::from_static(b"preserved-interframe"),
        PacketKeyframe::NotKeyframe,
    ));
    pipeline.mark_video_decoder_bootstrapped();
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::Started
    );

    for packet in [
        PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(30),
            generation,
            Bytes::from_static(b"selected-interframe"),
            PacketKeyframe::NotKeyframe,
        ),
        PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(40),
            generation,
            Bytes::from_static(b"selected-unknown"),
            PacketKeyframe::Unknown,
        ),
        PendingVideoPacket::new(
            TrackId::new(11),
            Duration::from_millis(50),
            generation,
            Bytes::from_static(b"foreign-keyframe"),
            PacketKeyframe::Keyframe,
        ),
        PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(60),
            generation.saturating_add(1),
            Bytes::from_static(b"stale-generation-keyframe"),
            PacketKeyframe::Keyframe,
        ),
    ] {
        assert_eq!(
            pipeline.route_pending_video_packet_for_backlog_recovery(packet),
            VideoBacklogRecoveryRouteOutcome::StagedWhileScanning
        );
    }

    assert_eq!(pipeline.pending_video_packet_len(), 2);
    assert_eq!(pipeline.video_backlog_recovery_staged_packet_len(), 4);
    assert_eq!(
        pipeline
            .front_pending_video_packet()
            .map(|packet| packet.pts),
        Some(Duration::from_millis(10))
    );
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::AlreadyScanning
    );
}

#[test]
fn video_backlog_recovery_keyframe_atomically_replaces_backlog_and_is_queued() {
    let mut pipeline = PlaybackPipeline::default();
    let selected_track_id = TrackId::new(10);
    let generation = pipeline.seek_generation();
    pipeline.select_video_track(
        selected_track_id,
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );

    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        selected_track_id,
        Duration::ZERO,
        generation,
        Bytes::from_static(b"already-sent-proof-keyframe"),
        PacketKeyframe::Keyframe,
    ));
    pipeline
        .pop_pending_video_packet_front()
        .expect("proof keyframe должен моделировать уже отправленный decoder packet");
    for packet_index in 1..=3u64 {
        pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(packet_index * 10),
            generation,
            Bytes::from_static(b"old-interframe-backlog"),
            PacketKeyframe::NotKeyframe,
        ));
    }
    pipeline.mark_video_decoder_bootstrapped();
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::Started
    );

    for (pts, keyframe) in [
        (Duration::from_millis(80), PacketKeyframe::NotKeyframe),
        (Duration::from_millis(90), PacketKeyframe::Unknown),
    ] {
        assert_eq!(
            pipeline.route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
                selected_track_id,
                pts,
                generation,
                Bytes::from_static(b"staged-video-packet"),
                keyframe,
            )),
            VideoBacklogRecoveryRouteOutcome::StagedWhileScanning
        );
    }
    assert_eq!(pipeline.pending_video_packet_len(), 3);
    assert_eq!(pipeline.video_backlog_recovery_staged_packet_len(), 2);

    let recovery_keyframe_pts = Duration::from_millis(100);
    let recovery_keyframe_dts = Some(Duration::from_millis(95));
    let recovery_track_dts = Some(media_core::TrackTimestamp::new(
        selected_track_id,
        9_500,
        media_core::TimeBase::new(1, 100_000).expect("test time base должен быть валиден"),
    ));
    let recovery_payload = Bytes::from_static(b"recovery-keyframe");
    assert_eq!(
        pipeline.route_pending_video_packet_for_backlog_recovery(
            PendingVideoPacket::new_with_decode_timestamps(
                selected_track_id,
                PendingVideoPacketTimestamps {
                    pts: recovery_keyframe_pts,
                    dts: recovery_keyframe_dts,
                    track_pts: None,
                    track_dts: recovery_track_dts,
                },
                generation,
                recovery_payload.clone(),
                PacketKeyframe::Keyframe,
            )
        ),
        VideoBacklogRecoveryRouteOutcome::SwitchedAtKeyframe {
            discarded_pending_packets: 3,
            discarded_staged_packets: 2,
            recovery_keyframe_pts,
        }
    );
    assert!(!pipeline.video_backlog_recovery_scan_allows_demux());
    assert_eq!(pipeline.video_backlog_recovery_staged_packet_len(), 0);
    assert_eq!(pipeline.pending_video_packet_len(), 1);
    assert_eq!(pipeline.seek_generation(), generation);
    assert!(!pipeline.video_decoder_needs_keyframe());
    let queued_recovery_packet = pipeline
        .front_pending_video_packet()
        .expect("proven recovery keyframe must be queued atomically");
    assert_eq!(queued_recovery_packet.track_id, selected_track_id);
    assert_eq!(queued_recovery_packet.pts, recovery_keyframe_pts);
    assert_eq!(queued_recovery_packet.dts, recovery_keyframe_dts);
    assert_eq!(queued_recovery_packet.track_dts, recovery_track_dts);
    assert_eq!(queued_recovery_packet.generation, generation);
    assert_eq!(queued_recovery_packet.encoded_bytes, recovery_payload);
    assert_eq!(queued_recovery_packet.keyframe, PacketKeyframe::Keyframe);

    assert_eq!(
        pipeline.route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(110),
            generation,
            Bytes::from_static(b"post-recovery-interframe"),
            PacketKeyframe::NotKeyframe,
        )),
        VideoBacklogRecoveryRouteOutcome::QueuedNormally
    );
    assert_eq!(pipeline.pending_video_packet_len(), 2);
}

#[test]
fn video_backlog_recovery_scan_limit_restores_continuation_and_rearms_after_drain() {
    let mut pipeline = PlaybackPipeline::default();
    let selected_track_id = TrackId::new(10);
    let generation = pipeline.seek_generation();
    pipeline.select_video_track(
        selected_track_id,
        VideoDecodeRequirement::new(VideoCodec::Av1),
    );
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        selected_track_id,
        Duration::ZERO,
        generation,
        Bytes::from_static(b"already-sent-proof-keyframe"),
        PacketKeyframe::Keyframe,
    ));
    pipeline
        .pop_pending_video_packet_front()
        .expect("proof keyframe должен моделировать уже отправленный decoder packet");
    for pending_pts_ms in [10, 20] {
        pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(pending_pts_ms),
            generation,
            Bytes::from_static(b"old-runway-interframe"),
            PacketKeyframe::NotKeyframe,
        ));
    }
    pipeline.mark_video_decoder_bootstrapped();
    let scan_limits = VideoBacklogRecoveryScanLimits {
        max_staged_packets: 3,
        max_staged_bytes: usize::MAX,
        rearm_pending_packets: 1,
    };
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(scan_limits),
        VideoBacklogRecoveryScanStart::Started
    );

    for staged_pts_ms in [30, 40] {
        assert_eq!(
            pipeline.route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
                selected_track_id,
                Duration::from_millis(staged_pts_ms),
                generation,
                Bytes::from_static(b"bounded-staged-interframe"),
                PacketKeyframe::Unknown,
            )),
            VideoBacklogRecoveryRouteOutcome::StagedWhileScanning
        );
    }
    assert_eq!(
        pipeline.route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(50),
            generation,
            Bytes::from_static(b"scan-limit-interframe"),
            PacketKeyframe::NotKeyframe,
        )),
        VideoBacklogRecoveryRouteOutcome::ScanLimitReached {
            limit: VideoBacklogRecoveryScanLimit::PacketCount,
            restored_staged_packets: 3,
            restored_staged_bytes: 2 * b"bounded-staged-interframe".len()
                + b"scan-limit-interframe".len(),
            pending_packets_after_restore: 5,
        }
    );
    assert_eq!(pipeline.video_backlog_recovery_staged_packet_len(), 0);
    assert_eq!(pipeline.pending_video_packet_len(), 5);
    assert!(!pipeline.video_backlog_recovery_scan_allows_demux());
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(scan_limits),
        VideoBacklogRecoveryScanStart::BackoffUntilBacklogDrains
    );

    let restored_pts = (0..4)
        .map(|_| {
            pipeline
                .pop_pending_video_packet_front()
                .expect("restored continuation должен сохранить FIFO")
                .pts
        })
        .collect::<Vec<_>>();
    assert_eq!(
        restored_pts,
        vec![
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_millis(30),
            Duration::from_millis(40),
        ]
    );
    assert_eq!(pipeline.pending_video_packet_len(), 1);
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(scan_limits),
        VideoBacklogRecoveryScanStart::Started
    );
}

#[test]
fn video_backlog_recovery_byte_limit_restores_continuation_without_retained_payload() {
    let mut pipeline = PlaybackPipeline::default();
    let selected_track_id = TrackId::new(10);
    let generation = pipeline.seek_generation();
    pipeline.select_video_track(
        selected_track_id,
        VideoDecodeRequirement::new(VideoCodec::Av1),
    );
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        selected_track_id,
        Duration::ZERO,
        generation,
        Bytes::from_static(b"already-sent-proof-keyframe"),
        PacketKeyframe::Keyframe,
    ));
    pipeline
        .pop_pending_video_packet_front()
        .expect("proof keyframe должен моделировать уже отправленный decoder packet");
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        selected_track_id,
        Duration::from_millis(10),
        generation,
        Bytes::from_static(b"old-runway"),
        PacketKeyframe::NotKeyframe,
    ));
    pipeline.mark_video_decoder_bootstrapped();

    let first_staged_payload = Bytes::from_static(b"first");
    let byte_limit_payload = Bytes::from_static(b"second!");
    let scan_limits = VideoBacklogRecoveryScanLimits {
        max_staged_packets: 8,
        max_staged_bytes: first_staged_payload.len() + byte_limit_payload.len(),
        rearm_pending_packets: 1,
    };
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(scan_limits),
        VideoBacklogRecoveryScanStart::Started
    );
    assert_eq!(
        pipeline.route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(20),
            generation,
            first_staged_payload,
            PacketKeyframe::NotKeyframe,
        )),
        VideoBacklogRecoveryRouteOutcome::StagedWhileScanning
    );
    assert_eq!(pipeline.video_backlog_recovery_staged_bytes(), 5);
    assert_eq!(
        pipeline.route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(30),
            generation,
            byte_limit_payload,
            PacketKeyframe::NotKeyframe,
        )),
        VideoBacklogRecoveryRouteOutcome::ScanLimitReached {
            limit: VideoBacklogRecoveryScanLimit::ByteCount,
            restored_staged_packets: 2,
            restored_staged_bytes: 12,
            pending_packets_after_restore: 3,
        }
    );
    assert_eq!(pipeline.video_backlog_recovery_staged_packet_len(), 0);
    assert_eq!(pipeline.video_backlog_recovery_staged_bytes(), 0);

    let restored_payloads = (0..3)
        .map(|_| {
            pipeline
                .pop_pending_video_packet_front()
                .expect("byte-limit rollback должен сохранить полный FIFO")
                .encoded_bytes
        })
        .collect::<Vec<_>>();
    assert_eq!(
        restored_payloads,
        vec![
            Bytes::from_static(b"old-runway"),
            Bytes::from_static(b"first"),
            Bytes::from_static(b"second!"),
        ]
    );
}

#[test]
fn video_backlog_recovery_rejects_codecs_without_no_flush_recovery_proof() {
    for codec in [VideoCodec::H264, VideoCodec::H265, VideoCodec::Vp8] {
        let mut pipeline = PlaybackPipeline::default();
        let selected_track_id = TrackId::new(10);
        pipeline.select_video_track(selected_track_id, VideoDecodeRequirement::new(codec));
        pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
            selected_track_id,
            Duration::ZERO,
            pipeline.seek_generation(),
            Bytes::from_static(b"codec-keyframe-proof"),
            PacketKeyframe::Keyframe,
        ));
        pipeline.mark_video_decoder_bootstrapped();

        assert_eq!(
            pipeline.begin_video_backlog_recovery_scan(VideoBacklogRecoveryScanLimits::for_tests()),
            VideoBacklogRecoveryScanStart::CodecWithoutNoFlushRecoveryProof { codec }
        );
        assert!(!pipeline.video_backlog_recovery_scan_allows_demux());
        assert_eq!(pipeline.video_backlog_recovery_staged_packet_len(), 0);
        assert_eq!(pipeline.pending_video_packet_len(), 1);
    }
}

#[test]
fn video_backlog_recovery_scan_is_cancelled_by_queue_or_track_lifecycle_reset() {
    let mut pipeline = PlaybackPipeline::default();
    let selected_track_id = TrackId::new(10);
    let generation = pipeline.seek_generation();
    pipeline.select_video_track(
        selected_track_id,
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        selected_track_id,
        Duration::ZERO,
        generation,
        Bytes::from_static(b"proof-keyframe"),
        PacketKeyframe::Keyframe,
    ));
    pipeline.mark_video_decoder_bootstrapped();
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::Started
    );
    assert_eq!(
        pipeline.route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(1),
            generation,
            Bytes::from_static(b"staged-before-queue-clear"),
            PacketKeyframe::NotKeyframe,
        )),
        VideoBacklogRecoveryRouteOutcome::StagedWhileScanning
    );
    assert_eq!(pipeline.video_backlog_recovery_staged_packet_len(), 1);

    pipeline.clear_pending_video_packets();
    assert!(!pipeline.video_backlog_recovery_scan_allows_demux());
    assert_eq!(pipeline.video_backlog_recovery_staged_packet_len(), 0);
    assert_eq!(
        pipeline.route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(10),
            generation,
            Bytes::from_static(b"normally-queued-after-clear"),
            PacketKeyframe::Unknown,
        )),
        VideoBacklogRecoveryRouteOutcome::QueuedNormally
    );
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::Started
    );
    assert_eq!(
        pipeline.route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(11),
            generation,
            Bytes::from_static(b"staged-before-generation-change"),
            PacketKeyframe::NotKeyframe,
        )),
        VideoBacklogRecoveryRouteOutcome::StagedWhileScanning
    );

    pipeline.begin_seek_generation();
    assert!(!pipeline.video_backlog_recovery_scan_allows_demux());
    assert_eq!(pipeline.video_backlog_recovery_staged_packet_len(), 0);
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::Started
    );
    assert_eq!(
        pipeline.route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
            selected_track_id,
            Duration::from_millis(12),
            pipeline.seek_generation(),
            Bytes::from_static(b"staged-before-track-clear"),
            PacketKeyframe::NotKeyframe,
        )),
        VideoBacklogRecoveryRouteOutcome::StagedWhileScanning
    );

    pipeline.clear_selected_tracks();
    assert_eq!(pipeline.video_backlog_recovery_staged_packet_len(), 0);
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::NoSelectedVideo
    );
    pipeline.select_video_track(
        selected_track_id,
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    assert_eq!(
        pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        VideoBacklogRecoveryScanStart::NoProvenKeyframeObserved
    );
}
