// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Сквозной тест синхронизации A/V по спискам правок ISO BMFF (`elst`, §8.6.6):
//! реальный MP4 от ffmpeg с B-frame видео и аудио, стартующим позже → production
//! `SymphoniaDemuxer` → нейтральные media-core пакеты. Ожидание берётся не из нашего кода,
//! а из `ffprobe`: `start_time` видео 0.000000, аудио 0.228667 (см. README фикстуры).

use std::path::{Path, PathBuf};
use std::time::Duration;

use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, TrackKind};
use symphonia_demux::SymphoniaDemuxer;

/// Тиков аудио (48 кГц) в ведущей пустой правке: `elst = [(10976, -1), ...]`.
const AUDIO_DWELL_TICKS: i64 = 10_976;
/// Частота дискретизации аудио-дорожки (она же её media timescale).
const AUDIO_TIMESCALE: f64 = 48_000.0;
/// `ffprobe` `start_time` аудио минус `start_time` видео, секунды.
const FFPROBE_START_DIFFERENCE_SECONDS: f64 = 0.228_667;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/edit-list/audio-delayed-bframes.mp4")
}

fn read_all_packets(demuxer: &mut SymphoniaDemuxer) -> Vec<Packet> {
    let mut packets = Vec::new();
    loop {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::Packet(packet) => packets.push(packet),
            DemuxReadEvent::EndOfStream => return packets,
            DemuxReadEvent::TemporarilyUnavailable(_) => panic!("local file cannot be not ready"),
            _ => {}
        }
    }
}

fn first_pts(packets: &[Packet], kind: TrackKind) -> Duration {
    packets
        .iter()
        .filter(|packet| packet.kind == kind)
        .map(|packet| packet.pts)
        .min()
        .expect("fixture has packets of this kind")
}

#[test]
fn delayed_audio_starts_at_ffprobe_start_time_while_bframe_video_starts_at_zero() {
    let mut demuxer = SymphoniaDemuxer::from_file(&fixture()).expect("fixture must open");
    let packets = read_all_packets(&mut demuxer);

    let video_start = first_pts(&packets, TrackKind::Video);
    let audio_start = first_pts(&packets, TrackKind::Audio);

    // Видео: media CTS первого кадра сдвинут правкой `media_time`, первый показываемый кадр
    // в нуле; ни один кадр не показывается раньше нуля.
    assert_eq!(video_start, Duration::ZERO, "first presented video frame");
    let video_raw_pts: Vec<i64> = packets
        .iter()
        .filter(|packet| packet.kind == TrackKind::Video)
        .map(|packet| packet.track_pts.expect("raw pts").units.get())
        .collect();
    assert!(
        video_raw_pts.iter().all(|pts| *pts >= 0),
        "{video_raw_pts:?}"
    );

    // Аудио: ровно 10976 тиков пустой правки в исходных единицах дорожки.
    let audio_raw_start = packets
        .iter()
        .filter(|packet| packet.kind == TrackKind::Audio)
        .map(|packet| packet.track_pts.expect("raw pts").units.get())
        .min()
        .expect("audio packets");
    assert_eq!(audio_raw_start, AUDIO_DWELL_TICKS);

    // Разница старта A/V совпадает с ffprobe с точностью до одного тика аудио.
    let measured_difference = audio_start.as_secs_f64() - video_start.as_secs_f64();
    assert!(
        (measured_difference - FFPROBE_START_DIFFERENCE_SECONDS).abs() <= 1.0 / AUDIO_TIMESCALE,
        "A/V start difference {measured_difference} vs ffprobe {FFPROBE_START_DIFFERENCE_SECONDS}"
    );
}

#[test]
fn seek_into_audio_dwell_resumes_audio_at_its_first_sample_not_before() {
    let mut demuxer = SymphoniaDemuxer::from_file(&fixture()).expect("fixture must open");
    demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_millis(100)))
        .expect("seek inside dwell");
    let packets = read_all_packets(&mut demuxer);

    let audio_start = first_pts(&packets, TrackKind::Audio);
    assert!(
        (audio_start.as_secs_f64() - 0.228_667).abs() <= 1.0 / AUDIO_TIMESCALE,
        "audio after seek into the dwell must resume at its first sample: {audio_start:?}"
    );
}
