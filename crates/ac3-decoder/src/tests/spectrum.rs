//! Проверка в частотной области: каждый канал фикстуры несёт свой тон, и
//! декодированные коэффициенты обязаны иметь пик в бине этого тона (для
//! MDCT из §7.9.4 бин `k` соответствует частоте `(k + ½)·fs/512`).
//!
//! Тест ловит ошибки, которые не видны учёту бит: перепутанные каналы,
//! неверные экспоненты/мантиссы, неверное восстановление из coupling.

use super::support::{FIXTURES, decoder, split_frames};
use crate::config::CrcPolicy;
use crate::output::{ChannelPosition, ac3_channel_positions};
use crate::spectral::dither::DitherMode;
use crate::syntax::audblk::output::ChannelSpectrum;

/// Частоты тонов по ролям каналов — те же, что в `tests/fixtures/generate.sh`
/// (у фикстуры с тонами выше начала coupling — свои частоты).
fn tone_frequency(fixture: &str, position: ChannelPosition) -> f64 {
    match (fixture, position) {
        ("stereo-48k-160k-cpl-high.ac3", ChannelPosition::Left) => return 5000.0,
        ("stereo-48k-160k-cpl-high.ac3", ChannelPosition::Right) => return 11_000.0,
        _ => {}
    }
    match position {
        ChannelPosition::Left => 440.0,
        ChannelPosition::Right => 1000.0,
        ChannelPosition::Center => 2000.0,
        ChannelPosition::LowFrequencyEffects => 60.0,
        ChannelPosition::LeftSurround | ChannelPosition::CenterSurround => 3000.0,
        ChannelPosition::RightSurround => 7000.0,
        other => panic!("no tone for {other:?}"),
    }
}

fn expected_bin(frequency: f64, sample_rate: f64) -> usize {
    (frequency * 512.0 / sample_rate - 0.5).round().max(0.0) as usize
}

/// Индекс бина с наибольшей энергией и доля энергии в окрестности ±2 бина.
fn peak_and_concentration(spectrum: &ChannelSpectrum) -> (usize, f64) {
    let energy: Vec<f64> = spectrum
        .coefficients
        .iter()
        .map(|&value| f64::from(value) * f64::from(value))
        .collect();
    let (peak, _) =
        energy.iter().enumerate().fold(
            (0, 0.0),
            |best, (bin, &value)| if value > best.1 { (bin, value) } else { best },
        );
    let total: f64 = energy.iter().sum();
    let near: f64 = energy[peak.saturating_sub(2)..(peak + 3).min(energy.len())]
        .iter()
        .sum();
    (peak, near / total)
}

#[test]
fn each_channel_carries_its_own_tone_at_the_right_bin() {
    let mut tones_above_bandwidth = Vec::new();
    for fixture in &FIXTURES {
        let mut decoder = decoder(CrcPolicy::Verify, DitherMode::Disabled);
        // Кадр 0 содержит разгон кодера; проверяем кадры 1–5.
        for frame in split_frames(fixture.bytes).into_iter().skip(1) {
            let decoded = decoder.decode_coefficients(frame).unwrap();
            let bsi = &decoded.info.bsi;
            let sample_rate = f64::from(decoded.info.sample_rate_hz);
            let positions = ac3_channel_positions(bsi.audio_coding_mode, bsi.lfe_on);
            for (block_index, block) in decoder.decoded_blocks().iter().enumerate() {
                let channels = block
                    .full_bandwidth
                    .iter()
                    .take(bsi.full_bandwidth_channels())
                    .chain(bsi.lfe_on.then_some(&block.lfe));
                for (position, spectrum) in positions.iter().zip(channels) {
                    let expected =
                        expected_bin(tone_frequency(fixture.name, *position), sample_rate);
                    if expected >= spectrum.end_bin {
                        // Кодер урезал полосу канала ниже тона: искать нечего.
                        tones_above_bandwidth.push((fixture.name, *position));
                        continue;
                    }
                    let (peak, concentration) = peak_and_concentration(spectrum);
                    assert!(
                        peak.abs_diff(expected) <= 1 && concentration > 0.8,
                        "{} block {block_index} {position:?}: peak {peak} (expected {expected}), concentration {concentration:.3}, end {}",
                        fixture.name,
                        spectrum.end_bin
                    );
                    assert!(
                        spectrum.coefficients[spectrum.end_bin..]
                            .iter()
                            .all(|&value| value == 0.0)
                    );
                }
            }
        }
    }
    // Единственный канал, чей тон кодер отрезал: Rs (7 кГц) у 5.1 32 кГц 192 кбит/с
    // (полоса кодера — 73 бина ≈ 4,5 кГц).
    tones_above_bandwidth.dedup();
    assert_eq!(
        tones_above_bandwidth,
        [("5p1-32k-192k.ac3", ChannelPosition::RightSurround)]
    );
}

#[test]
fn coupled_high_tones_are_restored_into_their_own_channels() {
    // Оба тона лежат выше cplbegf: в потоке они есть только в общем coupling
    // канале, и разнести их по L/R могут лишь координаты coupling (§7.4).
    let stream = super::support::fixture("stereo-48k-160k-cpl-high.ac3");
    let mut decoder = decoder(CrcPolicy::Verify, DitherMode::Disabled);
    for frame in split_frames(stream).into_iter().skip(1) {
        decoder.decode_coefficients(frame).unwrap();
        for block in decoder.decoded_blocks() {
            let begin = block
                .coupling_begin_subband
                .expect("coupling must be in use");
            let first_coupled_bin = crate::spectral::coupling::subband_first_bin(begin);
            assert!(expected_bin(5000.0, 48_000.0) >= first_coupled_bin);
            let (left_peak, _) = peak_and_concentration(&block.full_bandwidth[0]);
            let (right_peak, _) = peak_and_concentration(&block.full_bandwidth[1]);
            assert!(
                left_peak.abs_diff(expected_bin(5000.0, 48_000.0)) <= 1,
                "left peak {left_peak}"
            );
            assert!(
                right_peak.abs_diff(expected_bin(11_000.0, 48_000.0)) <= 1,
                "right peak {right_peak}"
            );
        }
    }
}
