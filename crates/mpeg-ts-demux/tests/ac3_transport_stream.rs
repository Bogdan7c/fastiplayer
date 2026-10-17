//! Настоящие MPEG-TS от ffmpeg с AC-3/E-AC-3 (`tests/fixtures/ac3`, см. README.md):
//! дорожка находится, пакеты — целые syncframe-ы, PTS идут с шагом кадра,
//! seek по локальному файлу не ломает нарезку аудио.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test helpers fail the test by panicking"
)]

use std::path::PathBuf;
use std::time::Duration;

use ac3_syncframe::{AccessUnitScan, StreamContinuation, SyncframeCodec, scan_access_unit};
use demux_api::DemuxInput;
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, TrackInfo, TrackKind};
use mpeg_ts_demux::{MpegTsDemuxOptions, MpegTsDemuxer};
use source_core::{CancellationToken, LocalFileSource};

/// Свойства фикстуры, снятые `ffprobe` (см. README.md).
struct TransportFixture {
    file: &'static str,
    codec_id: &'static str,
    codec: SyncframeCodec,
    sample_rate: u32,
    channels: u32,
    language: Option<&'static str>,
    has_video: bool,
    audio_packets: usize,
    /// Возможные длины syncframe-а (у 44.1 кГц чередуются из-за padding-слова).
    frame_lengths: &'static [usize],
    /// Длительность кадра 1536 сэмплов в тиках 90 кГц (с отбрасыванием дроби).
    frame_duration_90khz: i64,
}

const FIXTURES: [TransportFixture; 4] = [
    TransportFixture {
        file: "h264-ac3-stereo-48k.ts",
        codec_id: "A_AC3",
        codec: SyncframeCodec::Ac3,
        sample_rate: 48_000,
        channels: 2,
        language: None,
        has_video: true,
        audio_packets: 16,
        frame_lengths: &[384],
        frame_duration_90khz: 2_880,
    },
    TransportFixture {
        file: "h264-eac3-5p1-44k1.ts",
        codec_id: "A_EAC3",
        codec: SyncframeCodec::EnhancedAc3,
        sample_rate: 44_100,
        channels: 6,
        language: None,
        has_video: true,
        audio_packets: 15,
        frame_lengths: &[834, 836],
        frame_duration_90khz: 3_134,
    },
    TransportFixture {
        file: "audio-only-ac3-dvb-mono.ts",
        codec_id: "A_AC3",
        codec: SyncframeCodec::Ac3,
        sample_rate: 48_000,
        channels: 1,
        language: Some("rus"),
        has_video: false,
        audio_packets: 16,
        frame_lengths: &[256],
        frame_duration_90khz: 2_880,
    },
    TransportFixture {
        file: "audio-only-eac3-dvb-stereo-32k.ts",
        codec_id: "A_EAC3",
        codec: SyncframeCodec::EnhancedAc3,
        sample_rate: 32_000,
        channels: 2,
        language: Some("eng"),
        has_video: false,
        audio_packets: 11,
        frame_lengths: &[576],
        frame_duration_90khz: 4_320,
    },
];

/// Первый audio PTS у всех фикстур по ffprobe (ffmpeg muxer сдвигает старт на 1.4 с).
const FIRST_AUDIO_PTS_90KHZ: i64 = 126_000;

fn fixture_path(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ac3")
        .join(file)
}

fn open_stream(file: &str) -> MpegTsDemuxer {
    let bytes = std::fs::read(fixture_path(file)).expect("read fixture");
    MpegTsDemuxer::open(
        DemuxInput::byte_stream(Box::new(std::io::Cursor::new(bytes))),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open AC-3 transport stream")
}

fn audio_track(tracks: &[TrackInfo]) -> &TrackInfo {
    tracks
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .expect("audio track")
}

fn drain_packets(demuxer: &mut MpegTsDemuxer) -> Vec<Packet> {
    let mut packets = Vec::new();
    loop {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::Packet(packet) => packets.push(packet),
            DemuxReadEvent::EndOfStream => return packets,
            _ => {}
        }
    }
}

fn pts_90khz(packet: &Packet) -> i64 {
    packet
        .track_pts
        .expect("MPEG-TS packets carry track timestamps")
        .units
        .get()
}

/// Пакет — ровно один целый syncframe, свойства которого совпадают с дорожкой.
fn assert_whole_syncframe(packet: &Packet, fixture: &TransportFixture) {
    let scan = scan_access_unit(&packet.data, StreamContinuation::EndOfStream)
        .unwrap_or_else(|error| panic!("{}: packet is not a syncframe: {error}", fixture.file));
    let AccessUnitScan::Complete(access_unit) = scan else {
        panic!("{}: packet is not a complete access unit", fixture.file);
    };
    assert_eq!(access_unit.total_length_bytes(), packet.data.len());
    assert_eq!(access_unit.syncframe_count(), 1);
    assert_eq!(access_unit.codec(), fixture.codec);
    assert_eq!(
        access_unit.primary_header().sample_rate_hz(),
        fixture.sample_rate
    );
    assert!(
        fixture.frame_lengths.contains(&packet.data.len()),
        "{}: unexpected frame length {}",
        fixture.file,
        packet.data.len()
    );
}

