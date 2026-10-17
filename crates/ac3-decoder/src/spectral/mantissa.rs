//! Чтение и деквантование мантисс (A/52 §7.3).
//!
//! Мантиссы читаются строго в порядке потока. bap 1, 2, 4 упакованы
//! группами (тройки/пары): группа читается целиком на месте своей первой
//! мантиссы, остальные берутся из «кармана» без чтения бит, причём группа
//! может переходить из одного набора экспонент (канала) в следующий внутри
//! того же блока (§7.3.5). Поэтому состояние групп живёт в
//! [`MantissaUnpacker`], который создаётся заново на каждый аудиоблок.

use crate::bitstream::{BitReader, ReadPastLimit};
use crate::tables::quantization::{
    BAP1_GROUP_BITS, BAP1_LEVELS, BAP2_GROUP_BITS, BAP2_LEVELS, BAP3_LEVELS, BAP3_MANTISSA_BITS,
    BAP4_GROUP_BITS, BAP4_LEVELS, BAP5_LEVELS, BAP5_MANTISSA_BITS, asymmetric_mantissa_bits,
};

/// Наибольшие корректные групповые коды (§7.10.2, условия 18–20).
const MAX_BAP1_GROUP_CODE: u16 = 26;
const MAX_BAP2_GROUP_CODE: u16 = 124;
const MAX_BAP4_GROUP_CODE: u16 = 120;

/// Результат чтения одной мантиссы.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Mantissa {
    /// Под мантиссу не выделено бит (bap = 0): значение решает dither.
    ZeroBits,
    /// Деквантованное значение в (−1, 1) до сдвига на экспоненту.
    Value(f32),
}

/// Ошибки чтения мантиссы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MantissaReadError {
    /// Кончилась отведённая область кадра.
    Overrun,
    /// Код вне таблицы квантователя.
    CodeOutOfRange { bap: u8, code: u16 },
}

impl From<ReadPastLimit> for MantissaReadError {
    fn from(_: ReadPastLimit) -> Self {
        Self::Overrun
    }
}

/// Незабранные мантиссы последней прочитанной группы одного квантователя.
#[derive(Debug, Clone, Copy, Default)]
struct PendingGroup {
    values: [f32; 2],
    remaining: usize,
}

impl PendingGroup {
    /// Забирает следующую мантиссу группы, если она есть.
    fn take(&mut self) -> Option<f32> {
        if self.remaining == 0 {
            return None;
        }
        let index = self.values.len() - self.remaining;
        self.remaining -= 1;
        self.values.get(index).copied()
    }

    /// Запоминает хвост только что прочитанной группы.
    fn store(&mut self, tail: &[f32]) {
        self.remaining = 0;
        for (slot, &value) in self.values.iter_mut().rev().zip(tail.iter().rev()) {
            *slot = value;
            self.remaining += 1;
        }
    }
}

/// Распаковщик мантисс одного аудиоблока.
#[derive(Debug, Clone, Default)]
pub(crate) struct MantissaUnpacker {
    bap1: PendingGroup,
    bap2: PendingGroup,
    bap4: PendingGroup,
}

impl MantissaUnpacker {
    /// Читает мантиссу с указателем `bap` (0–15).
    pub(crate) fn read(
        &mut self,
        reader: &mut BitReader<'_>,
        bap: u8,
    ) -> Result<Mantissa, MantissaReadError> {
        let value = match bap {
            0 => return Ok(Mantissa::ZeroBits),
            1 => Self::grouped(
                reader,
                &mut self.bap1,
                bap,
                BAP1_GROUP_BITS,
                MAX_BAP1_GROUP_CODE,
                ungroup_bap1,
            )?,
            2 => Self::grouped(
                reader,
                &mut self.bap2,
                bap,
                BAP2_GROUP_BITS,
                MAX_BAP2_GROUP_CODE,
                ungroup_bap2,
            )?,
            3 => symmetric(reader, bap, BAP3_MANTISSA_BITS, &BAP3_LEVELS)?,
            4 => Self::grouped(
                reader,
                &mut self.bap4,
                bap,
                BAP4_GROUP_BITS,
                MAX_BAP4_GROUP_CODE,
                ungroup_bap4,
            )?,
            5 => symmetric(reader, bap, BAP5_MANTISSA_BITS, &BAP5_LEVELS)?,
            _ => asymmetric(reader, bap)?,
        };
        Ok(Mantissa::Value(value))
    }

