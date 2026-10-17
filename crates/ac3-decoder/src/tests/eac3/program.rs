//! Сквозные тесты PCM E-AC-3 через публичный [`Ac3Decoder`] (пакет D4):
//! тоны фикстур на своих позициях, сведение программы (Annex E §3.8.2),
//! DRC последнего зависимого substream-а (§3.8.5), TPNP (§3.7), форматы
//! кадров (1/2/3/6 блоков, пониженные частоты), переключение AC-3 ↔ E-AC-3
//! в одном декодере, `reset` и восстановление после порчи.
//!
//! Зависимые substream-ы кодер ffmpeg не пишет; они собираются побитно
//! (`builder`, тихие кадры) и сводятся с настоящими кадрами фикстур — так
//! видно, что замена/добавление каналов затрагивают ровно нужные позиции.

use super::builder::{FrameSpec, build_frame, refresh_crc};
use super::support::{eac3_fixture, split_frames};
use crate::config::{CrcPolicy, DecoderConfig, DynamicRangeControl};
use crate::decoder::Ac3Decoder;
use crate::error::{Ac3DecodeError, CrcRegion};
use crate::output::ChannelPosition::{
    self, Center, CenterSurround, Left, LeftRearSurround, LeftSurround, LeftWide,
    LowFrequencyEffects, Right, RightRearSurround, RightSurround, RightWide,
};
use crate::output::DecodedAudio;
use crate::spectral::dither::DitherMode;
use crate::synthesis::transient_pre_noise::TransientPreNoiseProcessing;
use crate::tests::pcm_behaviour::tone_amplitude;
use crate::tests::support::{fixture as ac3_fixture, split_frames as split_ac3_frames};

/// Амплитуда тонов фикстур (0,4) и допуск декодированной амплитуды.
const TONE_AMPLITUDE: f64 = 0.4;
const TONE_TOLERANCE: f64 = 0.03;
/// Канал «молчит»: пик ниже −100 дБ FS.
const SILENCE_PEAK: f32 = 1e-5;
/// `chanmap` (Таблица E2.5, бит 0 — старший): пара Lrs/Rrs (бит 6).
const REAR_PAIR_MAP: u16 = 1 << 9;
/// Ls (бит 3) и Rs (бит 4).
const SURROUND_PAIR_MAP: u16 = (1 << 12) | (1 << 11);
/// Пара Lw/Rw (бит 10).
const WIDE_PAIR_MAP: u16 = 1 << 5;
/// `dynrng` = 110 00000₂ → 1/4; `compr` = 0001 0000₂ → 2 (как в `pcm_behaviour`).
const QUARTER_GAIN_WORD: u8 = 0b110_00000;
const DOUBLE_GAIN_COMPR: u8 = 0b0001_0000;
/// `acmod` 2/0 и 3/2.
const STEREO: u32 = 2;
const THREE_TWO: u32 = 7;

fn quiet(crc: CrcPolicy) -> Ac3Decoder {
    Ac3Decoder::new(DecoderConfig {
        crc,
        dither: DitherMode::Disabled,
        ..DecoderConfig::default()
    })
}

fn concat(frames: &[&[u8]]) -> Vec<u8> {
    frames
        .iter()
        .flat_map(|frame| frame.iter().copied())
        .collect()
}

/// Все access unit-ы декодером, без ошибок.
fn decode_units(decoder: &mut Ac3Decoder, units: &[Vec<u8>]) -> Vec<DecodedAudio> {
    units
        .iter()
        .map(|unit| decoder.decode_access_unit(unit).unwrap())
        .collect()
}

/// Канал `position` по всем кадрам начиная с `first`.
fn track(audio: &[DecodedAudio], position: ChannelPosition, first: usize) -> Vec<f32> {
    audio
        .iter()
        .skip(first)
        .flat_map(|frame| {
            let index = frame
                .channels()
                .iter()
                .position(|&candidate| candidate == position)
                .unwrap();
            frame.channel_samples(index).unwrap().to_vec()
        })
        .collect()
}

fn assert_tone(audio: &[DecodedAudio], position: ChannelPosition, hertz: f64) {
    let samples = track(audio, position, 1);
    let amplitude = tone_amplitude(&samples, hertz, audio[0].sample_rate_hz());
    assert!(
        (amplitude - TONE_AMPLITUDE).abs() < TONE_TOLERANCE,
        "{position:?}: {hertz} Hz amplitude {amplitude:.3}"
    );
}

