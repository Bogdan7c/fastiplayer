//! Dev-сверка PCM E-AC-3 (и access unit-ов «ядро AC-3 + зависимые») с
//! бинарником ffmpeg: `#[ignore]`, нужен каталог от `tools/eac3_sweep.py`.
//!
//! Запуск:
//! `AC3_DECODER_EAC3_SWEEP_DIR=<dir> cargo test -p ac3-decoder --release eac3_ffmpeg_differential -- --ignored --nocapture`
//!
//! Методика — как у `ffmpeg_differential` (пакет D2), с поправками на
//! инструменты Annex E. Каждый поток декодируется тремя декодерами без TPNP
//! (ffmpeg его не применяет, см. `tools/eac3_sweep.py`):
//! - «plain» — без dither-а;
//! - «dithered» — dither по умолчанию;
//! - «perturbed» — без dither-а, но с другим зерном шума SPX.
//!
//! Остаток `ffmpeg − plain` по блокам раскладывается прямым MDCT, бины
//! делятся на три класса:
//! - SPX-шумовые (plain ≠ perturbed): там шум SPX ffmpeg и наш независимы;
//!   ожидание — энергия остатка ≈ энергии `plain − perturbed` (две
//!   независимые реализации шума единичной дисперсии дают одинаковую
//!   энергию разности), и энергия бинов ffmpeg ≈ нашей по полосам;
//! - dither (plain ≠ dithered, не SPX): энергия остатка ≈ энергии нашего dither-а;
//! - чистые: остаток обязан объясняться сеткой фиксированной точки ffmpeg
//!   (`reference_comparison::FFMPEG_COEFFICIENT_GRID`).
//!
//! `AC3_DECODER_SWEEP_FILTER=<подстрока>` — фильтр случаев;
//! `AC3_DEBUG_BANDS=1` — энергия ffmpeg/наша по 12-бинным полосам в
//! SPX-бинах и в целом по каналу (поиск ошибок масштаба SPX/AHT/coupling).

use std::collections::BTreeMap;
use std::path::Path;

use super::eac3::support::split_access_units;
use super::reference_comparison::{
    Analysis, FFMPEG_COEFFICIENT_GRID, SPAN, deinterleave_ffmpeg_order, ratio_db,
};
use crate::config::{CompressionScale, CrcPolicy, DecoderConfig, DynamicRangeControl};
use crate::decoder::Ac3Decoder;
use crate::output::ChannelPosition;
use crate::spectral::COEFFICIENTS_PER_BLOCK;
use crate::spectral::dither::DitherMode;
use crate::syntax::audblk::output::TransformLength;
use crate::syntax::syncframe::SAMPLES_PER_AUDIO_BLOCK;
use crate::synthesis::transient_pre_noise::TransientPreNoiseProcessing;

/// Порог «совпадения до округления».
const CLEAN_THRESHOLD_DB: f64 = 90.0;
/// Допуск совпадения энергий двух независимых шумов.
const NOISE_ENERGY_TOLERANCE_DB: f64 = 3.0;
/// Меньше бинов — сравнение энергий шума статистически бессмысленно.
const MIN_NOISE_BINS: usize = 500;
/// Наш шум должен быть хотя бы в 16 шагов сетки ffmpeg: мельче ffmpeg
/// округляет его к сетке, и энергии несравнимы (молчащий канал C, который
/// кодер ffmpeg пишет для 2/2+LFE, — экспоненты 24, dither ≈ 2^−24).
const NOISE_RESOLVABLE_STEPS: f64 = 16.0;
/// Допуск сетки ffmpeg в шагах: 2 вне coupling, +8 в coupling-полосах (см.
/// `reference_comparison`). Начало coupling блока E-AC-3 синтезу не нужно и
/// не сохраняется, поэтому допуск один — с запасом coupling-полос; что он не
/// маскирует ошибки, видно по `max-clean` в шагах.
const GRID_TOLERANCE_STEPS: f64 = 10.0;
/// Сколько блоков потока анализировать спектрально (стоимость O(N²) на блок).
const MAX_SPECTRAL_BLOCKS: usize = 1800;
/// Зерно шума SPX «perturbed»-декодера (любое, кроме зерна по умолчанию).
const PERTURBED_SPX_SEED: u32 = 0x1234_5678;
const BAND_BINS: usize = 12;
/// Наибольший кадр E-AC-3/AC-3 в отсчётах на канал.
const SAMPLES_PER_FRAME_MAX: usize = 6 * SAMPLES_PER_AUDIO_BLOCK;
const BANDS: usize = COEFFICIENTS_PER_BLOCK / BAND_BINS + 1;

