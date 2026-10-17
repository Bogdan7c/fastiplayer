//! Поведение синтеза PCM на фикстурах ffmpeg без внешнего эталона: тоны на
//! своих каналах, частоты, перегрузка без обрезки, детерминизм, `reset`,
//! восстановление после битого кадра, DRC (включая 1+1), bsid 9/10.

use super::support::{FIXTURES, decoder, fixture, split_frames};
use crate::config::{CrcPolicy, DecoderConfig, DynamicRangeControl};
use crate::decoder::Ac3Decoder;
use crate::error::{Ac3DecodeError, CrcRegion};
use crate::output::{ChannelPosition, DecodedAudio};
use crate::spectral::dither::DitherMode;
use crate::syntax::bsi_values::AudioCodingMode;
use crate::synthesis::program::frame::Synthesizer;

use ChannelPosition::{
    Center, CenterSurround, Left, LeftSurround, LowFrequencyEffects, Right, RightSurround,
};

/// Амплитуда тонов фикстур (`generate.sh`: `0.4*sin(...)`).
const TONE_AMPLITUDE: f64 = 0.4;
/// Допуск амплитуды: кодирование + шум ±0,015 фикстур.
const TONE_AMPLITUDE_TOLERANCE: f64 = 0.03;
/// Частоты, среди которых ищется тон канала (все роли фикстур).
const CANDIDATE_TONES_HZ: [f64; 8] = [
    60.0, 440.0, 1000.0, 2000.0, 3000.0, 5000.0, 7000.0, 11_000.0,
];
/// Чужие тоны в канале не громче этого (перекрёстные помехи каналов).
const FOREIGN_TONE_LIMIT: f64 = 0.02;

/// Ожидаемые частота и позиции каналов фикстуры (README фикстур).
fn expected_layout(name: &str) -> (u32, Vec<(ChannelPosition, Option<f64>)>) {
    let rate = if name.contains("44k1") {
        44_100
    } else if name.contains("32k-") {
        32_000
    } else {
        48_000
    };
    let tones: Vec<(ChannelPosition, Option<f64>)> = match name {
        "mono-48k-96k.ac3" => vec![(Center, Some(2000.0))],
        "mono-lfe-48k-128k.ac3" => vec![(Center, Some(2000.0)), (LowFrequencyEffects, Some(60.0))],
        "stereo-48k-160k-cpl-high.ac3" => vec![(Left, Some(5000.0)), (Right, Some(11_000.0))],
        "stereo-lfe-44k1-192k.ac3" => vec![
            (Left, Some(440.0)),
            (Right, Some(1000.0)),
            (LowFrequencyEffects, Some(60.0)),
        ],
        name if name.starts_with("stereo") => vec![(Left, Some(440.0)), (Right, Some(1000.0))],
        "3f-48k-256k.ac3" => vec![
            (Left, Some(440.0)),
            (Center, Some(2000.0)),
            (Right, Some(1000.0)),
        ],
        "3f-lfe-32k-256k.ac3" => vec![
            (Left, Some(440.0)),
            (Center, Some(2000.0)),
            (Right, Some(1000.0)),
            (LowFrequencyEffects, Some(60.0)),
        ],
        "2f1r-32k-192k.ac3" => vec![
            (Left, Some(440.0)),
            (Right, Some(1000.0)),
            (CenterSurround, Some(3000.0)),
        ],
        "2f1r-lfe-48k-256k.ac3" => vec![
            (Left, Some(440.0)),
            (Right, Some(1000.0)),
            (CenterSurround, Some(3000.0)),
            (LowFrequencyEffects, Some(60.0)),
        ],
        "3f1r-48k-256k.ac3" => vec![
            (Left, Some(440.0)),
            (Center, Some(2000.0)),
            (Right, Some(1000.0)),
            (CenterSurround, Some(3000.0)),
        ],
        "3f1r-lfe-48k-384k.ac3" => vec![
            (Left, Some(440.0)),
            (Center, Some(2000.0)),
            (Right, Some(1000.0)),
            (CenterSurround, Some(3000.0)),
            (LowFrequencyEffects, Some(60.0)),
        ],
        "2f2r-48k-320k.ac3" => vec![
            (Left, Some(440.0)),
            (Right, Some(1000.0)),
            (LeftSurround, Some(3000.0)),
            (RightSurround, Some(7000.0)),
        ],
        "3f2r-48k-448k-cpl.ac3" => vec![
            (Left, Some(440.0)),
            (Center, Some(2000.0)),
            (Right, Some(1000.0)),
            (LeftSurround, Some(3000.0)),
            (RightSurround, Some(7000.0)),
        ],
        // Полоса кодера на 32 кГц/192 кбит/с урезана до 73 бинов (≈ 4,6 кГц):
        // тон Rs 7 кГц отрезан кодером, канал Rs почти молчит.
        "5p1-32k-192k.ac3" => vec![
            (Left, Some(440.0)),
            (Center, Some(2000.0)),
            (Right, Some(1000.0)),
            (LeftSurround, Some(3000.0)),
            (RightSurround, None),
            (LowFrequencyEffects, Some(60.0)),
        ],
        name if name.starts_with("5p1") => vec![
            (Left, Some(440.0)),
            (Center, Some(2000.0)),
            (Right, Some(1000.0)),
            (LeftSurround, Some(3000.0)),
            (RightSurround, Some(7000.0)),
            (LowFrequencyEffects, Some(60.0)),
        ],
        other => panic!("no expectation for {other}"),
    };
    (rate, tones)
}

