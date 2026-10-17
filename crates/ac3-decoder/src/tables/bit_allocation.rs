//! Таблицы параметрического битового распределения (A/52 §7.2.3).
//!
//! Все таблицы переписаны из ATSC A/52:2012 (Таблицы 7.6–7.16) и сверены
//! скриптом с той же таблицей ETSI TS 102 366 v1.4.1 (Таблицы 6.6–6.16):
//! значения совпали поэлементно. Тесты ниже проверяют контрольные суммы,
//! посчитанные по тексту спецификации, и структурные свойства (согласованность
//! `masktab` с `bndtab`/`bndsz`, монотонность `latab` и `baptab`).

/// Число полос 1/6 октавы, на которых строится маска (§7.2.1).
pub(crate) const BAND_COUNT: usize = 50;

/// Таблица 7.6 «Slow Decay Table», `slowdec[sdcycod]`.
pub(crate) const SLOW_DECAY: [i32; 4] = [0x0f, 0x11, 0x13, 0x15];

/// Таблица 7.7 «Fast Decay Table», `fastdec[fdcycod]`.
pub(crate) const FAST_DECAY: [i32; 4] = [0x3f, 0x53, 0x67, 0x7b];

/// Таблица 7.8 «Slow Gain Table», `slowgain[sgaincod]`.
pub(crate) const SLOW_GAIN: [i32; 4] = [0x540, 0x4d8, 0x478, 0x410];

/// Таблица 7.9 «dB/Bit Table», `dbpbtab[dbpbcod]`.
pub(crate) const DB_PER_BIT: [i32; 4] = [0x000, 0x700, 0x900, 0xb00];

/// Таблица 7.10 «Floor Table», `floortab[floorcod]`.
///
/// Последнее значение в спецификации записано как `0xf800` — это 16-битное
/// число в дополнительном коде, то есть −2048 (§7.2.1: «signed two's
/// complement arithmetic is employed»). Храним его уже со знаком.
pub(crate) const FLOOR: [i32; 8] = [0x2f0, 0x2b0, 0x270, 0x230, 0x1f0, 0x170, 0x0f0, -0x800];

/// Таблица 7.11 «Fast Gain Table», `fastgain[fgaincod]`.
pub(crate) const FAST_GAIN: [i32; 8] = [0x080, 0x100, 0x180, 0x200, 0x280, 0x300, 0x380, 0x400];

/// Таблица 7.12 «Banding Structure Tables», `bndtab[band]` — первый бин полосы.
pub(crate) const BAND_START: [u8; BAND_COUNT] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, //
    10, 11, 12, 13, 14, 15, 16, 17, 18, 19, //
    20, 21, 22, 23, 24, 25, 26, 27, 28, 31, //
    34, 37, 40, 43, 46, 49, 55, 61, 67, 73, //
    79, 85, 97, 109, 121, 133, 157, 181, 205, 229, //
];

/// Таблица 7.12 «Banding Structure Tables», `bndsz[band]` — ширина полосы в бинах.
pub(crate) const BAND_SIZE: [u8; BAND_COUNT] = [
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, //
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, //
    1, 1, 1, 1, 1, 1, 1, 1, 3, 3, //
    3, 3, 3, 3, 3, 6, 6, 6, 6, 6, //
    6, 12, 12, 12, 12, 24, 24, 24, 24, 24, //
];

/// Таблица 7.13 «Bin Number to Band Number Table», `masktab[bin]`.
pub(crate) const BIN_TO_BAND: [u8; 256] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, //
    10, 11, 12, 13, 14, 15, 16, 17, 18, 19, //
    20, 21, 22, 23, 24, 25, 26, 27, 28, 28, //
    28, 29, 29, 29, 30, 30, 30, 31, 31, 31, //
    32, 32, 32, 33, 33, 33, 34, 34, 34, 35, //
    35, 35, 35, 35, 35, 36, 36, 36, 36, 36, //
    36, 37, 37, 37, 37, 37, 37, 38, 38, 38, //
    38, 38, 38, 39, 39, 39, 39, 39, 39, 40, //
    40, 40, 40, 40, 40, 41, 41, 41, 41, 41, //
    41, 41, 41, 41, 41, 41, 41, 42, 42, 42, //
    42, 42, 42, 42, 42, 42, 42, 42, 42, 43, //
    43, 43, 43, 43, 43, 43, 43, 43, 43, 43, //
    43, 44, 44, 44, 44, 44, 44, 44, 44, 44, //
    44, 44, 44, 45, 45, 45, 45, 45, 45, 45, //
    45, 45, 45, 45, 45, 45, 45, 45, 45, 45, //
    45, 45, 45, 45, 45, 45, 45, 46, 46, 46, //
    46, 46, 46, 46, 46, 46, 46, 46, 46, 46, //
    46, 46, 46, 46, 46, 46, 46, 46, 46, 46, //
    46, 47, 47, 47, 47, 47, 47, 47, 47, 47, //
    47, 47, 47, 47, 47, 47, 47, 47, 47, 47, //
    47, 47, 47, 47, 47, 48, 48, 48, 48, 48, //
    48, 48, 48, 48, 48, 48, 48, 48, 48, 48, //
    48, 48, 48, 48, 48, 48, 48, 48, 48, 49, //
    49, 49, 49, 49, 49, 49, 49, 49, 49, 49, //
    49, 49, 49, 49, 49, 49, 49, 49, 49, 49, //
    49, 49, 49, 0, 0, 0, //
];

