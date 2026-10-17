//! Таблицы адаптивного гибридного преобразования AHT (A/52 Annex E §3.4.3–3.4.4;
//! ETSI TS 102 366 v1.4.1 §E.2.4.3–E.2.4.4).
//!
//! - Таблица E3.1 — `hebaptab[]`: адрес «PSD − маска» → указатель hebap;
//! - Таблица E3.2 — тип квантователя и число бит мантиссы для каждого hebap;
//! - Таблица E3.6 — константы «перекладки» (remapping) мантисс GAQ.
//!
//! Все три таблицы сверены с текстом ATSC и ETSI скриптом
//! `tools/extract_eac3_tables.py` (печать `E3.1`, `E3.6`).

/// Первый hebap скалярного квантователя (8–19; 1–7 — векторные, Таблица E3.2).
pub(crate) const FIRST_SCALAR_HEBAP: u8 = 8;

/// Таблица E3.1 «High Efficiency Bit Allocation Pointers», `hebaptab[address]`.
pub(crate) const HIGH_EFFICIENCY_BAP_TABLE: [u8; 64] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 8, //
    8, 8, 9, 9, 9, 10, 10, 10, 10, 11, //
    11, 11, 11, 12, 12, 12, 12, 13, 13, 13, //
    13, 14, 14, 14, 14, 15, 15, 15, 15, 16, //
    16, 16, 16, 17, 17, 17, 17, 18, 18, 18, //
    18, 18, 18, 18, 18, 19, 19, 19, 19, 19, //
    19, 19, 19, 19, //
];

/// Таблица E3.2, колонка «Mantissa Bits» для VQ (hebap 1–7): дробь `N/6`
/// означает N-битный индекс вектора на 6 мантисс.
const VQ_INDEX_BITS: [u32; 7] = [2, 3, 4, 5, 7, 8, 9];

/// Таблица E3.2, колонка «Mantissa Bits» для скалярных квантователей
/// (hebap 8–19): `m` бит, `2^m − 1` уровней.
const SCALAR_MANTISSA_BITS: [u32; 12] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16];

/// Сколько бит занимает индекс вектора для hebap 1–7.
pub(crate) fn vq_index_bits(hebap: u8) -> Option<u32> {
    let index = usize::from(hebap).checked_sub(1)?;
    VQ_INDEX_BITS.get(index).copied()
}

/// `m` — длина кодового слова скалярного квантователя для hebap 8–19.
pub(crate) fn scalar_mantissa_bits(hebap: u8) -> Option<u32> {
    let index = usize::from(hebap.checked_sub(FIRST_SCALAR_HEBAP)?);
    SCALAR_MANTISSA_BITS.get(index).copied()
}

/// Усиление `Gk` блока GAQ (Таблицы E3.3–E3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GaqGain {
    /// `Gk = 1`: один квантователь без тега (и все мантиссы без GAQ).
    One,
    /// `Gk = 2`: малые мантиссы `m − 1` бит, большие — `m − 1` бит после тега.
    Two,
    /// `Gk = 4`: малые мантиссы `m − 2` бит, большие — `m` бит после тега.
    Four,
}

/// Константы перекладки `y = x + a·x + b` (Таблица E3.6) в 16-битном
/// дополнительном коде с точкой левее старшего бита.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemapConstants {
    /// `a`.
    pub(crate) slope_correction: i16,
    /// `b` при `x ≥ 0`.
    pub(crate) offset_non_negative: i16,
    /// `b` при `x < 0`.
    pub(crate) offset_negative: i16,
}

/// Сокращённый конструктор строки таблицы из шестнадцатеричных слов спецификации.
const fn remap(a: u16, b_non_negative: u16, b_negative: u16) -> RemapConstants {
    RemapConstants {
        slope_correction: a as i16,
        offset_non_negative: b_non_negative as i16,
        offset_negative: b_negative as i16,
    }
}