/// Амплитуда синусоиды частоты `hertz` в сигнале (окно Ханна, без утечки
/// от соседних тонов фикстур).
pub(crate) fn tone_amplitude(samples: &[f32], hertz: f64, rate: u32) -> f64 {
    let length = samples.len() as f64;
    let (mut re, mut im, mut window_sum) = (0.0, 0.0, 0.0);
    for (index, &sample) in samples.iter().enumerate() {
        let window = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * index as f64 / length).cos();
        let phase = 2.0 * std::f64::consts::PI * hertz * index as f64 / f64::from(rate);
        re += f64::from(sample) * window * phase.cos();
        im += f64::from(sample) * window * phase.sin();
        window_sum += window;
    }
    2.0 * (re * re + im * im).sqrt() / window_sum
}

fn decode_all(decoder: &mut Ac3Decoder, frames: &[&[u8]]) -> Vec<DecodedAudio> {
    frames
        .iter()
        .map(|frame| decoder.decode_access_unit(frame).unwrap())
        .collect()
}

/// Канал по всем кадрам, начиная с `first_frame`.
fn channel(audio: &[DecodedAudio], channel: usize, first_frame: usize) -> Vec<f32> {
    audio
        .iter()
        .skip(first_frame)
        .flat_map(|frame| frame.channel_samples(channel).unwrap().iter().copied())
        .collect()
}

#[test]
fn every_fixture_plays_its_tones_on_the_right_speakers() {
    for fixture in &FIXTURES {
        let (rate, expected) = expected_layout(fixture.name);
        let frames = split_frames(fixture.bytes);
        let audio = decode_all(&mut Ac3Decoder::new(DecoderConfig::default()), &frames);
        let positions: Vec<ChannelPosition> =
            expected.iter().map(|(position, _)| *position).collect();
        for frame in &audio {
            assert_eq!(frame.sample_rate_hz(), rate, "{}", fixture.name);
            assert_eq!(frame.channels(), positions, "{}", fixture.name);
            assert_eq!(frame.samples_per_channel(), 1536);
        }
        for (index, (position, tone)) in expected.iter().enumerate() {
            // Кадр 0 — разгон TDAC и задержка кодера; дальше сигнал установился.
            let samples = channel(&audio, index, 1);
            let amplitude_of = |hertz| tone_amplitude(&samples, hertz, rate);
            match tone {
                Some(hertz) => {
                    let amplitude = amplitude_of(*hertz);
                    assert!(
                        (amplitude - TONE_AMPLITUDE).abs() < TONE_AMPLITUDE_TOLERANCE,
                        "{} {position:?}: {hertz} Hz amplitude {amplitude}",
                        fixture.name
                    );
                    for other in CANDIDATE_TONES_HZ.iter().filter(|&&other| other != *hertz) {
                        let foreign = amplitude_of(*other);
                        assert!(
                            foreign < FOREIGN_TONE_LIMIT,
                            "{} {position:?}: {other} Hz leaks {foreign}",
                            fixture.name
                        );
                    }
                }
                None => {
                    let peak = samples
                        .iter()
                        .fold(0.0f32, |peak, value| peak.max(value.abs()));
                    assert!(
                        peak < 0.05,
                        "{} {position:?} should be nearly silent: {peak}",
                        fixture.name
                    );
                }
            }
        }
    }
}

