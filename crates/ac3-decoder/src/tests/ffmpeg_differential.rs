//! Dev-сверка с ffmpeg (не коммитится в CI-прогон: `#[ignore]`, нужен
//! каталог, подготовленный `tools/ffmpeg_sweep.py`).
//!
//! Запуск:
//! `AC3_DECODER_FFMPEG_SWEEP_DIR=<dir> cargo test -p ac3-decoder --release ffmpeg_differential_sweep -- --ignored --nocapture`
//!
//! Для каждого случая из `manifest.tsv` декодер без dither-а сравнивается с
//! PCM ffmpeg (f32, каналы уже в порядке декодера):
//!
//! - PCM: PSNR (пик 1,0), SNR, max|e| по каналам; поиск задержки ±768
//!   отсчётов — ожидается 0;
//! - спектр (см. `reference_comparison`): остаток `ffmpeg − наш` по блокам
//!   раскладывается прямым MDCT; ожидание — вне dither-бинов остаток целиком
//!   объясняется сеткой фиксированной точки ffmpeg (≥ 90 дБ ниже сигнала
//!   то, что за её пределами), энергия остатка в dither-бинах совпадает с
//!   энергией нашего dither-а в пределах ±3 дБ.
//!
//! Кадры, отвергнутые нашим декодером (CRC и т. п.) или не выданные
//! ffmpeg-ом, и следующий за ними кадр (хвост overlap-add) из метрик
//! исключаются и считаются отдельно. `AC3_DECODER_SWEEP_FILTER=<подстрока>`
//! ограничивает случаи, `AC3_DEBUG_WORST=1` печатает блоки с остатком вне
//! сетки ffmpeg (номер блока, канал, длина преобразования, бины).

use std::collections::BTreeMap;
use std::path::Path;

use super::reference_comparison::{
    Analysis, FRAME_SAMPLES, OurDecode, SpectralTally, best_lag, decode_both, ratio_db,
};
use crate::config::{CompressionScale, CrcPolicy, DynamicRangeControl};
use crate::syntax::syncframe::{AUDIO_BLOCKS_PER_SYNCFRAME, SAMPLES_PER_AUDIO_BLOCK};

/// Сколько кадров реального потока анализировать спектрально (стоимость O(N²)).
const MAX_SPECTRAL_FRAMES: usize = 300;
const MAX_LAG: usize = 768;
/// Кадров в окне поиска задержки.
const LAG_SEARCH_FRAMES: usize = 8;
/// Энергия кадра эталона, с которой начинается поиск задержки (≈ −60 дБ FS RMS).
const LAG_SEARCH_MIN_ENERGY: f64 = 1e-6 * FRAME_SAMPLES as f64;
/// Энергия на отсчёт, ниже которой канал считается молчащим (−140 дБ FS).
const SILENT_CHANNEL_ENERGY_PER_SAMPLE: f64 = 1e-14;
/// Порог «совпадения до округления».
const CLEAN_THRESHOLD_DB: f64 = 90.0;
/// Допуск совпадения энергии dither-а (два независимых равномерных шума).
const DITHER_ENERGY_TOLERANCE_DB: f64 = 3.0;

/// Нарезка, по которой `tools/ffmpeg_sweep.py` выровнял эталон
/// (`<name>.frames.tsv`: смещение и длина каждого кадра).
fn frames_from_list<'a>(stream: &'a [u8], list: &str) -> Vec<&'a [u8]> {
    list.lines()
        .map(|line| {
            let (offset, length) = line.split_once('\t').unwrap();
            let offset: usize = offset.parse().unwrap();
            &stream[offset..offset + length.parse::<usize>().unwrap()]
        })
        .collect()
}

#[derive(Default)]
struct ChannelMetrics {
    error_energy: f64,
    reference_energy: f64,
    samples: usize,
    max_abs_error: f64,
    our_peak: f64,
    spectral: SpectralTally,
}

