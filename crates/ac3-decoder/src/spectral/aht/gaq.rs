//! Адаптивное по усилению квантование GAQ мантисс AHT (A/52 Annex E §3.4.2,
//! §3.4.4.2, Таблицы E3.3–E3.6; ETSI TS 102 366 v1.4.1 §E.2.4.2, §E.2.4.4.2).
//!
//! Каждый бин с hebap 8–19 передаёт 6 скалярных мантисс подряд. Для «активных»
//! бинов (hebap в диапазоне режима, Таблица E3.3) поток несёт усиление `Gk`
//! (1, 2 или 4). При `Gk > 1` мантисса читается сначала коротким «малым»
//! словом; самое отрицательное слово — тег, после которого идёт «большое»
//! слово с перекладкой `y = x + a·x + b` (Таблица E3.6). Без усиления (`Gk = 1`)
//! одно m-битное слово тоже проходит перекладку — так квантователь
//! становится симметричным (`2^m − 1` уровней).

use crate::bitstream::{BitReader, ReadPastLimit};
use crate::tables::eac3::aht::{
    FIRST_SCALAR_HEBAP, GaqGain, RemapConstants, remap_constants, scalar_mantissa_bits,
};

/// Верхняя граница (исключая) активного диапазона hebap при `gaqmod` 0–1 и 2–3
/// (§3.4.2: `endbap = 12` / `17`).
const END_BAP_NARROW: u8 = 12;
const END_BAP_WIDE: u8 = 17;
/// Наибольший составной код трёх троичных усилений: 3³ − 1 (§3.4.4.2).
const MAX_COMPOSITE_GAIN_CODE: u8 = 26;
/// Ширина составного кода усилений (`gaqmod = 3`).
const COMPOSITE_GAIN_BITS: u32 = 5;
/// Масштаб 16-битных дробных констант Таблицы E3.6.
const CONSTANT_SCALE: f32 = 32_768.0;

/// Режим GAQ канала в кадре (`chgaqmod`/`cplgaqmod`/`lfegaqmod`, Таблица E3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GaqMode {
    /// `0x0` — GAQ не используется.
    Disabled,
    /// `0x1` — 1-битные усиления `Gk ∈ {1, 2}` для hebap 8–11.
    OneBitTwo,
    /// `0x2` — 1-битные усиления `Gk ∈ {1, 4}` для hebap 8–16.
    OneBitFour,
    /// `0x3` — составные усиления `Gk ∈ {1, 2, 4}` (5 бит на тройку) для hebap 8–16.
    Composite,
}

impl GaqMode {
    pub(crate) const fn from_code(code: u8) -> Self {
        match code & 0x3 {
            0 => Self::Disabled,
            1 => Self::OneBitTwo,
            2 => Self::OneBitFour,
            _ => Self::Composite,
        }
    }

    /// `endbap` псевдокода §3.4.2.
    const fn end_bap(self) -> u8 {
        match self {
            Self::Disabled | Self::OneBitTwo => END_BAP_NARROW,
            Self::OneBitFour | Self::Composite => END_BAP_WIDE,
        }
    }
}

/// Как кодирован бин канала AHT (`chgaqbin[bin]`, §3.4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinCoding {
    /// hebap 0–7: VQ-индекс (или ничего при hebap 0); `gaqbin = 0`.
    Vector,
    /// hebap в активном диапазоне режима: есть слово усиления; `gaqbin = 1`.
    GainCoded,
    /// hebap выше активного диапазона: скаляр без усиления; `gaqbin = −1`.
    Scalar,
}

/// Классификация бина по hebap и режиму.
pub(crate) fn classify_bin(hebap: u8, mode: GaqMode) -> BinCoding {
    if hebap < FIRST_SCALAR_HEBAP {
        BinCoding::Vector
    } else if hebap < mode.end_bap() {
        BinCoding::GainCoded
    } else {
        BinCoding::Scalar
    }
}

/// `*gaqsections`: сколько слов усиления передано для `active_bins` активных бинов.
pub(crate) fn gain_word_count(mode: GaqMode, active_bins: usize) -> usize {
    match mode {
        GaqMode::Disabled => 0,
        GaqMode::OneBitTwo | GaqMode::OneBitFour => active_bins,
        GaqMode::Composite => active_bins.div_ceil(3),
    }
}

/// Ошибки чтения GAQ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GaqReadError {
    Overrun,
    /// Составной код > 26.
    CompositeCodeOutOfRange {
        code: u8,
    },
    /// Код мантиссы вне квантователя (самое отрицательное слово при `Gk = 1`).
    MantissaCodeOutOfRange {
        hebap: u8,
        code: u16,
    },
}

impl From<ReadPastLimit> for GaqReadError {
    fn from(_: ReadPastLimit) -> Self {
        Self::Overrun
    }
}