/// Таблица E3.6, колонки `Gk = 1`, hebap 8–19.
const REMAP_GAIN_ONE: [RemapConstants; 12] = [
    remap(0x1249, 0x0000, 0x0000),
    remap(0x0889, 0x0000, 0x0000),
    remap(0x0421, 0x0000, 0x0000),
    remap(0x0208, 0x0000, 0x0000),
    remap(0x0102, 0x0000, 0x0000),
    remap(0x0081, 0x0000, 0x0000),
    remap(0x0040, 0x0000, 0x0000),
    remap(0x0020, 0x0000, 0x0000),
    remap(0x0010, 0x0000, 0x0000),
    remap(0x0008, 0x0000, 0x0000),
    remap(0x0002, 0x0000, 0x0000),
    remap(0x0000, 0x0000, 0x0000),
];

/// Таблица E3.6, колонки `Gk = 2`, hebap 8–16 (выше — N/A).
const REMAP_GAIN_TWO: [RemapConstants; 9] = [
    remap(0xd555, 0x4000, 0xeaab),
    remap(0xc925, 0x4000, 0xd249),
    remap(0xc444, 0x4000, 0xc889),
    remap(0xc211, 0x4000, 0xc421),
    remap(0xc104, 0x4000, 0xc208),
    remap(0xc081, 0x4000, 0xc102),
    remap(0xc040, 0x4000, 0xc081),
    remap(0xc020, 0x4000, 0xc040),
    remap(0xc010, 0x4000, 0xc020),
];

/// Таблица E3.6, колонки `Gk = 4`, hebap 8–16 (выше — N/A).
const REMAP_GAIN_FOUR: [RemapConstants; 9] = [
    remap(0xedb7, 0x2000, 0xfb6e),
    remap(0xe666, 0x2000, 0xeccd),
    remap(0xe319, 0x2000, 0xe632),
    remap(0xe186, 0x2000, 0xe30c),
    remap(0xe0c2, 0x2000, 0xe183),
    remap(0xe060, 0x2000, 0xe0c1),
    remap(0xe030, 0x2000, 0xe060),
    remap(0xe018, 0x2000, 0xe030),
    remap(0xe00c, 0x2000, 0xe018),
];

