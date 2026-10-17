//! Кодовые книги векторного квантования AHT (A/52 Annex E §3.4.4.1,
//! Таблицы E4.1–E4.7; ETSI TS 102 366 v1.4.1 Таблицы E.3.1–E.3.7).
//!
//! При hebap 1–7 шесть мантисс одного бина (по одной на каждый блок кадра)
//! передаются одним индексом вектора. Значения таблиц — 16-битные числа в
//! дополнительном коде, дробная точка левее старшего значащего бита (как у
//! мантисс AC-3), то есть значение = `i16 / 32768`.
//!
//! Таблицы извлечены программно из текстов ОБЕИХ спецификаций и сверены
//! поэлементно (`tools/extract_eac3_tables.py … check`); тесты ниже
//! повторяют контрольные суммы, которые печатает этот скрипт, и проверяют
//! размеры, заданные Таблицей E3.2 (2^бит индекса векторов).

mod hebap5;
mod hebap6;
mod hebap7;

use hebap5::VQ_HEBAP_5;
use hebap6::VQ_HEBAP_6;
use hebap7::VQ_HEBAP_7;

/// Размерность вектора: 6 блоков кадра AHT.
pub(crate) const VQ_DIMENSION: usize = 6;

/// Масштаб 16-битного дробного значения: точка левее старшего бита.
const FRACTION_SCALE: f32 = 32_768.0;

/// Таблица E4.1 (ATSC A/52:2012) = Таблица E.3.1 (ETSI TS 102 366 v1.4.1):
/// VQ-кодовая книга для hebap 1, 4 векторов × 6 значений в 16-битном
/// дополнительном коде (сгенерировано `tools/extract_eac3_tables.py`, не править руками).
pub(crate) const VQ_HEBAP_1: [[u16; 6]; 4] = [
    [0x1bff, 0x1283, 0x0452, 0x10ad, 0x28ac, 0x12d4],
    [0xe9ba, 0xf38d, 0xc76d, 0xfa90, 0xf815, 0x0351],
    [0x0279, 0x1837, 0x1b61, 0xce15, 0xf6fe, 0xf5b4],
    [0xfa44, 0xe489, 0x1da8, 0x2979, 0xe8c6, 0xf40a],
];

/// Таблица E4.2 (ATSC A/52:2012) = Таблица E.3.2 (ETSI TS 102 366 v1.4.1):
/// VQ-кодовая книга для hebap 2, 8 векторов × 6 значений в 16-битном
/// дополнительном коде (сгенерировано `tools/extract_eac3_tables.py`, не править руками).
pub(crate) const VQ_HEBAP_2: [[u16; 6]; 8] = [
    [0xd0d7, 0x0260, 0xe495, 0x024e, 0x0fa0, 0x0365],
    [0x1a24, 0x3d49, 0xe7de, 0xdbe9, 0xffb6, 0x0085],
    [0x073f, 0xfc23, 0x5074, 0xf498, 0xee85, 0x00e1],
    [0xfb56, 0xf0c3, 0xfccb, 0xe65a, 0xfc95, 0xb0b6],
    [0xf536, 0xf393, 0xf002, 0xea09, 0xbdcf, 0x2625],
    [0x060b, 0x1ab7, 0x07bc, 0x4f09, 0xfbd1, 0xec86],
    [0x184d, 0xba05, 0xea74, 0x187a, 0x0166, 0x048a],
    [0x0ea9, 0xfbd6, 0x10bb, 0xf365, 0x3e38, 0x27ca],
];

/// Таблица E4.3 (ATSC A/52:2012) = Таблица E.3.3 (ETSI TS 102 366 v1.4.1):
/// VQ-кодовая книга для hebap 3, 16 векторов × 6 значений в 16-битном
/// дополнительном коде (сгенерировано `tools/extract_eac3_tables.py`, не править руками).
pub(crate) const VQ_HEBAP_3: [[u16; 6]; 16] = [
    [0xd8d4, 0x512b, 0x2ae6, 0xee30, 0x031e, 0xffbc],
    [0x2b2a, 0x500a, 0xe627, 0xeb22, 0xf8fb, 0xf9a1],
    [0x0f89, 0xfde2, 0x1bce, 0xfb72, 0x499c, 0x3956],
    [0xef20, 0xffa0, 0xe381, 0xfe14, 0xa9de, 0xef4b],
    [0x0a84, 0x16e0, 0x159a, 0x5566, 0xe3d4, 0xeb33],
    [0xff79, 0xa4a1, 0x03c2, 0x1fb3, 0xfd7c, 0x017e],
    [0xf9e5, 0x0d48, 0xf31d, 0x1255, 0xe514, 0x577e],
    [0x0dcf, 0x0bd6, 0x1c80, 0x1846, 0x4ffc, 0xd0bd],
    [0x0039, 0xe559, 0x0738, 0xa8b3, 0xe8e1, 0x1aa7],
    [0xfccb, 0xf1b9, 0xfe7d, 0xe793, 0xf939, 0xa89b],
    [0xe862, 0x0632, 0xb636, 0xc7c8, 0x23fe, 0x02c1],
    [0xe9ac, 0x0108, 0xb9d4, 0x391a, 0x1ef1, 0xfeaf],
    [0xff92, 0x006c, 0x0008, 0x004a, 0xffa7, 0xffce],
    [0x19d4, 0xfa13, 0x54b7, 0xf986, 0xe0f3, 0xff0a],
    [0x54a3, 0xe741, 0xdf9e, 0xff9b, 0xfabb, 0xffea],
    [0xaa0d, 0xe6b4, 0x1f26, 0x0288, 0x0806, 0xfeb5],
];

