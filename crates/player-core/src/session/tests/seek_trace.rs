use super::*;

#[test]
fn seek_trace_limits_packet_logs_but_keeps_first_video_marker() {
    let mut seek_trace = SeekTraceState::default();
    assert_eq!(seek_trace.record_post_seek_packet(TrackKind::Audio), None);

    seek_trace.begin(7);
    for packet_index in 1..=POST_SEEK_PACKET_TRACE_LIMIT {
        assert_eq!(
            seek_trace.record_post_seek_packet(TrackKind::Audio),
            Some(PostSeekPacketTraceDecision {
                packet_index,
                first_video_packet: false,
            })
        );
    }

    assert_eq!(seek_trace.record_post_seek_packet(TrackKind::Audio), None);
    assert_eq!(
        seek_trace.record_post_seek_packet(TrackKind::Video),
        Some(PostSeekPacketTraceDecision {
            packet_index: POST_SEEK_PACKET_TRACE_LIMIT + 2,
            first_video_packet: true,
        })
    );
    assert_eq!(seek_trace.record_post_seek_packet(TrackKind::Video), None);
}

#[test]
fn seek_trace_frame_and_track_markers_are_one_shot_per_generation() {
    let mut seek_trace = SeekTraceState::default();
    assert!(!seek_trace.record_first_decoded_frame());
    assert!(!seek_trace.record_first_queued_frame());
    assert!(!seek_trace.record_first_presented_frame(Duration::from_secs(1)));
    assert!(!seek_trace.record_first_track_list_update());

    seek_trace.begin(3);
    assert!(seek_trace.record_first_decoded_frame());
    assert!(!seek_trace.record_first_decoded_frame());
    assert!(seek_trace.record_first_queued_frame());
    assert!(!seek_trace.record_first_queued_frame());
    assert!(seek_trace.record_first_presented_frame(Duration::from_secs(1)));
    assert!(!seek_trace.record_first_presented_frame(Duration::from_secs(2)));
    assert_eq!(
        seek_trace.first_presented_frame_position_for_generation(3),
        Some(Duration::from_secs(1))
    );
    assert!(seek_trace.record_first_track_list_update());
    assert!(!seek_trace.record_first_track_list_update());

    seek_trace.begin(4);
    assert!(seek_trace.record_first_decoded_frame());
    assert!(seek_trace.record_first_queued_frame());
    assert!(seek_trace.record_first_presented_frame(Duration::from_secs(3)));
    assert_eq!(
        seek_trace.first_presented_frame_position_for_generation(3),
        None
    );
    assert_eq!(
        seek_trace.first_presented_frame_position_for_generation(4),
        Some(Duration::from_secs(3))
    );
    assert!(seek_trace.record_first_track_list_update());

    seek_trace.clear();
    assert!(!seek_trace.record_first_decoded_frame());
    assert!(!seek_trace.record_first_queued_frame());
}

#[test]
fn seek_trace_records_accurate_preroll_stage_timings_and_counters() {
    let mut seek_trace = SeekTraceState::default();

    seek_trace.record_accurate_preroll_demux_packet(
        TrackKind::Audio,
        false,
        Duration::from_millis(1),
    );
    assert!(!seek_trace.accurate_preroll_snapshot(true).active);

    seek_trace.begin(9);
    seek_trace.record_accurate_preroll_demux_packet(
        TrackKind::Audio,
        false,
        Duration::from_millis(2),
    );
    seek_trace.record_accurate_preroll_demux_packet(
        TrackKind::Video,
        true,
        Duration::from_millis(3),
    );
    seek_trace.record_accurate_preroll_demux_event(AccuratePrerollDemuxEventKind::TracksChanged);
    seek_trace.record_skipped_audio_preroll_packet();
    seek_trace.record_video_preroll_packet_sent();
    seek_trace.record_target_or_after_video_packet_sent();
    seek_trace.record_decoded_pre_target_frame_dropped();
    seek_trace.record_decoder_backpressure_pause();
    seek_trace.record_accurate_preroll_decoded_frame(false, Duration::from_millis(4));
    seek_trace.record_accurate_preroll_decoded_frame(true, Duration::from_millis(5));
    seek_trace.record_accurate_preroll_queued_frame(true, Duration::from_millis(6));
    seek_trace.record_accurate_preroll_presented_frame(true, Duration::from_millis(7));

    let diagnostics = seek_trace.accurate_preroll_snapshot(true);
    assert!(diagnostics.active);
    assert_eq!(
        diagnostics.stages.first_post_seek_packet_elapsed,
        Some(Duration::from_millis(2))
    );
    assert_eq!(
        diagnostics
            .stages
            .first_target_or_after_video_packet_elapsed,
        Some(Duration::from_millis(3))
    );
    assert_eq!(
        diagnostics.stages.first_decoded_target_frame_elapsed,
        Some(Duration::from_millis(5))
    );
    assert_eq!(
        diagnostics.stages.first_queued_target_frame_elapsed,
        Some(Duration::from_millis(6))
    );
    assert_eq!(
        diagnostics.stages.first_presented_target_frame_elapsed,
        Some(Duration::from_millis(7))
    );
    assert_eq!(diagnostics.counters.demux_events.audio_packets, 1);
    assert_eq!(diagnostics.counters.demux_events.video_packets, 1);
    assert_eq!(diagnostics.counters.demux_events.tracks_changed, 1);
    assert_eq!(diagnostics.counters.skipped_audio_preroll_packets, 1);
    assert_eq!(diagnostics.counters.seek_video_packets_sent, 2);
    assert_eq!(diagnostics.counters.video_preroll_packets_sent, 1);
    assert_eq!(diagnostics.counters.target_or_after_video_packets_sent, 1);
    assert_eq!(diagnostics.counters.decoded_pre_target_frames_dropped, 1);
    assert_eq!(diagnostics.counters.decoder_backpressure_pauses, 1);
    assert!(!seek_trace.accurate_preroll_snapshot(false).active);

    seek_trace.clear();
    assert!(!seek_trace.accurate_preroll_snapshot(true).active);
}