#[test]
fn overload_is_reproduced_above_full_scale() {
    let frames = split_frames(fixture("overload-stereo-48k-192k.ac3"));
    let audio = decode_all(&mut Ac3Decoder::new(DecoderConfig::default()), &frames);
    let samples: Vec<f32> = (0..2).flat_map(|index| channel(&audio, index, 1)).collect();
    let peak = samples
        .iter()
        .fold(0.0f32, |peak, value| peak.max(value.abs()));
    // Квадрат 0,98 со срезанным спектром даёт выбросы Гиббса ≈ +18 %.
    assert!(peak > 1.1, "peak {peak}");
    let above_full_scale = samples.iter().filter(|value| value.abs() > 1.0).count();
    let pinned_to_full_scale = samples.iter().filter(|value| value.abs() == 1.0).count();
    assert!(above_full_scale > 100, "{above_full_scale}");
    assert_eq!(pinned_to_full_scale, 0, "values are clamped to ±1.0");
}

#[test]
fn decoding_is_deterministic_and_reset_restarts_the_stream() {
    let frames = split_frames(fixture("5p1-48k-640k-annexd.ac3"));
    let first = decode_all(&mut Ac3Decoder::new(DecoderConfig::default()), &frames);
    let mut decoder = Ac3Decoder::new(DecoderConfig::default());
    let second = decode_all(&mut decoder, &frames);
    assert_eq!(first, second);
    // Без reset тот же кадр звучит иначе: хвост overlap-add и dither идут дальше.
    let continued = decoder.decode_access_unit(frames[0]).unwrap();
    assert_ne!(continued, first[0]);
    decoder.reset();
    let restarted = decode_all(&mut decoder, &frames);
    assert_eq!(restarted, first);
}

#[test]
fn corrupt_frame_breaks_overlap_and_next_frames_recover() {
    let frames = split_frames(fixture("3f2r-48k-448k-cpl.ac3"));
    let mut continuous = decoder(CrcPolicy::Verify, DitherMode::Disabled);
    let reference = decode_all(&mut continuous, &frames);
    let mut decoder_under_test = decoder(CrcPolicy::Verify, DitherMode::Disabled);
    for frame in &frames[..2] {
        decoder_under_test.decode_access_unit(frame).unwrap();
    }
    let mut corrupted = frames[2].to_vec();
    corrupted[40] ^= 0x10;
    assert_eq!(
        decoder_under_test.decode_access_unit(&corrupted),
        Err(Ac3DecodeError::CrcMismatch {
            region: CrcRegion::FirstFiveEighths
        })
    );
    // После ошибки хвосты забыты: кадр 3 звучит как первый кадр нового потока.
    let after_error = decoder_under_test.decode_access_unit(frames[3]).unwrap();
    let as_first_frame = decoder(CrcPolicy::Verify, DitherMode::Disabled)
        .decode_access_unit(frames[3])
        .unwrap();
    assert_eq!(after_error, as_first_frame);
    assert_ne!(after_error, reference[3]);
    // Кадр 4 снова совпадает с непрерывным декодированием (хвост кадра 3 тот же).
    assert_eq!(
        decoder_under_test.decode_access_unit(frames[4]).unwrap(),
        reference[4]
    );
}

/// Синтез кадра `frame_index` фикстуры с изменёнными словами DRC.
fn synthesize_with(
    name: &str,
    frame_index: usize,
    edit: impl Fn(
        &mut crate::syntax::syncframe::SyncframeInfo,
        &mut [crate::syntax::audblk::output::DecodedAudioBlock; 6],
    ),
    dynamic_range: DynamicRangeControl,
) -> DecodedAudio {
    let frames = split_frames(fixture(name));
    let mut decoder = decoder(CrcPolicy::Verify, DitherMode::Disabled);
    let mut info = decoder
        .decode_coefficients(frames[frame_index])
        .unwrap()
        .info;
    let mut blocks = decoder.decoded_blocks().clone();
    edit(&mut info, &mut blocks);
    Synthesizer::new().synthesize_ac3_frame(&info, &blocks, dynamic_range)
}

fn scaled(audio: &DecodedAudio, channel: usize, gain: f32) -> Vec<f32> {
    audio
        .channel_samples(channel)
        .unwrap()
        .iter()
        .map(|&value| value * gain)
        .collect()
}

