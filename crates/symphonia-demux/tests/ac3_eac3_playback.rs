// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Сквозной путь AC-3 / E-AC-3 в MKV и MP4/M4A: файл → production
//! `SymphoniaDemuxer` (патченные mkv/isomp4) → метаданные дорожки (`A_AC3`/`A_EAC3`,
//! частота, каналы, метки) → пакеты → production `audio::create_audio_decoder`
//! → PCM, где в каждой lane звучит свой тон. Плюс seek в середину и декод после него.
//!
//! Фикстуры — `tests/fixtures/ac3/` (README и `generate.sh`): H.264 64×64 + 5.1(side),
//! в каждом канале свой синус, поэтому энергия тонов по lanes доказывает и
//! декодирование, и раскладку каналов.

use std::f32::consts::PI;
use std::path::{Path, PathBuf};
use std::time::Duration;

use audio::{AudioChannelPosition, AudioDecoderConfig, EncodedAudioPacket, create_audio_decoder};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, TrackInfo, TrackKind, TrackLanguage,
};
use symphonia_demux::SymphoniaDemuxer;

/// Частота дискретизации всех фикстур.
const SAMPLE_RATE: u32 = 48_000;
/// Samples на канал в одном AC-3/E-AC-3 пакете (6 блоков × 256).
const SAMPLES_PER_PACKET: usize = 1_536;
/// Пакетов звука в каждой фикстуре (1 с ≈ 31.25 кадра → ffmpeg пишет 32).
const FIXTURE_PACKETS: usize = 32;
/// Длительность каждой фикстуры (см. README фикстур).
const FIXTURE_DURATION: Duration = Duration::from_secs(1);
/// Тон каждого канала 5.1(side) — см. README фикстур.
const SURROUND_TONES: [(AudioChannelPosition, f32); 6] = [
    (AudioChannelPosition::FrontLeft, 300.0),
    (AudioChannelPosition::FrontRight, 700.0),
    (AudioChannelPosition::FrontCenter, 500.0),
    (AudioChannelPosition::LowFrequencyEffects, 60.0),
    (AudioChannelPosition::SideLeft, 900.0),
    (AudioChannelPosition::SideRight, 1_100.0),
];
/// Цель seek-а — середина файла; ближайший keyframe видео (GOP 5 при 10 к/с) — 0.5 с.
const MIDDLE_SEEK_TARGET: Duration = Duration::from_millis(600);
/// Самая ранняя допустимая посадка звука: MKV садится на начало cluster-а
/// с keyframe-ом 0.5 с, где звук interleaved немного раньше видео. Откат к
/// началу файла (0 с) этот порог не пропускает.
const EARLIEST_SEEK_LANDING: Duration = Duration::from_millis(400);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ac3")
        .join(name)
}

fn open(name: &str) -> SymphoniaDemuxer {
    SymphoniaDemuxer::from_file(&fixture(name)).expect("AC-3 fixture must open")
}

fn audio_track(demuxer: &SymphoniaDemuxer) -> TrackInfo {
    demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .cloned()
        .expect("fixture publishes an audio track")
}

/// Читает пакеты выбранной дорожки до конца файла.
fn drain_audio_packets(demuxer: &mut SymphoniaDemuxer, track: &TrackInfo) -> Vec<Packet> {
    let mut packets = Vec::new();
    loop {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::Packet(packet) if packet.track_id == track.id => packets.push(packet),
            DemuxReadEvent::EndOfStream => return packets,
            DemuxReadEvent::TemporarilyUnavailable(_) => panic!("local file cannot be not ready"),
            _ => {}
        }
    }
}

/// Декодирует пакеты production-декодером, созданным так же, как это делает player-core.
fn decode(track: &TrackInfo, packets: &[Packet]) -> (Vec<f32>, Vec<AudioChannelPosition>) {
    let config = AudioDecoderConfig::from_track_metadata(
        track.id.get(),
        track.codec_id.clone(),
        track.sample_rate,
        track.channels,
    )
    .with_codec_private(track.codec_private.as_ref().map(|bytes| bytes.to_vec()));
    let mut decoder = create_audio_decoder(config).expect("production AC-3 family decoder");
    let mut interleaved = Vec::new();
    for packet in packets {
        let samples = decoder
            .decode(&EncodedAudioPacket::without_timing(
                track.id.get(),
                &packet.data,
            ))
            .expect("container packet must decode");
        interleaved.extend_from_slice(&samples);
    }
    let spec = decoder.decoded_output_spec().expect("decoded spec");
    assert_eq!(spec.sample_rate, SAMPLE_RATE);
    let layout = spec.channel_layout;
    let positions = (0..layout.channel_count() as usize)
        .map(|lane| layout.position_at(lane).expect("positioned layout"))
        .collect();
    (interleaved, positions)
}

