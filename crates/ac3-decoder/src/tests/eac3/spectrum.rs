//! Частотная проверка E-AC-3: у каждого канала фикстуры свой тон, и его
//! коэффициенты обязаны иметь пик в бине тона (`(k + ½)·fs/512`, §7.9.4).
//! Ловит то, что не видно учёту бит: перепутанные каналы, неверные
//! экспоненты/мантиссы/coupling в синтаксисе Annex E.

use super::support::{EAC3_FIXTURES, EnhancedHarness, eac3_fixture, split_frames};
use crate::config::CrcPolicy;
use crate::output::{ChannelPosition, ac3_channel_positions};
use crate::spectral::dither::DitherMode;
use crate::syntax::audblk::output::ChannelSpectrum;

fn tone_frequency(fixture: &str, position: ChannelPosition) -> f64 {
    if fixture == "stereo-48k-128k-cpl-high.eac3" {
        return if position == ChannelPosition::Left {
            5000.0
        } else {
            11_000.0
        };
    }
    if fixture == "mono-48k-64k.eac3" {
        return 2000.0;
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

/// Бин с наибольшей энергией и доля энергии в окрестности ±2 бина.
fn peak_and_concentration(spectrum: &ChannelSpectrum) -> (usize, f64) {
    let energy: Vec<f64> = spectrum
        .coefficients
        .iter()
        .map(|&v| f64::from(v) * f64::from(v))
        .collect();
    let peak = (0..energy.len())
        .max_by(|&a, &b| energy[a].total_cmp(&energy[b]))
        .unwrap();
    let total: f64 = energy.iter().sum();
    let near: f64 = energy[peak.saturating_sub(2)..(peak + 3).min(energy.len())]
        .iter()
        .sum();
    (peak, near / total)
}

#[test]
fn each_channel_carries_its_own_tone_at_the_right_bin() {
    for fixture in &EAC3_FIXTURES {
        let mut harness = EnhancedHarness::new(DitherMode::Disabled);
        // Кадр 0 — разгон кодера.
        for frame in split_frames(fixture.bytes).into_iter().skip(1) {
            let decoded = harness.decode(frame, CrcPolicy::Verify).unwrap();
            let bsi = &decoded.info.bsi;
            let sample_rate = f64::from(decoded.info.sample_rate_hz);
            let positions = ac3_channel_positions(bsi.audio_coding_mode, bsi.lfe_on);
            for (block_index, block) in harness
                .blocks
                .iter()
                .enumerate()
                .take(decoded.info.audio_blocks)
            {
                let channels = block
                    .full_bandwidth
                    .iter()
                    .take(bsi.full_bandwidth_channels())
                    .chain(bsi.lfe_on.then_some(&block.lfe));
                for (position, spectrum) in positions.iter().zip(channels) {
                    let expected =
                        expected_bin(tone_frequency(fixture.name, *position), sample_rate);
                    assert!(
                        expected < spectrum.end_bin,
                        "{} {position:?}: band {}",
                        fixture.name,
                        spectrum.end_bin
                    );
                    let (peak, concentration) = peak_and_concentration(spectrum);
                    assert!(
                        peak.abs_diff(expected) <= 1 && concentration > 0.8,
                        "{} block {block_index} {position:?}: peak {peak} (expected {expected}), concentration {concentration:.3}",
                        fixture.name
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
}

/// Оба тона выше начала coupling: разнести их по L/R могут только
/// координаты coupling в синтаксисе E-AC-3 (`firstcplcos`, Annex E §2.3.2.29).
#[test]
fn coupled_high_tones_are_restored_into_their_own_channels() {
    let mut harness = EnhancedHarness::new(DitherMode::Disabled);
    for frame in split_frames(eac3_fixture("stereo-48k-128k-cpl-high.eac3"))
        .into_iter()
        .skip(1)
    {
        harness.decode(frame, CrcPolicy::Verify).unwrap();
        assert_eq!(harness.usage.coupling_blocks, harness.usage.audio_blocks);
        for block in harness.blocks.iter() {
            // cpl_start_band 1 → coupling с бина 49; оба тона (бины 53 и 117) выше.
            assert!(block.full_bandwidth[0].end_bin > expected_bin(11_000.0, 48_000.0));
            let (left, _) = peak_and_concentration(&block.full_bandwidth[0]);
            let (right, _) = peak_and_concentration(&block.full_bandwidth[1]);
            assert!(
                left.abs_diff(expected_bin(5000.0, 48_000.0)) <= 1,
                "left {left}"
            );
            assert!(
                right.abs_diff(expected_bin(11_000.0, 48_000.0)) <= 1,
                "right {right}"
            );
        }
    }
}
