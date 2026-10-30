// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! `DecodePointBefore` в файле, где видео начинается позже звука (видео с 2 с, звук с 0).
//!
//! Раньше seek к цели раньше первого кадра (0 с, 1.2 с) отказывал с
//! `first_video_after_target`: keyframe-а ≤ цели нет, retry назад упирался в начало файла.
//! Теперь demux доказывает поздний старт видео чтением с начала и приземляется на начало
//! медиа (`actual_position` = 0 ≤ цели): звук идёт с начала (player-core обрезает его до цели),
//! видео — со своего первого keyframe-а. Seek после начала видео идёт прежним путём.

use std::path::{Path, PathBuf};
use std::time::Duration;

use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, PacketKeyframe, TrackKind};
use symphonia_demux::SymphoniaDemuxer;

/// Первый кадр видео в фикстуре (см. `fixtures/matroska/README.md`).
const VIDEO_START: Duration = Duration::from_secs(2);
/// Защитный предел чтения после seek-а.
const MAX_EVENTS_AFTER_SEEK: usize = 1_000;

fn open_fixture() -> SymphoniaDemuxer {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/matroska/h264-video-starts-at-2s.mkv");
    SymphoniaDemuxer::from_file(&path).expect("late-video fixture opens")
}

/// Что отдал demuxer после seek-а: первый video packet и звук до него.
struct DeliveredAfterSeek {
    first_video: Packet,
    audio_before_first_video: Vec<Packet>,
}

fn read_until_first_video(demuxer: &mut SymphoniaDemuxer) -> DeliveredAfterSeek {
    let mut audio_before_first_video = Vec::new();
    for _ in 0..MAX_EVENTS_AFTER_SEEK {
        match demuxer.next_event().expect("demux read after seek") {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                return DeliveredAfterSeek {
                    first_video: packet,
                    audio_before_first_video,
                };
            }
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Audio => {
                audio_before_first_video.push(packet);
            }
            DemuxReadEvent::EndOfStream => panic!("end of stream before the first video packet"),
            _ => {}
        }
    }
    panic!("no video packet within {MAX_EVENTS_AFTER_SEEK} events after seek")
}

/// Цель раньше начала видео: seek успешен, приземление в начале медиа, звук покрывает цель
/// без пропуска, видео начинается со своего первого keyframe-а.
#[test]
fn decode_point_before_target_earlier_than_video_start_lands_at_media_start() {
    let mut demuxer = open_fixture();

    // Сначала уходим вперёд, чтобы seek назад был настоящим переходом.
    demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            4,
        )))
        .expect("seek inside the video range");
    read_until_first_video(&mut demuxer);

    for target in [Duration::ZERO, Duration::from_millis(1_200)] {
        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(target))
            .unwrap_or_else(|error| panic!("DecodePointBefore({target:?}) failed: {error:#}"));

        assert_eq!(seek_result.requested_position.as_duration(), target);
        assert_eq!(
            seek_result.actual_position.as_duration(),
            Duration::ZERO,
            "late video start must land at media start (not after target)"
        );

        let delivered = read_until_first_video(&mut demuxer);
        assert_eq!(delivered.first_video.pts, VIDEO_START);
        assert_ne!(delivered.first_video.keyframe, PacketKeyframe::NotKeyframe);

        // Звук идёт с начала медиа и доходит до видео: цель покрыта, ничего не выброшено.
        let first_audio = delivered
            .audio_before_first_video
            .first()
            .expect("audio before the video start");
        assert_eq!(first_audio.pts, Duration::ZERO);
        let audio_covers_target = delivered.audio_before_first_video.iter().any(|packet| {
            let end = packet.pts + packet.duration.expect("aac packet duration");
            packet.pts <= target && target < end
        });
        assert!(
            audio_covers_target,
            "audio packet covering {target:?} was skipped"
        );
        let last_audio = delivered.audio_before_first_video.last().unwrap();
        assert!(
            VIDEO_START.saturating_sub(last_audio.pts) < Duration::from_millis(100),
            "audio before the first video frame stops at {:?}",
            last_audio.pts
        );
    }
}

/// Цель после начала видео идёт прежним путём: ближайший keyframe ≤ цели, а не начало медиа.
#[test]
fn decode_point_before_target_after_video_start_keeps_keyframe_landing() {
    let mut demuxer = open_fixture();
    let target = Duration::from_millis(3_200);

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .expect("seek inside the video range");
    let delivered = read_until_first_video(&mut demuxer);

    // GOP 5 при 10 к/с: keyframe-ы каждые 0.5 с, ближайший ≤ 3.2 с — 3.0 с.
    assert_eq!(delivered.first_video.pts, Duration::from_secs(3));
    assert_ne!(delivered.first_video.keyframe, PacketKeyframe::NotKeyframe);
    assert_eq!(
        seek_result.actual_position.as_duration(),
        Duration::from_secs(3)
    );
}