    /// Общая логика групповых квантователей: взять из кармана или прочитать
    /// новую группу, отдать первую мантиссу, остальные отложить.
    fn grouped(
        reader: &mut BitReader<'_>,
        pending: &mut PendingGroup,
        bap: u8,
        group_bits: u32,
        max_code: u16,
        ungroup: fn(u16) -> GroupValues,
    ) -> Result<f32, MantissaReadError> {
        if let Some(value) = pending.take() {
            return Ok(value);
        }
        let code = reader.read_bits(group_bits)?;
        if code > max_code {
            return Err(MantissaReadError::CodeOutOfRange { bap, code });
        }
        let values = ungroup(code);
        let (first, tail) = values
            .as_slice()
            .split_first()
            .ok_or(MantissaReadError::CodeOutOfRange { bap, code })?;
        pending.store(tail);
        Ok(*first)
    }
}

/// Деквантованные значения одной группы (2 или 3 мантиссы).
#[derive(Debug, Clone, Copy)]
struct GroupValues {
    values: [f32; 3],
    count: usize,
}

impl GroupValues {
    fn as_slice(&self) -> &[f32] {
        self.values.get(..self.count).unwrap_or_default()
    }
}

/// bap = 1: `code = 9a + 3b + c` (§7.3.5), уровни Таблицы 7.19.
fn ungroup_bap1(code: u16) -> GroupValues {
    let codes = [code / 9, (code % 9) / 3, code % 3];
    GroupValues {
        values: codes.map(|digit| level(&BAP1_LEVELS, digit)),
        count: 3,
    }
}

/// bap = 2: `code = 25a + 5b + c` (§7.3.5), уровни Таблицы 7.20.
fn ungroup_bap2(code: u16) -> GroupValues {
    let codes = [code / 25, (code % 25) / 5, code % 5];
    GroupValues {
        values: codes.map(|digit| level(&BAP2_LEVELS, digit)),
        count: 3,
    }
}

/// bap = 4: `code = 11a + b` (§7.3.5), уровни Таблицы 7.22.
fn ungroup_bap4(code: u16) -> GroupValues {
    let first = level(&BAP4_LEVELS, code / 11);
    let second = level(&BAP4_LEVELS, code % 11);
    GroupValues {
        values: [first, second, 0.0],
        count: 2,
    }
}

/// Значение уровня; код уже проверен по максимуму группы, поэтому
/// `get` никогда не промахивается, а 0.0 — только защитная ветка.
fn level(levels: &[f32], digit: u16) -> f32 {
    levels.get(usize::from(digit)).copied().unwrap_or_default()
}

/// bap = 3 и 5: одиночный код, индекс таблицы (§7.3.3). Коды 7 (bap 3) и 15
/// (bap 5) в таблицах отсутствуют и считаются ошибкой потока.
fn symmetric(
    reader: &mut BitReader<'_>,
    bap: u8,
    bits: u32,
    levels: &[f32],
) -> Result<f32, MantissaReadError> {
    let code = reader.read_bits(bits)?;
    levels
        .get(usize::from(code))
        .copied()
        .ok_or(MantissaReadError::CodeOutOfRange { bap, code })
}

/// bap 6–15: дробное число в дополнительном коде длиной `qntztab[bap]`
/// бит, точка левее старшего бита (§7.3.2): диапазон −1,0 … 1 − 2^−(n−1).
fn asymmetric(reader: &mut BitReader<'_>, bap: u8) -> Result<f32, MantissaReadError> {
    let bits =
        asymmetric_mantissa_bits(bap).ok_or(MantissaReadError::CodeOutOfRange { bap, code: 0 })?;
    let bits = u32::from(bits);
    let raw = i32::from(reader.read_bits(bits)?);
    // Расширение знака: старший бит n-битного поля — знаковый.
    let signed = (raw << (32 - bits)) >> (32 - bits);
    Ok(signed as f32 / (1i32 << (bits - 1)) as f32)
}