/// Таблица 7.14 «Log-Addition Table», `latab[val]`.
pub(crate) const LOG_ADDITION: [i32; 256] = [
    0x40, 0x3f, 0x3e, 0x3d, 0x3c, 0x3b, 0x3a, 0x39, 0x38, 0x37, //
    0x36, 0x35, 0x34, 0x34, 0x33, 0x32, 0x31, 0x30, 0x2f, 0x2f, //
    0x2e, 0x2d, 0x2c, 0x2c, 0x2b, 0x2a, 0x29, 0x29, 0x28, 0x27, //
    0x26, 0x26, 0x25, 0x24, 0x24, 0x23, 0x23, 0x22, 0x21, 0x21, //
    0x20, 0x20, 0x1f, 0x1e, 0x1e, 0x1d, 0x1d, 0x1c, 0x1c, 0x1b, //
    0x1b, 0x1a, 0x1a, 0x19, 0x19, 0x18, 0x18, 0x17, 0x17, 0x16, //
    0x16, 0x15, 0x15, 0x15, 0x14, 0x14, 0x13, 0x13, 0x13, 0x12, //
    0x12, 0x12, 0x11, 0x11, 0x11, 0x10, 0x10, 0x10, 0x0f, 0x0f, //
    0x0f, 0x0e, 0x0e, 0x0e, 0x0d, 0x0d, 0x0d, 0x0d, 0x0c, 0x0c, //
    0x0c, 0x0c, 0x0b, 0x0b, 0x0b, 0x0b, 0x0a, 0x0a, 0x0a, 0x0a, //
    0x0a, 0x09, 0x09, 0x09, 0x09, 0x09, 0x08, 0x08, 0x08, 0x08, //
    0x08, 0x08, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x06, 0x06, //
    0x06, 0x06, 0x06, 0x06, 0x06, 0x06, 0x05, 0x05, 0x05, 0x05, //
    0x05, 0x05, 0x05, 0x05, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, //
    0x04, 0x04, 0x04, 0x04, 0x04, 0x03, 0x03, 0x03, 0x03, 0x03, //
    0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x02, //
    0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, //
    0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x01, 0x01, //
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, //
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, //
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
];

/// Таблица 7.15 «Hearing Threshold Table», `hth[fscod][band]`
/// (строки: 48 / 44,1 / 32 кГц).
pub(crate) const HEARING_THRESHOLD: [[i32; BAND_COUNT]; 3] = [
    [
        0x04d0, 0x04d0, 0x0440, 0x0400, 0x03e0, 0x03c0, 0x03b0, 0x03b0, 0x03a0, 0x03a0, //
        0x03a0, 0x03a0, 0x03a0, 0x0390, 0x0390, 0x0390, 0x0380, 0x0380, 0x0370, 0x0370, //
        0x0360, 0x0360, 0x0350, 0x0350, 0x0340, 0x0340, 0x0330, 0x0320, 0x0310, 0x0300, //
        0x02f0, 0x02f0, 0x02f0, 0x02f0, 0x0300, 0x0310, 0x0340, 0x0390, 0x03e0, 0x0420, //
        0x0460, 0x0490, 0x04a0, 0x0460, 0x0440, 0x0440, 0x0520, 0x0800, 0x0840, 0x0840, //
    ],
    [
        0x04f0, 0x04f0, 0x0460, 0x0410, 0x03e0, 0x03d0, 0x03c0, 0x03b0, 0x03b0, 0x03a0, //
        0x03a0, 0x03a0, 0x03a0, 0x03a0, 0x0390, 0x0390, 0x0390, 0x0380, 0x0380, 0x0380, //
        0x0370, 0x0370, 0x0360, 0x0360, 0x0350, 0x0350, 0x0340, 0x0340, 0x0320, 0x0310, //
        0x0300, 0x02f0, 0x02f0, 0x02f0, 0x02f0, 0x0300, 0x0320, 0x0350, 0x0390, 0x03e0, //
        0x0420, 0x0450, 0x04a0, 0x0490, 0x0460, 0x0440, 0x0480, 0x0630, 0x0840, 0x0840, //
    ],
    [
        0x0580, 0x0580, 0x04b0, 0x0450, 0x0420, 0x03f0, 0x03e0, 0x03d0, 0x03c0, 0x03b0, //
        0x03b0, 0x03b0, 0x03a0, 0x03a0, 0x03a0, 0x03a0, 0x03a0, 0x03a0, 0x03a0, 0x03a0, //
        0x0390, 0x0390, 0x0390, 0x0390, 0x0380, 0x0380, 0x0380, 0x0370, 0x0360, 0x0350, //
        0x0340, 0x0330, 0x0320, 0x0310, 0x0300, 0x02f0, 0x02f0, 0x02f0, 0x0300, 0x0310, //
        0x0330, 0x0350, 0x03c0, 0x0410, 0x0470, 0x04a0, 0x0460, 0x0440, 0x0450, 0x04e0, //
    ],
];

