// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Сквозной тест: HEVC-в-MP4 от x265, у которого `hvcC` больше 1 КиБ (SEI со строкой
//! настроек энкодера), открывается production `SymphoniaDemuxer`, отдаёт декодеру полную
//! запись с VPS/SPS/PPS и все кадры. До исправления лимита в isomp4-патче такой файл
//! не открывался вовсе (найдено квалификацией cpu-parity, сессия 06).

use std::path::{Path, PathBuf};
use std::time::Duration;

use codec_core::{VideoCodec, parse_hevc_decoder_configuration_record};
use media_core::{DemuxReadEvent, Demuxer, Packet, PacketKeyframe, TrackKind};
use symphonia_demux::SymphoniaDemuxer;

/// Кадров в фикстуре (`ffprobe -count_frames`).
const FIXTURE_FRAMES: usize = 6;
/// PTS ключевых кадров при keyint 3 и 24 к/с.
const KEYFRAME_PTS: [Duration; 2] = [Duration::ZERO, Duration::from_millis(125)];

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hevc-x265/x265-large-hvcc.mp4")
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

#[test]
fn x265_mp4_with_large_hvcc_opens_and_hands_complete_parameter_sets_to_the_decoder() {
    let mut demuxer = SymphoniaDemuxer::from_file(&fixture()).expect("x265 HEVC MP4 must open");

    let track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .cloned()
        .expect("video track");
    assert_eq!(
        VideoCodec::from_container_codec_id(&track.codec_id),
        Some(VideoCodec::H265)
    );
    let codec_private = track.codec_private.as_deref().expect("hvcC codec private");
    assert!(
        codec_private.len() > 1024,
        "fixture must exceed the old 1 KiB limit, got {} bytes",
        codec_private.len()
    );
    let record = parse_hevc_decoder_configuration_record(codec_private).expect("complete hvcC");
    assert_eq!(record.video_parameter_sets().len(), 1);
    assert_eq!(record.sequence_parameter_sets().len(), 1);
    assert_eq!(record.picture_parameter_sets().len(), 1);

    let packets: Vec<Packet> = read_all_packets(&mut demuxer)
        .into_iter()
        .filter(|packet| packet.track_id == track.id)
        .collect();
    assert_eq!(packets.len(), FIXTURE_FRAMES);
    assert!(packets.iter().all(|packet| !packet.data.is_empty()));
    let mut keyframes: Vec<Duration> = packets
        .iter()
        .filter(|packet| packet.keyframe == PacketKeyframe::Keyframe)
        .map(|packet| packet.pts)
        .collect();
    keyframes.sort();
    assert_eq!(keyframes, KEYFRAME_PTS);
}