/// Таблица E4.4 (ATSC A/52:2012) = Таблица E.3.4 (ETSI TS 102 366 v1.4.1):
/// VQ-кодовая книга для hebap 4, 32 векторов × 6 значений в 16-битном
/// дополнительном коде (сгенерировано `tools/extract_eac3_tables.py`, не править руками).
pub(crate) const VQ_HEBAP_4: [[u16; 6]; 32] = [
    [0x5903, 0x15c0, 0xe9e6, 0xff64, 0xfe06, 0xffdf],
    [0x19ec, 0xee0f, 0x375d, 0xbc6f, 0xbf75, 0x0360],
    [0x0e4a, 0x580c, 0x0068, 0xf91d, 0xffac, 0x0006],
    [0x544c, 0xba69, 0xe38e, 0xf9d9, 0xf7e2, 0xfec0],
    [0xf747, 0x2721, 0xf558, 0x3a5a, 0xcab8, 0xbb05],
    [0xf9e4, 0xbab6, 0xb527, 0x35a7, 0x0ac5, 0x0b87],
    [0x11a8, 0x1586, 0x1ce1, 0x2a2f, 0x4b1f, 0xca36],
    [0x018f, 0x0ba0, 0xfbb5, 0x1395, 0xfb79, 0x564f],
    [0x0e28, 0xf6c9, 0x1248, 0xf742, 0x58ae, 0x0eb5],
    [0xef97, 0xdfa3, 0xe566, 0xcf9a, 0xb812, 0x3c16],
    [0xebb2, 0xe52b, 0xd8c1, 0xdf54, 0xc16a, 0xafae],
    [0xff72, 0xa771, 0xfe90, 0x1127, 0xfe30, 0xfff3],
    [0x032e, 0xfba2, 0xfbbf, 0xa9fd, 0x004a, 0x0611],
    [0xf9ae, 0x4b16, 0xbb16, 0xcb4e, 0x034a, 0xf6fb],
    [0x1251, 0x406a, 0x514d, 0xc3e5, 0xefbc, 0xf080],
    [0xf314, 0x2bce, 0xcb1a, 0x351f, 0xb3ef, 0x35ca],
    [0x0719, 0x0356, 0x52e9, 0xfc3a, 0xf995, 0xfef4],
    [0xf5e5, 0xff95, 0xb146, 0x0178, 0x0496, 0xfed0],
    [0xf499, 0x01c5, 0xeaf2, 0x02ee, 0xa9ee, 0xfc2e],
    [0xb5bc, 0x41c7, 0x2710, 0xf204, 0x08a3, 0x05b3],
    [0x0553, 0xf59e, 0xffdf, 0xf01d, 0x048d, 0xaa1f],
    [0xde70, 0xf538, 0xbb90, 0xc18f, 0x3a31, 0x052b],
    [0x028c, 0xdb8d, 0x0cb5, 0xc6e2, 0x2f95, 0x4cec],
    [0xe727, 0x168d, 0xc3dd, 0x438b, 0x40ce, 0xf496],
    [0xfd6b, 0xfda7, 0x0649, 0x5852, 0x03e0, 0xfbeb],
    [0x1361, 0x2393, 0x2bd9, 0x1e95, 0x3fc0, 0x48c3],
    [0xaa90, 0xfa67, 0x008a, 0x05be, 0xf89d, 0xff3c],
    [0xb3d5, 0xb8e5, 0x2b30, 0xfdfc, 0x09ef, 0xf737],
    [0xfb54, 0xbb5a, 0x4eb6, 0x2cc6, 0xfe6f, 0x0a3b],
    [0x121e, 0xe026, 0x2e73, 0xc271, 0x44cf, 0xc595],
    [0xffad, 0x0116, 0x0143, 0x0037, 0xff66, 0x00e8],
    [0x1e6c, 0x05b6, 0x47db, 0x3bc0, 0xc26d, 0xfb95],
];

