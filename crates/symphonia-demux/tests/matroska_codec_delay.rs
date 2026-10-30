// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! `TrackEntry/CodecDelay` вычитается из pts каждого блока (RFC 9559: «MUST be subtracted from
//! each Block timestamp»). Раньше MKV-патч его прибавлял: pts звука в MKV/WebM были на
//! 2·CodecDelay позже ffmpeg (13 мс у Opus, ~10 мс у AC-3), звук отставал от картинки.
//!
//! Эталон — ffprobe по тем же файлам (первый пакет звука, первый блок в кластере 0 с):
//! - `matroska/opus-audio-only-beeps.mka`: CodecDelay 6.5 мс → ffprobe −0.007 с;
//! - `ac3/h264-ac3-5p1.mkv`: CodecDelay 5.333 мс → ffprobe −0.005 с.
//!
//! Track ticks здесь — 1 мс (TimestampScale 1 мс), а патч переводит CodecDelay в тики
//! отбрасыванием дробной части: −6 мс и −5 мс. ffprobe округляет −6.5 до −7; расхождение
//! меньше одного тика контейнера и не является дефектом.

use std::path::{Path, PathBuf};

use media_core::{DemuxReadEvent, Demuxer, Packet, TrackKind};
use symphonia_demux::SymphoniaDemuxer;

fn fixture(relative_path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative_path)
}

/// Первый packet звука при чтении файла с начала.
fn first_audio_packet(relative_path: &str) -> Packet {
    let mut demuxer = SymphoniaDemuxer::from_file(&fixture(relative_path)).expect("fixture opens");
    loop {
        match demuxer.next_event().expect("demux read") {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Audio => return packet,
            DemuxReadEvent::EndOfStream => panic!("{relative_path}: no audio packet"),
            _ => {}
        }
    }
}

/// Signed pts первого пакета звука в миллисекундах (track ticks фикстур — 1 мс).
fn first_audio_track_pts_ms(relative_path: &str) -> i64 {
    let track_pts = first_audio_packet(relative_path)
        .track_pts
        .expect("matroska packet keeps signed track pts");
    assert_eq!(
        (track_pts.time_base.numer, track_pts.time_base.denom),
        (1, 1_000),
        "{relative_path}: fixture track ticks are milliseconds"
    );
    track_pts.units.get()
}

#[test]
fn opus_first_block_is_shifted_back_by_codec_delay() {
    assert_eq!(
        first_audio_track_pts_ms("matroska/opus-audio-only-beeps.mka"),
        -6
    );
}

#[test]
fn ac3_first_block_is_shifted_back_by_codec_delay() {
    assert_eq!(first_audio_track_pts_ms("ac3/h264-ac3-5p1.mkv"), -5);
}