#[test]
fn ffmpeg_transport_streams_expose_ac3_tracks_with_header_evidence() {
    for fixture in &FIXTURES {
        let demuxer = open_stream(fixture.file);
        let tracks = demuxer.tracks();
        let audio = audio_track(tracks);
        assert_eq!(audio.codec_id, fixture.codec_id, "{}", fixture.file);
        assert_eq!(
            audio.sample_rate,
            Some(fixture.sample_rate),
            "{}",
            fixture.file
        );
        assert_eq!(audio.channels, Some(fixture.channels), "{}", fixture.file);
        assert_eq!(
            audio
                .labels
                .language
                .as_ref()
                .map(|language| language.as_str()),
            fixture.language,
            "{}",
            fixture.file
        );
        let video_tracks = tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .count();
        assert_eq!(
            video_tracks,
            usize::from(fixture.has_video),
            "{}",
            fixture.file
        );
    }
}

#[test]
fn audio_packets_are_whole_syncframes_with_frame_spaced_monotonic_pts() {
    for fixture in &FIXTURES {
        let mut demuxer = open_stream(fixture.file);
        let audio_track_id = audio_track(demuxer.tracks()).id;
        let packets = drain_packets(&mut demuxer);
        let audio: Vec<&Packet> = packets
            .iter()
            .filter(|packet| packet.track_id == audio_track_id)
            .collect();
        assert_eq!(audio.len(), fixture.audio_packets, "{}", fixture.file);
        assert_eq!(
            pts_90khz(audio[0]),
            FIRST_AUDIO_PTS_90KHZ,
            "{}",
            fixture.file
        );
        for packet in &audio {
            assert_whole_syncframe(packet, fixture);
            let duration = packet.track_duration.expect("audio packet duration");
            assert_eq!(duration.units.get() as i64, fixture.frame_duration_90khz);
        }
        for pair in audio.windows(2) {
            let step = pts_90khz(pair[1]) - pts_90khz(pair[0]);
            // Внутри PES шаг ровно длительность кадра; на границе PES берётся PES
            // PTS, который у 44.1 кГц отличается от суммы усечённых длительностей
            // не больше чем на пару тиков.
            assert!(
                (step - fixture.frame_duration_90khz).abs() <= 2,
                "{}: PTS step {step}",
                fixture.file
            );
        }
        if fixture.has_video {
            assert!(
                packets.iter().any(|packet| packet.kind == TrackKind::Video),
                "{}: video packets must still flow next to AC-3",
                fixture.file
            );
        }
    }
}

/// Seek в локальном файле сбрасывает буфер PID-а; новый поток аудио обязан
/// снова синхронизироваться и выдавать только целые syncframe-ы.
///
/// В 0.5-секундной фикстуре единственный decode point не позже цели — keyframe
/// 1.4 с (PCR-якоря раньше по времени), поэтому точный seek садится на него и
/// обязан вернуть ВСЕ кадры заново: без дублей, обрывков и потерь. Разрез кадра
/// границей PES после seek-а проверяют ручные потоки `src/tests/ac3_elementary.rs`.
#[test]
fn seek_in_local_file_resynchronizes_audio_on_syncframe_boundaries() {
    let fixture = &FIXTURES[1];
    let source = LocalFileSource::open(fixture_path(fixture.file)).expect("open local fixture");
    let mut demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open seekable fixture");
    let audio_track_id = audio_track(demuxer.tracks()).id;
    let audio_before_seek: Vec<Packet> = drain_packets(&mut demuxer)
        .into_iter()
        .filter(|packet| packet.track_id == audio_track_id)
        .collect();
    // Seekable вход играет с начала: probe/index scan не съедают первые кадры.
    assert_eq!(audio_before_seek.len(), fixture.audio_packets);
    assert_eq!(pts_90khz(&audio_before_seek[0]), FIRST_AUDIO_PTS_90KHZ);

    let target = Duration::from_millis(1_650);
    let result = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(target))
        .expect("seek inside fixture");
    assert!(result.actual_position.as_duration() <= target);
    let audio_after_seek: Vec<Packet> = drain_packets(&mut demuxer)
        .into_iter()
        .filter(|packet| packet.track_id == audio_track_id)
        .collect();
    assert_eq!(
        audio_after_seek
            .iter()
            .map(|packet| (packet.pts, packet.data.len()))
            .collect::<Vec<_>>(),
        audio_before_seek
            .iter()
            .map(|packet| (packet.pts, packet.data.len()))
            .collect::<Vec<_>>(),
        "seek to the only decode point replays every syncframe exactly once"
    );
    for packet in &audio_after_seek {
        assert_whole_syncframe(packet, fixture);
    }
}
