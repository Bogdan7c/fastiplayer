//! Совпадение PCM с декодером ffmpeg на коммитнутых выдержках
//! (`tests/fixtures/*.ffmpeg-f32le`, команды — в `generate.sh`).
//!
//! Метод — `reference_comparison`: остаток раскладывается по блокам прямым
//! MDCT, чистые бины обязаны совпасть в пределах сетки фиксированной точки
//! ffmpeg, dither-бины — по энергии. Каждая выдержка дополнительно
//! доказывает, что нужный путь синтеза действительно работал (короткие
//! блоки, rematrixing, coupling, LFE, перегрузка).

use super::reference_comparison::{
    Analysis, FRAME_SAMPLES, OurDecode, SPAN, SpectralTally, best_lag, decode_both,
    deinterleave_ffmpeg_order, ratio_db,
};
use super::support::{fixture, split_frames};
use crate::config::{CrcPolicy, DynamicRangeControl};
use crate::output::ChannelPosition;
use crate::syntax::audblk::output::TransformLength;
use crate::syntax::syncframe::SAMPLES_PER_AUDIO_BLOCK;

/// Какой путь синтеза выдержка обязана задействовать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExercisedPath {
    /// Первые кадры потока: нулевые хвосты, задержка 0 относительно ffmpeg.
    StreamStart,
    /// Блоки с `blksw = 1` (§7.9.4.2).
    ShortBlocks,
    /// Полосы с `rematflg = 1` (§7.5).
    Rematrixing,
    /// Coupling (§7.4) в анализируемых блоках.
    Coupling,
    /// Канал LFE: dither-а нет, сверка PCM напрямую.
    LowFrequencyEffects,
    /// Пики выше полной шкалы не обрезаются.
    Overload,
    /// Без coupling и rematrixing, только длинные блоки (44,1 кГц).
    PlainLongBlocks,
    /// Моно-surround S (режим 2/1) при 32 кГц.
    MonoSurround,
}

struct ReferenceExcerpt {
    stream: &'static str,
    reference: &'static [u8],
    /// Первый кадр выдержки и число кадров (`reference <name> <from> <to>`).
    first_frame: usize,
    frames: usize,
    crc: CrcPolicy,
    path: ExercisedPath,
}

macro_rules! reference {
    ($name:literal) => {
        include_bytes!(concat!("../../tests/fixtures/", $name, ".ffmpeg-f32le"))
    };
}

const EXCERPTS: [ReferenceExcerpt; 8] = [
    ReferenceExcerpt {
        stream: "stereo-48k-192k.ac3",
        reference: reference!("stereo-48k-192k"),
        first_frame: 0,
        frames: 2,
        crc: CrcPolicy::Verify,
        path: ExercisedPath::StreamStart,
    },
    ReferenceExcerpt {
        stream: "stereo-48k-192k-short0.ac3",
        reference: reference!("stereo-48k-192k-short0"),
        first_frame: 2,
        frames: 2,
        // blksw выставлен вручную, CRC не пересчитан (см. generate.sh).
        crc: CrcPolicy::Skip,
        path: ExercisedPath::ShortBlocks,
    },
    ReferenceExcerpt {
        stream: "stereo-48k-256k-rematrix.ac3",
        reference: reference!("stereo-48k-256k-rematrix"),
        first_frame: 2,
        frames: 2,
        crc: CrcPolicy::Verify,
        path: ExercisedPath::Rematrixing,
    },
    ReferenceExcerpt {
        stream: "3f2r-48k-448k-cpl.ac3",
        reference: reference!("3f2r-48k-448k-cpl"),
        first_frame: 2,
        frames: 1,
        crc: CrcPolicy::Verify,
        path: ExercisedPath::Coupling,
    },
    ReferenceExcerpt {
        stream: "5p1-48k-640k-annexd.ac3",
        reference: reference!("5p1-48k-640k-annexd"),
        first_frame: 2,
        frames: 1,
        crc: CrcPolicy::Verify,
        path: ExercisedPath::LowFrequencyEffects,
    },
    ReferenceExcerpt {
        stream: "overload-stereo-48k-192k.ac3",
        reference: reference!("overload-stereo-48k-192k"),
        first_frame: 2,
        frames: 2,
        crc: CrcPolicy::Verify,
        path: ExercisedPath::Overload,
    },
    ReferenceExcerpt {
        stream: "stereo-44k1-128k-plain.ac3",
        reference: reference!("stereo-44k1-128k-plain"),
        first_frame: 2,
        frames: 2,
        crc: CrcPolicy::Verify,
        path: ExercisedPath::PlainLongBlocks,
    },
    ReferenceExcerpt {
        stream: "2f1r-32k-192k.ac3",
        reference: reference!("2f1r-32k-192k"),
        first_frame: 2,
        frames: 1,
        crc: CrcPolicy::Verify,
        path: ExercisedPath::MonoSurround,
    },
];

