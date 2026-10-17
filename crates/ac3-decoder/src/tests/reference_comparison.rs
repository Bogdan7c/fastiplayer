//! Общая часть сверки с PCM ffmpeg: наш декод (без dither-а и с ним), маски
//! dither-бинов, спектральный разбор остатка прямым MDCT §8.2.3.2 и модель
//! фиксированной точки ffmpeg. Используется коммитнутым тестом
//! `pcm_reference` (выдержки в `tests/fixtures/*.ffmpeg-f32le`) и dev-прогоном
//! `ffmpeg_differential`.
//!
//! Почему спектр: кодер ffmpeg всегда ставит `dithflag = 1`, и в бинах без
//! бит (bap = 0) ffmpeg кладёт свой шум, а мы — свой (или ноль). Разность PCM
//! поэтому не может быть «нулём до округления». TDAC ортогонален, и прямое
//! MDCT остатка по 512 отсчётам блока точно возвращает разность
//! коэффициентов этого блока (проверено тестом точного восстановления в
//! `synthesis::transform::tests`). Дальше бины делятся на dither-бины (там
//! наш шум ≠ 0) и чистые: в чистых разность обязана объясняться сеткой
//! ffmpeg, в dither-бинах энергия остатка должна совпадать с энергией нашего
//! шума (ffmpeg кладёт туда шум той же амплитуды).

use crate::config::{CrcPolicy, DecoderConfig, DynamicRangeControl};
use crate::decoder::Ac3Decoder;
use crate::output::ChannelPosition;
use crate::read_syncframe_info;
use crate::spectral::COEFFICIENTS_PER_BLOCK;
use crate::spectral::dither::DitherMode;
use crate::syntax::audblk::output::{DecodedAudioBlock, TransformLength};
use crate::syntax::syncframe::{AUDIO_BLOCKS_PER_SYNCFRAME, SAMPLES_PER_AUDIO_BLOCK};
use crate::synthesis::dynamic_range::GainWords;
use crate::synthesis::rematrix::{StereoPairBandwidth, rematrixed_ranges};
use crate::synthesis::transform::window::kbd_half_window;

pub(super) const FRAME_SAMPLES: usize = AUDIO_BLOCKS_PER_SYNCFRAME * SAMPLES_PER_AUDIO_BLOCK;
/// Отсчётов PCM, которые покрывает окно одного блока.
pub(super) const SPAN: usize = 2 * SAMPLES_PER_AUDIO_BLOCK;
/// `cplstrtmant = 37 + 12·cplbegf` (§5.4.3.11).
const COUPLING_START_BASE_BIN: usize = 37;
const BINS_PER_COUPLING_SUBBAND: usize = 12;

/// ffmpeg (n9.0.2) хранит мантиссы с фиксированной точкой и сдвигает их на
/// экспоненту целочисленно: коэффициент = ⌊m·2^23 / 2^exp⌋ · 2^−23. Это
/// видно по анализу остатка: в почти тишине коэффициенты ffmpeg — целые
/// кратные 2^−23, например 8/11·2^−19 (уровень bap 4) у нас = 11,64 шага, у
/// ffmpeg ровно 11. Наши коэффициенты — точное значение §7.3 (мантисса ×
/// 2^−exp) в f32. Допуск — 2 шага (по округлению на каждый канал
/// rematrix-суммы L' ± R').
pub(super) const FFMPEG_COEFFICIENT_GRID: f64 = 1.0 / 8_388_608.0;
const FFMPEG_GRID_TOLERANCE_STEPS: f64 = 2.0;
/// В coupling-полосах ffmpeg округляет мантиссу coupling-канала до
/// умножения на координату × 8 (§7.4.3), поэтому шаг сетки усиливается до 8
/// раз (координата < 1): например, бин 226 стерео-потока THX — 2/3·2^−17 у
/// нас и 5/8·2^−17 у ffmpeg в обоих каналах с разными координатами.
const FFMPEG_COUPLING_AMPLIFICATION: f64 = 8.0;
/// Наш dither должен быть хотя бы в 16 шагов сетки ffmpeg, чтобы сравнивать энергии.
const DITHER_RESOLVABLE_STEPS: f64 = 16.0;
/// Меньше бинов — энергия двух независимых шумов сравнивается со слишком
/// большим статистическим разбросом (5 бинов дают ±5 дБ).
const DITHER_MIN_BINS_FOR_ENERGY_CHECK: usize = 500;

