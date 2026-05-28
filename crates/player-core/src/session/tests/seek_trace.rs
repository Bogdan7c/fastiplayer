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