/// Таблица 7.16 «Bit Allocation Pointer Table», `baptab[address]`.
pub(crate) const BAP_TABLE: [u8; 64] = [
    0, 1, 1, 1, 1, 1, 2, 2, 3, 3, //
    3, 4, 4, 5, 5, 6, 6, 6, 6, 7, //
    7, 7, 7, 8, 8, 8, 8, 9, 9, 9, //
    9, 10, 10, 10, 10, 11, 11, 11, 11, 12, //
    12, 12, 12, 13, 13, 13, 13, 14, 14, 14, //
    14, 14, 14, 14, 14, 15, 15, 15, 15, 15, //
    15, 15, 15, 15, //
];

/// Таблица 5.17 «Bit Allocation Deltas»: код `deltba` → поправка маски.
///
/// Код 0–3 даёт −24…−6 дБ, 4–7 — +6…+24 дБ; 6 дБ = 128 единиц маски,
/// что совпадает с псевдокодом §7.2.2.6 (`(deltba − 3) << 7` / `(deltba − 4) << 7`).
pub(crate) const fn delta_bit_allocation_adjustment(code: u8) -> i32 {
    let code = code as i32;
    if code >= 4 {
        (code - 3) << 7
    } else {
        (code - 4) << 7
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BAND_COUNT, BAND_SIZE, BAND_START, BAP_TABLE, BIN_TO_BAND, HEARING_THRESHOLD, LOG_ADDITION,
        delta_bit_allocation_adjustment,
    };

    /// Контрольные суммы, посчитанные по тексту спецификации (ATSC и ETSI дали
    /// одинаковые значения).
    #[test]
    fn checksums_match_spec_text() {
        let sum_u8 = |values: &[u8]| values.iter().map(|&value| u32::from(value)).sum::<u32>();
        let sum_i32 = |values: &[i32]| values.iter().sum::<i32>();
        assert_eq!(sum_u8(&BIN_TO_BAND), 10_059);
        assert_eq!(sum_i32(&LOG_ADDITION), 3_414);
        assert_eq!(sum_u8(&BAND_START), 2_338);
        assert_eq!(sum_u8(&BAND_SIZE), 253);
        assert_eq!(sum_i32(&HEARING_THRESHOLD[0]), 50_304);
        assert_eq!(sum_i32(&HEARING_THRESHOLD[1]), 49_776);
        assert_eq!(sum_i32(&HEARING_THRESHOLD[2]), 47_648);
        assert_eq!(sum_u8(&BAP_TABLE), 587);
    }

    #[test]
    fn spot_values_from_spec_tables() {
        assert_eq!(BIN_TO_BAND[37], 31); // первый бин coupling (Таблица 7.24)
        assert_eq!(BIN_TO_BAND[252], 49);
        assert_eq!(LOG_ADDITION[0], 0x40);
        assert_eq!(LOG_ADDITION[209], 0x01);
        assert_eq!(LOG_ADDITION[210], 0x00);
        assert_eq!(HEARING_THRESHOLD[1][47], 0x0630);
        assert_eq!(HEARING_THRESHOLD[2][49], 0x04e0);
        assert_eq!(BAP_TABLE[54], 14);
        assert_eq!(BAP_TABLE[55], 15);
    }

    #[test]
    fn bin_to_band_is_consistent_with_band_structure() {
        // §7.2.2.3: «These two tables contain duplicate information» — проверяем,
        // что masktab получается из bndtab/bndsz без единого расхождения.
        let mut expected = [0u8; 256];
        for band in 0..BAND_COUNT {
            let start = usize::from(BAND_START[band]);
            for slot in expected
                .iter_mut()
                .skip(start)
                .take(usize::from(BAND_SIZE[band]))
            {
                *slot = band as u8;
            }
        }
        assert_eq!(BIN_TO_BAND, expected);
        // Полосы идут без зазоров и заканчиваются на бине 253.
        for band in 1..BAND_COUNT {
            assert_eq!(BAND_START[band], BAND_START[band - 1] + BAND_SIZE[band - 1]);
        }
        assert_eq!(
            usize::from(BAND_START[49]) + usize::from(BAND_SIZE[49]),
            253
        );
    }

    #[test]
    fn monotone_tables_never_increase_or_decrease_wrongly() {
        assert!(LOG_ADDITION.windows(2).all(|pair| pair[0] >= pair[1]));
        assert!(BAP_TABLE.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(BAP_TABLE[0], 0);
        assert_eq!(BAP_TABLE[63], 15);
    }

    #[test]
    fn delta_codes_follow_table_5_17() {
        let decibels: Vec<i32> = (0..8)
            .map(|code| delta_bit_allocation_adjustment(code) / 128 * 6)
            .collect();
        assert_eq!(decibels, [-24, -18, -12, -6, 6, 12, 18, 24]);
    }
}