/// `dynrng` = 110 00000₂: X = −2, Y = 0 → 2^−1 · 1/2 = 1/4 (−12,04 дБ).
const QUARTER_GAIN_WORD: u8 = 0b110_00000;
/// `compr` = 0001 0000₂: X = 1, Y = 0 → 2^2 · 1/2 = 2 (+6,02 дБ).
const DOUBLE_GAIN_COMPR: u8 = 0b0001_0000;

#[test]
fn dynamic_range_policies_scale_every_channel_including_lfe() {
    let name = "5p1-48k-640k-annexd.ac3";
    let baseline = synthesize_with(name, 3, |_, _| {}, DynamicRangeControl::Standard);
    let quarter_words =
        |_: &mut _, blocks: &mut [crate::syntax::audblk::output::DecodedAudioBlock; 6]| {
            for block in blocks.iter_mut() {
                block.dynamic_range_word = QUARTER_GAIN_WORD;
            }
        };
    let standard = synthesize_with(name, 3, quarter_words, DynamicRangeControl::Standard);
    let disabled = synthesize_with(name, 3, quarter_words, DynamicRangeControl::Disabled);
    let heavy = synthesize_with(
        name,
        3,
        |info, blocks| {
            quarter_words(info, blocks);
            info.bsi.program.compression_gain_word = Some(DOUBLE_GAIN_COMPR);
        },
        DynamicRangeControl::Heavy,
    );
    assert_eq!(baseline.channels().last(), Some(&LowFrequencyEffects));
    for index in 0..baseline.channels().len() {
        // Множитель — степень двойки, а синтез линеен: совпадение точное.
        assert_eq!(
            standard.channel_samples(index).unwrap(),
            scaled(&baseline, index, 0.25),
            "channel {index}"
        );
        assert_eq!(
            disabled.channel_samples(index),
            baseline.channel_samples(index)
        );
        assert_eq!(
            heavy.channel_samples(index).unwrap(),
            scaled(&baseline, index, 2.0)
        );
    }
}

#[test]
fn dual_mono_programs_follow_their_own_dynamic_range_words() {
    // Стерео-кадр без rematrixing, объявленный как 1+1: Ch1 получает dynrng,
    // Ch2 — dynrng2 (§7.7.1.2).
    let as_dual_mono =
        |info: &mut crate::syntax::syncframe::SyncframeInfo,
         blocks: &mut [crate::syntax::audblk::output::DecodedAudioBlock; 6]| {
            info.bsi.audio_coding_mode = AudioCodingMode::DualMono;
            for block in blocks.iter_mut() {
                block.rematrix = None;
            }
        };
    let name = "stereo-44k1-128k-plain.ac3";
    let baseline = synthesize_with(name, 2, as_dual_mono, DynamicRangeControl::Standard);
    let second_quieter = synthesize_with(
        name,
        2,
        |info, blocks| {
            as_dual_mono(info, blocks);
            for block in blocks.iter_mut() {
                block.dual_mono_dynamic_range_word = QUARTER_GAIN_WORD;
            }
        },
        DynamicRangeControl::Standard,
    );
    assert_eq!(baseline.channels(), [Left, Right]);
    assert_eq!(
        second_quieter.channel_samples(0),
        baseline.channel_samples(0)
    );
    assert_eq!(
        second_quieter.channel_samples(1).unwrap(),
        scaled(&baseline, 1, 0.25)
    );
}

#[test]
fn reduced_rate_bitstream_ids_decode_like_bsid_8_at_lower_rates() {
    // bsid 9/10 спеками не описаны; договорённость (mem:ac3-decoder/core):
    // синтаксис AC-3, частота ÷2/÷4. ffmpeg n9.0.2 такие кадры не декодирует.
    let frames = split_frames(fixture("stereo-48k-192k.ac3"));
    let original = decode_all(&mut decoder(CrcPolicy::Skip, DitherMode::Disabled), &frames);
    for (bitstream_id, rate) in [(9u8, 24_000), (10, 12_000)] {
        let patched: Vec<Vec<u8>> = frames
            .iter()
            .map(|frame| {
                let mut bytes = frame.to_vec();
                bytes[5] = (bitstream_id << 3) | (bytes[5] & 0x07);
                bytes
            })
            .collect();
        let mut reduced = decoder(CrcPolicy::Skip, DitherMode::Disabled);
        for (frame, expected) in patched.iter().zip(&original) {
            let audio = reduced.decode_access_unit(frame).unwrap();
            assert_eq!(audio.sample_rate_hz(), rate);
            assert_eq!(audio.planar_samples(), expected.planar_samples());
        }
    }
}