/// Кодовая книга для `hebap` 1–7 (`None` для остальных значений).
fn codebook(hebap: u8) -> Option<&'static [[u16; VQ_DIMENSION]]> {
    match hebap {
        1 => Some(&VQ_HEBAP_1),
        2 => Some(&VQ_HEBAP_2),
        3 => Some(&VQ_HEBAP_3),
        4 => Some(&VQ_HEBAP_4),
        5 => Some(&VQ_HEBAP_5),
        6 => Some(&VQ_HEBAP_6),
        7 => Some(&VQ_HEBAP_7),
        _ => None,
    }
}

/// Вектор шести деквантованных значений по индексу, прочитанному из потока.
///
/// Индекс имеет ровно столько бит, сколько векторов в книге, поэтому
/// промах (`None`) возможен только при неверном `hebap` у вызывающего кода.
pub(crate) fn vq_vector(hebap: u8, index: u16) -> Option<[f32; VQ_DIMENSION]> {
    #[cfg(test)]
    let index = quirk::shifted_index(hebap, index);
    let row = codebook(hebap)?.get(usize::from(index))?;
    Some(row.map(|raw| f32::from(raw as i16) / FRACTION_SCALE))
}

/// Dev-эмуляция особенности ffmpeg n9.0.2 (только тесты): декодер ffmpeg
/// берёт для hebap 4 строку `index + 1` Таблицы E4.4 (доказано сверкой с
/// Nero, см. память `ac3-decoder/core`). Включается на поток теста, чтобы
/// показать, что остальной остаток AHT против ffmpeg — сетка округления.
#[cfg(test)]
pub(crate) mod quirk {
    use std::cell::Cell;

    thread_local! {
        static FFMPEG_HEBAP4_SHIFT: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn emulate_ffmpeg_hebap4_shift(enabled: bool) {
        FFMPEG_HEBAP4_SHIFT.with(|flag| flag.set(enabled));
    }

    pub(crate) fn shifted_index(hebap: u8, index: u16) -> u16 {
        if hebap == 4 && FFMPEG_HEBAP4_SHIFT.with(Cell::get) {
            (index + 1).min(31)
        } else {
            index
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{VQ_DIMENSION, codebook, vq_vector};
    use crate::tables::eac3::aht::vq_index_bits;

    /// Контрольные суммы `checksum()` из `tools/extract_eac3_tables.py`
    /// (одинаковы для текста ATSC и ETSI): сумма знаковых значений и
    /// Σ value·(позиция+1) mod 2^32 по беззнаковым словам в порядке строк.
    const CHECKSUMS: [(u8, i64, u64); 7] = [
        (1, 3393, 11_747_069),
        (2, -3230, 38_888_692),
        (3, 7998, 174_505_124),
        (4, -26584, 616_291_495),
        (5, -147_342, 1_056_581_474),
        (6, 383_917, 320_407_692),
        (7, 380_749, 1_564_532_889),
    ];

    #[test]
    fn codebooks_match_spec_text_checksums_and_sizes() {
        for (hebap, plain_sum, weighted_sum) in CHECKSUMS {
            let rows = codebook(hebap).unwrap();
            // Таблица E3.2: индекс hebap 1–7 занимает 2, 3, 4, 5, 7, 8, 9 бит.
            assert_eq!(
                rows.len(),
                1 << vq_index_bits(hebap).unwrap(),
                "hebap {hebap}"
            );
            let mut plain = 0i64;
            let mut weighted = 0u64;
            let mut position = 0u64;
            for row in rows {
                for &value in row {
                    position += 1;
                    plain += i64::from(value as i16);
                    weighted = (weighted + u64::from(value) * position) % (1 << 32);
                }
            }
            assert_eq!(
                (plain, weighted),
                (plain_sum, weighted_sum),
                "hebap {hebap}"
            );
        }
    }

    #[test]
    fn lookup_dequantizes_signed_fractions() {
        // Таблица E4.1, индекс 1: 0xe9ba 0xf38d 0xc76d 0xfa90 0xf815 0x0351.
        let vector = vq_vector(1, 1).unwrap();
        assert_eq!(vector[0], f32::from(0xe9ba_u16 as i16) / 32_768.0);
        assert_eq!(vector[5], f32::from(0x0351_i16) / 32_768.0);
        assert!(vector.iter().all(|value| value.abs() < 1.0));
        // Последний вектор E4.7 (индекс 511) и границы.
        assert_eq!(
            vq_vector(7, 511).unwrap()[5],
            f32::from(0x01f1_i16) / 32_768.0
        );
        assert_eq!(vq_vector(7, 512), None);
        assert_eq!(vq_vector(0, 0), None);
        assert_eq!(vq_vector(8, 0), None);
        assert_eq!(VQ_DIMENSION, 6);
    }
}