/// Матрицы прямого MDCT §8.2.3.2 с окном Таблицы 7.33: длинное и два коротких.
pub(super) struct Analysis {
    long: Vec<f64>,
    short_first: Vec<f64>,
    short_second: Vec<f64>,
}

/// `XD[k] = −2/N · Σ x[n]·w[n]·cos(2π/(4N)·(2n+1)(2k+1) + π/4·(2k+1)(1+α))`.
fn mdct_matrix(length: usize, alpha: f64, window: impl Fn(usize) -> f64) -> Vec<f64> {
    let mut matrix = vec![0.0; length / 2 * length];
    for k in 0..length / 2 {
        for n in 0..length {
            let phase = 2.0 * std::f64::consts::PI / (4 * length) as f64
                * ((2 * n + 1) * (2 * k + 1)) as f64
                + std::f64::consts::PI / 4.0 * (2 * k + 1) as f64 * (1.0 + alpha);
            matrix[k * length + n] = -2.0 / length as f64 * window(n) * phase.cos();
        }
    }
    matrix
}

impl Analysis {
    pub(super) fn new() -> Self {
        let half = kbd_half_window();
        let full = move |n: usize| half[if n < SPAN / 2 { n } else { SPAN - 1 - n }];
        Self {
            long: mdct_matrix(SPAN, 0.0, full),
            short_first: mdct_matrix(SPAN / 2, -1.0, full),
            short_second: mdct_matrix(SPAN / 2, 1.0, move |n| full(n + SPAN / 2)),
        }
    }

    fn apply(matrix: &[f64], segment: &[f64], output: &mut [f64], stride: usize, offset: usize) {
        let length = segment.len();
        for k in 0..length / 2 {
            let row = &matrix[k * length..(k + 1) * length];
            output[k * stride + offset] = row.iter().zip(segment).map(|(a, b)| a * b).sum();
        }
    }

    /// Коэффициенты блока по 512 отсчётам PCM, начиная с первого отсчёта блока.
    pub(super) fn coefficients(
        &self,
        segment: &[f64],
        length: TransformLength,
    ) -> [f64; COEFFICIENTS_PER_BLOCK] {
        let mut output = [0.0; COEFFICIENTS_PER_BLOCK];
        match length {
            TransformLength::Long => Self::apply(&self.long, segment, &mut output, 1, 0),
            TransformLength::ShortPair => {
                Self::apply(&self.short_first, &segment[..SPAN / 2], &mut output, 2, 0);
                Self::apply(&self.short_second, &segment[SPAN / 2..], &mut output, 2, 1);
            }
        }
        output
    }
}

/// Что известно о блоке канала из нашего разбора.
#[derive(Clone, Copy)]
pub(super) struct BlockInfo {
    pub(super) transform: TransformLength,
    /// Бины, где наш dither ненулевой (после rematrixing — объединение L/R).
    pub(super) dither: [bool; COEFFICIENTS_PER_BLOCK],
    /// Шаг сетки ffmpeg в этом блоке с учётом множителя DRC.
    pub(super) grid_step: f64,
    /// Первый бин coupling (`cplstrtmant`), если coupling включён.
    pub(super) coupling_start_bin: Option<usize>,
    /// В блоке есть полоса с `rematflg = 1`.
    pub(super) rematrixed: bool,
}

impl BlockInfo {
    /// Допуск расхождения с ffmpeg в бине, объяснимый его сеткой.
    pub(super) fn tolerance(&self, bin: usize) -> f64 {
        let coupled = self.coupling_start_bin.is_some_and(|start| bin >= start);
        let steps = if coupled {
            FFMPEG_GRID_TOLERANCE_STEPS + FFMPEG_COUPLING_AMPLIFICATION
        } else {
            FFMPEG_GRID_TOLERANCE_STEPS
        };
        self.grid_step * steps
    }
}