fn assert_silent(audio: &[DecodedAudio], position: ChannelPosition) {
    let peak = track(audio, position, 0)
        .iter()
        .fold(0.0f32, |peak, value| peak.max(value.abs()));
    assert!(peak < SILENCE_PEAK, "{position:?}: peak {peak}");
}

/// Кадры фикстуры E-AC-3 + к каждому — тихий зависимый substream `dependent`.
fn with_dependent(independent: &[&[u8]], dependent: &FrameSpec) -> Vec<Vec<u8>> {
    let extension = build_frame(dependent);
    independent
        .iter()
        .map(|frame| concat(&[frame, &extension]))
        .collect()
}

/// Фикстура, частота, отсчётов на канал в кадре, тоны по позициям.
type ToneCase = (&'static str, u32, usize, &'static [(ChannelPosition, f64)]);

#[test]
fn independent_streams_play_tones_on_their_positions_with_frame_sizes() {
    let cases: [ToneCase; 4] = [
        (
            "stereo-48k-96k.eac3",
            48_000,
            1536,
            &[(Left, 440.0), (Right, 1000.0)],
        ),
        (
            "2f1r-48k-192k.eac3",
            48_000,
            1536,
            &[(Left, 440.0), (Right, 1000.0), (CenterSurround, 3000.0)],
        ),
        (
            "quad-44k1-256k.eac3",
            44_100,
            1536,
            &[
                (Left, 440.0),
                (Right, 1000.0),
                (LeftSurround, 3000.0),
                (RightSurround, 7000.0),
            ],
        ),
        (
            "5p1-32k-1000k.eac3",
            32_000,
            768,
            &[
                (Left, 440.0),
                (Center, 2000.0),
                (Right, 1000.0),
                (LeftSurround, 3000.0),
                (RightSurround, 7000.0),
                (LowFrequencyEffects, 60.0),
            ],
        ),
    ];
    for (name, rate, samples, tones) in cases {
        let units: Vec<Vec<u8>> = split_frames(eac3_fixture(name))
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();
        let audio = decode_units(&mut quiet(CrcPolicy::Verify), &units);
        for frame in &audio {
            assert_eq!(frame.sample_rate_hz(), rate, "{name}");
            assert_eq!(frame.samples_per_channel(), samples, "{name}");
            assert_eq!(frame.channels().len(), tones.len(), "{name}");
        }
        for &(position, hertz) in tones {
            assert_tone(&audio, position, hertz);
        }
    }
}

/// 7.1 поверх 5.1 (§3.8.2): зависимый с `chanmap` Ls, Rs, Lrs, Rrs заменяет
/// Ls/Rs независимого (тоны 3000/7000 Гц пропадают) и добавляет Lrs/Rrs;
/// L, C, R, LFE остаются данными независимого — отсчёт в отсчёт.
#[test]
fn dependent_substream_replaces_matching_channels_and_adds_new_ones() {
    let independent = split_frames(eac3_fixture("5p1-48k-384k.eac3"));
    let map = SURROUND_PAIR_MAP | REAR_PAIR_MAP;
    let units = with_dependent(&independent, &FrameSpec::dependent(6, false, Some(map)));
    let audio = decode_units(&mut quiet(CrcPolicy::Verify), &units);
    assert_eq!(
        audio[0].channels(),
        [
            Left,
            Center,
            Right,
            LeftSurround,
            RightSurround,
            LeftRearSurround,
            RightRearSurround,
            LowFrequencyEffects
        ]
    );
    let plain_units: Vec<Vec<u8>> = independent.iter().map(|frame| frame.to_vec()).collect();
    let alone = decode_units(&mut quiet(CrcPolicy::Verify), &plain_units);
    for position in [Left, Center, Right, LowFrequencyEffects] {
        assert_eq!(
            track(&audio, position, 0),
            track(&alone, position, 0),
            "{position:?}"
        );
    }
    assert_tone(&audio, Left, 440.0);
    assert_tone(&audio, LowFrequencyEffects, 60.0);
    for position in [
        LeftSurround,
        RightSurround,
        LeftRearSurround,
        RightRearSurround,
    ] {
        assert_silent(&audio, position);
    }
    // Тот же зависимый, но только с добавочной парой Lw/Rw: всё 5.1 сохраняется.
    let wide = with_dependent(
        &independent,
        &FrameSpec::dependent(STEREO, false, Some(WIDE_PAIR_MAP)),
    );
    let audio = decode_units(&mut quiet(CrcPolicy::Verify), &wide);
    assert_eq!(audio[0].channels().len(), 8);
    assert_tone(&audio, LeftSurround, 3000.0);
    assert_tone(&audio, RightSurround, 7000.0);
    assert_silent(&audio, LeftWide);
    assert_silent(&audio, RightWide);
}