#[cfg(test)]
mod tests {
    use super::{Mantissa, MantissaReadError, MantissaUnpacker};
    use crate::bitstream::BitReader;

    #[test]
    fn grouped_bap1_reads_once_and_spans_channels() {
        // Код 5 бит = 9·2 + 3·0 + 1 = 19 → +2/3, −2/3, 0; дальше новая группа 0 → −2/3 ×3.
        let bytes = [0b1001_1000, 0b0000_0000];
        let mut reader = BitReader::new(&bytes);
        let mut unpacker = MantissaUnpacker::default();
        let first = unpacker.read(&mut reader, 1).unwrap();
        assert_eq!(reader.position_bits(), 5);
        let second = unpacker.read(&mut reader, 1).unwrap();
        // Между мантиссами одной группы может встретиться другой bap — карман не теряется.
        assert_eq!(unpacker.read(&mut reader, 0).unwrap(), Mantissa::ZeroBits);
        let third = unpacker.read(&mut reader, 1).unwrap();
        assert_eq!(reader.position_bits(), 5);
        assert_eq!(
            [first, second, third],
            [
                Mantissa::Value(2.0 / 3.0),
                Mantissa::Value(-2.0 / 3.0),
                Mantissa::Value(0.0)
            ]
        );
        assert_eq!(
            unpacker.read(&mut reader, 1).unwrap(),
            Mantissa::Value(-2.0 / 3.0)
        );
        assert_eq!(reader.position_bits(), 10);
    }

    #[test]
    fn grouped_bap4_is_a_pair_and_bap2_a_triple() {
        // bap 4: 7 бит = 11·10 + 0 = 110 → 10/11, −10/11.
        let bytes = [110u8 << 1, 0, 0];
        let mut reader = BitReader::new(&bytes);
        let mut unpacker = MantissaUnpacker::default();
        assert_eq!(
            unpacker.read(&mut reader, 4).unwrap(),
            Mantissa::Value(10.0 / 11.0)
        );
        assert_eq!(
            unpacker.read(&mut reader, 4).unwrap(),
            Mantissa::Value(-10.0 / 11.0)
        );
        assert_eq!(reader.position_bits(), 7);
        // bap 2: следующий код 0 → три раза −4/5, читается один раз.
        for _ in 0..3 {
            assert_eq!(
                unpacker.read(&mut reader, 2).unwrap(),
                Mantissa::Value(-4.0 / 5.0)
            );
        }
        assert_eq!(reader.position_bits(), 14);
    }

    #[test]
    fn asymmetric_mantissas_are_twos_complement_fractions() {
        // bap 6 → 5 бит: 0b10000 = −1.0, 0b01111 = 15/16.
        let bytes = [0b1000_0011, 0b1100_0000];
        let mut reader = BitReader::new(&bytes);
        let mut unpacker = MantissaUnpacker::default();
        assert_eq!(
            unpacker.read(&mut reader, 6).unwrap(),
            Mantissa::Value(-1.0)
        );
        assert_eq!(
            unpacker.read(&mut reader, 6).unwrap(),
            Mantissa::Value(15.0 / 16.0)
        );
        // bap 15 → 16 бит.
        let wide = [0xFF, 0xFF];
        let mut reader = BitReader::new(&wide);
        assert_eq!(
            unpacker.read(&mut reader, 15).unwrap(),
            Mantissa::Value(-1.0 / 32_768.0)
        );
    }

    #[test]
    fn out_of_table_codes_and_overruns_are_errors() {
        let ones = [0xFF, 0xFF];
        let mut unpacker = MantissaUnpacker::default();
        let cases = [(1u8, 31u16), (2, 127), (3, 7), (4, 127), (5, 15)];
        for (bap, code) in cases {
            let mut reader = BitReader::new(&ones);
            assert_eq!(
                unpacker.read(&mut reader, bap),
                Err(MantissaReadError::CodeOutOfRange { bap, code }),
                "bap {bap}"
            );
        }
        let mut short = BitReader::new(&ones[..1]);
        assert_eq!(
            unpacker.read(&mut short, 12),
            Err(MantissaReadError::Overrun)
        );
    }
}