/// Читает слова усилений и раскладывает их по активным бинам по порядку
/// возрастания частоты (Таблица E3.4: 0 → 1, 1 → 2, 2 → 4).
pub(crate) fn read_gains(
    reader: &mut BitReader<'_>,
    mode: GaqMode,
    active_bins: usize,
    gains: &mut [GaqGain],
) -> Result<(), GaqReadError> {
    let active = gains.iter_mut().take(active_bins);
    match mode {
        GaqMode::Disabled => {
            for gain in active {
                *gain = GaqGain::One;
            }
        }
        GaqMode::OneBitTwo | GaqMode::OneBitFour => {
            let raised = if mode == GaqMode::OneBitTwo {
                GaqGain::Two
            } else {
                GaqGain::Four
            };
            for gain in active {
                *gain = if reader.read_flag()? {
                    raised
                } else {
                    GaqGain::One
                };
            }
        }
        GaqMode::Composite => {
            for triplet in gains.chunks_mut(3).take(gain_word_count(mode, active_bins)) {
                let code = reader.read_u8(COMPOSITE_GAIN_BITS)?;
                if code > MAX_COMPOSITE_GAIN_CODE {
                    return Err(GaqReadError::CompositeCodeOutOfRange { code });
                }
                // M1 = code / 9, M2 = (code % 9) / 3, M3 = code % 3 (§3.4.4.2).
                let mapped = [code / 9, (code % 9) / 3, code % 3];
                for (gain, value) in triplet.iter_mut().zip(mapped) {
                    *gain = match value {
                        0 => GaqGain::One,
                        1 => GaqGain::Two,
                        _ => GaqGain::Four,
                    };
                }
            }
        }
    }
    Ok(())
}

/// Знаковое `bits`-битное слово как дробь с точкой левее старшего бита.
fn signed_fraction(raw: u16, bits: u32) -> f32 {
    let shift = 32 - bits;
    let signed = (i32::from(raw) << shift) >> shift;
    signed as f32 / (1i32 << (bits - 1)) as f32
}

/// Самое отрицательное `bits`-битное слово — тег большой мантиссы.
const fn tag_code(bits: u32) -> u16 {
    1 << (bits - 1)
}

/// Перекладка `y = x + a·x + b`; знак x выбирает `b` (§3.4.4.2).
fn remap(x: f32, constants: RemapConstants) -> f32 {
    let offset = if x >= 0.0 {
        constants.offset_non_negative
    } else {
        constants.offset_negative
    };
    x + f32::from(constants.slope_correction) / CONSTANT_SCALE * x
        + f32::from(offset) / CONSTANT_SCALE
}

/// Одна скалярная мантисса hebap 8–19 с усилением `gain` (Таблица E3.5).
pub(crate) fn read_scalar_mantissa(
    reader: &mut BitReader<'_>,
    hebap: u8,
    gain: GaqGain,
) -> Result<f32, GaqReadError> {
    let out_of_range = |code| GaqReadError::MantissaCodeOutOfRange { hebap, code };
    let bits = scalar_mantissa_bits(hebap).ok_or(out_of_range(0))?;
    let constants = remap_constants(hebap, gain).ok_or(out_of_range(0))?;
    let (small_bits, large_bits, divisor) = match gain {
        GaqGain::One => {
            let raw = reader.read_bits(bits)?;
            // У квантователя 2^m − 1 уровней: самого отрицательного слова нет.
            if raw == tag_code(bits) {
                return Err(out_of_range(raw));
            }
            return Ok(remap(signed_fraction(raw, bits), constants));
        }
        GaqGain::Two => (bits - 1, bits - 1, 2.0),
        GaqGain::Four => (bits - 2, bits, 4.0),
    };
    let small = reader.read_bits(small_bits)?;
    if small != tag_code(small_bits) {
        // Малая мантисса: кодер усилил её в Gk раз — декодер ослабляет (1/Gk).
        return Ok(signed_fraction(small, small_bits) / divisor);
    }
    let large = reader.read_bits(large_bits)?;
    Ok(remap(signed_fraction(large, large_bits), constants))
}

#[cfg(test)]
mod tests {
    use super::{
        BinCoding, GaqGain, GaqMode, GaqReadError, classify_bin, gain_word_count, read_gains,
        read_scalar_mantissa,
    };
    use crate::bitstream::BitReader;

