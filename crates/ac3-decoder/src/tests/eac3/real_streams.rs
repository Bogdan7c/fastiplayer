//! Dev-прогон на реальных потоках E-AC-3 (не коммитятся): учёт бит, CRC,
//! состав access unit-ов и счётчики инструментов Annex E.
//!
//! Запуск: `AC3_DECODER_REAL_EAC3_DIR=<dir> cargo test -p ac3-decoder --release
//! real_eac3_streams -- --ignored --nocapture`.

use super::support::{
    EnhancedHarness, bits_are_zero, bitstream_id, quiet_config, split_access_units, split_frames,
};
use crate::config::CrcPolicy;
use crate::spectral::dither::DitherMode;
use crate::syntax::eac3::inspection::inspect_access_unit;
use crate::syntax::eac3::tool_usage::EnhancedToolUsage;
use std::collections::BTreeMap;

/// Биты хвоста без `auxdatal`: `auxdatae` (1) + `encinfo` (1) + `crc2` (16).
const TRAILER_BITS_WITHOUT_USER_DATA: usize = 18;
/// Кодеры Dolby кладут в конец `auxbits` 16-битное слово (в 7.1-потоке с
/// однoблочными кадрами — раз в 6 блоков, ~равномерно распределённое
/// значение). Остальные `auxbits` — нули: недочитанные мантиссы оказались бы
/// сразу после конца аудио, а не в последних 16 битах.
const ENCODER_AUX_WORD_BITS: usize = 16;

#[derive(Debug, Default)]
struct StreamSummary {
    eac3_frames: usize,
    ac3_frames: usize,
    dependent_frames: usize,
    errors: BTreeMap<String, usize>,
    nonzero_aux_frames: usize,
    tools: EnhancedToolUsage,
    access_units: usize,
    access_unit_errors: BTreeMap<String, usize>,
    layouts: BTreeMap<String, usize>,
    /// PCM через публичный декодер (D4): ошибки, форматы, события TPNP.
    pcm_errors: BTreeMap<String, usize>,
    pcm_formats: BTreeMap<String, usize>,
    tpnp_scheduled: usize,
    tpnp_rejected: usize,
}

fn summarize(stream: &[u8]) -> StreamSummary {
    let mut summary = StreamSummary::default();
    let mut harness = EnhancedHarness::new(DitherMode::Disabled);
    for frame in split_frames(stream) {
        if bitstream_id(frame) <= 10 {
            summary.ac3_frames += 1;
            continue;
        }
        summary.eac3_frames += 1;
        match harness.decode(frame, CrcPolicy::Verify) {
            Ok(decoded) => {
                if decoded.info.stream_type == crate::syntax::eac3::bsi::StreamType::Dependent {
                    summary.dependent_frames += 1;
                }
                let trailer = decoded.trailer;
                let user = trailer.user_data_bits.map_or(0, |bits| bits + 14);
                assert_eq!(
                    trailer.audio_end_bits
                        + trailer.auxiliary_bits
                        + user
                        + TRAILER_BITS_WITHOUT_USER_DATA,
                    frame.len() * 8
                );
                let checked_bits = trailer.auxiliary_bits.saturating_sub(ENCODER_AUX_WORD_BITS);
                if trailer.user_data_bits.is_none()
                    && !bits_are_zero(frame, trailer.audio_end_bits, checked_bits)
                {
                    summary.nonzero_aux_frames += 1;
                }
            }
            Err(error) => *summary.errors.entry(format!("{error:?}")).or_default() += 1,
        }
    }
    summary.tools = harness.usage;
    corrupt_first_units_without_panic(stream);
    for unit in split_access_units(stream) {
        summary.access_units += 1;
        match inspect_access_unit(&unit, quiet_config(CrcPolicy::Verify)) {
            Ok(inspection) => {
                let layout: Vec<String> = inspection
                    .substreams
                    .iter()
                    .map(|substream| format!("{:?}", substream.channels))
                    .collect();
                *summary.layouts.entry(layout.join(" + ")).or_default() += 1;
            }
            Err(error) => {
                *summary
                    .access_unit_errors
                    .entry(format!("{error:?}"))
                    .or_default() += 1
            }
        }
    }
    let mut decoder = crate::decoder::Ac3Decoder::new(quiet_config(CrcPolicy::Verify));
    for unit in split_access_units(stream) {
        match decoder.decode_access_unit(&unit) {
            Ok(audio) => {
                let format = format!(
                    "{} Hz × {} {:?}",
                    audio.sample_rate_hz(),
                    audio.samples_per_channel(),
                    audio.channels()
                );
                *summary.pcm_formats.entry(format).or_default() += 1;
            }
            Err(error) => *summary.pcm_errors.entry(format!("{error:?}")).or_default() += 1,
        }
    }
    let counters = decoder.transient_pre_noise_counters();
    summary.tpnp_scheduled = counters.scheduled;
    summary.tpnp_rejected = counters.rejected;
    summary
}