/// Классы бинов одного блока одного канала.
#[derive(Clone)]
struct BlockClasses {
    transform: TransformLength,
    spx_noise: [bool; COEFFICIENTS_PER_BLOCK],
    dither: [bool; COEFFICIENTS_PER_BLOCK],
    grid_step: f64,
    /// Блок access unit-а с AHT (dither AHT считается отдельно).
    aht: bool,
}

pub(super) struct StreamDecode {
    pub(super) positions: Vec<ChannelPosition>,
    pub(super) plain: Vec<Vec<f32>>,
    dithered: Vec<Vec<f32>>,
    perturbed: Vec<Vec<f32>>,
    /// По блоку потока — классы по каналам (`None` у отвергнутых access unit-ов).
    blocks: Vec<Option<Vec<BlockClasses>>>,
    rejected_units: usize,
    access_units: usize,
}

impl StreamDecode {
    /// Первый блок с коротким преобразованием — дефект ffmpeg (см. `reference_comparison`).
    fn first_short_block(&self) -> Option<usize> {
        self.blocks.iter().position(|block| {
            block.as_ref().is_some_and(|channels| {
                channels
                    .iter()
                    .any(|classes| classes.transform == TransformLength::ShortPair)
            })
        })
    }
}

pub(super) fn dynamic_range(policy: &str) -> DynamicRangeControl {
    match policy {
        "off" => DynamicRangeControl::Disabled,
        "heavy" => DynamicRangeControl::Heavy,
        "scaled:50:50" => {
            let half = CompressionScale::from_percent(50).unwrap();
            DynamicRangeControl::Scaled {
                cut: half,
                boost: half,
            }
        }
        _ => DynamicRangeControl::Standard,
    }
}

/// ffmpeg TPNP не применяет — по умолчанию сверка без него; эталону Nero
/// (dev-сверка, см. память) нужно `AC3_DIFF_TPNP=apply`.
fn transient_pre_noise_policy() -> TransientPreNoiseProcessing {
    if std::env::var("AC3_DIFF_TPNP").is_ok_and(|value| value == "apply") {
        TransientPreNoiseProcessing::Apply
    } else {
        TransientPreNoiseProcessing::Skip
    }
}

fn decoders(crc: CrcPolicy, drc: DynamicRangeControl) -> [Ac3Decoder; 3] {
    let config = |dither| DecoderConfig {
        crc,
        dither,
        dynamic_range: drc,
        transient_pre_noise: transient_pre_noise_policy(),
    };
    let plain = Ac3Decoder::new(config(DitherMode::Disabled));
    let dithered = Ac3Decoder::new(config(DitherMode::default()));
    let mut perturbed = Ac3Decoder::new(config(DitherMode::Disabled));
    perturbed.reseed_spx_noise(PERTURBED_SPX_SEED);
    [plain, dithered, perturbed]
}