/// Пример §3.8.2: без `chanmap` позиции зависимого задаёт его `acmod` — 2/0
/// заменяет L и R, раскладка остаётся 5.1.
#[test]
fn dependent_without_channel_map_replaces_channels_named_by_acmod() {
    let independent = split_frames(eac3_fixture("5p1-48k-384k.eac3"));
    let units = with_dependent(&independent, &FrameSpec::dependent(STEREO, false, None));
    let audio = decode_units(&mut quiet(CrcPolicy::Verify), &units);
    assert_eq!(
        audio[0].channels(),
        [
            Left,
            Center,
            Right,
            LeftSurround,
            RightSurround,
            LowFrequencyEffects
        ]
    );
    assert_silent(&audio, Left);
    assert_silent(&audio, Right);
    assert_tone(&audio, Center, 2000.0);
    assert_tone(&audio, RightSurround, 7000.0);
}

/// Blu-ray: ядро AC-3 (bsid 6) + зависимый E-AC-3 в одном access unit-е.
#[test]
fn ac3_core_with_dependent_substream_decodes_as_one_program() {
    let core = split_ac3_frames(ac3_fixture("5p1-48k-640k-annexd.ac3"));
    let units = with_dependent(
        &core,
        &FrameSpec::dependent(STEREO, false, Some(REAR_PAIR_MAP)),
    );
    let audio = decode_units(&mut quiet(CrcPolicy::Verify), &units);
    assert_eq!(audio[0].channels().len(), 8);
    assert_eq!(audio[0].samples_per_channel(), 1536);
    assert_tone(&audio, LeftSurround, 3000.0);
    assert_tone(&audio, Center, 2000.0);
    assert_silent(&audio, LeftRearSurround);
    // Отдельный кадр AC-3 тем же декодером — снова 5.1 (формат каждого access unit-а свой).
    let mut decoder = quiet(CrcPolicy::Verify);
    let combined = decoder.decode_access_unit(&units[0]).unwrap();
    let alone = decoder.decode_access_unit(core[1]).unwrap();
    assert_eq!(combined.channels().len(), 8);
    assert_eq!(alone.channels().len(), 6);
}

/// §3.8.5: при зависимых substream-ах действуют `dynrng`/`compr` последнего
/// из них — для ВСЕХ каналов программы, включая каналы независимого.
#[test]
fn dynamic_range_words_of_the_last_dependent_apply_to_the_whole_program() {
    let independent = split_frames(eac3_fixture("5p1-48k-384k.eac3"));
    let spec = FrameSpec {
        dynamic_range: Some(QUARTER_GAIN_WORD),
        compression: Some(DOUBLE_GAIN_COMPR),
        ..FrameSpec::dependent(STEREO, false, Some(REAR_PAIR_MAP))
    };
    let units = with_dependent(&independent, &spec);
    let decode = |dynamic_range| {
        let mut decoder = Ac3Decoder::new(DecoderConfig {
            dither: DitherMode::Disabled,
            dynamic_range,
            ..DecoderConfig::default()
        });
        decode_units(&mut decoder, &units)
    };
    let full = decode(DynamicRangeControl::Disabled);
    for (policy, gain) in [
        (DynamicRangeControl::Standard, 0.25f32),
        (DynamicRangeControl::Heavy, 2.0),
    ] {
        let scaled = decode(policy);
        for position in [Left, Center, LeftSurround, LowFrequencyEffects] {
            let expected: Vec<f32> = track(&full, position, 0)
                .iter()
                .map(|value| value * gain)
                .collect();
            assert_eq!(
                track(&scaled, position, 0),
                expected,
                "{policy:?} {position:?}"
            );
        }
    }
}