/// Энергия тона в одной lane (Гёрцель, нормированный на квадрат длины).
fn tone_energy(interleaved: &[f32], channels: usize, lane: usize, frequency: f32) -> f32 {
    let coefficient = 2.0 * (2.0 * PI * frequency / SAMPLE_RATE as f32).cos();
    let (mut previous, mut before_previous, mut count) = (0.0_f32, 0.0_f32, 0_usize);
    for sample in interleaved.iter().skip(lane).step_by(channels) {
        let current = sample + coefficient * previous - before_previous;
        before_previous = previous;
        previous = current;
        count += 1;
    }
    let power = previous * previous + before_previous * before_previous
        - coefficient * previous * before_previous;
    power / (count as f32 * count as f32)
}

/// В lane каждой позиции доминирует её тон: чужие тоны слабее минимум на 20 дБ.
fn assert_surround_tones(interleaved: &[f32], positions: &[AudioChannelPosition]) {
    assert_eq!(positions.len(), SURROUND_TONES.len(), "{positions:?}");
    for (position, tone) in SURROUND_TONES {
        let lane = positions
            .iter()
            .position(|candidate| *candidate == position)
            .unwrap_or_else(|| panic!("decoded layout lacks {position:?}: {positions:?}"));
        let own = tone_energy(interleaved, positions.len(), lane, tone);
        assert!(
            own > 1e-4,
            "{position:?}: tone {tone} Hz is missing (energy {own})"
        );
        for (_, other_tone) in SURROUND_TONES
            .iter()
            .filter(|(other, _)| *other != position)
        {
            let foreign = tone_energy(interleaved, positions.len(), lane, *other_tone);
            assert!(
                own > foreign * 100.0,
                "{position:?}: {other_tone} Hz leaks into the {tone} Hz lane"
            );
        }
    }
}

/// Общая проверка метаданных: codec id, частота, каналы, видео рядом.
fn assert_surround_track(demuxer: &SymphoniaDemuxer, expected_codec_id: &str) -> TrackInfo {
    let track = audio_track(demuxer);
    assert_eq!(track.codec_id, expected_codec_id);
    assert_eq!(
        (track.sample_rate, track.channels),
        (Some(SAMPLE_RATE), Some(6))
    );
    track
}

/// Полный проход файла: все пакеты → PCM нужной длины с тоном в каждой lane.
fn assert_full_playback(name: &str, expected_codec_id: &str) -> TrackInfo {
    let mut demuxer = open(name);
    let track = assert_surround_track(&demuxer, expected_codec_id);
    let packets = drain_audio_packets(&mut demuxer, &track);
    assert_eq!(packets.len(), FIXTURE_PACKETS, "{name}: every syncframe");
    let (pcm, positions) = decode(&track, &packets);
    assert_eq!(pcm.len(), FIXTURE_PACKETS * SAMPLES_PER_PACKET * 6);
    assert_surround_tones(&pcm, &positions);
    track
}

/// Seek в середину: звук садится не позже цели и не откатывается к началу, декод цел.
fn assert_middle_seek_playback(name: &str) {
    let mut demuxer = open(name);
    let track = audio_track(&demuxer);
    demuxer
        .seek_with_request(DemuxSeekRequest::accurate(MIDDLE_SEEK_TARGET))
        .expect("seek to the middle");
    let packets = drain_audio_packets(&mut demuxer, &track);
    let first_pts = packets.first().expect("audio after seek").pts;
    assert!(
        (EARLIEST_SEEK_LANDING..=MIDDLE_SEEK_TARGET).contains(&first_pts),
        "{name}: audio landed at {first_pts:?} for target {MIDDLE_SEEK_TARGET:?}"
    );
    assert!(
        packets.len() <= FIXTURE_PACKETS / 2,
        "{name}: seek must skip the first half, got {} packets",
        packets.len()
    );
    assert_audio_continues_to_the_end(name, &packets);
    let (pcm, positions) = decode(&track, &packets);
    assert_eq!(pcm.len(), packets.len() * SAMPLES_PER_PACKET * 6);
    assert_surround_tones(&pcm, &positions);
}

