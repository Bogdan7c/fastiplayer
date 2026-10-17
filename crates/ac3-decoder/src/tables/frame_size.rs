//! Частоты дискретизации и длины syncframe-а AC-3.
//!
//! Источник: ATSC A/52:2012 Таблица 5.6 (`fscod`), Таблица 5.18 (`frmsizecod`),
//! §7.10.1 / Таблица 7.34 (граница 5/8 кадра для `crc1`). Таблица 5.18
//! переписана из спецификации целиком; тесты сверяют её с формулой битрейта
//! и Таблицу 7.34 — с формулой из §7.10.1.

/// Код частоты дискретизации `fscod` (Таблица 5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleRateCode {
    /// `'00'` — 48 кГц.
    Rate48000,
    /// `'01'` — 44,1 кГц.
    Rate44100,
    /// `'10'` — 32 кГц.
    Rate32000,
}

impl SampleRateCode {
    /// Разбирает 2-битный `fscod`; `'11'` зарезервирован.
    pub(crate) const fn from_code(fscod: u8) -> Option<Self> {
        match fscod {
            0 => Some(Self::Rate48000),
            1 => Some(Self::Rate44100),
            2 => Some(Self::Rate32000),
            _ => None,
        }
    }

    /// Номинальная частота в герцах (без сдвига пониженной частоты bsid 9/10).
    #[must_use]
    pub const fn hertz(self) -> u32 {
        match self {
            Self::Rate48000 => 48_000,
            Self::Rate44100 => 44_100,
            Self::Rate32000 => 32_000,
        }
    }

    /// Индекс строки `hth[fscod][]` в Таблице 7.15 (совпадает с `fscod`).
    pub(crate) const fn table_index(self) -> usize {
        match self {
            Self::Rate48000 => 0,
            Self::Rate44100 => 1,
            Self::Rate32000 => 2,
        }
    }
}

/// Одна строка Таблицы 5.18: номинальный битрейт и длина кадра в 16-битных словах.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameSizeRow {
    /// Номинальный битрейт, кбит/с.
    pub(crate) nominal_kbps: u16,
    /// Слов на кадр при 32 кГц.
    pub(crate) words_32khz: u16,
    /// Слов на кадр при 44,1 кГц.
    pub(crate) words_44khz: u16,
    /// Слов на кадр при 48 кГц.
    pub(crate) words_48khz: u16,
}

impl FrameSizeRow {
    /// Длина кадра в словах для данной частоты.
    pub(crate) const fn words(self, sample_rate: SampleRateCode) -> u16 {
        match sample_rate {
            SampleRateCode::Rate48000 => self.words_48khz,
            SampleRateCode::Rate44100 => self.words_44khz,
            SampleRateCode::Rate32000 => self.words_32khz,
        }
    }
}

