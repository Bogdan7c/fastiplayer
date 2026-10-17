//! Совпадение PCM E-AC-3 с декодером ffmpeg на коммитнутых выдержках
//! (`tests/fixtures/eac3/*.ffmpeg-f32le`, команды — в `generate.sh`).
//!
//! Метод — `eac3_differential` (как D2): остаток по блокам раскладывается
//! прямым MDCT; бины без dither-а обязаны совпасть в пределах сетки
//! фиксированной точки ffmpeg, dither-бины — по энергии. Выдержки
//! покрывают начало потока (нулевые хвосты, задержка 0), 3/2 + LFE и кадры
//! по 3 блока при 32 кГц (`numblkscod = 2`).

use super::support::{eac3_fixture, split_access_units};
use crate::config::{CrcPolicy, DynamicRangeControl};
use crate::output::ChannelPosition;
use crate::syntax::syncframe::SAMPLES_PER_AUDIO_BLOCK;
use crate::tests::eac3_differential::{analyze_channels, decode_stream};
use crate::tests::reference_comparison::deinterleave_ffmpeg_order;

/// Порог «совпадения до округления» (как у AC-3 в D2).
const CLEAN_THRESHOLD_DB: f64 = 90.0;
/// Допуск энергии dither-а (два независимых равномерных шума).
const DITHER_TOLERANCE_DB: f64 = 3.0;

struct Excerpt {
    stream: &'static str,
    reference: &'static [u8],
    /// Отсчёты потока на канал `[first, end)` (`reference <name> <from> <to>`).
    first_sample: usize,
    end_sample: usize,
    positions: &'static [ChannelPosition],
}

macro_rules! reference {
    ($name:literal) => {
        include_bytes!(concat!(
            "../../../tests/fixtures/eac3/",
            $name,
            ".ffmpeg-f32le"
        ))
    };
}

const FIVE_ONE: [ChannelPosition; 6] = [
    ChannelPosition::Left,
    ChannelPosition::Center,
    ChannelPosition::Right,
    ChannelPosition::LeftSurround,
    ChannelPosition::RightSurround,
    ChannelPosition::LowFrequencyEffects,
];

const EXCERPTS: [Excerpt; 3] = [
    Excerpt {
        stream: "stereo-48k-96k.eac3",
        reference: reference!("stereo-48k-96k"),
        first_sample: 0,
        end_sample: 3072,
        positions: &[ChannelPosition::Left, ChannelPosition::Right],
    },
    Excerpt {
        stream: "5p1-48k-384k.eac3",
        reference: reference!("5p1-48k-384k"),
        first_sample: 3072,
        end_sample: 4608,
        positions: &FIVE_ONE,
    },
    Excerpt {
        stream: "5p1-32k-1000k.eac3",
        reference: reference!("5p1-32k-1000k"),
        first_sample: 3072,
        end_sample: 4608,
        positions: &FIVE_ONE,
    },
];

#[test]
fn eac3_pcm_matches_ffmpeg_up_to_its_fixed_point_grid_and_dither() {
    for excerpt in &EXCERPTS {
        let units = split_access_units(eac3_fixture(excerpt.stream));
        let decoded = decode_stream(&units, CrcPolicy::Verify, DynamicRangeControl::Standard);
        assert_eq!(decoded.positions, excerpt.positions, "{}", excerpt.stream);
        // Эталон кладётся на временную ось потока (вне выдержки — нули, туда анализ не смотрит).
        let excerpt_channels = deinterleave_ffmpeg_order(excerpt.reference, &decoded.positions);
        let total = decoded.plain[0].len();
        let reference: Vec<Vec<f32>> = excerpt_channels
            .iter()
            .map(|samples| {
                assert_eq!(
                    samples.len(),
                    excerpt.end_sample - excerpt.first_sample,
                    "{}",
                    excerpt.stream
                );
                let mut full = vec![0.0; total];
                full[excerpt.first_sample..excerpt.end_sample].copy_from_slice(samples);
                full
            })
            .collect();
        let blocks = excerpt.first_sample / SAMPLES_PER_AUDIO_BLOCK
            ..excerpt.end_sample / SAMPLES_PER_AUDIO_BLOCK;
        let results = analyze_channels(&decoded, &reference, blocks);
        let mut analysed_channels = 0;
        for channel in &results {
            let beyond = channel.tally.beyond_grid_snr_db();
            assert!(
                beyond >= CLEAN_THRESHOLD_DB,
                "{} {:?}: clean residual beyond ffmpeg grid {beyond:.1} dB",
                excerpt.stream,
                channel.position
            );
            if let Some(ratio) = channel.tally.dither_ratio_db() {
                assert!(
                    ratio.abs() <= DITHER_TOLERANCE_DB,
                    "{} {:?}: dither energy {ratio:+.2} dB",
                    excerpt.stream,
                    channel.position
                );
            }
            // Анализ не пуст: сигнал есть, и это не пропуск всех блоков.
            assert!(
                channel.tally.signal > 0.0,
                "{} {:?}",
                excerpt.stream,
                channel.position
            );
            analysed_channels += 1;
        }
        assert_eq!(analysed_channels, excerpt.positions.len());
    }
}
