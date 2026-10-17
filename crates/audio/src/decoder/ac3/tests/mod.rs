//! Функциональные тесты AC-3 / E-AC-3: настоящие packet-ы ffmpeg → публичная
//! factory → `ac3-decoder` → canonical PCM → production `ChannelMixer`.
//!
//! Фикстуры и команда генерации: `crates/audio/tests/fixtures/ac3/README.md`
//! (тон на канал) и `crates/ac3-decoder/tests/fixtures/README.md` (раскладки
//! всех acmod, эталонные выдержки ffmpeg). В каждом канале свой синус, поэтому
//! энергия тонов по lanes доказывает раскладку, а не только факт декодирования.
//! ffmpeg во время тестов не нужен.
//!
//! - `playback` — factory, тоны по lanes, RMS против ffmpeg, downmix в stereo;
//! - `differential` — adapter против прямого вызова `ac3-decoder` и эталона ffmpeg;
//! - `error_policy` — битые packet-ы, постоянные причины, reset, смена формата;
//! - `layout` — сопоставление позиций (Lsd/Rsd, неконвертируемые раскладки);
//! - `robustness` — случайная порча и обрезка packet-ов.

mod differential;
mod error_policy;
mod layout;
mod playback;
mod robustness;

use std::f32::consts::PI;

use audio_core::{
    AudioChannelLayout, AudioChannelPosition, AudioDecoder, AudioDecoderConfig, AudioDecoderHandle,
    EncodedAudioPacket,
};

use super::{Ac3CodecIdentity, Ac3FamilyDecoder};
use crate::decoder::create_audio_decoder;

/// ffmpeg AC-3 stereo: L = 440 Гц, R = 1000 Гц.
const STEREO_AC3: &[u8] = include_bytes!("../../../../tests/fixtures/ac3/tones-stereo.ac3");
/// ffmpeg AC-3 5.1(side): FL 300, FR 700, FC 500, LFE 60, SL 900, SR 1100 Гц.
const SURROUND_AC3: &[u8] = include_bytes!("../../../../tests/fixtures/ac3/tones-5.1.ac3");
/// ffmpeg E-AC-3 5.1(side) с теми же тонами.
const SURROUND_EAC3: &[u8] = include_bytes!("../../../../tests/fixtures/ac3/tones-5.1.eac3");

/// Частоты stereo фикстуры в canonical порядке (FL, FR).
const STEREO_TONES: [f32; 2] = [440.0, 1_000.0];
/// Частоты 5.1 фикстур в canonical порядке (FL, FR, FC, LFE, SL, SR).
const SURROUND_TONES: [f32; 6] = [300.0, 700.0, 500.0, 60.0, 900.0, 1_100.0];
/// Частота дискретизации фикстур `tones-*`.
const SAMPLE_RATE: u32 = 48_000;
/// Samples на канал в одном syncframe-е AC-3 / E-AC-3 (6 блоков по 256).
const FRAMES_PER_PACKET: usize = 1_536;
/// Track id, которым тесты помечают packet-ы.
const TRACK_ID: u32 = 7;
/// Минимальная длина заголовка syncframe-а, по которому определяется его длина.
const SYNCFRAME_HEADER_BYTES: usize = 6;
/// Последний `bsid` синтаксиса AC-3 (9/10 — пониженная частота).
const MAX_AC3_BITSTREAM_ID: u8 = 10;
/// `strmtyp` зависимого substream-а E-AC-3 (A/52 Annex E, Т. E2.1).
const DEPENDENT_SUBSTREAM_TYPE: u8 = 1;

/// Режет elementary stream на packet-ы так, как их отдаёт Matroska / MP4.
///
/// E-AC-3 dependent substream приклеивается к предыдущему независимому — это и
/// есть один access unit E-AC-3.
fn packets(stream: &[u8]) -> Vec<&[u8]> {
    let mut packet_ranges: Vec<(usize, usize)> = Vec::new();
    let mut offset = 0;
    while offset + SYNCFRAME_HEADER_BYTES <= stream.len() {
        let bitstream_id = stream[offset + 5] >> 3;
        let (frame_bytes, is_dependent) = if bitstream_id <= MAX_AC3_BITSTREAM_ID {
            let info = ac3_decoder::read_syncframe_info(&stream[offset..]).unwrap();
            (info.frame_length_bytes, false)
        } else {
            // frmsiz — 11 бит после strmtyp(2) и substreamid(3), длина = (frmsiz + 1) слов.
            let frame_size_words =
                (usize::from(stream[offset + 2] & 0x07) << 8) | usize::from(stream[offset + 3]);
            (
                (frame_size_words + 1) * 2,
                stream[offset + 2] >> 6 == DEPENDENT_SUBSTREAM_TYPE,
            )
        };
        match packet_ranges.last_mut() {
            Some(last) if is_dependent => last.1 = offset + frame_bytes,
            _ => packet_ranges.push((offset, offset + frame_bytes)),
        }
        offset += frame_bytes;
    }
    assert_eq!(
        offset,
        stream.len(),
        "fixture must contain whole syncframes"
    );
    packet_ranges
        .into_iter()
        .map(|(start, end)| &stream[start..end])
        .collect()
}

