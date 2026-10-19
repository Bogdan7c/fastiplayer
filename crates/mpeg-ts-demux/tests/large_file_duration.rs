//! Сквозная проверка: локальный TS больше окна начального индекса (~6.16 МБ)
//! открывается с известной длительностью и допускает точный seek.
//!
//! Раньше такой файл открывался без длительности: player-core не строил таймлайн,
//! UI показывал `--:--` и отказывал в seek-е. Файл генерируется программно
//! (валидные PAT/PMT с CRC, H.264-подобные keyframe AU), внешних утилит не нужно.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test helpers fail the test by panicking"
)]

use std::time::Duration;

use demux_api::DemuxInput;
use media_core::{DemuxReadEvent, DemuxSeekRequest, DemuxSeekability, Demuxer};
use mpeg_ts_demux::{MpegTsDemuxOptions, MpegTsDemuxer};
use source_core::{CancellationToken, LocalFileSource};

const PMT_PID: u16 = 0x0100;
const VIDEO_PID: u16 = 0x0101;
/// Старт PTS записи эфира (1.4 с).
const START_TICKS: u64 = 126_000;
/// Кадр (keyframe) каждые 0.5 с.
const FRAME_STEP_TICKS: u64 = 45_000;
/// Кадров: 120 × ~60 КБ = ~7.2 МБ > окна индекса по умолчанию (32768 пакетов).
const FRAMES: u64 = 120;
const FILLER_BYTES: usize = 60_000;
/// Нижняя граница размера, при которой окно по умолчанию не доходит до EOF.
const DEFAULT_INDEX_WINDOW_BYTES: usize = 32_768 * 188;

fn crc32_mpeg(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn timestamp_field(prefix: u8, value: u64) -> [u8; 5] {
    let value = value & ((1 << 33) - 1);
    [
        (prefix << 4) | (((value >> 30) as u8 & 0x07) << 1) | 1,
        (value >> 22) as u8,
        (((value >> 15) as u8 & 0x7f) << 1) | 1,
        (value >> 7) as u8,
        ((value as u8 & 0x7f) << 1) | 1,
    ]
}

/// Складывает payload в 188-байтные пакеты PID-а (последний добивается adaptation stuffing-ом).
fn push_packets(out: &mut Vec<u8>, counter: &mut u8, pid: u16, payload: &[u8]) {
    for (index, chunk) in payload.chunks(184).enumerate() {
        let mut packet = [0xff_u8; 188];
        packet[0] = 0x47;
        packet[1] = (u8::from(index == 0) << 6) | (pid >> 8) as u8;
        packet[2] = pid as u8;
        if chunk.len() == 184 {
            packet[3] = 0x10 | *counter;
            packet[4..].copy_from_slice(chunk);
        } else {
            let adaptation = 183 - chunk.len();
            packet[3] = 0x30 | *counter;
            packet[4] = adaptation as u8;
            if adaptation > 0 {
                packet[5] = 0;
            }
            packet[5 + adaptation..5 + adaptation + chunk.len()].copy_from_slice(chunk);
        }
        *counter = (*counter + 1) & 0x0f;
        out.extend_from_slice(&packet);
    }
}

fn psi_payload(mut section: Vec<u8>) -> Vec<u8> {
    let length = section.len() - 3 + 4;
    section[1] = 0xb0 | ((length >> 8) as u8 & 0x0f);
    section[2] = length as u8;
    let crc = crc32_mpeg(&section);
    section.extend_from_slice(&crc.to_be_bytes());
    let mut payload = vec![0];
    payload.extend(section);
    payload
}

fn long_video_recording() -> Vec<u8> {
    let mut out = Vec::new();
    let (mut pat_counter, mut pmt_counter, mut video_counter) = (0, 0, 0);
    let pat = vec![
        0x00,
        0xb0,
        0,
        0x00,
        0x01,
        0xc1,
        0,
        0,
        0x00,
        0x01,
        0xe0 | (PMT_PID >> 8) as u8,
        PMT_PID as u8,
    ];
    push_packets(&mut out, &mut pat_counter, 0, &psi_payload(pat));
    let pmt = vec![
        0x02,
        0xb0,
        0,
        0x00,
        0x01,
        0xc1,
        0,
        0,
        0xe0 | (VIDEO_PID >> 8) as u8,
        VIDEO_PID as u8,
        0xf0,
        0x00,
        0x1b,
        0xe0 | (VIDEO_PID >> 8) as u8,
        VIDEO_PID as u8,
        0xf0,
        0x00,
    ];
    push_packets(&mut out, &mut pmt_counter, PMT_PID, &psi_payload(pmt));
    let mut access_unit = vec![
        0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00, 0x00, 0x01,
        0x65, 0x88,
    ];
    access_unit.extend(std::iter::repeat_n(0x11_u8, FILLER_BYTES));
    for index in 0..FRAMES {
        let mut pes = vec![0x00, 0x00, 0x01, 0xe0, 0, 0, 0x80, 0x80, 5];
        pes.extend_from_slice(&timestamp_field(
            0b0010,
            START_TICKS + index * FRAME_STEP_TICKS,
        ));
        pes.extend_from_slice(&access_unit);
        let packet_length = pes.len() - 6;
        pes[4..6].copy_from_slice(&(packet_length as u16).to_be_bytes());
        push_packets(&mut out, &mut video_counter, VIDEO_PID, &pes);
    }
    out
}

fn ticks(value: u64) -> Duration {
    Duration::from_nanos(value * 1_000_000_000 / 90_000)
}

#[test]
fn local_ts_larger_than_index_window_opens_seekable_and_seeks_accurately() {
    let bytes = long_video_recording();
    assert!(
        bytes.len() > DEFAULT_INDEX_WINDOW_BYTES,
        "фикстура должна не влезать в окно индекса по умолчанию"
    );
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("long-recording.ts");
    std::fs::write(&path, &bytes).expect("write fixture");
    let source = LocalFileSource::open(&path).expect("open local file");

    let mut demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open long TS");

    assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
    assert_eq!(
        demuxer.duration(),
        Some(ticks(START_TICKS + (FRAMES - 1) * FRAME_STEP_TICKS)),
        "длительность известна при open и равна последнему PTS"
    );

    let landed = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(30)))
        .expect("accurate seek to 30 s")
        .actual_position
        .as_duration();
    assert!(
        landed <= Duration::from_secs(30) && landed >= Duration::from_millis(29_000),
        "приземлились у 30 с: {landed:?}"
    );
    let first_after_seek = loop {
        match demuxer.next_event().expect("event after seek") {
            DemuxReadEvent::Packet(packet) => break packet,
            DemuxReadEvent::EndOfStream => panic!("после seek нет пакетов"),
            _ => {}
        }
    };
    assert_eq!(first_after_seek.pts, landed);
    assert!(first_after_seek.keyframe.is_known_keyframe());
}
