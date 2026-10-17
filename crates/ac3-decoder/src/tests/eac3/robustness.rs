//! Устойчивость разбора E-AC-3: обрезки, одиночные флипы бит, случайная
//! порча и мусор дают типизированные ошибки (или корректный разбор), но
//! никогда не панику и не выход за границы.

use super::builder::{FrameSpec, build_frame};
use super::support::{EAC3_FIXTURES, quiet_config, split_frames};
use crate::config::CrcPolicy;
use crate::error::{Ac3DecodeError, CrcRegion};
use crate::syntax::eac3::inspection::inspect_access_unit;

/// Детерминированный генератор для порчи (xorshift32).
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

/// Кадры для проверок: по два кадра каждой фикстуры и тихие кадры с AHT/SPX/
/// зависимым substream-ом (их синтаксические ветки ffmpeg не покрывает).
fn sample_units() -> Vec<Vec<u8>> {
    let mut units: Vec<Vec<u8>> = EAC3_FIXTURES
        .iter()
        .flat_map(|fixture| split_frames(fixture.bytes).into_iter().skip(1).take(2))
        .map(<[u8]>::to_vec)
        .collect();
    units.push(build_frame(&FrameSpec {
        aht: true,
        ..FrameSpec::independent(7, true)
    }));
    units.push(build_frame(&FrameSpec {
        spx: true,
        ..FrameSpec::independent(2, false)
    }));
    let mut with_dependent = build_frame(&FrameSpec::independent(7, true));
    with_dependent.extend(build_frame(&FrameSpec::dependent(
        6,
        false,
        Some(0b0001_1010_0000_0000),
    )));
    units.push(with_dependent);
    units
}

#[test]
fn every_truncated_prefix_is_rejected_without_panic() {
    for unit in sample_units() {
        assert!(inspect_access_unit(&unit, quiet_config(CrcPolicy::Verify)).is_ok());
        // Первые 64 байта (заголовки) — каждый префикс, дальше — каждый 5-й.
        for length in (0..unit.len()).filter(|&length| length < 64 || length % 5 == 0) {
            let result = inspect_access_unit(&unit[..length], quiet_config(CrcPolicy::Skip));
            assert!(
                result.is_err(),
                "prefix {length} of {} accepted",
                unit.len()
            );
        }
    }
}

#[test]
fn single_bit_flips_fail_crc_and_never_panic_without_it() {
    for unit in sample_units() {
        // Флипы после синхрослова каждого 13-го бита: с проверкой CRC — всегда ошибка
        // (CRC-16 ловит все одиночные ошибки), без проверки — любой исход, но не паника.
        for bit in (16..unit.len() * 8).step_by(13) {
            let mut corrupted = unit.clone();
            corrupted[bit / 8] ^= 0x80 >> (bit % 8);
            let verified = inspect_access_unit(&corrupted, quiet_config(CrcPolicy::Verify));
            assert!(verified.is_err(), "bit {bit}");
            let _ = inspect_access_unit(&corrupted, quiet_config(CrcPolicy::Skip));
        }
    }
}

#[test]
fn random_corruption_and_garbage_yield_typed_errors() {
    let mut noise = Noise(0x1234_5678);
    for unit in sample_units() {
        for _ in 0..100 {
            let mut corrupted = unit.clone();
            for _ in 0..1 + noise.next() % 8 {
                let position = 2 + (noise.next() as usize) % (corrupted.len() - 2);
                corrupted[position] = noise.next() as u8;
            }
            let _ = inspect_access_unit(&corrupted, quiet_config(CrcPolicy::Skip));
        }
    }
    // Мусор с синхрословом и заголовком E-AC-3 «любой длины».
    for _ in 0..2000 {
        let length = 6 + (noise.next() as usize) % 600;
        let mut garbage: Vec<u8> = (0..length).map(|_| noise.next() as u8).collect();
        garbage[0] = 0x0B;
        garbage[1] = 0x77;
        garbage[5] = (garbage[5] & 0x07) | (16 << 3);
        // Исход любой (мусор может случайно оказаться разбираемым), но без паники.
        let _ = inspect_access_unit(&garbage, quiet_config(CrcPolicy::Skip));
    }
    // Хвост после корректного кадра без синхрослова — не новый substream.
    let mut tail = split_frames(EAC3_FIXTURES[1].bytes)[0].to_vec();
    tail.extend_from_slice(&[0xAA; 9]);
    assert_eq!(
        inspect_access_unit(&tail, quiet_config(CrcPolicy::Verify)),
        Err(Ac3DecodeError::MissingSyncWord)
    );
}

#[test]
fn crc_mismatch_is_reported_for_the_whole_frame() {
    let mut frame = split_frames(EAC3_FIXTURES[2].bytes)[0].to_vec();
    let middle = frame.len() / 2;
    frame[middle] ^= 0x01;
    assert_eq!(
        inspect_access_unit(&frame, quiet_config(CrcPolicy::Verify)),
        Err(Ac3DecodeError::CrcMismatch {
            region: CrcRegion::WholeFrame
        })
    );
}
