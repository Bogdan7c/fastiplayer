// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Фикстура с двумя аудиодорожками (`tests/fixtures/subtitles/two-audio-tracks.mkv`):
//! демуксер обязан опубликовать обе дорожки с разными кодеками/параметрами/метками
//! и отдавать пакеты каждой — на этом держится переключение аудиодорожки в `player-core`.

use std::path::{Path, PathBuf};

use media_core::{DemuxReadEvent, Demuxer, TrackKind, TrackLanguage};
use symphonia_demux::SymphoniaDemuxer;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/subtitles/two-audio-tracks.mkv")
}

#[test]
fn mkv_publishes_two_audio_tracks_with_distinct_codecs_params_and_labels() {
    let demuxer = SymphoniaDemuxer::from_file(&fixture()).expect("fixture must open");
    let audio: Vec<_> = demuxer
        .tracks()
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .collect();
    assert_eq!(audio.len(), 2, "fixture has exactly two audio tracks");

    assert_ne!(audio[0].id, audio[1].id);
    assert_ne!(audio[0].codec_id, audio[1].codec_id, "different codecs");
    assert_eq!(
        (audio[0].sample_rate, audio[0].channels),
        (Some(22_050), Some(2))
    );
    assert_eq!(
        (audio[1].sample_rate, audio[1].channels),
        (Some(48_000), Some(1))
    );
    assert_eq!(audio[0].labels.language, Some(TrackLanguage::new("rus")));
    assert_eq!(audio[0].labels.title.as_deref(), Some("Dub"));
    assert_eq!(audio[1].labels.language, Some(TrackLanguage::new("eng")));
    assert_eq!(audio[1].labels.title.as_deref(), Some("Original"));
}

#[test]
fn mkv_delivers_packets_of_both_audio_tracks() {
    let mut demuxer = SymphoniaDemuxer::from_file(&fixture()).expect("fixture must open");
    let audio_ids: Vec<_> = demuxer
        .tracks()
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .map(|track| track.id)
        .collect();
    let mut packet_counts = [0_usize; 2];
    loop {
        match demuxer.next_event().expect("read event") {
            DemuxReadEvent::Packet(packet) => {
                if let Some(index) = audio_ids.iter().position(|id| *id == packet.track_id) {
                    packet_counts[index] += 1;
                }
            }
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::TemporarilyUnavailable(_) => panic!("local file cannot be not ready"),
            DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
        }
    }
    assert!(packet_counts[0] > 10, "FLAC packets: {}", packet_counts[0]);
    assert!(packet_counts[1] > 10, "Opus packets: {}", packet_counts[1]);
}