/// Константы перекладки для hebap и усиления; `None` — клетка «N/A».
pub(crate) fn remap_constants(hebap: u8, gain: GaqGain) -> Option<RemapConstants> {
    let index = usize::from(hebap.checked_sub(FIRST_SCALAR_HEBAP)?);
    let column: &[RemapConstants] = match gain {
        GaqGain::One => &REMAP_GAIN_ONE,
        GaqGain::Two => &REMAP_GAIN_TWO,
        GaqGain::Four => &REMAP_GAIN_FOUR,
    };
    column.get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::{
        FIRST_SCALAR_HEBAP, GaqGain, HIGH_EFFICIENCY_BAP_TABLE, remap_constants,
        scalar_mantissa_bits, vq_index_bits,
    };

    /// Наибольший указатель hebap (Таблица E3.2: 0–19).
    const MAX_HEBAP: u8 = 19;

    /// Значения, напечатанные `extract_eac3_tables.py` (`E3.1`) по тексту ATSC и ETSI.
    #[test]
    fn hebap_table_matches_spec_text() {
        let table_sum: u32 = HIGH_EFFICIENCY_BAP_TABLE
            .iter()
            .map(|&v| u32::from(v))
            .sum();
        assert_eq!(table_sum, 834);
        // Адреса 0–8 дают hebap 0–8 подряд, затем по 4 адреса на шаг до 18 (8 адресов) и 19.
        assert_eq!(
            &HIGH_EFFICIENCY_BAP_TABLE[..9],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(HIGH_EFFICIENCY_BAP_TABLE[47..55], [18; 8]);
        assert_eq!(HIGH_EFFICIENCY_BAP_TABLE[55..], [19; 9]);
        assert!(
            HIGH_EFFICIENCY_BAP_TABLE
                .windows(2)
                .all(|pair| pair[0] <= pair[1])
        );
        assert_eq!(HIGH_EFFICIENCY_BAP_TABLE[63], MAX_HEBAP);
    }

    #[test]
    fn mantissa_bits_follow_table_e3_2() {
        assert_eq!(
            (1..=7)
                .map(|h| vq_index_bits(h).unwrap())
                .collect::<Vec<_>>(),
            [2, 3, 4, 5, 7, 8, 9]
        );
        assert_eq!(vq_index_bits(0), None);
        assert_eq!(vq_index_bits(8), None);
        // Уровней 2^m − 1 = 7, 15, …, 4095, 16 383, 65 535 (Таблица E3.2).
        let levels: Vec<u32> = (FIRST_SCALAR_HEBAP..=MAX_HEBAP)
            .map(|hebap| (1 << scalar_mantissa_bits(hebap).unwrap()) - 1)
            .collect();
        assert_eq!(
            levels,
            [
                7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 16_383, 65_535
            ]
        );
        assert_eq!(scalar_mantissa_bits(20), None);
    }

    fn fraction(value: i16) -> f64 {
        f64::from(value) / 32_768.0
    }

    /// Константы Таблицы E3.6 обязаны давать шаги квантователей Таблицы E3.5
    /// и симметричную характеристику; сверяем с точностью до 2 младших бит.
    #[test]
    fn remap_constants_realize_table_e3_5_step_sizes() {
        const TOLERANCE: f64 = 2.0 / 32_768.0;
        for hebap in FIRST_SCALAR_HEBAP..=MAX_HEBAP {
            let m = scalar_mantissa_bits(hebap).unwrap() as i32;
            let one = remap_constants(hebap, GaqGain::One).unwrap();
            // Gk = 1: x — m-битная дробь (шаг 2^−(m−1)), нужный шаг 2/(2^m − 1).
            let slope = 2.0 / (2f64.powi(m) - 1.0) / 2f64.powi(-(m - 1));
            assert!(
                (1.0 + fraction(one.slope_correction) - slope).abs() <= TOLERANCE,
                "{hebap}"
            );
            assert_eq!((one.offset_non_negative, one.offset_negative), (0, 0));
            if hebap > 16 {
                assert_eq!(remap_constants(hebap, GaqGain::Two), None);
                assert_eq!(remap_constants(hebap, GaqGain::Four), None);
                continue;
            }
            // Gk = 2, большие мантиссы: x — (m−1)-битная дробь, шаг 1/(2^(m−1) − 1), начало 0,5.
            let two = remap_constants(hebap, GaqGain::Two).unwrap();
            let step_x = 2f64.powi(-(m - 2));
            let gain_two = 1.0 + fraction(two.slope_correction);
            assert!((gain_two * step_x - 1.0 / (2f64.powi(m - 1) - 1.0)).abs() <= TOLERANCE);
            assert_eq!(fraction(two.offset_non_negative), 0.5);
            assert!((fraction(two.offset_negative) - (gain_two * step_x - 0.5)).abs() <= TOLERANCE);
            // Gk = 4, большие мантиссы: x — m-битная дробь, шаг 3/(2^(m+1) − 2), начало 0,25.
            let four = remap_constants(hebap, GaqGain::Four).unwrap();
            let step_x = 2f64.powi(-(m - 1));
            let gain_four = 1.0 + fraction(four.slope_correction);
            assert!((gain_four * step_x - 3.0 / (2f64.powi(m + 1) - 2.0)).abs() <= TOLERANCE);
            assert_eq!(fraction(four.offset_non_negative), 0.25);
            assert!(
                (fraction(four.offset_negative) - (gain_four * step_x - 0.25)).abs() <= TOLERANCE
            );
        }
        assert_eq!(remap_constants(7, GaqGain::One), None);
    }
}
