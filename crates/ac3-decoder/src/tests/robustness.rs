//! CRC, обрывы и порча: любой вход → типизированный результат, без паники.

use super::support::{FIXTURES, decoder, fixture, split_frames};
use crate::config::{CrcPolicy, DecoderConfig};
use crate::decoder::Ac3Decoder;
use crate::error::{Ac3DecodeError, AccessUnitViolation, CrcRegion, UnsupportedFeature};
use crate::spectral::dither::{DitherMode, DitherSeed};
use crate::tables::frame_size::five_eighths_frame_bytes;

/// Биты syncinfo + `bsid`: их порча меняет саму интерпретацию кадра
/// (синхрослово, длина, синтаксис), поэтому там допустима любая ошибка.
const HEADER_BITS: usize = 45;

/// Детерминированный генератор для «случайной» порчи.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit as u64) as usize
    }
}

#[test]
fn valid_frames_pass_both_crcs() {
    for fixture in &FIXTURES {
        let mut decoder = decoder(CrcPolicy::Verify, DitherMode::Disabled);
        for frame in split_frames(fixture.bytes) {
            assert!(
                decoder.decode_coefficients(frame).is_ok(),
                "{}",
                fixture.name
            );
        }
    }
}

#[test]
fn every_single_bit_flip_is_detected() {
    for fixture in &FIXTURES {
        let frame = split_frames(fixture.bytes)[1].to_vec();
        let boundary_bits = five_eighths_frame_bytes(frame.len() / 2) * 8;
        let mut decoder = decoder(CrcPolicy::Verify, DitherMode::Disabled);
        for bit in 0..frame.len() * 8 {
            let mut corrupted = frame.clone();
            corrupted[bit / 8] ^= 0x80 >> (bit % 8);
            let result = decoder.decode_coefficients(&corrupted);
            if bit < HEADER_BITS {
                assert!(result.is_err(), "{} bit {bit}", fixture.name);
                continue;
            }
            let region = if bit < boundary_bits {
                CrcRegion::FirstFiveEighths
            } else {
                CrcRegion::LastThreeEighths
            };
            assert_eq!(
                result,
                Err(Ac3DecodeError::CrcMismatch { region }),
                "{} bit {bit}",
                fixture.name
            );
        }
    }
}

#[test]
fn every_truncated_prefix_is_a_typed_truncation() {
    for fixture in &FIXTURES {
        let frame = split_frames(fixture.bytes)[0];
        let mut decoder = decoder(CrcPolicy::Verify, DitherMode::Disabled);
        for length in 0..frame.len() {
            let required_bytes = if length < 6 { 6 } else { frame.len() };
            let expected = Err(Ac3DecodeError::Truncated {
                available_bytes: length,
                required_bytes,
            });
            assert_eq!(
                decoder.decode_coefficients(&frame[..length]).map(|_| ()),
                expected,
                "{} len {length}",
                fixture.name
            );
            assert_eq!(
                decoder.decode_access_unit(&frame[..length]).map(|_| ()),
                expected
            );
            assert_eq!(
                crate::read_syncframe_info(&frame[..length]).map(|_| ()),
                expected
            );
        }
    }
}

#[test]
fn random_corruption_without_crc_never_panics() {
    let mut random = Lcg(0xAC3);
    let (mut decoded, mut rejected) = (0usize, 0usize);
    for fixture in &FIXTURES {
        let mut decoder = decoder(CrcPolicy::Skip, DitherMode::default());
        for frame in split_frames(fixture.bytes) {
            for _ in 0..60 {
                let mut corrupted = frame.to_vec();
                for _ in 0..1 + random.below(8) {
                    // Синхрослово и длину не трогаем: интересна глубина разбора.
                    let bit = HEADER_BITS + random.below(corrupted.len() * 8 - HEADER_BITS);
                    corrupted[bit / 8] ^= 0x80 >> (bit % 8);
                }
                match decoder.decode_coefficients(&corrupted) {
                    Ok(_) => decoded += 1,
                    Err(_) => rejected += 1,
                }
            }
        }
    }
    // Обе ветки реально встречаются: часть порчи попадает в мантиссы и
    // остаётся «корректной», часть ломает синтаксис.
    assert!(
        decoded > 100 && rejected > 100,
        "decoded {decoded}, rejected {rejected}"
    );
}