pub(super) fn decode_stream(
    units: &[Vec<u8>],
    crc: CrcPolicy,
    drc: DynamicRangeControl,
) -> StreamDecode {
    let [mut plain, mut dithered, mut perturbed] = decoders(crc, drc);
    let mut result = StreamDecode {
        positions: Vec::new(),
        plain: Vec::new(),
        dithered: Vec::new(),
        perturbed: Vec::new(),
        blocks: Vec::new(),
        rejected_units: 0,
        access_units: units.len(),
    };
    let mut last_length = 0usize;
    for unit in units {
        let outputs = [
            plain.decode_access_unit(unit),
            dithered.decode_access_unit(unit),
            perturbed.decode_access_unit(unit),
        ];
        let [Ok(a), Ok(b), Ok(c)] = &outputs else {
            assert!(outputs.iter().all(Result::is_err), "validity differs");
            result.rejected_units += 1;
            for channels in [
                &mut result.plain,
                &mut result.dithered,
                &mut result.perturbed,
            ] {
                for channel in channels.iter_mut() {
                    channel.extend(std::iter::repeat_n(0.0, last_length));
                }
            }
            result.blocks.extend(std::iter::repeat_n(
                None,
                last_length / SAMPLES_PER_AUDIO_BLOCK,
            ));
            continue;
        };
        if result.positions.is_empty() {
            result.positions = a.channels().to_vec();
            let count = result.positions.len();
            let leading = vec![0.0; result.blocks.len() * SAMPLES_PER_AUDIO_BLOCK];
            result.plain = vec![leading.clone(); count];
            result.dithered = vec![leading.clone(); count];
            result.perturbed = vec![leading; count];
        }
        assert_eq!(a.channels(), result.positions, "layout changed mid-stream");
        last_length = a.samples_per_channel();
        for channel in 0..result.positions.len() {
            result.plain[channel].extend_from_slice(a.channel_samples(channel).unwrap());
            result.dithered[channel].extend_from_slice(b.channel_samples(channel).unwrap());
            result.perturbed[channel].extend_from_slice(c.channel_samples(channel).unwrap());
        }
        let spectra = [
            plain.program_spectra(),
            dithered.program_spectra(),
            perturbed.program_spectra(),
        ];
        for block in 0..spectra[0].gains.len() {
            let grid_step = FFMPEG_COEFFICIENT_GRID * f64::from(spectra[0].gains[block]).max(1.0);
            let classes = (0..result.positions.len())
                .map(|channel| {
                    let [p, d, q] =
                        [0, 1, 2].map(|decoder| &spectra[decoder].channels[channel][block]);
                    BlockClasses {
                        transform: p.transform,
                        spx_noise: std::array::from_fn(|bin| {
                            p.coefficients[bin] != q.coefficients[bin]
                        }),
                        dither: std::array::from_fn(|bin| {
                            p.coefficients[bin] != d.coefficients[bin]
                        }),
                        grid_step,
                        aht: spectra[0].uses_aht,
                    }
                })
                .collect();
            result.blocks.push(Some(classes));
        }
    }
    result
}

/// Спектральные итоги одного канала.
#[derive(Clone, Copy)]
pub(super) struct Tally {
    pub(super) signal: f64,
    clean_residual: f64,
    clean_excess: f64,
    max_clean_steps: f64,
    dither_residual: f64,
    our_dither: f64,
    dither_bins: usize,
    aht_dither_residual: f64,
    aht_our_dither: f64,
    aht_dither_bins: usize,
    spx_residual: f64,
    our_spx_noise: f64,
    spx_bins: usize,
    /// Энергия бинов ffmpeg / наших (plain) по полосам: SPX-бины и все бины.
    band_reference_spx: [f64; BANDS],
    band_ours_spx: [f64; BANDS],
    band_reference_all: [f64; BANDS],
    band_ours_all: [f64; BANDS],
}

impl Default for Tally {
    fn default() -> Self {
        Self {
            signal: 0.0,
            clean_residual: 0.0,
            clean_excess: 0.0,
            max_clean_steps: 0.0,
            dither_residual: 0.0,
            our_dither: 0.0,
            dither_bins: 0,
            aht_dither_residual: 0.0,
            aht_our_dither: 0.0,
            aht_dither_bins: 0,
            spx_residual: 0.0,
            our_spx_noise: 0.0,
            spx_bins: 0,
            band_reference_spx: [0.0; BANDS],
            band_ours_spx: [0.0; BANDS],
            band_reference_all: [0.0; BANDS],
            band_ours_all: [0.0; BANDS],
        }
    }
}

struct ChannelSignals<'a> {
    reference: &'a [f32],
    plain: &'a [f32],
    dithered: &'a [f32],
    perturbed: &'a [f32],
}

fn segment(signal: &[f32], start: usize) -> Vec<f64> {
    signal[start..start + SPAN]
        .iter()
        .map(|&value| f64::from(value))
        .collect()
}

impl Tally {
    /// Энергия сигнала к остатку чистых бинов сверх сетки ffmpeg, дБ.
    pub(super) fn beyond_grid_snr_db(&self) -> f64 {
        ratio_db(self.signal, self.clean_excess)
    }

    /// Энергия остатка dither-бинов (вне и внутри кадров AHT) к нашей, дБ.
    pub(super) fn dither_ratio_db(&self) -> Option<f64> {
        Self::noise_ratio(
            self.dither_residual + self.aht_dither_residual,
            self.our_dither + self.aht_our_dither,
            self.dither_bins + self.aht_dither_bins,
        )
    }

