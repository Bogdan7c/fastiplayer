// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! MKV с главами, записанный ffmpeg без необязательного `EditionUID` (RFC 9559).
//! Раньше MKV-патч требовал этот элемент, и такой файл не открывался вовсе
//! («mkv: missing edition uid»), хотя это обычный результат ремукса ffmpeg.

use std::path::{Path, PathBuf};

use media_core::{DemuxReadEvent, Demuxer, TrackKind};
use symphonia_demux::SymphoniaDemuxer;

/// Сколько кадров в фикстуре: 2 с × 10 кадров/с (см. `fixtures/matroska/README.md`).
const FIXTURE_VIDEO_FRAMES: usize = 20;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/matroska")
        .join(name)
}

#[test]
fn mkv_with_chapters_but_without_edition_uid_opens_and_plays_to_the_end() {
    let mut demuxer = SymphoniaDemuxer::from_file(&fixture("chapters-without-edition-uid.mkv"))
        .expect("MKV with chapters and without EditionUID must open");

    let video_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .map(|track| track.id)
        .expect("video track is published");

    let mut video_packets = 0;
    loop {
        match demuxer
            .next_event()
            .expect("packets must read without errors")
        {
            DemuxReadEvent::Packet(packet) if packet.track_id == video_track => video_packets += 1,
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) => panic!("local file cannot be not ready"),
            DemuxReadEvent::EndOfStream => break,
        }
    }

    assert_eq!(
        video_packets, FIXTURE_VIDEO_FRAMES,
        "all video frames are demuxed"
    );
}
