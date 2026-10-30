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
//!
//! Вторая фикстура (видео с 30 с, ~1500 пакетов звука до него) ловила предел доказательства:
//! оно шло общим циклом проверки с лимитом 1024 пакета и копило весь звуковой prefix в памяти.
//! Теперь доказательство только считает пакеты (свой бюджет в `DemuxerOptions`) и после него
//! перечитывает источник с начала заново.

use std::path::{Path, PathBuf};
use std::time::Duration;

use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, PacketKeyframe, TrackKind};
use symphonia_demux::{DemuxerOptions, SymphoniaDemuxer};

/// Первый кадр видео в фикстуре (см. `fixtures/matroska/README.md`).
const VIDEO_START: Duration = Duration::from_secs(2);
/// Первый кадр видео в фикстуре с далёким поздним стартом.
const FAR_VIDEO_START: Duration = Duration::from_secs(30);
/// Фикстура: видео с 2 с.
const NEAR_LATE_VIDEO_FIXTURE: &str = "h264-video-starts-at-2s.mkv";
/// Фикстура: видео с 30 с, до него ~1500 пакетов Opus по 20 мс (больше прежнего лимита 1024).
const FAR_LATE_VIDEO_FIXTURE: &str = "h264-video-starts-at-30s.mkv";
/// Защитный предел чтения после seek-а (покрывает 30 с звука до первого кадра).
const MAX_EVENTS_AFTER_SEEK: usize = 4_000;

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/matroska")
        .join(name)
}

fn open_fixture() -> SymphoniaDemuxer {
    SymphoniaDemuxer::from_file(&fixture_path(NEAR_LATE_VIDEO_FIXTURE))
        .expect("late-video fixture opens")
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

        read_until_first_video(&mut demuxer).assert_media_start_up_to_video(target, VIDEO_START);
    }
}

impl DeliveredAfterSeek {
    /// Звук идёт с начала медиа и без пропусков доходит до первого кадра видео (keyframe-а).
    fn assert_media_start_up_to_video(&self, target: Duration, video_start: Duration) {
        assert_eq!(self.first_video.pts, video_start);
        assert_ne!(self.first_video.keyframe, PacketKeyframe::NotKeyframe);

        let first_audio = self
            .audio_before_first_video
            .first()
            .expect("audio before the video start");
        assert_eq!(first_audio.pts, Duration::ZERO);
        let audio_covers_target = self.audio_before_first_video.iter().any(|packet| {
            let end = packet.pts + packet.duration.expect("audio packet duration");
            packet.pts <= target && target < end
        });
        assert!(
            audio_covers_target,
            "audio packet covering {target:?} was skipped"
        );
        // Звук подряд: каждый пакет начинается там, где кончился предыдущий (±2 мс: pts и
        // длительность пакета в MKV округлены до тика 1 мс каждая).
        // Сравниваем signed pts: первый пакет из-за CodecDelay лежит чуть раньше нуля.
        for pair in self.audio_before_first_video.windows(2) {
            let previous_end = signed_pts_seconds(&pair[0])
                + pair[0]
                    .duration
                    .expect("audio packet duration")
                    .as_secs_f64();
            assert!(
                (signed_pts_seconds(&pair[1]) - previous_end).abs() <= 0.002,
                "audio gap between {:?} and {:?}",
                pair[0].pts,
                pair[1].pts
            );
        }
        let last_audio = self.audio_before_first_video.last().unwrap();
        assert!(
            video_start.saturating_sub(last_audio.pts) < Duration::from_millis(100),
            "audio before the first video frame stops at {:?}",
            last_audio.pts
        );
    }
}

/// Signed pts packet-а в секундах (public `Packet::pts` клампится в 0).
fn signed_pts_seconds(packet: &Packet) -> f64 {
    let track_pts = packet.track_pts.expect("matroska packet keeps signed pts");
    track_pts.units.get() as f64 * f64::from(track_pts.time_base.numer)
        / f64::from(track_pts.time_base.denom)
}

/// Видео начинается через 30 с после звука (больше 1024 пакетов звука до первого кадра):
/// seek к цели до начала видео всё равно доказывает поздний старт и приземляется в начало
/// медиа, а звук отдаётся целиком с начала.
#[test]
fn decode_point_before_far_before_late_video_start_lands_at_media_start() {
    let mut demuxer = SymphoniaDemuxer::from_file(&fixture_path(FAR_LATE_VIDEO_FIXTURE))
        .expect("far late-video fixture opens");

    for target in [Duration::from_secs(5), Duration::from_millis(25_500)] {
        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(target))
            .unwrap_or_else(|error| panic!("DecodePointBefore({target:?}) failed: {error:#}"));

        assert_eq!(seek_result.actual_position.as_duration(), Duration::ZERO);
        read_until_first_video(&mut demuxer)
            .assert_media_start_up_to_video(target, FAR_VIDEO_START);
    }
}

/// Доказательство ограничено своим бюджетом пакетов: при бюджете меньше звукового prefix-а
/// demux не сканирует дальше, а честно возвращает прежнюю ошибку `DecodePointBefore`.
#[test]
fn late_video_start_proof_stops_at_its_packet_budget() {
    let options = DemuxerOptions::default()
        .with_late_video_start_proof_packet_limit(500)
        .expect("non-zero proof budget");
    let mut demuxer =
        SymphoniaDemuxer::from_file_with_options(&fixture_path(FAR_LATE_VIDEO_FIXTURE), options)
            .expect("far late-video fixture opens");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            5,
        )))
        .expect_err("proof budget is shorter than the audio prefix");

    assert!(
        format!("{error:#}").contains("first_video_after_target"),
        "unexpected error: {error:#}"
    );
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