/// ATSC A/52:2012 Таблица 5.18 «Frame Size Code Table» (1 слово = 16 бит),
/// индекс — `frmsizecod` 0–37.
#[rustfmt::skip]
pub(crate) const FRAME_SIZE_TABLE: [FrameSizeRow; 38] = [
    FrameSizeRow { nominal_kbps: 32, words_32khz: 96, words_44khz: 69, words_48khz: 64 },
    FrameSizeRow { nominal_kbps: 32, words_32khz: 96, words_44khz: 70, words_48khz: 64 },
    FrameSizeRow { nominal_kbps: 40, words_32khz: 120, words_44khz: 87, words_48khz: 80 },
    FrameSizeRow { nominal_kbps: 40, words_32khz: 120, words_44khz: 88, words_48khz: 80 },
    FrameSizeRow { nominal_kbps: 48, words_32khz: 144, words_44khz: 104, words_48khz: 96 },
    FrameSizeRow { nominal_kbps: 48, words_32khz: 144, words_44khz: 105, words_48khz: 96 },
    FrameSizeRow { nominal_kbps: 56, words_32khz: 168, words_44khz: 121, words_48khz: 112 },
    FrameSizeRow { nominal_kbps: 56, words_32khz: 168, words_44khz: 122, words_48khz: 112 },
    FrameSizeRow { nominal_kbps: 64, words_32khz: 192, words_44khz: 139, words_48khz: 128 },
    FrameSizeRow { nominal_kbps: 64, words_32khz: 192, words_44khz: 140, words_48khz: 128 },
    FrameSizeRow { nominal_kbps: 80, words_32khz: 240, words_44khz: 174, words_48khz: 160 },
    FrameSizeRow { nominal_kbps: 80, words_32khz: 240, words_44khz: 175, words_48khz: 160 },
    FrameSizeRow { nominal_kbps: 96, words_32khz: 288, words_44khz: 208, words_48khz: 192 },
    FrameSizeRow { nominal_kbps: 96, words_32khz: 288, words_44khz: 209, words_48khz: 192 },
    FrameSizeRow { nominal_kbps: 112, words_32khz: 336, words_44khz: 243, words_48khz: 224 },
    FrameSizeRow { nominal_kbps: 112, words_32khz: 336, words_44khz: 244, words_48khz: 224 },
    FrameSizeRow { nominal_kbps: 128, words_32khz: 384, words_44khz: 278, words_48khz: 256 },
    FrameSizeRow { nominal_kbps: 128, words_32khz: 384, words_44khz: 279, words_48khz: 256 },
    FrameSizeRow { nominal_kbps: 160, words_32khz: 480, words_44khz: 348, words_48khz: 320 },
    FrameSizeRow { nominal_kbps: 160, words_32khz: 480, words_44khz: 349, words_48khz: 320 },
    FrameSizeRow { nominal_kbps: 192, words_32khz: 576, words_44khz: 417, words_48khz: 384 },
    FrameSizeRow { nominal_kbps: 192, words_32khz: 576, words_44khz: 418, words_48khz: 384 },
    FrameSizeRow { nominal_kbps: 224, words_32khz: 672, words_44khz: 487, words_48khz: 448 },
    FrameSizeRow { nominal_kbps: 224, words_32khz: 672, words_44khz: 488, words_48khz: 448 },
    FrameSizeRow { nominal_kbps: 256, words_32khz: 768, words_44khz: 557, words_48khz: 512 },
    FrameSizeRow { nominal_kbps: 256, words_32khz: 768, words_44khz: 558, words_48khz: 512 },
    FrameSizeRow { nominal_kbps: 320, words_32khz: 960, words_44khz: 696, words_48khz: 640 },
    FrameSizeRow { nominal_kbps: 320, words_32khz: 960, words_44khz: 697, words_48khz: 640 },
    FrameSizeRow { nominal_kbps: 384, words_32khz: 1152, words_44khz: 835, words_48khz: 768 },
    FrameSizeRow { nominal_kbps: 384, words_32khz: 1152, words_44khz: 836, words_48khz: 768 },
    FrameSizeRow { nominal_kbps: 448, words_32khz: 1344, words_44khz: 975, words_48khz: 896 },
    FrameSizeRow { nominal_kbps: 448, words_32khz: 1344, words_44khz: 976, words_48khz: 896 },
    FrameSizeRow { nominal_kbps: 512, words_32khz: 1536, words_44khz: 1114, words_48khz: 1024 },
    FrameSizeRow { nominal_kbps: 512, words_32khz: 1536, words_44khz: 1115, words_48khz: 1024 },
    FrameSizeRow { nominal_kbps: 576, words_32khz: 1728, words_44khz: 1253, words_48khz: 1152 },
    FrameSizeRow { nominal_kbps: 576, words_32khz: 1728, words_44khz: 1254, words_48khz: 1152 },
    FrameSizeRow { nominal_kbps: 640, words_32khz: 1920, words_44khz: 1393, words_48khz: 1280 },
    FrameSizeRow { nominal_kbps: 640, words_32khz: 1920, words_44khz: 1394, words_48khz: 1280 },
];

/// Строка Таблицы 5.18 по `frmsizecod`; коды 38–63 зарезервированы.
pub(crate) fn frame_size_row(frame_size_code: u8) -> Option<FrameSizeRow> {
    FRAME_SIZE_TABLE.get(usize::from(frame_size_code)).copied()
}