/// Параметры TPNP идут к выходному каналу, источник которого — их кодированный
/// канал: событие независимого C исполняется, а если C заменил зависимый
/// substream (без TPNP), — нет. `Skip` выключает исполнение целиком.
#[test]
fn transient_pre_noise_parameters_follow_the_source_of_each_channel() {
    // transprocloc 500 → 2000 + 256 отсчётов; область начинается после отсчёта 0.
    let independent = build_frame(&FrameSpec {
        transient_pre_noise: Some((1, 500, 100)),
        ..FrameSpec::independent(THREE_TWO, true)
    });
    let counters = |units: &[Vec<u8>], policy| {
        let mut decoder = Ac3Decoder::new(DecoderConfig {
            dither: DitherMode::Disabled,
            transient_pre_noise: policy,
            ..DecoderConfig::default()
        });
        for unit in units {
            let audio = decoder.decode_access_unit(unit).unwrap();
            // Тихий кадр остаётся тихим и с TPNP.
            assert!(audio.planar_samples().iter().all(|&sample| sample == 0.0));
        }
        decoder.transient_pre_noise_counters()
    };
    let alone = vec![independent.clone(); 3];
    assert_eq!(
        counters(&alone, TransientPreNoiseProcessing::Apply).scheduled,
        3
    );
    assert_eq!(
        counters(&alone, TransientPreNoiseProcessing::Skip).scheduled,
        0
    );
    let replaced_center = build_frame(&FrameSpec::dependent(1, false, None));
    let program = vec![concat(&[&independent, &replaced_center]); 3];
    assert_eq!(
        counters(&program, TransientPreNoiseProcessing::Apply).scheduled,
        0
    );
    // Событие слишком близко к началу кадра (область в уже выданных отсчётах).
    let early = build_frame(&FrameSpec {
        transient_pre_noise: Some((1, 10, 255)),
        ..FrameSpec::independent(THREE_TWO, true)
    });
    let rejected = counters(&[early], TransientPreNoiseProcessing::Apply);
    assert_eq!((rejected.scheduled, rejected.rejected), (0, 1));
}

/// Кадры по 1/2/3 блока и пониженные частоты (Таблица E2.3): длина кадра PCM
/// и частота — из заголовка каждого access unit-а.
#[test]
fn frame_formats_follow_block_count_and_reduced_sample_rates() {
    let mut decoder = quiet(CrcPolicy::Verify);
    for (blocks_code, samples) in [(0u32, 256usize), (1, 512), (2, 768), (3, 1536)] {
        let frame = build_frame(&FrameSpec {
            second_code: blocks_code,
            ..FrameSpec::independent(STEREO, true)
        });
        let audio = decoder.decode_access_unit(&frame).unwrap();
        assert_eq!(audio.samples_per_channel(), samples);
        assert_eq!(audio.sample_rate_hz(), 48_000);
        assert_eq!(audio.channels(), [Left, Right, LowFrequencyEffects]);
    }
    for (fscod2, rate) in [(0u32, 24_000), (1, 22_050), (2, 16_000)] {
        let frame = build_frame(&FrameSpec {
            fscod: 3,
            second_code: fscod2,
            ..FrameSpec::independent(1, false)
        });
        let audio = decoder.decode_access_unit(&frame).unwrap();
        assert_eq!(
            (audio.sample_rate_hz(), audio.samples_per_channel()),
            (rate, 1536)
        );
    }
}

/// Пониженная частота меняет только частоту: таблицы битового распределения
/// берутся по строке `fscod2` (= исходный `fscod`), 6 блоков подразумеваются.
/// Переписанный заголовок реальных кадров даёт тот же PCM на половине частоты
/// (ffmpeg n9.0.2 пониженные частоты не поддерживает — эталона нет).
/// Фикстуры — без `infomdate`: там поле `sourcefscod` есть только при
/// `fscod < 3`, и переписанный заголовок сдвинул бы остальные биты.
#[test]
fn reduced_rate_rewrite_of_real_frames_decodes_identically() {
    for (name, rate) in [
        ("stereo-48k-128k-cpl-high.eac3", 24_000),
        ("quad-44k1-256k.eac3", 22_050),
        ("stereo-32k-640k.eac3", 16_000),
    ] {
        let frames = split_frames(eac3_fixture(name));
        let mut original = quiet(CrcPolicy::Verify);
        let mut reduced = quiet(CrcPolicy::Verify);
        for frame in frames {
            let expected = original.decode_access_unit(frame).unwrap();
            let mut rewritten = frame.to_vec();
            let fscod = rewritten[4] >> 6;
            // fscod (2) = 3, fscod2 (2) = прежний fscod; numblkscod был 3 (6 блоков).
            assert_eq!((rewritten[4] >> 4) & 0x3, 3, "{name}");
            rewritten[4] = (0b11 << 6) | (fscod << 4) | (rewritten[4] & 0x0F);
            refresh_crc(&mut rewritten);
            let audio = reduced.decode_access_unit(&rewritten).unwrap();
            assert_eq!(audio.sample_rate_hz(), rate, "{name}");
            assert_eq!(audio.planar_samples(), expected.planar_samples(), "{name}");
        }
    }
}

