//! Адаптивное гибридное преобразование AHT (A/52 Annex E §3.4;
//! ETSI TS 102 366 v1.4.1 §E.2.4).
//!
//! Канал с AHT (`chahtinu`/`cplahtinu`/`lfeahtinu = 1`) передаёт мантиссы
//! всех шести блоков кадра один раз, в блоке 0: для каждого бина — либо
//! VQ-индекс шести значений (hebap 1–7, §3.4.4.1), либо шесть скалярных
//! мантисс GAQ (hebap 8–19, §3.4.4.2), либо ничего (hebap 0). Эти шесть
//! значений — коэффициенты DCT-II по времени; обратное DCT (§3.4.5) даёт
//! мантиссы MDCT каждого блока, которые затем умножаются на экспоненты.

mod gaq;
mod idct;

pub(crate) use gaq::GaqMode;

use self::gaq::{BinCoding, GaqReadError, classify_bin, read_gains};
use self::idct::InverseAhtTransform;
use super::COEFFICIENTS_PER_BLOCK;
use crate::bitstream::{BitReader, ReadPastLimit};
use crate::tables::eac3::aht::{GaqGain, vq_index_bits};
use crate::tables::eac3::vq::{VQ_DIMENSION, vq_vector};

/// Блоков в кадре с AHT (§3.4.2: «AHT is only available in 6 block mode»).
pub(crate) const AHT_BLOCKS: usize = VQ_DIMENSION;

/// Мантиссы MDCT канала AHT для всех шести блоков (до экспонент).
#[derive(Debug, Clone)]
pub(crate) struct AhtMantissas {
    /// `[блок][бин]`.
    pub(crate) per_block: [[f32; COEFFICIENTS_PER_BLOCK]; AHT_BLOCKS],
}

impl AhtMantissas {
    pub(crate) fn new() -> Self {
        Self {
            per_block: [[0.0; COEFFICIENTS_PER_BLOCK]; AHT_BLOCKS],
        }
    }
}

/// Ошибки чтения канала AHT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AhtReadError {
    /// Кончилась отведённая область кадра.
    Overrun,
    /// Составной код усилений GAQ > 26.
    GainCodeOutOfRange { code: u8 },
    /// Код мантиссы вне квантователя.
    MantissaCodeOutOfRange { hebap: u8, code: u16 },
}

impl From<ReadPastLimit> for AhtReadError {
    fn from(_: ReadPastLimit) -> Self {
        Self::Overrun
    }
}

impl From<GaqReadError> for AhtReadError {
    fn from(error: GaqReadError) -> Self {
        match error {
            GaqReadError::Overrun => Self::Overrun,
            GaqReadError::CompositeCodeOutOfRange { code } => Self::GainCodeOutOfRange { code },
            GaqReadError::MantissaCodeOutOfRange { hebap, code } => {
                Self::MantissaCodeOutOfRange { hebap, code }
            }
        }
    }
}

/// Бины канала и их указатели hebap (уже рассчитанные по экспонентам блока 0).
#[derive(Debug, Clone, Copy)]
pub(crate) struct AhtChannelLayout<'a> {
    pub(crate) hebap: &'a [u8; COEFFICIENTS_PER_BLOCK],
    /// Первый бин набора (0 у fbw/LFE, `cplstrtmant` у coupling канала).
    pub(crate) start: usize,
    /// Бин за последним (`endmant`/`cplendmant`/7).
    pub(crate) end: usize,
}

/// Читает `*gaqmod`, усиления и «предмантиссы» канала AHT и выполняет
/// обратное DCT; результат — мантиссы MDCT шести блоков в `output`.
///
/// У бинов с hebap 0 под шесть коэффициентов AHT не выделено ни одного бита:
/// их значения даёт `zero_bit_mantissa` (dither §7.3.4 или ноль) — до
/// обратного DCT, как и у любой другой «предмантиссы».
pub(crate) fn read_aht_channel(
    reader: &mut BitReader<'_>,
    layout: &AhtChannelLayout<'_>,
    zero_bit_mantissa: &mut dyn FnMut() -> f32,
    output: &mut AhtMantissas,
) -> Result<(), AhtReadError> {
    let end = layout.end.min(COEFFICIENTS_PER_BLOCK);
    let start = layout.start.min(end);
    let mode = GaqMode::from_code(reader.read_u8(2)?);
    let bins = start..end;
    let active_bins = bins
        .clone()
        .filter(|&bin| classify_bin(layout.hebap[bin], mode) == BinCoding::GainCoded)
        .count();
    let mut gains = [GaqGain::One; COEFFICIENTS_PER_BLOCK];
    read_gains(reader, mode, active_bins, &mut gains)?;
    let transform = InverseAhtTransform::new();
    let mut next_gain = gains.iter().copied();
    for slot in output.per_block.iter_mut() {
        slot.fill(0.0);
    }
    for bin in bins {
        let hebap = layout.hebap[bin];
        let aht = match classify_bin(hebap, mode) {
            BinCoding::Vector if hebap == 0 => std::array::from_fn(|_| zero_bit_mantissa()),
            BinCoding::Vector => read_vector(reader, hebap)?,
            BinCoding::GainCoded => {
                let gain = next_gain.next().unwrap_or(GaqGain::One);
                read_scalars(reader, hebap, gain)?
            }
            BinCoding::Scalar => read_scalars(reader, hebap, GaqGain::One)?,
        };
        for (block, value) in transform.apply(&aht).into_iter().enumerate() {
            output.per_block[block][bin] = value;
        }
    }
    Ok(())
}