/// Длина первых 5/8 кадра в байтах (A/52 §7.10.1):
/// `5/8_framesize = (framesize >> 1) + (framesize >> 3)` слов.
pub(crate) const fn five_eighths_frame_bytes(frame_words: usize) -> usize {
    ((frame_words >> 1) + (frame_words >> 3)) * 2
}

#[cfg(test)]
mod tests {
    use super::{FRAME_SIZE_TABLE, SampleRateCode, five_eighths_frame_bytes};

    /// ATSC A/52:2012 Таблица 7.34 (5/8 кадра в словах: 32 / 44,1 / 48 кГц),
    /// переписанная из спецификации только для проверки формулы.
    #[rustfmt::skip]
    const TABLE_7_34: [[usize; 3]; 38] = [
        [60, 42, 40], [60, 43, 40], [75, 53, 50], [75, 55, 50], [90, 65, 60],
        [90, 65, 60], [105, 75, 70], [105, 76, 70], [120, 86, 80], [120, 87, 80],
        [150, 108, 100], [150, 108, 100], [180, 130, 120], [180, 130, 120],
        [210, 151, 140], [210, 152, 140], [240, 173, 160], [240, 173, 160],
        [300, 217, 200], [300, 217, 200], [360, 260, 240], [360, 261, 240],
        [420, 303, 280], [420, 305, 280], [480, 347, 320], [480, 348, 320],
        [600, 435, 400], [600, 435, 400], [720, 521, 480], [720, 522, 480],
        [840, 608, 560], [840, 610, 560], [960, 696, 640], [960, 696, 640],
        [1080, 782, 720], [1080, 783, 720], [1200, 870, 800], [1200, 871, 800],
    ];

    #[test]
    fn frame_lengths_follow_bitrate_formula_of_table_5_18() {
        // Слов в кадре = битрейт × 1536 сэмплов / частота / 16 бит; при 44,1 кГц
        // дробь округляется вниз для чётного кода и вверх для нечётного.
        for (code, row) in FRAME_SIZE_TABLE.iter().enumerate() {
            assert_eq!(
                usize::from(row.nominal_kbps),
                [
                    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512,
                    576, 640
                ][code / 2]
            );
            for rate in [
                SampleRateCode::Rate48000,
                SampleRateCode::Rate44100,
                SampleRateCode::Rate32000,
            ] {
                let numerator = u64::from(row.nominal_kbps) * 1000 * 1536;
                let denominator = u64::from(rate.hertz()) * 16;
                let floor_words = numerator / denominator;
                let has_remainder = numerator % denominator != 0;
                let expected = if has_remainder && code % 2 == 1 {
                    floor_words + 1
                } else {
                    floor_words
                };
                assert_eq!(
                    u64::from(row.words(rate)),
                    expected,
                    "frmsizecod {code} at {}",
                    rate.hertz()
                );
            }
        }
    }

    #[test]
    fn table_5_18_checksum_matches_spec_text() {
        // Сумма всех 114 длин, посчитанная по тексту Таблицы 5.18 спецификации.
        let total: u32 = FRAME_SIZE_TABLE
            .iter()
            .map(|row| {
                u32::from(row.words_32khz) + u32::from(row.words_44khz) + u32::from(row.words_48khz)
            })
            .sum();
        assert_eq!(total, 62_695);
    }

    #[test]
    fn five_eighths_formula_reproduces_table_7_34() {
        for (code, row) in FRAME_SIZE_TABLE.iter().enumerate() {
            let rates = [
                SampleRateCode::Rate32000,
                SampleRateCode::Rate44100,
                SampleRateCode::Rate48000,
            ];
            for (column, rate) in rates.into_iter().enumerate() {
                let words = usize::from(row.words(rate));
                assert_eq!(
                    five_eighths_frame_bytes(words),
                    TABLE_7_34[code][column] * 2,
                    "code {code}"
                );
            }
        }
    }
}
