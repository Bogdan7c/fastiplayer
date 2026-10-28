// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Сквозной тест `DecodePointBefore` на MP4 с B-frames: реальный файл ffmpeg (IDR каждые 2 с,
//! `-bf 2`) -> production `SymphoniaDemuxer::seek_with_request` -> первый video packet.
//!
//! Регрессия: isomp4 ищет sync-sample по DTS, а PTS keyframe-а на ~67 мс больше его DTS.
//! Цель чуть ДО keyframe-а (например 3.95 с при keyframe 4.0 с) выбирала этот keyframe, он
//! оказывался после цели, и retry прыгал на целый `preroll` (5 с) назад: seek приземлялся на
//! 3-4 GOP раньше цели вместо ближайшего keyframe <= цели.

use std::path::{Path, PathBuf};
use std::time::Duration;

use media_core::{
    DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, Demuxer, PacketKeyframe, TrackKind,
};
use symphonia_demux::SymphoniaDemuxer;

/// Шаг GOP фикстуры: IDR ровно каждые 2.000 с (см. `fixtures/seek-gop/generate.sh`).
const KEYFRAME_INTERVAL_MS: u64 = 2_000;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/seek-gop/gop2s-bframes.mp4")
}

/// Первый video packet после seek-а: его PTS и keyframe-классификация.
fn first_video_packet_after_seek(demuxer: &mut SymphoniaDemuxer) -> (Duration, PacketKeyframe) {
    loop {
        match demuxer.next_event().expect("demux event after seek") {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                return (packet.pts, packet.keyframe);
            }
            DemuxReadEvent::EndOfStream => panic!("EOF before any video packet after seek"),
            DemuxReadEvent::TemporarilyUnavailable(_) => panic!("local file cannot be not ready"),
            _ => {}
        }
    }
}

#[test]
fn decode_point_before_lands_on_nearest_keyframe_at_or_before_target() {
    // (цель, мс) -> ближайший keyframe <= цели, мс. Включает цели в окне [DTS, PTS) перед
    // keyframe-ом (3950, 9950, 13950 — аналоги живых 63.955 / 21.959 / 53.999 с), ровно на
    // keyframe-е, сразу после него и в середине GOP.
    let cases: [(u64, u64); 10] = [
        (1_999, 0),
        (2_000, 2_000),
        (2_001, 2_000),
        (3_000, 2_000),
        (3_950, 2_000),
        (3_999, 2_000),
        (9_950, 8_000),
        (13_950, 12_000),
        (17_990, 16_000),
        (19_000, 18_000),
    ];

    for (target_ms, expected_keyframe_ms) in cases {
        assert!(
            expected_keyframe_ms % KEYFRAME_INTERVAL_MS == 0
                && expected_keyframe_ms <= target_ms
                && target_ms - expected_keyframe_ms < KEYFRAME_INTERVAL_MS,
            "bad test case {target_ms}"
        );
        let mut demuxer = SymphoniaDemuxer::from_file(&fixture()).expect("fixture must open");
        let result = demuxer
            .seek_with_request(DemuxSeekRequest {
                timestamp: Duration::from_millis(target_ms),
                mode: DemuxSeekMode::DecodePointBefore,
            })
            .expect("DecodePointBefore seek must succeed");
        let (pts, keyframe) = first_video_packet_after_seek(&mut demuxer);

        assert_eq!(
            pts,
            Duration::from_millis(expected_keyframe_ms),
            "target {target_ms} ms: first video packet must be the nearest keyframe <= target"
        );
        assert_eq!(
            keyframe,
            PacketKeyframe::Keyframe,
            "target {target_ms} ms: decode start must be a keyframe"
        );
        assert_eq!(
            result.actual_position.as_duration(),
            pts,
            "target {target_ms} ms: reported landing must equal the first video packet"
        );
    }
}