/// Сведения о блоках всех каналов (LFE последним).
fn block_infos(
    plain: &DecodedAudioBlock,
    dithered: &DecodedAudioBlock,
    full_bandwidth_channels: usize,
    lfe: bool,
    gain: f64,
) -> Vec<BlockInfo> {
    let grid_step = FFMPEG_COEFFICIENT_GRID * gain.max(1.0);
    let coupling_start_bin = plain
        .coupling_begin_subband
        .map(|begin| COUPLING_START_BASE_BIN + BINS_PER_COUPLING_SUBBAND * begin);
    let rematrixed = plain
        .rematrix
        .is_some_and(|flags| flags.flags.iter().take(flags.band_count).any(|&flag| flag));
    let mut infos: Vec<BlockInfo> = (0..full_bandwidth_channels)
        .map(|channel| {
            let (a, b) = (
                &plain.full_bandwidth[channel],
                &dithered.full_bandwidth[channel],
            );
            BlockInfo {
                transform: a.transform,
                dither: std::array::from_fn(|bin| a.coefficients[bin] != b.coefficients[bin]),
                grid_step,
                coupling_start_bin,
                rematrixed,
            }
        })
        .collect();
    if let Some(flags) = plain.rematrix.as_ref() {
        let bandwidth = StereoPairBandwidth {
            coupling_begin_subband: plain.coupling_begin_subband,
            left_end_bin: plain.full_bandwidth[0].end_bin,
            right_end_bin: plain.full_bandwidth[1].end_bin,
        };
        for range in rematrixed_ranges(flags, bandwidth) {
            for bin in range {
                let either = infos[0].dither[bin] || infos[1].dither[bin];
                infos[0].dither[bin] = either;
                infos[1].dither[bin] = either;
            }
        }
    }
    if lfe {
        infos.push(BlockInfo {
            transform: TransformLength::Long,
            dither: [false; COEFFICIENTS_PER_BLOCK],
            grid_step,
            coupling_start_bin: None,
            rematrixed: false,
        });
    }
    infos
}

/// Результат нашего декодирования потока двумя декодерами.
pub(super) struct OurDecode {
    pub(super) positions: Vec<ChannelPosition>,
    /// PCM без dither-а по каналам.
    pub(super) plain: Vec<Vec<f32>>,
    /// PCM с dither-ом по умолчанию.
    pub(super) dithered: Vec<Vec<f32>>,
    /// По блоку потока — сведения по каналам (пусто у отвергнутых кадров).
    pub(super) blocks: Vec<Vec<BlockInfo>>,
    /// Кадр декодирован (не отвергнут).
    pub(super) valid: Vec<bool>,
}

impl OurDecode {
    /// Первый блок потока с коротким преобразованием.
    ///
    /// Дефект эталона: ffmpeg n9.0.2 портит выходной блок, в котором ВПЕРВЫЕ
    /// за время жизни декодера встречается короткое преобразование, — во всех
    /// полнополосных каналах, кроме первого, даже если короткий блок только у
    /// одного канала. Воспроизведено на контролируемом потоке (blksw выставлен
    /// в кадрах 10 и 20 тонального 5.1): в кадре 10 ffmpeg отклоняется от
    /// исходного тона на 0,3 в каналах без коротких блоков, наш выход — на
    /// 5·10⁻⁶; в кадре 20 (вторая встреча) оба совпадают. Этот выходной блок и
    /// два спектральных окна, которые его покрывают, из сравнения исключаются.
    pub(super) fn first_short_block(&self) -> Option<usize> {
        self.blocks.iter().position(|block| {
            block
                .iter()
                .any(|info| info.transform == TransformLength::ShortPair)
        })
    }
}