/// Остаток вне сетки ffmpeg должен быть ниже сигнала хотя бы на столько.
const BEYOND_GRID_SNR_DB: f64 = 90.0;
/// Допуск совпадения энергии dither-а.
const DITHER_ENERGY_TOLERANCE_DB: f64 = 3.0;
/// LFE без dither-а: совпадение до округления f32.
const LFE_PSNR_DB: f64 = 120.0;
/// Поиск задержки ±256 отсчётов.
const MAX_LAG: usize = 256;

/// Выдержка эталона, разложенная на каналы и сдвинутая на своё место в потоке
/// (до выдержки — нули, они в сравнение не попадают).
fn placed_reference(excerpt: &ReferenceExcerpt, positions: &[ChannelPosition]) -> Vec<Vec<f32>> {
    let offset = excerpt.first_frame * FRAME_SAMPLES;
    deinterleave_ffmpeg_order(excerpt.reference, positions)
        .into_iter()
        .map(|channel| {
            assert_eq!(
                channel.len(),
                excerpt.frames * FRAME_SAMPLES,
                "{}",
                excerpt.stream
            );
            let mut placed = vec![0.0; offset];
            placed.extend(channel);
            placed
        })
        .collect()
}

/// Сколько разобранных блоков задействуют путь выдержки.
fn path_blocks(
    decoded: &OurDecode,
    block_range: std::ops::Range<usize>,
    path: ExercisedPath,
) -> usize {
    block_range
        .filter(|&block| {
            let infos = &decoded.blocks[block];
            match path {
                ExercisedPath::ShortBlocks => infos
                    .iter()
                    .any(|info| info.transform == TransformLength::ShortPair),
                ExercisedPath::Rematrixing => infos.iter().any(|info| info.rematrixed),
                ExercisedPath::Coupling => {
                    infos.iter().any(|info| info.coupling_start_bin.is_some())
                }
                ExercisedPath::LowFrequencyEffects => decoded
                    .positions
                    .contains(&ChannelPosition::LowFrequencyEffects),
                ExercisedPath::StreamStart => block == 0,
                ExercisedPath::MonoSurround => {
                    decoded.positions.contains(&ChannelPosition::CenterSurround)
                }
                ExercisedPath::Overload => true,
                ExercisedPath::PlainLongBlocks => infos.iter().all(|info| {
                    info.transform == TransformLength::Long
                        && !info.rematrixed
                        && info.coupling_start_bin.is_none()
                }),
            }
        })
        .count()
}