    #[test]
    fn active_range_and_section_counts_follow_section_3_4_2() {
        assert_eq!(classify_bin(7, GaqMode::Composite), BinCoding::Vector);
        assert_eq!(classify_bin(11, GaqMode::OneBitTwo), BinCoding::GainCoded);
        assert_eq!(classify_bin(12, GaqMode::OneBitTwo), BinCoding::Scalar);
        assert_eq!(classify_bin(16, GaqMode::OneBitFour), BinCoding::GainCoded);
        assert_eq!(classify_bin(17, GaqMode::Composite), BinCoding::Scalar);
        assert_eq!(classify_bin(9, GaqMode::Disabled), BinCoding::GainCoded);
        assert_eq!(gain_word_count(GaqMode::Disabled, 7), 0);
        assert_eq!(gain_word_count(GaqMode::OneBitFour, 7), 7);
        assert_eq!(gain_word_count(GaqMode::Composite, 7), 3);
        assert_eq!(gain_word_count(GaqMode::Composite, 6), 2);
    }

    #[test]
    fn composite_gains_unpack_base_three_and_reject_codes_above_26() {
        // 5-битный код 19 = 2·9 + 0·3 + 1 → Gk = 4, 1, 2; затем 0 → 1, 1, 1.
        let bytes = [0b1001_1000, 0b0000_0000];
        let mut reader = BitReader::new(&bytes);
        let mut gains = [GaqGain::One; 5];
        read_gains(&mut reader, GaqMode::Composite, 5, &mut gains).unwrap();
        assert_eq!(
            gains,
            [
                GaqGain::Four,
                GaqGain::One,
                GaqGain::Two,
                GaqGain::One,
                GaqGain::One
            ]
        );
        assert_eq!(reader.position_bits(), 10);
        let ones = [0xFF];
        let mut reader = BitReader::new(&ones);
        assert_eq!(
            read_gains(&mut reader, GaqMode::Composite, 1, &mut gains),
            Err(GaqReadError::CompositeCodeOutOfRange { code: 31 })
        );
    }

    /// hebap 8 (m = 3), Gk = 1: 7-уровневый симметричный квантователь ±6/7.
    #[test]
    fn unit_gain_quantizer_is_symmetric_with_2m_minus_1_levels() {
        let read = |code: u8| {
            let bytes = [code << 5];
            read_scalar_mantissa(&mut BitReader::new(&bytes), 8, GaqGain::One)
        };
        let expect =
            |value: f32, target: f32| assert!((value - target).abs() < 2e-4, "{value} {target}");
        expect(read(0b011).unwrap(), 6.0 / 7.0);
        expect(read(0b001).unwrap(), 2.0 / 7.0);
        expect(read(0b000).unwrap(), 0.0);
        expect(read(0b101).unwrap(), -6.0 / 7.0);
        assert_eq!(
            read(0b100),
            Err(GaqReadError::MantissaCodeOutOfRange { hebap: 8, code: 4 })
        );
    }

    /// hebap 8, Gk = 2: малые мантиссы {−1/4, 0, 1/4}; тег `10` + большая
    /// мантисса из {±1/2, ±5/6} (мёртвая зона перекладки).
    #[test]
    fn gain_two_uses_tagged_large_mantissas() {
        let read = |bits: u8| {
            let bytes = [bits];
            let mut reader = BitReader::new(&bytes);
            let value = read_scalar_mantissa(&mut reader, 8, GaqGain::Two).unwrap();
            (value, reader.position_bits())
        };
        assert_eq!(read(0b0100_0000), (0.25, 2));
        assert_eq!(read(0b1100_0000), (-0.25, 2));
        let (large, consumed) = read(0b1001_0000);
        assert_eq!(consumed, 4);
        assert!((large - 5.0 / 6.0).abs() < 2e-4, "{large}");
        let (large, _) = read(0b1010_0000);
        assert!((large + 5.0 / 6.0).abs() < 2e-4, "{large}");
        let (large, _) = read(0b1000_0000);
        assert!((large - 0.5).abs() < 2e-4, "{large}");
    }

    /// hebap 8, Gk = 4: малое слово 1 бит (0 → 0), тег `1` + 3-битная большая
    /// мантисса, переложенная на ±[0,25; 0,893].
    #[test]
    fn gain_four_reads_full_width_large_mantissa_after_tag() {
        let bytes = [0b0101_1110, 0b0000_0000];
        let mut reader = BitReader::new(&bytes);
        assert_eq!(
            read_scalar_mantissa(&mut reader, 8, GaqGain::Four).unwrap(),
            0.0
        );
        let large = read_scalar_mantissa(&mut reader, 8, GaqGain::Four).unwrap();
        // Тег 1, затем 011 = 0,75 → 0,75·6/7 + 0,25.
        assert!((large - (0.75 * 6.0 / 7.0 + 0.25)).abs() < 2e-4, "{large}");
        assert_eq!(reader.position_bits(), 5);
        let large = read_scalar_mantissa(&mut reader, 8, GaqGain::Four).unwrap();
        // Тег 1, затем 100 = −1 → −6/7 − 0,0357.
        assert!((large + (6.0 / 7.0 + 0.25 / 7.0)).abs() < 2e-4, "{large}");
    }
}