/// Декодирует кадры без dither-а и с dither-ом по умолчанию.
pub(super) fn decode_both(
    frames: &[&[u8]],
    crc: CrcPolicy,
    dynamic_range: DynamicRangeControl,
) -> OurDecode {
    let config = |dither| DecoderConfig {
        crc,
        dither,
        dynamic_range,
        ..DecoderConfig::default()
    };
    let mut plain_decoder = Ac3Decoder::new(config(DitherMode::Disabled));
    let mut dithered_decoder = Ac3Decoder::new(config(DitherMode::default()));
    let mut result = OurDecode {
        positions: Vec::new(),
        plain: Vec::new(),
        dithered: Vec::new(),
        blocks: Vec::new(),
        valid: Vec::new(),
    };
    for frame in frames {
        let plain_result = plain_decoder.decode_access_unit(frame);
        let dithered_result = dithered_decoder.decode_access_unit(frame);
        let (Ok(plain), Ok(dithered)) = (plain_result.as_ref(), dithered_result) else {
            assert!(plain_result.is_err(), "dither changed frame validity");
            for channel in 0..result.plain.len() {
                result.plain[channel].extend(std::iter::repeat_n(0.0, FRAME_SAMPLES));
                result.dithered[channel].extend(std::iter::repeat_n(0.0, FRAME_SAMPLES));
            }
            result
                .blocks
                .extend(std::iter::repeat_n(Vec::new(), AUDIO_BLOCKS_PER_SYNCFRAME));
            result.valid.push(false);
            continue;
        };
        if result.positions.is_empty() {
            // Отвергнутые кадры до первого удачного — тишина той же раскладки.
            let leading = vec![0.0; result.valid.len() * FRAME_SAMPLES];
            result.positions = plain.channels().to_vec();
            result.plain = vec![leading.clone(); result.positions.len()];
            result.dithered = vec![leading; result.positions.len()];
        }
        assert_eq!(
            plain.channels(),
            result.positions,
            "layout changed mid-stream"
        );
        for channel in 0..result.positions.len() {
            result.plain[channel].extend_from_slice(plain.channel_samples(channel).unwrap());
            result.dithered[channel].extend_from_slice(dithered.channel_samples(channel).unwrap());
        }
        let lfe = result.positions.last() == Some(&ChannelPosition::LowFrequencyEffects);
        let full_bandwidth = result.positions.len() - usize::from(lfe);
        let bsi = read_syncframe_info(frame).unwrap().bsi;
        let first_compr = bsi.program.compression_gain_word;
        let second_compr = bsi
            .dual_mono_second_program
            .and_then(|program| program.compression_gain_word);
        let policy_gain = |word, heavy_compression| {
            f64::from(dynamic_range.gain(GainWords {
                dynamic_range: word,
                heavy_compression,
            }))
        };
        for (plain_block, dithered_block) in plain_decoder
            .decoded_blocks()
            .iter()
            .zip(dithered_decoder.decoded_blocks())
        {
            let gain = policy_gain(plain_block.dynamic_range_word, first_compr).max(policy_gain(
                plain_block.dual_mono_dynamic_range_word,
                second_compr,
            ));
            result.blocks.push(block_infos(
                plain_block,
                dithered_block,
                full_bandwidth,
                lfe,
                gain,
            ));
        }
        result.valid.push(true);
    }
    result
}

/// Спектральные итоги по набору блоков одного канала.
#[derive(Default, Clone, Copy)]
pub(super) struct SpectralTally {
    pub(super) signal: f64,
    pub(super) residual_clean: f64,
    pub(super) residual_dither: f64,
    pub(super) our_dither: f64,
    /// Σ (|R| − допуск сетки)² по чистым бинам.
    pub(super) excess_clean: f64,
    /// Наибольший |R| в чистых бинах в шагах сетки ffmpeg.
    pub(super) max_clean_steps: f64,
    pub(super) dithered_bins: usize,
    pub(super) blocks: usize,
}

/// Отрезок `[start, start + SPAN)` разности двух сигналов в f64.
fn difference(minuend: &[f32], subtrahend: Option<&[f32]>, start: usize) -> Vec<f64> {
    (start..start + SPAN)
        .map(|index| {
            f64::from(minuend[index]) - subtrahend.map_or(0.0, |other| f64::from(other[index]))
        })
        .collect()
}

impl SpectralTally {
    /// Разбирает блок, окно которого начинается с отсчёта `start`.
    pub(super) fn add_block(
        &mut self,
        analysis: &Analysis,
        info: &BlockInfo,
        reference: &[f32],
        plain: &[f32],
        dithered: &[f32],
        start: usize,
    ) {
        let signal = analysis.coefficients(&difference(reference, None, start), info.transform);
        let residual =
            analysis.coefficients(&difference(reference, Some(plain), start), info.transform);
        let our_dither =
            analysis.coefficients(&difference(dithered, Some(plain), start), info.transform);
        for bin in 0..COEFFICIENTS_PER_BLOCK {
            self.signal += signal[bin] * signal[bin];
            if info.dither[bin] {
                self.dithered_bins += 1;
                self.residual_dither += residual[bin] * residual[bin];
                self.our_dither += our_dither[bin] * our_dither[bin];
            } else {
                self.residual_clean += residual[bin] * residual[bin];
                let excess = (residual[bin].abs() - info.tolerance(bin)).max(0.0);
                self.excess_clean += excess * excess;
                self.max_clean_steps = self
                    .max_clean_steps
                    .max(residual[bin].abs() / info.grid_step);
            }
        }
        self.blocks += 1;
    }