/// Создаёт decoder через публичную production factory.
fn factory_decoder(codec_id: &str, channels: u32) -> AudioDecoderHandle {
    create_audio_decoder(AudioDecoderConfig::new(
        TRACK_ID,
        codec_id,
        SAMPLE_RATE,
        channels,
    ))
    .unwrap()
}

/// Concrete decoder для тестов внутренних состояний.
fn concrete_decoder(identity: Ac3CodecIdentity, channels: u32) -> Ac3FamilyDecoder {
    Ac3FamilyDecoder::new(
        &AudioDecoderConfig::new(TRACK_ID, identity.codec_id(), SAMPLE_RATE, channels),
        identity,
    )
    .unwrap()
}

/// Декодирует один packet через neutral boundary.
fn decode_packet(decoder: &mut dyn AudioDecoder, packet_bytes: &[u8]) -> Vec<f32> {
    decoder
        .decode(&EncodedAudioPacket::without_timing(TRACK_ID, packet_bytes))
        .unwrap()
}

/// Декодирует packet-ы и проверяет атомарный spec каждого непустого buffer-а.
fn decode_all(
    decoder: &mut dyn AudioDecoder,
    packet_list: &[&[u8]],
    expected_layout: AudioChannelLayout,
) -> Vec<f32> {
    let mut interleaved = Vec::new();
    for packet_bytes in packet_list {
        let samples = decode_packet(decoder, packet_bytes);
        let spec = decoder.decoded_output_spec().unwrap();
        assert_eq!(spec.sample_rate, SAMPLE_RATE);
        assert_eq!(spec.channel_layout, expected_layout);
        assert_eq!(
            samples.len(),
            FRAMES_PER_PACKET * expected_layout.channel_count() as usize
        );
        interleaved.extend_from_slice(&samples);
    }
    interleaved
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

/// Сколько энергии тона в lane по сравнению с самым сильным тоном набора.
fn relative_tone_energy(interleaved: &[f32], channels: usize, lane: usize, tone: f32) -> f32 {
    let strongest = SURROUND_TONES
        .iter()
        .chain(&STEREO_TONES)
        .map(|frequency| tone_energy(interleaved, channels, lane, *frequency))
        .fold(f32::MIN_POSITIVE, f32::max);
    tone_energy(interleaved, channels, lane, tone) / strongest
}

/// В каждой lane доминирует свой тон, чужие тоны подавлены минимум на 20 дБ.
fn assert_lane_tones(interleaved: &[f32], lane_tones: &[f32]) {
    let channels = lane_tones.len();
    for (lane, own_tone) in lane_tones.iter().enumerate() {
        assert!(
            relative_tone_energy(interleaved, channels, lane, *own_tone) > 0.99,
            "lane {lane} lost its own {own_tone} Hz tone"
        );
        for foreign_tone in lane_tones.iter().filter(|tone| *tone != own_tone) {
            assert!(
                relative_tone_energy(interleaved, channels, lane, *foreign_tone) < 0.01,
                "lane {lane} leaks {foreign_tone} Hz"
            );
        }
    }
}

/// RMS одной lane.
fn lane_rms(interleaved: &[f32], channels: usize, lane: usize) -> f32 {
    let (sum_of_squares, count) = interleaved
        .iter()
        .skip(lane)
        .step_by(channels)
        .fold((0.0_f32, 0_usize), |(sum, count), sample| {
            (sum + sample * sample, count + 1)
        });
    (sum_of_squares / count as f32).sqrt()
}

/// Canonical 5.1 AC-3: side-пара, как FFmpeg `5.1(side)`.
fn surround_side_layout() -> AudioChannelLayout {
    AudioChannelLayout::positioned(&[
        AudioChannelPosition::FrontLeft,
        AudioChannelPosition::FrontRight,
        AudioChannelPosition::FrontCenter,
        AudioChannelPosition::LowFrequencyEffects,
        AudioChannelPosition::SideLeft,
        AudioChannelPosition::SideRight,
    ])
    .unwrap()
}

/// Packet с испорченным syncword-ом.
fn corrupted(packet_bytes: &[u8]) -> Vec<u8> {
    let mut broken = packet_bytes.to_vec();
    broken[0] = 0;
    broken
}