#[test]
fn random_payload_after_a_valid_header_never_panics() {
    let mut random = Lcg(7);
    let template = split_frames(fixture("5p1-48k-640k-annexd.ac3"))[0];
    let mut decoder = decoder(CrcPolicy::Skip, DitherMode::default());
    for _ in 0..2000 {
        let mut frame = template.to_vec();
        for byte in frame.iter_mut().skip(5) {
            *byte = random.next() as u8;
        }
        // bsid держим в диапазоне AC-3, остальное — мусор.
        frame[5] &= 0x47;
        let _ = decoder.decode_coefficients(&frame);
    }
}

#[test]
fn decode_access_unit_reports_unsupported_and_corrupt_units() {
    let stream = fixture("stereo-48k-192k.ac3");
    let frames = split_frames(stream);
    let mut decoder = Ac3Decoder::new(DecoderConfig::default());
    let audio = decoder.decode_access_unit(frames[0]).unwrap();
    assert_eq!(audio.samples_per_channel(), 1536);
    assert_eq!(audio.channels().len(), 2);
    // Два кадра AC-3 подряд — не access unit: ядро AC-3 бывает только первым.
    assert_eq!(
        decoder.decode_access_unit(&stream[..frames[0].len() * 2]),
        Err(Ac3DecodeError::InvalidAccessUnit(
            AccessUnitViolation::MisplacedAc3Syncframe
        ))
    );
    // bsid 16 — кадр разбирается синтаксисом E-AC-3, байты AC-3 ему не подходят.
    let mut enhanced = frames[0].to_vec();
    enhanced[5] = (16 << 3) | (enhanced[5] & 0x7);
    let error = decoder.decode_access_unit(&enhanced).unwrap_err();
    assert!(
        !matches!(error, Ac3DecodeError::Unsupported(_)),
        "{error:?}"
    );
    // Заголовочный разбор только AC-3 по-прежнему отсылает к E-AC-3.
    assert_eq!(
        crate::read_syncframe_info(&enhanced),
        Err(Ac3DecodeError::Unsupported(UnsupportedFeature::EnhancedAc3))
    );
    let mut corrupted = frames[0].to_vec();
    corrupted[100] ^= 1;
    assert_eq!(
        decoder.decode_access_unit(&corrupted),
        Err(Ac3DecodeError::CrcMismatch {
            region: CrcRegion::FirstFiveEighths
        })
    );
}

/// Коэффициенты всех блоков кадра одним вектором.
fn coefficients(decoder: &Ac3Decoder) -> Vec<f32> {
    decoder
        .decoded_blocks()
        .iter()
        .flat_map(|block| {
            block
                .full_bandwidth
                .iter()
                .chain(std::iter::once(&block.lfe))
        })
        .flat_map(|channel| channel.coefficients)
        .collect()
}

#[test]
fn dither_fills_only_zero_bit_mantissas_and_is_reproducible() {
    let frame = split_frames(fixture("stereo-48k-32k.ac3"))[2];
    let mut silent = decoder(CrcPolicy::Verify, DitherMode::Disabled);
    silent.decode_coefficients(frame).unwrap();
    let without_dither = coefficients(&silent);
    let seeded = DitherMode::Pseudorandom(DitherSeed(42));
    let mut noisy = decoder(CrcPolicy::Verify, seeded);
    noisy.decode_coefficients(frame).unwrap();
    let with_dither = coefficients(&noisy);
    let differing: Vec<usize> = (0..with_dither.len())
        .filter(|&index| with_dither[index] != without_dither[index])
        .collect();
    // 32 кбит/с на стерео: много мантисс без бит, и кодер просит dither.
    assert!(differing.len() > 100, "{} dithered bins", differing.len());
    // Dither трогает только нулевые (без бит) мантиссы — ненулевые коэффициенты совпадают.
    assert!(differing.iter().all(|&index| without_dither[index] == 0.0));
    // Тот же вход + reset → те же значения; без reset — генератор идёт дальше.
    noisy.decode_coefficients(frame).unwrap();
    assert_ne!(coefficients(&noisy), with_dither);
    noisy.reset();
    noisy.decode_coefficients(frame).unwrap();
    assert_eq!(coefficients(&noisy), with_dither);
}