    fn add_block(
        &mut self,
        analysis: &Analysis,
        classes: &BlockClasses,
        signals: &ChannelSignals<'_>,
        start: usize,
    ) {
        let transform = classes.transform;
        let mut reference = analysis.coefficients(&segment(signals.reference, start), transform);
        let plain = analysis.coefficients(&segment(signals.plain, start), transform);
        if std::env::var("AC3_FIT_BLOCK_GAIN").is_ok() {
            // Эталон с другим DRC (Nero): множитель блока подбирается по чистым бинам.
            let clean = |bin: &usize| !classes.spx_noise[*bin] && !classes.dither[*bin];
            let cross: f64 = (0..COEFFICIENTS_PER_BLOCK)
                .filter(clean)
                .map(|bin| reference[bin] * plain[bin])
                .sum();
            let power: f64 = (0..COEFFICIENTS_PER_BLOCK)
                .filter(clean)
                .map(|bin| plain[bin] * plain[bin])
                .sum();
            if power > 0.0 && cross != 0.0 {
                let gain = cross / power;
                for value in &mut reference {
                    *value /= gain;
                }
            }
        }
        let dithered = analysis.coefficients(&segment(signals.dithered, start), transform);
        let perturbed = analysis.coefficients(&segment(signals.perturbed, start), transform);
        let debug_steps: Option<f64> = std::env::var("AC3_DEBUG_BINS")
            .ok()
            .map(|value| value.parse().unwrap_or(1000.0));
        for bin in 0..COEFFICIENTS_PER_BLOCK {
            let residual = reference[bin] - plain[bin];
            if let Some(steps) = debug_steps
                && !classes.spx_noise[bin]
                && !classes.dither[bin]
                && residual.abs() > steps * classes.grid_step
            {
                println!(
                    "BIN start {start} block {} bin {bin} ff {:+.6e} ours {:+.6e} ratio {:+.4}",
                    start / SAMPLES_PER_AUDIO_BLOCK,
                    reference[bin],
                    plain[bin],
                    reference[bin] / plain[bin]
                );
            }
            let band = bin / BAND_BINS;
            self.signal += reference[bin] * reference[bin];
            self.band_reference_all[band] += reference[bin] * reference[bin];
            // Наш уровень — с dither-ом: эталоны тоже заполняют бины без бит шумом.
            self.band_ours_all[band] += dithered[bin] * dithered[bin];
            if classes.spx_noise[bin] {
                self.spx_bins += 1;
                self.spx_residual += residual * residual;
                let noise = plain[bin] - perturbed[bin];
                self.our_spx_noise += noise * noise;
                self.band_reference_spx[band] += reference[bin] * reference[bin];
                self.band_ours_spx[band] += plain[bin] * plain[bin];
            } else if classes.dither[bin] {
                let dither = dithered[bin] - plain[bin];
                if classes.aht {
                    self.aht_dither_bins += 1;
                    self.aht_dither_residual += residual * residual;
                    self.aht_our_dither += dither * dither;
                } else {
                    self.dither_bins += 1;
                    self.dither_residual += residual * residual;
                    self.our_dither += dither * dither;
                }
            } else {
                self.clean_residual += residual * residual;
                let tolerance = classes.grid_step * GRID_TOLERANCE_STEPS;
                let excess = (residual.abs() - tolerance).max(0.0);
                self.clean_excess += excess * excess;
                self.max_clean_steps = self.max_clean_steps.max(residual.abs() / classes.grid_step);
            }
        }
    }

    fn noise_ratio(residual: f64, ours: f64, bins: usize) -> Option<f64> {
        let rms = (ours / bins.max(1) as f64).sqrt();
        let resolvable = rms > NOISE_RESOLVABLE_STEPS * FFMPEG_COEFFICIENT_GRID;
        (bins >= MIN_NOISE_BINS && resolvable).then(|| ratio_db(residual, ours))
    }
}

pub(super) struct ChannelResult {
    pub(super) position: ChannelPosition,
    pub(super) psnr: f64,
    pub(super) snr: f64,
    pub(super) tally: Tally,
}

struct CaseResult {
    name: String,
    family: String,
    units: usize,
    rejected: usize,
    channels: Vec<ChannelResult>,
    glitch_block: Option<usize>,
}