#[test]
fn committed_ffmpeg_excerpts_agree_within_reference_precision() {
    let analysis = Analysis::new();
    for excerpt in &EXCERPTS {
        let frames = split_frames(fixture(excerpt.stream));
        let decoded = decode_both(&frames, excerpt.crc, DynamicRangeControl::Standard);
        assert!(
            decoded.valid.iter().all(|&valid| valid),
            "{}",
            excerpt.stream
        );
        let reference = placed_reference(excerpt, &decoded.positions);
        let excerpt_start = excerpt.first_frame * FRAME_SAMPLES;
        let excerpt_end = excerpt_start + excerpt.frames * FRAME_SAMPLES;
        // Задержка 0: наш отсчёт n — отсчёт n ffmpeg (в т. ч. с первого кадра).
        let lag = best_lag(
            &reference[0],
            &decoded.plain[0],
            excerpt_start + MAX_LAG,
            excerpt_end - MAX_LAG,
            MAX_LAG,
        );
        assert_eq!(lag, 0, "{}", excerpt.stream);
        // Блоки, чьё окно целиком внутри выдержки.
        let first_block = excerpt_start / SAMPLES_PER_AUDIO_BLOCK;
        let last_block = (excerpt_end - SPAN) / SAMPLES_PER_AUDIO_BLOCK;
        let glitch = decoded.first_short_block();
        let blocks: Vec<usize> = (first_block..=last_block)
            .filter(|&block| !glitch.is_some_and(|glitch| block == glitch || block + 1 == glitch))
            .collect();
        assert!(blocks.len() >= 5, "{}", excerpt.stream);
        let mut total = SpectralTally::default();
        for (channel, position) in decoded.positions.iter().enumerate() {
            let mut tally = SpectralTally::default();
            for &block in &blocks {
                let info = &decoded.blocks[block][channel];
                tally.add_block(
                    &analysis,
                    info,
                    &reference[channel],
                    &decoded.plain[channel],
                    &decoded.dithered[channel],
                    block * SAMPLES_PER_AUDIO_BLOCK,
                );
            }
            assert!(
                tally.beyond_grid_snr_db() >= BEYOND_GRID_SNR_DB,
                "{} {position:?}: residual beyond ffmpeg grid {:.1} dB (max {:.1} steps)",
                excerpt.stream,
                tally.beyond_grid_snr_db(),
                tally.max_clean_steps
            );
            total.merge(&tally);
        }
        if let Some(ratio) = total.dither_energy_ratio_db() {
            assert!(
                ratio.abs() <= DITHER_ENERGY_TOLERANCE_DB,
                "{}: dither {ratio:.2} dB",
                excerpt.stream
            );
        }
        let exercised = path_blocks(&decoded, first_block..last_block + 1, excerpt.path);
        assert!(
            exercised > 0,
            "{}: path {:?} not exercised",
            excerpt.stream,
            excerpt.path
        );
        check_path_specific_outcome(excerpt, &decoded, &reference, excerpt_start..excerpt_end);
    }
}

/// Проверки, которые нужны конкретному пути сверх спектрального сравнения.
fn check_path_specific_outcome(
    excerpt: &ReferenceExcerpt,
    decoded: &OurDecode,
    reference: &[Vec<f32>],
    samples: std::ops::Range<usize>,
) {
    match excerpt.path {
        ExercisedPath::LowFrequencyEffects => {
            let lfe = decoded
                .positions
                .iter()
                .position(|&position| position == ChannelPosition::LowFrequencyEffects)
                .unwrap();
            let error: f64 = samples
                .clone()
                .map(|index| {
                    (f64::from(reference[lfe][index]) - f64::from(decoded.plain[lfe][index]))
                        .powi(2)
                })
                .sum();
            let psnr = ratio_db(samples.len() as f64, error);
            assert!(psnr >= LFE_PSNR_DB, "LFE PSNR {psnr:.1} dB");
        }
        ExercisedPath::Overload => {
            let peak = |channels: &[Vec<f32>]| {
                channels
                    .iter()
                    .flat_map(|channel| channel[samples.clone()].iter())
                    .fold(0.0f32, |peak, &value| peak.max(value.abs()))
            };
            // Оба декодера выдают > 0 dBFS — значит, ни один не обрезал.
            assert!(peak(&decoded.plain) > 1.05, "ours {}", peak(&decoded.plain));
            assert!(peak(reference) > 1.05, "ffmpeg {}", peak(reference));
        }
        _ => {}
    }
}