/// Один декодер на поток, где AC-3 и E-AC-3 чередуются (одна дорожка TS):
/// формат сообщается на каждом access unit-е, со второго access unit-а после
/// переключения PCM совпадает с отдельным декодером (первый — разгон хвостов).
#[test]
fn one_decoder_switches_between_ac3_and_eac3_per_access_unit() {
    let ac3 = split_ac3_frames(ac3_fixture("stereo-48k-192k.ac3"));
    let eac3 = split_frames(eac3_fixture("5p1-48k-384k.eac3"));
    let mut shared = quiet(CrcPolicy::Verify);
    for segment in [&ac3[..3], &eac3[..3], &ac3[3..]] {
        let mut fresh = quiet(CrcPolicy::Verify);
        for (index, unit) in segment.iter().enumerate() {
            let expected = fresh.decode_access_unit(unit).unwrap();
            let audio = shared.decode_access_unit(unit).unwrap();
            assert_eq!(audio.channels(), expected.channels());
            assert_eq!(audio.sample_rate_hz(), expected.sample_rate_hz());
            if index > 0 {
                assert_eq!(audio.planar_samples(), expected.planar_samples());
            }
        }
    }
}

/// `reset` возвращает dither, шум SPX, хвосты и TPNP к началу: тот же вход —
/// тот же PCM (с dither-ом по умолчанию).
#[test]
fn reset_makes_eac3_decoding_repeatable_from_scratch() {
    let units: Vec<Vec<u8>> = split_frames(eac3_fixture("stereo-48k-96k.eac3"))
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect();
    let mut decoder = Ac3Decoder::new(DecoderConfig::default());
    let first = decode_units(&mut decoder, &units);
    let continued = decode_units(&mut decoder, &units);
    // Без reset dither продолжается — PCM другой.
    assert_ne!(first[1].planar_samples(), continued[1].planar_samples());
    decoder.reset();
    let again = decode_units(&mut decoder, &units);
    assert_eq!(first, again);
}

/// Порченый access unit — типизированная ошибка; следующий декодируется
/// (с нулевыми хвостами), через один PCM совпадает с чистым потоком.
#[test]
fn corrupt_access_unit_is_rejected_and_the_stream_recovers() {
    let frames = split_frames(eac3_fixture("5p1-48k-384k.eac3"));
    let mut clean = quiet(CrcPolicy::Verify);
    let expected: Vec<DecodedAudio> = frames
        .iter()
        .map(|frame| clean.decode_access_unit(frame).unwrap())
        .collect();
    let mut decoder = quiet(CrcPolicy::Verify);
    for (index, frame) in frames.iter().enumerate() {
        let mut bytes = frame.to_vec();
        if index == 2 {
            bytes[200] ^= 0x10;
            assert_eq!(
                decoder.decode_access_unit(&bytes),
                Err(Ac3DecodeError::CrcMismatch {
                    region: CrcRegion::WholeFrame
                })
            );
            continue;
        }
        let audio = decoder.decode_access_unit(&bytes).unwrap();
        if index == 3 {
            assert_ne!(audio.planar_samples(), expected[index].planar_samples());
            assert_eq!(audio.channels(), expected[index].channels());
        } else {
            assert_eq!(audio, expected[index], "access unit {index}");
        }
    }
}

/// Порча и обрезка access unit-ов с зависимыми substream-ами (CRC не
/// проверяется) — любой исход, кроме паники; потом поток декодируется.
#[test]
fn corrupted_program_access_units_never_panic() {
    let core = split_ac3_frames(ac3_fixture("5p1-48k-640k-annexd.ac3"));
    let independent = split_frames(eac3_fixture("5p1-48k-384k.eac3"));
    let mut units = with_dependent(
        &core[..2],
        &FrameSpec::dependent(6, true, Some(SURROUND_PAIR_MAP | REAR_PAIR_MAP | 1)),
    );
    units.extend(with_dependent(
        &independent[..2],
        &FrameSpec::dependent(STEREO, false, Some(REAR_PAIR_MAP)),
    ));
    let mut state = 0x2545_F491u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let mut decoder = quiet(CrcPolicy::Skip);
    for unit in &units {
        for _ in 0..200 {
            let mut corrupted = unit.clone();
            let position = next() as usize % corrupted.len();
            corrupted[position] ^= 1 << (next() % 8);
            let _ = decoder.decode_access_unit(&corrupted);
            let _ = decoder.decode_access_unit(&corrupted[..position]);
        }
        assert!(decoder.decode_access_unit(unit).is_ok());
    }
}