fn run_case(directory: &Path, name: &str, family: &str, policy: &str, crc: &str) -> CaseResult {
    let read = |suffix: &str| std::fs::read(directory.join(format!("{name}{suffix}"))).unwrap();
    let stream = read(".eac3");
    let units = split_access_units(&stream);
    let crc = if crc == "skip" {
        CrcPolicy::Skip
    } else {
        CrcPolicy::Verify
    };
    crate::tables::eac3::vq::quirk::emulate_ffmpeg_hebap4_shift(
        std::env::var("AC3_EMULATE_FFMPEG_HEBAP4").is_ok(),
    );
    let decoded = decode_stream(&units, crc, dynamic_range(policy));
    let mut reference = deinterleave_ffmpeg_order(&read(".ref.f32"), &decoded.positions);
    let total = decoded.plain[0].len();
    // ffmpeg декодирует и обрезанный последний кадр файла (наш нарезчик его
    // отбрасывает): лишний хвост эталона не длиннее одного кадра срезается.
    let extra = reference[0].len().saturating_sub(total);
    assert!(
        extra <= SAMPLES_PER_FRAME_MAX,
        "{name}: ffmpeg has {extra} extra samples"
    );
    for channel in &mut reference {
        channel.truncate(total);
    }
    assert_eq!(
        reference[0].len(),
        total,
        "{name}: ffmpeg sample count differs"
    );
    let glitch_block = decoded.first_short_block();
    let blocks = decoded.blocks.len().min(MAX_SPECTRAL_BLOCKS);
    let channels = analyze_channels(&decoded, &reference, 0..blocks);
    CaseResult {
        name: name.to_owned(),
        family: family.to_owned(),
        units: decoded.access_units,
        rejected: decoded.rejected_units,
        channels,
        glitch_block,
    }
}

/// Метрики каналов по блокам `blocks` (анализ блока `b` захватывает отсчёты
/// `[256·b, 256·b + 512)`, поэтому используются блоки, у которых и следующий
/// внутри диапазона). Эталон выровнен с нашим выходом отсчёт в отсчёт.
pub(super) fn analyze_channels(
    decoded: &StreamDecode,
    reference: &[Vec<f32>],
    blocks: std::ops::Range<usize>,
) -> Vec<ChannelResult> {
    let glitch_block = decoded.first_short_block();
    let block_usable = |block: usize| {
        blocks.contains(&block)
            && decoded.blocks.get(block).is_some_and(Option::is_some)
            && (block == 0 || decoded.blocks.get(block - 1).is_some_and(Option::is_some))
            && Some(block) != glitch_block
    };
    let analysis = Analysis::new();
    let mut channels = Vec::new();
    for (channel, &position) in decoded.positions.iter().enumerate() {
        let signals = ChannelSignals {
            reference: &reference[channel],
            plain: &decoded.plain[channel],
            dithered: &decoded.dithered[channel],
            perturbed: &decoded.perturbed[channel],
        };
        let (mut error, mut energy, mut samples) = (0.0f64, 0.0f64, 0usize);
        for (index, (&r, &o)) in signals.reference.iter().zip(signals.plain).enumerate() {
            if !block_usable(index / SAMPLES_PER_AUDIO_BLOCK) {
                continue;
            }
            error += (f64::from(r) - f64::from(o)).powi(2);
            energy += f64::from(r).powi(2);
            samples += 1;
        }
        let mut tally = Tally::default();
        if std::env::var("AC3_DEBUG_BINS").is_ok() {
            println!("BIN channel {position:?}");
        }
        for block in blocks.clone() {
            if !block_usable(block) || !block_usable(block + 1) {
                continue;
            }
            let Some(Some(classes)) = decoded.blocks.get(block) else {
                continue;
            };
            tally.add_block(
                &analysis,
                &classes[channel],
                &signals,
                block * SAMPLES_PER_AUDIO_BLOCK,
            );
        }
        channels.push(ChannelResult {
            position,
            psnr: if error == 0.0 {
                999.0
            } else {
                -10.0 * (error / samples.max(1) as f64).log10()
            },
            snr: ratio_db(energy, error),
            tally,
        });
    }
    channels
}

