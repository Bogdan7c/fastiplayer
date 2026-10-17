//! Квантователи мантисс (A/52 §7.3).
//!
//! Таблицы 7.19–7.23 (симметричные квантователи bap 1–5) переписаны из
//! ATSC A/52:2012 дробями ровно в том виде, в каком они напечатаны. Таблица
//! 7.18 (`qntztab`) задаёт длину мантиссы для несимметричных квантователей
//! bap 6–15. Тесты сверяют структурное свойство таблиц из §7.3.3: значение
//! кода `k` у `L`-уровневого квантователя равно `(2k − (L − 1)) / L`.

/// Таблица 7.19 «bap=1 (3-Level) Quantization».
pub(crate) const BAP1_LEVELS: [f32; 3] = [-2.0 / 3.0, 0.0, 2.0 / 3.0];

/// Таблица 7.20 «bap=2 (5-Level) Quantization».
pub(crate) const BAP2_LEVELS: [f32; 5] = [-4.0 / 5.0, -2.0 / 5.0, 0.0, 2.0 / 5.0, 4.0 / 5.0];

/// Таблица 7.21 «bap=3 (7-Level) Quantization».
pub(crate) const BAP3_LEVELS: [f32; 7] = [
    -6.0 / 7.0,
    -4.0 / 7.0,
    -2.0 / 7.0,
    0.0,
    2.0 / 7.0,
    4.0 / 7.0,
    6.0 / 7.0,
];

/// Таблица 7.22 «bap=4 (11-Level) Quantization».
pub(crate) const BAP4_LEVELS: [f32; 11] = [
    -10.0 / 11.0,
    -8.0 / 11.0,
    -6.0 / 11.0,
    -4.0 / 11.0,
    -2.0 / 11.0,
    0.0,
    2.0 / 11.0,
    4.0 / 11.0,
    6.0 / 11.0,
    8.0 / 11.0,
    10.0 / 11.0,
];

/// Таблица 7.23 «bap=5 (15-Level) Quantization».
pub(crate) const BAP5_LEVELS: [f32; 15] = [
    -14.0 / 15.0,
    -12.0 / 15.0,
    -10.0 / 15.0,
    -8.0 / 15.0,
    -6.0 / 15.0,
    -4.0 / 15.0,
    -2.0 / 15.0,
    0.0,
    2.0 / 15.0,
    4.0 / 15.0,
    6.0 / 15.0,
    8.0 / 15.0,
    10.0 / 15.0,
    12.0 / 15.0,
    14.0 / 15.0,
];

/// Таблица 7.18, колонка «Mantissa Bits (qntztab[bap])» для несимметричных
/// квантователей: индекс — `bap − 6`.
pub(crate) const ASYMMETRIC_MANTISSA_BITS: [u8; 10] = [5, 6, 7, 8, 9, 10, 11, 12, 14, 16];

/// Длина несимметричной мантиссы для `bap` 6–15 (Таблица 7.18).
pub(crate) fn asymmetric_mantissa_bits(bap: u8) -> Option<u8> {
    ASYMMETRIC_MANTISSA_BITS
        .get(usize::from(bap.checked_sub(6)?))
        .copied()
}

/// Сколько бит занимает в потоке группа мантисс bap 1, 2, 4 (§7.3.5):
/// 5 бит на тройку (3 уровня), 7 бит на тройку (5 уровней), 7 бит на пару (11 уровней).
pub(crate) const BAP1_GROUP_BITS: u32 = 5;
/// См. [`BAP1_GROUP_BITS`].
pub(crate) const BAP2_GROUP_BITS: u32 = 7;
/// См. [`BAP1_GROUP_BITS`].
pub(crate) const BAP4_GROUP_BITS: u32 = 7;
/// Длина одиночной мантиссы bap 3 (7 уровней), Таблица 7.18.
pub(crate) const BAP3_MANTISSA_BITS: u32 = 3;
/// Длина одиночной мантиссы bap 5 (15 уровней), Таблица 7.18.
pub(crate) const BAP5_MANTISSA_BITS: u32 = 4;

#[cfg(test)]
mod tests {
    use super::{
        ASYMMETRIC_MANTISSA_BITS, BAP1_LEVELS, BAP2_LEVELS, BAP3_LEVELS, BAP4_LEVELS, BAP5_LEVELS,
        asymmetric_mantissa_bits,
    };

    fn assert_symmetric_quantizer(levels: &[f32]) {
        let level_count = levels.len() as f32;
        for (code, &value) in levels.iter().enumerate() {
            let expected = (2.0 * code as f32 - (level_count - 1.0)) / level_count;
            assert!(
                (value - expected).abs() < 1e-7,
                "{level_count} levels, code {code}"
            );
        }
        // Таблицы симметричны относительно нуля, нулевой код — посередине.
        assert_eq!(levels[levels.len() / 2], 0.0);
    }

    #[test]
    fn symmetric_tables_follow_their_structure() {
        assert_symmetric_quantizer(&BAP1_LEVELS);
        assert_symmetric_quantizer(&BAP2_LEVELS);
        assert_symmetric_quantizer(&BAP3_LEVELS);
        assert_symmetric_quantizer(&BAP4_LEVELS);
        assert_symmetric_quantizer(&BAP5_LEVELS);
    }

    #[test]
    fn asymmetric_bits_match_table_7_18() {
        // 32 … 65 536 уровней = 2^bits.
        let levels: Vec<u32> = ASYMMETRIC_MANTISSA_BITS
            .iter()
            .map(|&bits| 1u32 << bits)
            .collect();
        assert_eq!(
            levels,
            [32, 64, 128, 256, 512, 1024, 2048, 4096, 16_384, 65_536]
        );
        assert_eq!(asymmetric_mantissa_bits(5), None);
        assert_eq!(asymmetric_mantissa_bits(6), Some(5));
        assert_eq!(asymmetric_mantissa_bits(15), Some(16));
        assert_eq!(asymmetric_mantissa_bits(16), None);
    }
}