#[test]
#[ignore = "нужны реальные потоки E-AC-3 вне репозитория (AC3_DECODER_REAL_EAC3_DIR)"]
fn real_eac3_streams_parse_with_exact_bit_accounting() {
    let directory = std::env::var("AC3_DECODER_REAL_EAC3_DIR").unwrap();
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == "eac3" || ext == "ac3")
        })
        .collect();
    paths.sort();
    assert!(!paths.is_empty());
    let mut total = EnhancedToolUsage::default();
    for path in paths {
        let summary = summarize(&std::fs::read(&path).unwrap());
        println!("{}: {summary:#?}", path.display());
        assert!(
            summary.eac3_frames + summary.ac3_frames > 0,
            "{}",
            path.display()
        );
        assert!(summary.errors.is_empty(), "{}", path.display());
        assert!(summary.access_unit_errors.is_empty(), "{}", path.display());
        assert!(summary.pcm_errors.is_empty(), "{}", path.display());
        assert_eq!(summary.nonzero_aux_frames, 0, "{}", path.display());
        total.accumulate(&summary.tools);
    }
    println!("TOTAL tools: {total:?}");
}

/// Порча реальных access unit-ов (AHT/SPX/зависимые) без проверки CRC:
/// любой исход, кроме паники.
fn corrupt_first_units_without_panic(stream: &[u8]) {
    let mut state = 0x9E37_79B9u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let mut decoder = crate::decoder::Ac3Decoder::new(quiet_config(CrcPolicy::Skip));
    for unit in split_access_units(stream).into_iter().take(40) {
        for _ in 0..50 {
            let mut corrupted = unit.clone();
            let position = 2 + next() as usize % (corrupted.len() - 2);
            corrupted[position] ^= 1 << (next() % 8);
            let _ = inspect_access_unit(&corrupted, quiet_config(CrcPolicy::Skip));
            let _ = inspect_access_unit(&corrupted[..position], quiet_config(CrcPolicy::Skip));
            let _ = decoder.decode_access_unit(&corrupted);
            let _ = decoder.decode_access_unit(&corrupted[..position]);
        }
    }
}

/// Средняя энергия коэффициентов по бинам (канал 0 программы 0) — для
/// сверки формы спектра SPX/AHT с PCM эталонного декодера (dev-скрипт
/// `tools/compare_band_energy.py`). Запуск: `AC3_DIAG_FILE=<eac3>
/// AC3_DIAG_OUT=<csv> cargo test … dump_mean_bin_energy -- --ignored`.
#[test]
#[ignore = "dev-диагностика: нужен внешний поток (AC3_DIAG_FILE, AC3_DIAG_OUT)"]
fn dump_mean_bin_energy() {
    let stream = std::fs::read(std::env::var("AC3_DIAG_FILE").unwrap()).unwrap();
    // Dither включён: эталон тоже заполняет мантиссы без бит шумом (§7.3.4).
    let mut harness = EnhancedHarness::new(DitherMode::default());
    let mut energy = vec![[0.0f64; 256]; 6];
    let mut blocks = 0usize;
    for frame in split_frames(&stream) {
        if bitstream_id(frame) <= 10 {
            continue;
        }
        let decoded = harness.decode(frame, CrcPolicy::Verify).unwrap();
        if decoded.info.stream_type == crate::syntax::eac3::bsi::StreamType::Dependent {
            continue;
        }
        let channels = decoded.info.bsi.full_bandwidth_channels();
        for block in harness.blocks.iter_mut().take(decoded.info.audio_blocks) {
            if let Some(flags) = block.rematrix {
                // Rematrixing (§7.5) — шаг синтеза D2; здесь нужен для сравнения L/R с эталоном.
                let bandwidth = crate::synthesis::rematrix::StereoPairBandwidth {
                    coupling_begin_subband: block.coupling_begin_subband,
                    left_end_bin: block.full_bandwidth[0].end_bin,
                    right_end_bin: block.full_bandwidth[1].end_bin,
                };
                let (left, right) = block.full_bandwidth.split_at_mut(1);
                crate::synthesis::rematrix::restore_left_right(
                    &mut left[0].coefficients,
                    &mut right[0].coefficients,
                    &flags,
                    bandwidth,
                );
            }
            for (channel, spectrum) in block.full_bandwidth.iter().enumerate().take(channels) {
                for (bin, &value) in spectrum.coefficients.iter().enumerate() {
                    energy[channel][bin] += f64::from(value) * f64::from(value);
                }
            }
            blocks += 1;
        }
    }
    let rows: Vec<String> = (0..256)
        .map(|bin| {
            let values: Vec<String> = energy
                .iter()
                .map(|channel| format!("{:e}", channel[bin] / blocks as f64))
                .collect();
            format!("{bin},{}", values.join(","))
        })
        .collect();
    std::fs::write(std::env::var("AC3_DIAG_OUT").unwrap(), rows.join("\n")).unwrap();
}