impl ChannelMetrics {
    fn psnr_db(&self) -> f64 {
        let mse = self.error_energy / self.samples.max(1) as f64;
        if mse == 0.0 {
            999.0
        } else {
            -10.0 * mse.log10()
        }
    }

    /// SNR канала; у молчащего канала (эталон — тишина) SNR не определён,
    /// и он не участвует в минимуме (его ошибку видно по PSNR).
    fn snr_db(&self) -> f64 {
        if self.reference_energy <= SILENT_CHANNEL_ENERGY_PER_SAMPLE * self.samples as f64 {
            return f64::INFINITY;
        }
        ratio_db(self.reference_energy, self.error_energy)
    }
}

struct CaseResult {
    family: String,
    name: String,
    channels: Vec<ChannelMetrics>,
    lag: isize,
    frames: usize,
    rejected_frames: usize,
    ffmpeg_missing_frames: usize,
    /// Блок, исключённый из-за дефекта ffmpeg на первом коротком блоке.
    ffmpeg_glitch_block: Option<usize>,
}

fn dynamic_range(policy: &str) -> DynamicRangeControl {
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

/// Задержка на 8 кадрах, начиная с первого кадра, где эталон не молчит
/// (иначе любая задержка «лучшая»).
fn stream_lag(reference: &[f32], ours: &[f32]) -> isize {
    let length = reference.len().min(ours.len());
    let loud_frame = (0..length / FRAME_SAMPLES).find(|&frame| {
        let energy: f64 = reference[frame * FRAME_SAMPLES..(frame + 1) * FRAME_SAMPLES]
            .iter()
            .map(|&value| f64::from(value).powi(2))
            .sum();
        energy > LAG_SEARCH_MIN_ENERGY
    });
    let Some(loud_frame) = loud_frame else {
        return 0;
    };
    let start = (loud_frame * FRAME_SAMPLES).max(MAX_LAG);
    let end = length
        .min(start + LAG_SEARCH_FRAMES * FRAME_SAMPLES)
        .saturating_sub(MAX_LAG);
    if end <= start {
        return 0;
    }
    best_lag(reference, ours, start, end, MAX_LAG)
}

fn read_f32le(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

fn run_case(
    directory: &Path,
    name: &str,
    family: &str,
    policy: &str,
    crc: &str,
    analysis: &Analysis,
) -> CaseResult {
    let read = |suffix: &str| std::fs::read(directory.join(format!("{name}{suffix}"))).unwrap();
    let stream = read(".ac3");
    let reference = read_f32le(&read(".ref.f32"));
    let crc = if crc == "skip" {
        CrcPolicy::Skip
    } else {
        CrcPolicy::Verify
    };
    let frame_list = String::from_utf8(read(".frames.tsv")).unwrap();
    let frames = frames_from_list(&stream, &frame_list);
    // Кадры, которые ffmpeg не выдал (он пропускает повреждённые кадры).
    let missing: Vec<usize> = String::from_utf8(read(".ref.missing"))
        .unwrap()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect();
    let decoded: OurDecode = decode_both(&frames, crc, dynamic_range(policy));
    let channel_count = decoded.positions.len();
    assert!(channel_count > 0, "{name}: nothing decoded");
    assert_eq!(
        reference.len() / channel_count / FRAME_SAMPLES,
        frames.len(),
        "{name}: ffmpeg frame count differs"
    );
    let reference_channel = |channel: usize| -> Vec<f32> {
        reference
            .iter()
            .skip(channel)
            .step_by(channel_count)
            .copied()
            .collect()
    };
    // Кадр годен для метрик, если его выдали оба декодера и предыдущий
    // кадр тоже годен по обеим сторонам (иначе хвост overlap-add разный).
    let decoded_by_both = |frame: usize| decoded.valid[frame] && !missing.contains(&frame);
    let usable =
        |frame: usize| decoded_by_both(frame) && (frame == 0 || decoded_by_both(frame - 1));
    let ffmpeg_glitch_block = decoded.first_short_block();
    let mut channels = Vec::new();
    let mut lag = 0;
    for channel in 0..channel_count {
        let reference_samples = reference_channel(channel);
        let ours = &decoded.plain[channel];
        if channel == 0 {
            lag = stream_lag(&reference_samples, ours);
        }
        let mut metrics = ChannelMetrics::default();
        for frame in (0..frames.len()).filter(|&frame| usable(frame)) {
            for index in frame * FRAME_SAMPLES..(frame + 1) * FRAME_SAMPLES {
                if Some(index / SAMPLES_PER_AUDIO_BLOCK) == ffmpeg_glitch_block {
                    continue;
                }
                let error = f64::from(reference_samples[index]) - f64::from(ours[index]);
                metrics.error_energy += error * error;
                metrics.reference_energy += f64::from(reference_samples[index]).powi(2);
                metrics.max_abs_error = metrics.max_abs_error.max(error.abs());
                metrics.our_peak = metrics.our_peak.max(f64::from(ours[index]).abs());
                metrics.samples += 1;
            }
        }
        let spectral_blocks = frames.len().min(MAX_SPECTRAL_FRAMES) * AUDIO_BLOCKS_PER_SYNCFRAME;
        for block in 0..spectral_blocks.saturating_sub(1) {
            let frame = block / AUDIO_BLOCKS_PER_SYNCFRAME;
            let next_frame = (block + 1) / AUDIO_BLOCKS_PER_SYNCFRAME;
            if !usable(frame) || !usable(next_frame) {
                continue;
            }
            if ffmpeg_glitch_block.is_some_and(|glitch| block == glitch || block + 1 == glitch) {
                continue;
            }
            let Some(info) = decoded.blocks[block].get(channel) else {
                continue;
            };
            let start = block * SAMPLES_PER_AUDIO_BLOCK;
            let mut block_tally = SpectralTally::default();
            block_tally.add_block(
                analysis,
                info,
                &reference_samples,
                ours,
                &decoded.dithered[channel],
                start,
            );
            if std::env::var("AC3_DEBUG_WORST").is_ok()
                && block_tally.beyond_grid_snr_db() < CLEAN_THRESHOLD_DB
            {
                println!(
                    "WORST block {block} ch {channel} {:?} beyond-grid {:.1} dB max-clean {:.1} steps",
                    info.transform,
                    block_tally.beyond_grid_snr_db(),
                    block_tally.max_clean_steps
                );
            }
            metrics.spectral.merge(&block_tally);
        }
        channels.push(metrics);
    }
    CaseResult {
        family: family.to_owned(),
        name: name.to_owned(),
        channels,
        lag,
        frames: frames.len(),
        rejected_frames: decoded.valid.iter().filter(|&&ok| !ok).count(),
        ffmpeg_missing_frames: missing.len(),
        ffmpeg_glitch_block,
    }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(f64::NAN)
}

#[derive(Default)]
struct FamilySummary {
    cases: usize,
    psnr: Vec<f64>,
    clean: Vec<f64>,
    excess: Vec<f64>,
    rejected_frames: usize,
}

#[test]
#[ignore = "нужен каталог tools/ffmpeg_sweep.py: AC3_DECODER_FFMPEG_SWEEP_DIR"]
fn ffmpeg_differential_sweep() {
    let directory =
        std::path::PathBuf::from(std::env::var("AC3_DECODER_FFMPEG_SWEEP_DIR").unwrap());
    let manifest = std::fs::read_to_string(directory.join("manifest.tsv")).unwrap();
    let filter = std::env::var("AC3_DECODER_SWEEP_FILTER").unwrap_or_default();
    let analysis = Analysis::new();
    let mut results = Vec::new();
    for line in manifest.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [name, family, policy, crc] = fields[..] else {
            panic!("bad manifest line {line}")
        };
        if !name.contains(&filter) {
            continue;
        }
        results.push(run_case(&directory, name, family, policy, crc, &analysis));
    }
    let mut failures = Vec::new();
    let mut families: BTreeMap<String, FamilySummary> = BTreeMap::new();
    for result in &results {
        let min_of = |metric: &dyn Fn(&ChannelMetrics) -> f64| {
            result
                .channels
                .iter()
                .map(metric)
                .fold(f64::INFINITY, f64::min)
        };
        let max_of = |metric: &dyn Fn(&ChannelMetrics) -> f64| {
            result.channels.iter().map(metric).fold(0.0, f64::max)
        };
        let min_psnr = min_of(&ChannelMetrics::psnr_db);
        let min_snr = min_of(&ChannelMetrics::snr_db);
        let min_clean = min_of(&|metrics| metrics.spectral.clean_snr_db());
        let min_excess = min_of(&|metrics| metrics.spectral.beyond_grid_snr_db());
        let max_error = max_of(&|metrics| metrics.max_abs_error);
        let peak = max_of(&|metrics| metrics.our_peak);
        let max_steps = max_of(&|metrics| metrics.spectral.max_clean_steps);
        let mut total = SpectralTally::default();
        for metrics in &result.channels {
            total.merge(&metrics.spectral);
        }
        let dither_ratio = total.dither_energy_ratio_db();
        println!(
            "{:<20} {:<52} ch {} fr {:>5} rej {:>2}/{:>2} lag {:>2} PSNR {:>6.1} SNR {:>6.1} clean {:>6.1} beyond-grid {:>6.1} max-clean {:>5.1} steps max|e| {:.1e} peak {:.3} dith-bins {:>7} dith ff/ours {:>6} first-short {:?}",
            result.family,
            result.name,
            result.channels.len(),
            result.frames,
            result.rejected_frames,
            result.ffmpeg_missing_frames,
            result.lag,
            min_psnr,
            min_snr,
            min_clean,
            min_excess,
            max_steps,
            max_error,
            peak,
            total.dithered_bins,
            dither_ratio.map_or_else(|| "n/a".to_owned(), |ratio| format!("{ratio:.2}")),
            result.ffmpeg_glitch_block
        );
        if result.lag != 0 {
            failures.push(format!("{}: lag {}", result.name, result.lag));
        }
        if min_excess < CLEAN_THRESHOLD_DB {
            failures.push(format!(
                "{}: clean-bin residual beyond ffmpeg grid {min_excess:.1} dB",
                result.name
            ));
        }
        if total.dithered_bins == 0 && min_psnr < CLEAN_THRESHOLD_DB {
            failures.push(format!(
                "{}: PSNR {min_psnr:.1} dB without dither",
                result.name
            ));
        }
        if let Some(ratio) = dither_ratio
            && ratio.abs() > DITHER_ENERGY_TOLERANCE_DB
        {
            failures.push(format!(
                "{}: dither energy mismatch {ratio:.2} dB",
                result.name
            ));
        }
        let entry = families.entry(result.family.clone()).or_default();
        entry.cases += 1;
        entry.psnr.push(min_psnr);
        entry.clean.push(min_clean);
        entry.excess.push(min_excess);
        entry.rejected_frames += result.rejected_frames;
    }
    println!(
        "\nfamily               cases   PSNR min / median   clean-bin SNR min / median   beyond-grid SNR min / median   rejected frames"
    );
    for (family, mut summary) in families {
        let minimum = |values: &[f64]| values.iter().copied().fold(f64::INFINITY, f64::min);
        println!(
            "{family:<20} {:>6}   {:>6.1} / {:>6.1}      {:>6.1} / {:>6.1}             {:>6.1} / {:>6.1}             {}",
            summary.cases,
            minimum(&summary.psnr),
            median(&mut summary.psnr),
            minimum(&summary.clean),
            median(&mut summary.clean),
            minimum(&summary.excess),
            median(&mut summary.excess),
            summary.rejected_frames
        );
    }
    assert!(!results.is_empty());
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