    pub(super) fn merge(&mut self, other: &Self) {
        self.signal += other.signal;
        self.residual_clean += other.residual_clean;
        self.residual_dither += other.residual_dither;
        self.our_dither += other.our_dither;
        self.excess_clean += other.excess_clean;
        self.max_clean_steps = self.max_clean_steps.max(other.max_clean_steps);
        self.dithered_bins += other.dithered_bins;
        self.blocks += other.blocks;
    }

    /// Энергия сигнала к остатку в чистых бинах, дБ.
    pub(super) fn clean_snr_db(&self) -> f64 {
        ratio_db(self.signal, self.residual_clean)
    }

    /// Энергия сигнала к остатку, не объяснённому сеткой ffmpeg, дБ.
    pub(super) fn beyond_grid_snr_db(&self) -> f64 {
        ratio_db(self.signal, self.excess_clean)
    }

    /// Энергия остатка в dither-бинах к энергии нашего dither-а, дБ, если
    /// сравнение статистически осмысленно.
    pub(super) fn dither_energy_ratio_db(&self) -> Option<f64> {
        let rms = (self.our_dither / self.dithered_bins.max(1) as f64).sqrt();
        let resolvable = rms > DITHER_RESOLVABLE_STEPS * FFMPEG_COEFFICIENT_GRID
            && self.dithered_bins >= DITHER_MIN_BINS_FOR_ENERGY_CHECK;
        resolvable.then(|| ratio_db(self.residual_dither, self.our_dither))
    }
}

/// `10·log10(a/b)`; 999 при нулевом знаменателе.
pub(super) fn ratio_db(numerator: f64, denominator: f64) -> f64 {
    if denominator == 0.0 {
        999.0
    } else {
        10.0 * (numerator / denominator).log10()
    }
}

/// Сдвиг (в отсчётах) нашего сигнала относительно эталона с наименьшей
/// энергией разности на отрезке `[start, end)`; перебор ±`max_lag`.
pub(super) fn best_lag(
    reference: &[f32],
    ours: &[f32],
    start: usize,
    end: usize,
    max_lag: usize,
) -> isize {
    let max_lag = max_lag as isize;
    (-max_lag..=max_lag)
        .filter(|lag| {
            let first = start as isize + lag;
            let last = end as isize + lag;
            first >= 0 && last <= ours.len() as isize && end <= reference.len()
        })
        .map(|lag| {
            let energy: f64 = (start..end)
                .map(|index| {
                    let shifted = (index as isize + lag) as usize;
                    (f64::from(reference[index]) - f64::from(ours[shifted])).powi(2)
                })
                .sum();
            (lag, energy)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(0, |(lag, _)| lag)
}

/// Ранг позиции в порядке каналов ffmpeg (по номеру бита в AVChannelLayout:
/// FL 0, FR 1, FC 2, LFE 3, BL 4, BR 5, BC 8, SL 9, SR 10). Mono-surround S
/// режимов 2/1 и 3/1 ffmpeg отдаёт как BC, Ls/Rs режимов 2/2 и 3/2 — как
/// SL/SR, Lrs/Rrs зависимых substream-ов E-AC-3 (7.1) — как BL/BR
/// (проверено корреляцией каналов на реальных 7.1-потоках).
pub(super) fn ffmpeg_channel_rank(position: ChannelPosition) -> u8 {
    match position {
        ChannelPosition::Left => 0,
        ChannelPosition::Right => 1,
        ChannelPosition::Center => 2,
        ChannelPosition::LowFrequencyEffects => 3,
        ChannelPosition::LeftRearSurround => 4,
        ChannelPosition::RightRearSurround => 5,
        ChannelPosition::CenterSurround => 8,
        ChannelPosition::LeftSurround => 9,
        ChannelPosition::RightSurround => 10,
        other => panic!("{other:?} does not occur in the test streams"),
    }
}

/// Interleaved f32le в порядке ffmpeg → каналы в порядке `positions`.
pub(super) fn deinterleave_ffmpeg_order(
    bytes: &[u8],
    positions: &[ChannelPosition],
) -> Vec<Vec<f32>> {
    let samples: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect();
    let count = positions.len();
    let mut ffmpeg_order: Vec<usize> = (0..count).collect();
    ffmpeg_order.sort_by_key(|&channel| ffmpeg_channel_rank(positions[channel]));
    let mut channels = vec![Vec::with_capacity(samples.len() / count); count];
    for frame in samples.chunks_exact(count) {
        for (column, &channel) in ffmpeg_order.iter().enumerate() {
            channels[channel].push(frame[column]);
        }
    }
    channels
}