/// hebap 1–7: индекс VQ (Таблица E3.2 / E4.x).
fn read_vector(reader: &mut BitReader<'_>, hebap: u8) -> Result<[f32; VQ_DIMENSION], AhtReadError> {
    let bits =
        vq_index_bits(hebap).ok_or(AhtReadError::MantissaCodeOutOfRange { hebap, code: 0 })?;
    let index = reader.read_bits(bits)?;
    vq_vector(hebap, index).ok_or(AhtReadError::MantissaCodeOutOfRange { hebap, code: index })
}

/// Шесть скалярных мантисс бина подряд (`pre_chmant[n][ch][bin]`, n = 0–5).
fn read_scalars(
    reader: &mut BitReader<'_>,
    hebap: u8,
    gain: GaqGain,
) -> Result<[f32; VQ_DIMENSION], AhtReadError> {
    let mut values = [0.0f32; VQ_DIMENSION];
    for value in &mut values {
        *value = gaq::read_scalar_mantissa(reader, hebap, gain)?;
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::{AhtChannelLayout, AhtMantissas, AhtReadError, read_aht_channel};
    use crate::bitstream::BitReader;
    use crate::spectral::COEFFICIENTS_PER_BLOCK;
    use crate::tables::eac3::vq::vq_vector;

    /// Пишет биты MSB-first.
    struct BitWriter {
        bytes: Vec<u8>,
        bits: usize,
    }

    impl BitWriter {
        fn put(&mut self, value: u32, width: u32) {
            for bit in (0..width).rev() {
                if self.bits.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                if (value >> bit) & 1 == 1 {
                    let last = self.bytes.len() - 1;
                    self.bytes[last] |= 0x80 >> (self.bits % 8);
                }
                self.bits += 1;
            }
        }
    }

    #[test]
    fn vq_bin_reproduces_codebook_vector_through_inverse_dct() {
        // Бин 3: hebap 2 (3-битный индекс 5); бин 4: hebap 0 (ноль бит); бин 5: hebap 8, Gk = 1.
        let mut hebap = [0u8; COEFFICIENTS_PER_BLOCK];
        hebap[3] = 2;
        hebap[5] = 8;
        let mut writer = BitWriter {
            bytes: Vec::new(),
            bits: 0,
        };
        writer.put(0, 2); // gaqmod = 0
        writer.put(5, 3); // VQ-индекс бина 3
        for _ in 0..6 {
            writer.put(0b001, 3); // шесть мантисс бина 5: 2/7
        }
        writer.put(0, 16);
        let layout = AhtChannelLayout {
            hebap: &hebap,
            start: 3,
            end: 6,
        };
        let mut output = AhtMantissas::new();
        let mut reader = BitReader::new(&writer.bytes);
        read_aht_channel(&mut reader, &layout, &mut || 0.0, &mut output).unwrap();
        assert_eq!(reader.position_bits(), 2 + 3 + 18);
        // Прямое DCT-II (MᵀC / 6) возвращает исходный вектор книги.
        let expected = vq_vector(2, 5).unwrap();
        let transform = super::InverseAhtTransform::new();
        let blocks: Vec<f32> = (0..6).map(|block| output.per_block[block][3]).collect();
        let impulse = |index: usize| {
            let mut unit = [0.0f32; 6];
            unit[index] = 1.0;
            transform.apply(&unit)
        };
        for (index, &value) in expected.iter().enumerate() {
            let basis = impulse(index);
            let projection: f32 = basis.iter().zip(&blocks).map(|(b, c)| b * c).sum::<f32>() / 6.0;
            assert!(
                (projection - value).abs() < 1e-5,
                "{index}: {projection} vs {value}"
            );
        }
        // hebap 0 — нули во всех блоках; бин 5: шесть мантисс 2/7 через то же IDCT.
        assert!((0..6).all(|block| output.per_block[block][4] == 0.0));
        let scalar = transform.apply(&[2.0 / 7.0; 6]);
        for (block, &value) in scalar.iter().enumerate() {
            assert!((output.per_block[block][5] - value).abs() < 1e-4, "{block}");
        }
    }

    #[test]
    fn truncated_channel_is_an_overrun_not_a_panic() {
        let mut hebap = [0u8; COEFFICIENTS_PER_BLOCK];
        hebap[..10].fill(19);
        let layout = AhtChannelLayout {
            hebap: &hebap,
            start: 0,
            end: 10,
        };
        let bytes = [0u8; 8];
        let mut output = AhtMantissas::new();
        assert_eq!(
            read_aht_channel(
                &mut BitReader::new(&bytes),
                &layout,
                &mut || 0.0,
                &mut output
            ),
            Err(AhtReadError::Overrun)
        );
    }
}