/// После seek-а звук идёт без дыр до конца файла. Раньше MKV-патч после cue-перехода давал
/// кластеру timestamp ≈ 0, и из второй половины файла доходили лишь 3 пакета — подсчёт
/// «меньше половины» этого не видел.
fn assert_audio_continues_to_the_end(name: &str, packets: &[Packet]) {
    let packet_spacing =
        Duration::from_secs_f64(SAMPLES_PER_PACKET as f64 / f64::from(SAMPLE_RATE));
    for pair in packets.windows(2) {
        let spacing = pair[1].pts.saturating_sub(pair[0].pts);
        assert!(
            spacing.abs_diff(packet_spacing) <= Duration::from_millis(1),
            "{name}: audio gap after seek between {:?} and {:?}",
            pair[0].pts,
            pair[1].pts
        );
    }
    let last_pts = packets.last().expect("audio after seek").pts;
    assert!(
        last_pts + packet_spacing * 2 >= FIXTURE_DURATION,
        "{name}: audio after seek stops at {last_pts:?}, before the end of the file"
    );
}

fn assert_has_video(name: &str) {
    let demuxer = open(name);
    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Video && track.codec_id == "V_MPEG4/ISO/AVC"),
        "{name}: H.264 video stays next to Dolby audio: {:?}",
        demuxer.tracks()
    );
}

#[test]
fn mkv_ac3_surround_plays_every_channel_with_matroska_labels() {
    let track = assert_full_playback("h264-ac3-5p1.mkv", "A_AC3");
    assert_eq!(track.labels.language, Some(TrackLanguage::new("deu")));
    assert_eq!(track.labels.title.as_deref(), Some("Surround"));
    assert_has_video("h264-ac3-5p1.mkv");
}

#[test]
fn mkv_eac3_surround_plays_every_channel_with_matroska_labels() {
    let track = assert_full_playback("h264-eac3-5p1.mkv", "A_EAC3");
    assert_eq!(track.labels.language, Some(TrackLanguage::new("deu")));
    assert_eq!(track.labels.title.as_deref(), Some("Surround"));
    assert_has_video("h264-eac3-5p1.mkv");
}

#[test]
fn mp4_ac3_surround_plays_every_channel_with_language() {
    let track = assert_full_playback("h264-ac3-5p1.mp4", "A_AC3");
    assert_eq!(track.labels.language, Some(TrackLanguage::new("deu")));
    assert_has_video("h264-ac3-5p1.mp4");
}

#[test]
fn mp4_eac3_surround_plays_every_channel_with_language() {
    let track = assert_full_playback("h264-eac3-5p1.mp4", "A_EAC3");
    assert_eq!(track.labels.language, Some(TrackLanguage::new("deu")));
    assert_has_video("h264-eac3-5p1.mp4");
}

#[test]
fn audio_only_m4a_with_ec3_sample_entry_plays_every_channel() {
    let demuxer = open("audio-only-eac3-5p1.m4a");
    assert_eq!(demuxer.tracks().len(), 1, "audio-only file: one track");
    assert_full_playback("audio-only-eac3-5p1.m4a", "A_EAC3");
}

#[test]
fn fragmented_mp4_eac3_plays_across_fragments() {
    assert_full_playback("h264-eac3-5p1-fragmented.mp4", "A_EAC3");
}

#[test]
fn mkv_middle_seek_then_decode_keeps_every_channel() {
    assert_middle_seek_playback("h264-ac3-5p1.mkv");
    assert_middle_seek_playback("h264-eac3-5p1.mkv");
}

#[test]
fn mp4_middle_seek_then_decode_keeps_every_channel() {
    assert_middle_seek_playback("h264-ac3-5p1.mp4");
    assert_middle_seek_playback("h264-eac3-5p1.mp4");
    assert_middle_seek_playback("audio-only-eac3-5p1.m4a");
}