fn format_ratio(ratio: Option<f64>) -> String {
    ratio.map_or_else(|| "  n/a".to_owned(), |value| format!("{value:+5.2}"))
}

fn print_bands(channel: &ChannelResult) {
    let cells = |reference: &[f64; BANDS], ours: &[f64; BANDS]| -> String {
        (0..BANDS)
            .filter(|&band| reference[band] > 0.0 && ours[band] > 0.0)
            .map(|band| {
                format!(
                    "{}:{:+.1}",
                    band * BAND_BINS,
                    ratio_db(reference[band], ours[band])
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let tally = &channel.tally;
    println!(
        "    {:?} all bins ff/ours dB: {}",
        channel.position,
        cells(&tally.band_reference_all, &tally.band_ours_all)
    );
    println!(
        "    {:?} SPX bins ff/ours dB: {}",
        channel.position,
        cells(&tally.band_reference_spx, &tally.band_ours_spx)
    );
}

#[test]
#[ignore = "нужен каталог tools/eac3_sweep.py: AC3_DECODER_EAC3_SWEEP_DIR"]
fn eac3_ffmpeg_differential_sweep() {
    let directory = std::path::PathBuf::from(std::env::var("AC3_DECODER_EAC3_SWEEP_DIR").unwrap());
    let manifest = std::fs::read_to_string(directory.join("manifest.tsv")).unwrap();
    let filter = std::env::var("AC3_DECODER_SWEEP_FILTER").unwrap_or_default();
    let show_bands = std::env::var("AC3_DEBUG_BANDS").is_ok();
    let mut failures = Vec::new();
    let mut families: BTreeMap<String, (usize, f64, f64)> = BTreeMap::new();
    let mut cases = 0usize;
    for line in manifest.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [name, family, policy, crc] = fields[..] else {
            panic!("bad manifest line {line}")
        };
        if !name.contains(&filter) {
            continue;
        }
        cases += 1;
        let result = run_case(&directory, name, family, policy, crc);
        let mut min_excess = f64::INFINITY;
        let mut min_psnr = f64::INFINITY;
        println!(
            "{} [{}] units {} rejected {} first-short {:?}",
            result.name, result.family, result.units, result.rejected, result.glitch_block
        );
        for channel in &result.channels {
            let tally = &channel.tally;
            let excess = ratio_db(tally.signal, tally.clean_excess);
            let dither =
                Tally::noise_ratio(tally.dither_residual, tally.our_dither, tally.dither_bins);
            let spx = Tally::noise_ratio(tally.spx_residual, tally.our_spx_noise, tally.spx_bins);
            let aht_dither = Tally::noise_ratio(
                tally.aht_dither_residual,
                tally.aht_our_dither,
                tally.aht_dither_bins,
            );
            println!(
                "  {:<22} PSNR {:>6.1} SNR {:>6.1} clean {:>6.1} beyond-grid {:>6.1} max-clean {:>8.1} st | dither bins {:>7} ff/ours {} | AHT-frame dither bins {:>7} ff/ours {} | SPX bins {:>7} res/noise {}",
                format!("{:?}", channel.position),
                channel.psnr,
                channel.snr,
                ratio_db(tally.signal, tally.clean_residual),
                excess,
                tally.max_clean_steps,
                tally.dither_bins,
                format_ratio(dither),
                tally.aht_dither_bins,
                format_ratio(aht_dither),
                tally.spx_bins,
                format_ratio(spx),
            );
            if show_bands {
                print_bands(channel);
            }
            min_excess = min_excess.min(excess);
            min_psnr = min_psnr.min(channel.psnr);
            if excess < CLEAN_THRESHOLD_DB {
                failures.push(format!(
                    "{name} {:?}: clean residual beyond grid {excess:.1} dB",
                    channel.position
                ));
            }
            for (label, ratio) in [
                ("dither", dither),
                ("AHT-frame dither", aht_dither),
                ("SPX noise", spx),
            ] {
                if let Some(ratio) = ratio
                    && ratio.abs() > NOISE_ENERGY_TOLERANCE_DB
                {
                    failures.push(format!(
                        "{name} {:?}: {label} energy mismatch {ratio:+.2} dB",
                        channel.position
                    ));
                }
            }
        }
        let entry =
            families
                .entry(result.family.clone())
                .or_insert((0, f64::INFINITY, f64::INFINITY));
        entry.0 += 1;
        entry.1 = entry.1.min(min_psnr);
        entry.2 = entry.2.min(min_excess);
    }
    println!("\nfamily                         cases  min PSNR  min beyond-grid SNR");
    for (family, (count, psnr, excess)) in &families {
        println!("{family:<30} {count:>5}  {psnr:>8.1}  {excess:>8.1}");
    }
    assert!(cases > 0);
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Dev-диагностика AHT: коэффициенты бина по шести блокам кадра
/// (`AC3_DEBUG_AHT_FRAME=<кадр>`, канал 0) переводятся прямым DCT §3.4.5 в
/// область AHT — у нас и у ffmpeg; печатаются бины с расхождением.
#[test]
#[ignore = "dev-диагностика: AC3_DECODER_EAC3_SWEEP_DIR, AC3_DECODER_SWEEP_FILTER, AC3_DEBUG_AHT_FRAME"]
fn eac3_debug_aht_frame() {
    let directory = std::path::PathBuf::from(std::env::var("AC3_DECODER_EAC3_SWEEP_DIR").unwrap());
    let name = std::env::var("AC3_DECODER_SWEEP_FILTER").unwrap();
    let frame: usize = std::env::var("AC3_DEBUG_AHT_FRAME")
        .unwrap()
        .parse()
        .unwrap();
    let channel: usize =
        std::env::var("AC3_DEBUG_CHANNEL").map_or(0, |value| value.parse().unwrap());
    let read = |suffix: &str| std::fs::read(directory.join(format!("{name}{suffix}"))).unwrap();
    let units = split_access_units(&read(".eac3"));
    let policy = std::env::var("AC3_DEBUG_DRC").unwrap_or_else(|_| "off".to_owned());
    crate::tables::eac3::vq::quirk::emulate_ffmpeg_hebap4_shift(
        std::env::var("AC3_EMULATE_FFMPEG_HEBAP4").is_ok(),
    );
    let decoded = decode_stream(&units, CrcPolicy::Verify, dynamic_range(&policy));
    let reference = deinterleave_ffmpeg_order(&read(".ref.f32"), &decoded.positions);
    let analysis = Analysis::new();
    let bins_from: usize = std::env::var("AC3_DEBUG_FIRST_BIN").map_or(0, |v| v.parse().unwrap());
    let first_block = frame * 6;
    let mut ours = [[0.0f64; COEFFICIENTS_PER_BLOCK]; 6];
    let mut theirs = [[0.0f64; COEFFICIENTS_PER_BLOCK]; 6];
    for block in 0..6 {
        let classes = decoded.blocks[first_block + block].as_ref().unwrap();
        let transform = classes[channel].transform;
        let start = (first_block + block) * SAMPLES_PER_AUDIO_BLOCK;
        ours[block] = analysis.coefficients(&segment(&decoded.plain[channel], start), transform);
        theirs[block] = analysis.coefficients(&segment(&reference[channel], start), transform);
        println!("block {} transform {:?}", first_block + block, transform);
    }
    let dct = |values: [f64; 6]| -> [f64; 6] {
        std::array::from_fn(|j| {
            let r = if j == 0 {
                std::f64::consts::FRAC_1_SQRT_2
            } else {
                1.0
            };
            (0..6)
                .map(|m| {
                    let angle = (j * (2 * m + 1)) as f64 * std::f64::consts::PI / 12.0;
                    std::f64::consts::SQRT_2 * r * angle.cos() * values[m]
                })
                .sum::<f64>()
                / 6.0
        })
    };
    for bin in bins_from..COEFFICIENTS_PER_BLOCK {
        let a = dct(std::array::from_fn(|m| ours[m][bin]));
        let b = dct(std::array::from_fn(|m| theirs[m][bin]));
        let scale = a.iter().chain(&b).fold(0.0f64, |acc, v| acc.max(v.abs()));
        let diff = a
            .iter()
            .zip(&b)
            .fold(0.0f64, |acc, (x, y)| acc.max((x - y).abs()));
        if scale > 0.0 && diff > 0.02 * scale {
            println!(
                "bin {bin:3} ours {:?}\n        ff   {:?}",
                a.map(|v| (v * 1e6).round() / 1e6),
                b.map(|v| (v * 1e6).round() / 1e6)
            );
        }
    }
}
