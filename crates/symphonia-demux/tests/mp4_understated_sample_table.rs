// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Живая находка сессии 09c2 плана `user/browser-bridge/`: у части рекламных роликов длительность
//! дорожки в `mdhd` чуть больше суммы сэмплов в `stts`. Обычный, нефрагментированный MP4 такого не
//! прощал: разборщик считал, что «есть продолжение», при перемотке уходил искать следующие
//! сегменты до конца файла и терял текущий `mdat`. Следующий пакет падал с
//! `isomp4: no atom pending read`, и плеер навсегда вставал на старте ролика.
//!
//! Файл настоящий: фикстура ffmpeg, в которой правится только поле длительности `mdhd`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use media_core::{DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, Demuxer, TrackKind};
use symphonia_demux::SymphoniaDemuxer;

/// Настолько длительность `mdhd` превышает сумму сэмплов, тиков media timescale.
const OVERSTATED_DURATION_TICKS: u32 = 4_096;
/// Смещение версии от начала атома: размер и тип.
const MDHD_VERSION_OFFSET: usize = 8;
/// Смещение длительности от начала атома версии 0: версия с флагами, две даты, timescale.
const MDHD_V0_DURATION_OFFSET: usize = 24;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/edit-list/audio-delayed-bframes.mp4")
}

/// Все вхождения атома `mdhd`: фикстура мала, поиск по типу надёжнее ручного обхода дерева.
fn mdhd_offsets(bytes: &[u8]) -> Vec<usize> {
    bytes
        .windows(4)
        .enumerate()
        .filter(|(_, window)| *window == b"mdhd")
        .map(|(type_position, _)| type_position - 4)
        .collect()
}

/// Копия фикстуры, где `mdhd` каждой дорожки объявляет длительность больше суммы сэмплов.
fn write_understated_copy(name: &str) -> PathBuf {
    let mut bytes = fs::read(fixture()).expect("fixture must be readable");
    let offsets = mdhd_offsets(&bytes);
    assert!(!offsets.is_empty(), "fixture must have mdhd atoms");
    for offset in offsets {
        assert_eq!(
            bytes[offset + MDHD_VERSION_OFFSET],
            0,
            "fixture uses version 0 mdhd"
        );
        let field = offset + MDHD_V0_DURATION_OFFSET;
        let duration = u32::from_be_bytes(bytes[field..field + 4].try_into().unwrap());
        let overstated = duration + OVERSTATED_DURATION_TICKS;
        bytes[field..field + 4].copy_from_slice(&overstated.to_be_bytes());
    }
    let path = std::env::temp_dir().join(format!("fastiplayer-{}-{name}.mp4", std::process::id()));
    fs::write(&path, bytes).expect("temporary copy must be writable");
    path
}

fn first_packet_after_seek(demuxer: &mut SymphoniaDemuxer, kind: TrackKind) -> Duration {
    loop {
        match demuxer
            .next_event()
            .expect("packet after seek must be readable")
        {
            DemuxReadEvent::Packet(packet) if packet.kind == kind => return packet.pts,
            DemuxReadEvent::EndOfStream => panic!("stream ended before a packet of the track"),
            _ => {}
        }
    }
}

#[test]
fn seek_keeps_media_data_readable_when_mdhd_overstates_the_sample_table() {
    let path = write_understated_copy("seek");
    let mut demuxer = SymphoniaDemuxer::from_file(&path).expect("copy must open");
    let seek = demuxer.seek_with_request(DemuxSeekRequest {
        timestamp: Duration::ZERO,
        mode: DemuxSeekMode::DecodePointBefore,
    });
    let video_start = seek.map(|_| first_packet_after_seek(&mut demuxer, TrackKind::Video));
    fs::remove_file(&path).expect("temporary copy must be removable");
    assert!(
        video_start.expect("seek to the start must succeed") < Duration::from_secs(1),
        "the file must play from its start"
    );
}

#[test]
fn playback_still_reaches_the_end_of_such_a_file() {
    let path = write_understated_copy("whole");
    let mut demuxer = SymphoniaDemuxer::from_file(&path).expect("copy must open");
    let mut packets = 0_usize;
    let finished = loop {
        match demuxer.next_event() {
            Ok(DemuxReadEvent::Packet(_)) => packets += 1,
            Ok(DemuxReadEvent::EndOfStream) => break true,
            Ok(_) => {}
            Err(_) => break false,
        }
    };
    fs::remove_file(&path).expect("temporary copy must be removable");
    assert!(
        finished,
        "reading to the end must end with EndOfStream, not an error"
    );
    assert!(packets > 0);
}
