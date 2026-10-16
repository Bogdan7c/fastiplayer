// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! AAC-LC 5.1 с Program Config Element (`channel_configuration = 0` в ASC), как пишет ffmpeg
//! для 5.1(side): MKV (`tests/fixtures/subtitles/aac-pce-5p1.mkv`) должен не только открыться,
//! но и дойти до PCM — interleaved 6 каналов с тоном 880 Гц только в FC.

use std::path::{Path, PathBuf};

use audio::{AudioDecoderConfig, EncodedAudioPacket, create_audio_decoder};
use media_core::{DemuxReadEvent, Demuxer, TrackKind};
use symphonia_demux::SymphoniaDemuxer;

const CHANNELS: usize = 6;
/// Порядок interleaved lanes: FL, FR, FC, LFE, RL, RR.
const FRONT_CENTER_LANE: usize = 2;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/subtitles/aac-pce-5p1.mkv")
}

fn rms(samples: impl Iterator<Item = f32>) -> f64 {
    let (sum, count) = samples.fold((0.0_f64, 0_usize), |(sum, count), s| {
        (sum + f64::from(s).powi(2), count + 1)
    });
    (sum / count as f64).sqrt()
}

#[test]
fn pce_aac_mkv_decodes_to_six_channel_pcm_with_tone_on_front_center() {
    let mut demuxer = SymphoniaDemuxer::from_file(&fixture()).expect("fixture must open");
    let track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .expect("audio track")
        .clone();
    let codec_private = track.codec_private.as_ref().map(|bytes| bytes.to_vec());
    assert_eq!(
        codec_private.as_deref().and_then(|bytes| bytes.get(..2)),
        Some(&[0x11, 0x80][..]),
        "extradata must be the PCE form (channelConfiguration = 0)"
    );

    let config = AudioDecoderConfig::from_track_metadata(
        1,
        track.codec_id.clone(),
        track.sample_rate,
        track.channels,
    )
    .with_codec_private(codec_private);
    let mut decoder = create_audio_decoder(config).expect("decoder must be created");

    let mut pcm: Vec<f32> = Vec::new();
    loop {
        match demuxer.next_event().expect("read event") {
            DemuxReadEvent::Packet(packet) if packet.track_id == track.id => {
                let encoded = EncodedAudioPacket::without_timing(1, &packet.data);
                pcm.extend(
                    decoder
                        .decode(&encoded)
                        .expect("PCE AAC packet must decode"),
                );
            }
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::TemporarilyUnavailable(_) => panic!("local file cannot be not ready"),
            _ => {}
        }
    }

    assert_eq!((decoder.sample_rate(), decoder.channels()), (48_000, 6));
    assert!(
        pcm.len() >= 40_000 * CHANNELS,
        "about 1 s of PCM, got {} samples",
        pcm.len()
    );
    let center = rms(pcm
        .iter()
        .skip(FRONT_CENTER_LANE)
        .step_by(CHANNELS)
        .copied());
    assert!(center > 0.05, "FC lane must carry the tone, rms={center}");
    for lane in (0..CHANNELS).filter(|lane| *lane != FRONT_CENTER_LANE) {
        let level = rms(pcm.iter().skip(lane).step_by(CHANNELS).copied());
        assert!(level < 0.005, "lane {lane} must be silent, rms={level}");
    }
}
