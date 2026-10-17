//! Склеенные `cat`-ом TS-записи через production путь: local registry → MPEG-TS
//! demuxer → production AC-3 decoder, один decoder на весь поток (как в
//! player-core, когда `TracksChanged` нет).
//!
//! Доказывает «мягкий» стык: ни одного сброса дорожек (иначе player-core
//! пересобрал бы pipeline — потеря 0,2–0,4 с звука и ожидание keyframe), звук
//! без потерь и дыр на одной шкале, тон первой записи сменяется тоном второй
//! ровно на стыке.
//!
//! Фикстуры: `crates/mpeg-ts-demux/tests/fixtures/concatenated/` (README там же).

use audio::AudioChannelPosition;
use media_core::{DemuxReadEvent, Packet, TrackKind};

use super::ac3_elementary_tests::open_local;
use crate::ac3_test_support::{
    DecodedPcm, SAMPLES_PER_ACCESS_UNIT, decode_track_packets, first_audio_track, tone_energy,
};

const PART_A: &[u8] =
    include_bytes!("../../../mpeg-ts-demux/tests/fixtures/concatenated/part-a-h264-ac3-440hz.ts");
const PART_B: &[u8] =
    include_bytes!("../../../mpeg-ts-demux/tests/fixtures/concatenated/part-b-h264-ac3-700hz.ts");
/// AC-3 кадров в каждой записи (0.8 с @ 48 кГц).
const FRAMES_PER_PART: usize = 25;
/// Видеокадров в каждой записи (0.8 с @ 10 к/с).
const VIDEO_FRAMES_PER_PART: usize = 8;
/// Длительность AC-3 кадра в тиках 90 кГц.
const FRAME_90KHZ: i64 = 2_880;
const TONE_A_HZ: f32 = 440.0;
const TONE_B_HZ: f32 = 700.0;
/// Тон записи сильнее тона другой записи минимум в 100 раз (20 дБ).
const FOREIGN_TONE_REJECTION: f32 = 100.0;
/// Минимальная средняя мощность кадра: тишина/выпавший кадр дали бы ~0
/// (синус ffmpeg с амплитудой 1/8 даёт ~0.008).
const MINIMUM_FRAME_POWER: f32 = 1e-4;

fn pts_90khz(packet: &Packet) -> i64 {
    packet.track_pts.expect("track pts").units.get()
}

/// Кадры `range` (по 1536 сэмплов на канал) декодированного потока.
fn frames(decoded: &DecodedPcm, range: std::ops::Range<usize>) -> DecodedPcm {
    let samples_per_frame = SAMPLES_PER_ACCESS_UNIT * decoded.positions.len();
    DecodedPcm {
        interleaved: decoded.interleaved
            [range.start * samples_per_frame..range.end * samples_per_frame]
            .to_vec(),
        sample_rate: decoded.sample_rate,
        positions: decoded.positions.clone(),
    }
}

fn assert_recording_tone(decoded: &DecodedPcm, own: f32, foreign: f32, part: &str) {
    for lane in 0..decoded.positions.len() {
        let own_energy = tone_energy(decoded, lane, own);
        let foreign_energy = tone_energy(decoded, lane, foreign);
        assert!(
            own_energy > foreign_energy * FOREIGN_TONE_REJECTION,
            "{part}, lane {lane}: {own} Hz must dominate ({own_energy} vs {foreign_energy})"
        );
    }
}

#[test]
fn concatenated_local_ts_plays_continuous_audio_through_one_decoder() {
    let concatenated = [PART_A, PART_B].concat();
    let mut demuxer = open_local("two-recordings.ts", &concatenated);
    let track = first_audio_track(demuxer.as_ref());
    assert_eq!(track.codec_id, "A_AC3");

    let mut audio = Vec::new();
    let mut video = Vec::new();
    let mut track_resets = 0;
    loop {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::Packet(packet) if packet.track_id == track.id => audio.push(packet),
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => video.push(packet),
            DemuxReadEvent::TracksChanged(_) => track_resets += 1,
            DemuxReadEvent::EndOfStream => break,
            _ => {}
        }
    }
    assert_eq!(
        track_resets, 0,
        "the join must not rebuild the player pipeline"
    );
    assert_eq!(audio.len(), 2 * FRAMES_PER_PART, "no audio frame is lost");
    assert_eq!(
        video.len(),
        2 * VIDEO_FRAMES_PER_PART,
        "no video frame is lost"
    );
    // Одна шкала: каждый кадр начинается там, где закончился предыдущий.
    for pair in audio.windows(2) {
        assert_eq!(pts_90khz(&pair[1]) - pts_90khz(&pair[0]), FRAME_90KHZ);
    }
    for pair in video.windows(2) {
        assert!(
            pts_90khz(&pair[1]) > pts_90khz(&pair[0]),
            "video presentation is monotonic"
        );
    }
    assert!(
        video[VIDEO_FRAMES_PER_PART].keyframe.is_known_keyframe(),
        "decoder continues with the second recording's IDR"
    );

    let decoded = decode_track_packets(&track, &audio);
    assert_eq!(
        decoded.positions,
        [
            AudioChannelPosition::FrontLeft,
            AudioChannelPosition::FrontRight
        ]
    );
    let channels = decoded.positions.len();
    assert_eq!(
        decoded.interleaved.len(),
        2 * FRAMES_PER_PART * SAMPLES_PER_ACCESS_UNIT * channels,
        "every frame of both recordings reaches PCM"
    );
    // Ни одного выпавшего/немого кадра, в том числе на стыке.
    for frame_index in 0..2 * FRAMES_PER_PART {
        let frame = frames(&decoded, frame_index..frame_index + 1);
        let power = frame
            .interleaved
            .iter()
            .map(|sample| sample * sample)
            .sum::<f32>()
            / frame.interleaved.len() as f32;
        assert!(
            power > MINIMUM_FRAME_POWER,
            "frame {frame_index} is not silent (power {power})"
        );
    }
    // Тон меняется ровно на стыке: кадры с краёв стыка не берём (MDCT-перекрытие).
    assert_recording_tone(
        &frames(&decoded, 1..FRAMES_PER_PART - 1),
        TONE_A_HZ,
        TONE_B_HZ,
        "first recording",
    );
    assert_recording_tone(
        &frames(&decoded, FRAMES_PER_PART + 1..2 * FRAMES_PER_PART),
        TONE_B_HZ,
        TONE_A_HZ,
        "second recording",
    );
}
