use std::path::Path;
use std::time::Duration;

use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, PacketKeyframe};

use super::{FirstVideoScan, proven_late_video_start};
use crate::symphonia_demuxer::SymphoniaDemuxer;
use crate::symphonia_demuxer::decode_point_before::DecodePointBeforeVideoPacket;

/// Цель seek-а в тестах классификации.
const TARGET: Duration = Duration::from_secs(1);

fn video_packet(keyframe: PacketKeyframe) -> DecodePointBeforeVideoPacket {
    DecodePointBeforeVideoPacket {
        pts: Duration::from_secs(2),
        track_pts: None,
        keyframe,
    }
}

#[test]
fn keyframe_or_unknown_after_target_proves_late_video_start() {
    for keyframe in [PacketKeyframe::Keyframe, PacketKeyframe::Unknown] {
        let packet = video_packet(keyframe);

        assert_eq!(
            proven_late_video_start(TARGET, FirstVideoScan::Found(packet)),
            Some(packet)
        );
    }
}

#[test]
fn non_keyframe_first_frame_is_not_a_decode_start() {
    let scan = FirstVideoScan::Found(video_packet(PacketKeyframe::NotKeyframe));

    assert_eq!(proven_late_video_start(TARGET, scan), None);
}

#[test]
fn video_at_or_before_target_is_not_a_late_start() {
    let packet = video_packet(PacketKeyframe::Keyframe);

    assert_eq!(
        proven_late_video_start(packet.pts, FirstVideoScan::Found(packet)),
        None
    );
}

#[test]
fn scans_without_a_first_video_frame_prove_nothing() {
    for scan in [
        FirstVideoScan::BudgetExhausted { packets_scanned: 7 },
        FirstVideoScan::EndOfStream { packets_scanned: 3 },
        FirstVideoScan::TrackLayoutChanged,
    ] {
        assert_eq!(proven_late_video_start(TARGET, scan), None);
    }
}

/// Доказательство не держит звуковой prefix в памяти: после принятого seek-а в очереди
/// demuxer-а нет ни одного packet-а (раньше там лежал весь звук от 0 до первого кадра —
/// ~1500 packet-ов для этой фикстуры), а звук отдаётся обычным чтением с начала.
#[test]
fn accepted_late_video_start_does_not_retain_the_scanned_prefix() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/matroska/h264-video-starts-at-30s.mkv");
    let mut demuxer = SymphoniaDemuxer::from_file(&fixture).expect("fixture opens");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            5,
        )))
        .expect("late video start is proven");

    assert_eq!(seek_result.actual_position.as_duration(), Duration::ZERO);
    assert!(
        demuxer
            .pending_events
            .iter()
            .all(|event| !matches!(event, DemuxReadEvent::Packet(_))),
        "{} events are buffered after the proof",
        demuxer.pending_events.len()
    );
    let Ok(DemuxReadEvent::Packet(first_packet)) = demuxer.next_event() else {
        panic!("reader restarted at media start delivers a packet");
    };
    assert_eq!(first_packet.pts, Duration::ZERO);
}
