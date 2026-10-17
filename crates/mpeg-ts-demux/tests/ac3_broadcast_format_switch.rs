//! Вещательная смена формата AC-3 посреди потока (2.0 → 5.1 → 2.0, реклама ↔
//! фильм) на настоящем ffmpeg-TS `tests/fixtures/ac3/ac3-broadcast-2p0-5p1-2p0-48k.ts`.
//!
//! Смена формата — дело decoder-а (он декодирует каждый кадр в его формате, а
//! player-core пересобирает output по decoded spec-у). Demuxer не должен
//! публиковать `TracksChanged`: в player-core это seek-подобный полный сброс
//! (видео замирает до IDR, выбор дорожки сбрасывается к default).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test helpers fail the test by panicking"
)]

use std::path::PathBuf;

use ac3_syncframe::{AccessUnitScan, StreamContinuation, scan_access_unit};
use demux_api::DemuxInput;
use media_core::{DemuxReadEvent, Demuxer, Packet, TrackKind};
use mpeg_ts_demux::{MpegTsDemuxOptions, MpegTsDemuxer};
use source_core::{CancellationToken, LocalFileSource};

const FIXTURE: &str = "ac3-broadcast-2p0-5p1-2p0-48k.ts";
/// Кадров в каждом из трёх участков (16 кадров stereo, 16 — 5.1, 16 — stereo).
const FRAMES_PER_SEGMENT: usize = 16;
/// 1536 сэмплов @ 48 кГц в тиках 90 кГц.
const FRAME_DURATION_90KHZ: i64 = 2_880;
/// Первый audio PTS по ffprobe (ffmpeg muxer сдвигает старт на 1.4 с).
const FIRST_AUDIO_PTS_90KHZ: i64 = 126_000;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ac3")
        .join(FIXTURE)
}

/// Все события до EOS (без самого EOS).
fn drain_events(demuxer: &mut MpegTsDemuxer) -> Vec<DemuxReadEvent> {
    let mut events = Vec::new();
    loop {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::EndOfStream => return events,
            event => events.push(event),
        }
    }
}

/// Число каналов презентации, доказанное заголовком самого пакета.
fn packet_channels(packet: &Packet) -> u32 {
    let scan = scan_access_unit(&packet.data, StreamContinuation::EndOfStream)
        .expect("packet is a syncframe");
    let AccessUnitScan::Complete(access_unit) = scan else {
        panic!("packet is not a complete access unit");
    };
    access_unit.presentation_channel_count()
}

fn assert_format_switch_stays_inside_one_track(mut demuxer: MpegTsDemuxer, input: &str) {
    let track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .cloned()
        .expect("audio track");
    // Дорожку описывает первый кадр, как container metadata у MKV/MP4.
    assert_eq!(track.codec_id, "A_AC3", "{input}");
    assert_eq!(
        (track.sample_rate, track.channels),
        (Some(48_000), Some(2)),
        "{input}"
    );

    let events = drain_events(&mut demuxer);
    let lifecycle_events = events
        .iter()
        .filter(|event| matches!(event, DemuxReadEvent::TracksChanged(_)))
        .count();
    assert_eq!(
        lifecycle_events, 0,
        "{input}: mid-stream 2.0/5.1 switch must not reset the player"
    );
    assert_eq!(demuxer.tracks(), [track.clone()].as_slice(), "{input}");

    let audio: Vec<&Packet> = events
        .iter()
        .filter_map(|event| match event {
            DemuxReadEvent::Packet(packet) if packet.track_id == track.id => Some(packet),
            _ => None,
        })
        .collect();
    let channels: Vec<u32> = audio.iter().map(|packet| packet_channels(packet)).collect();
    let expected_channels: Vec<u32> = [2, 6, 2]
        .into_iter()
        .flat_map(|count| std::iter::repeat_n(count, FRAMES_PER_SEGMENT))
        .collect();
    assert_eq!(
        channels, expected_channels,
        "{input}: every frame of every segment is published"
    );
    for (index, packet) in audio.iter().enumerate() {
        let pts = packet.track_pts.expect("track pts").units.get();
        assert_eq!(
            pts,
            FIRST_AUDIO_PTS_90KHZ + index as i64 * FRAME_DURATION_90KHZ,
            "{input}: PTS stays continuous across the switch (packet {index})"
        );
    }
}

#[test]
fn broadcast_channel_switch_publishes_no_track_change_in_byte_stream() {
    let bytes = std::fs::read(fixture_path()).expect("read fixture");
    let demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_stream(Box::new(std::io::Cursor::new(bytes))),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open byte stream");
    assert_format_switch_stays_inside_one_track(demuxer, "byte stream");
}

/// Seekable вход дополнительно строит index по всему файлу: и там смена
/// формата не должна менять дорожку.
#[test]
fn broadcast_channel_switch_publishes_no_track_change_in_local_file() {
    let source = LocalFileSource::open(fixture_path()).expect("open local fixture");
    let demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open local file");
    assert_format_switch_stays_inside_one_track(demuxer, "local file");
}
