//! Управление динамическим диапазоном (A/52:2012 §7.7, §6.1.9).
//!
//! `dynrng` (по блокам, §7.7.1) и `compr` (по кадрам, «heavy compression»,
//! §7.7.2) — множители коэффициентов, которые кодер передаёт в потоке.
//! §6.1.9: декодер «по умолчанию обязан» применять `dynrng` к коэффициентам
//! (экспонента и мантисса), и слушатель может ослабить или выключить это.
//! Политику задаёт [`DynamicRangeControl`]; по умолчанию — полный `dynrng`
//! (то же, что ffmpeg с `drc_scale = 1`, и то, что Dolby называет «line
//! mode»). `dialnorm` декодер не применяет (§7.6: это уровень диалога для
//! приложения), downmix тоже не делает.

/// Доля сжатия в процентах (0–100) для частичного применения `dynrng`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CompressionScale(u8);

impl CompressionScale {
    /// Сжатие не применяется (множитель 1).
    pub const NONE: Self = Self(0);
    /// Сжатие применяется полностью.
    pub const FULL: Self = Self(100);

    /// Доля в процентах; `None`, если больше 100.
    #[must_use]
    pub const fn from_percent(percent: u8) -> Option<Self> {
        if percent <= 100 {
            Some(Self(percent))
        } else {
            None
        }
    }

    /// Доля в процентах.
    #[must_use]
    pub const fn percent(self) -> u8 {
        self.0
    }

    fn fraction(self) -> f32 {
        f32::from(self.0) / 100.0
    }
}

/// Как применять управляющие слова динамического диапазона.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DynamicRangeControl {
    /// `dynrng`/`dynrng2` применяются полностью — поведение по умолчанию,
    /// которого требует §7.7.1.1 («shall, by default»).
    #[default]
    Standard,
    /// Слова игнорируются: полный исходный динамический диапазон (§7.7.1.1
    /// разрешает это под управлением слушателя).
    Disabled,
    /// Применяется часть усиления в децибелах: `cut` — для ослабляющих слов
    /// (громкие места), `boost` — для усиливающих (тихие места). Это
    /// «частичное сжатие» §7.7.1.1; спецификация разрешает разные доли для
    /// положительных и отрицательных значений. Масштаб считается в лог-области
    /// (`gain^доля`), а не перекодированием 8-битного слова из примера §7.7.1.2
    /// (там это «may be operated on»), — так значение не огрубляется.
    Scaled {
        /// Доля для слов с усилением < 1.
        cut: CompressionScale,
        /// Доля для слов с усилением > 1.
        boost: CompressionScale,
    },
    /// «Heavy compression» (§7.7.2): `compr`/`compr2` кадра, если оно передано,
    /// иначе `dynrng`/`dynrng2` блока (§7.7.2.1: «if compr is not present …
    /// the dynrng control signal shall be used»).
    Heavy,
}

/// Управляющие слова одного блока для одной программы (Ch1 или Ch2 в 1+1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GainWords {
    /// Действующее `dynrng` блока (0 = 0 дБ).
    pub(crate) dynamic_range: u8,
    /// `compr` кадра, если передано.
    pub(crate) heavy_compression: Option<u8>,
}

impl DynamicRangeControl {
    /// Линейный множитель коэффициентов по словам блока.
    pub(crate) fn gain(self, words: GainWords) -> f32 {
        match self {
            Self::Standard => dynamic_range_gain(words.dynamic_range),
            Self::Disabled => 1.0,
            Self::Scaled { cut, boost } => {
                let gain = dynamic_range_gain(words.dynamic_range);
                let scale = if gain > 1.0 { boost } else { cut };
                match scale.percent() {
                    0 => 1.0,
                    100 => gain,
                    _ => gain.powf(scale.fraction()),
                }
            }
            Self::Heavy => words.heavy_compression.map_or_else(
                || dynamic_range_gain(words.dynamic_range),
                heavy_compression_gain,
            ),
        }
    }
}

/// Бит `X` слова `X.Y` как знаковое число и множитель `2^(X+1)·0.1Y₂`.
///
/// `exponent_bits` старших бит — знаковое X (Таблицы 7.29/7.30: усиление
/// `(X+1)·6,02 дБ`, то есть сдвиг на `X+1`), остальные `fraction_bits` — Y,
/// дробь `0.1Y` с ведущей единицей (от 1/2 до 1 − 2^−(fraction_bits+1)).
fn shift_and_fraction_gain(word: u8, fraction_bits: u32) -> f32 {
    let exponent_bits = 8 - fraction_bits;
    // Арифметический сдвиг i8 вправо даёт знаковое X из старших бит.
    let exponent = i32::from((word as i8) >> fraction_bits);
    let fraction_mask = (1u8 << fraction_bits) - 1;
    let leading_one = 1u32 << fraction_bits;
    let mantissa =
        (leading_one + u32::from(word & fraction_mask)) as f32 / (2 * leading_one) as f32;
    debug_assert!(exponent_bits == 3 || exponent_bits == 4);
    mantissa * 2.0f32.powi(exponent + 1)
}

/// `dynrng` → множитель (§7.7.1.2, Таблица 7.29: X — 3 бита, Y — 5 бит).
pub(crate) fn dynamic_range_gain(word: u8) -> f32 {
    shift_and_fraction_gain(word, 5)
}

/// `compr` → множитель (§7.7.2.2, Таблица 7.30: X — 4 бита, Y — 4 бита).
pub(crate) fn heavy_compression_gain(word: u8) -> f32 {
    shift_and_fraction_gain(word, 4)
}

#[cfg(test)]
mod tests {
    use super::{
        CompressionScale, DynamicRangeControl, GainWords, dynamic_range_gain,
        heavy_compression_gain,
    };

    fn decibels(gain: f32) -> f32 {
        20.0 * gain.log10()
    }

    #[test]
    fn dynrng_follows_table_7_29() {
        // «0000 0000» = 0 дБ (§7.7.1.2).
        assert_eq!(dynamic_range_gain(0x00), 1.0);
        // Старшие 3 бита: X = 3 … −4 → сдвиги 4 влево … 3 вправо при Y = 0 (×1/2).
        let expected_shifts = [
            (0b011, 4),
            (0b010, 3),
            (0b001, 2),
            (0b000, 1),
            (0b111, 0),
            (0b110, -1),
            (0b101, -2),
            (0b100, -3),
        ];
        for (x_bits, shift) in expected_shifts {
            let word = x_bits << 5;
            assert_eq!(
                dynamic_range_gain(word),
                2.0f32.powi(shift) * 0.5,
                "X = {x_bits:03b}"
            );
        }
        // Диапазон: от +23,95 дБ (0x7F) до −24,08 дБ (0x80).
        assert!((decibels(dynamic_range_gain(0x7F)) - 23.95).abs() < 0.01);
        assert!((decibels(dynamic_range_gain(0x80)) + 24.08).abs() < 0.01);
        // Y = 0.111111₂ = 63/64 при X = −1.
        assert_eq!(dynamic_range_gain(0b111_11111), 63.0 / 64.0);
    }

    #[test]
    fn compr_follows_table_7_30() {
        // X = 7 → 8 сдвигов влево, Y = 0 → ×1/2: +42,14 дБ; максимум +47,89 дБ.
        assert_eq!(heavy_compression_gain(0x70), 128.0);
        assert!((decibels(heavy_compression_gain(0x7F)) - 47.89).abs() < 0.01);
        // X = −8, Y = 0 → −48,16 дБ (§7.7.2.2).
        assert!((decibels(heavy_compression_gain(0x80)) + 48.16).abs() < 0.01);
        // X = −1, Y = 1111₂ → 31/32 (−0,28 дБ).
        assert_eq!(heavy_compression_gain(0xFF), 31.0 / 32.0);
    }

    #[test]
    fn policies_select_the_documented_gain() {
        let cut_word = 0b110_00000; // −12,04 дБ: X = −2, Y = 0 → 2^−1 · 1/2.
        let boost_word = 0b001_00000; // +6,02 дБ: X = 1 → 2^2 · 1/2.
        let words = |dynamic_range, heavy_compression| GainWords {
            dynamic_range,
            heavy_compression,
        };
        assert_eq!(
            DynamicRangeControl::default(),
            DynamicRangeControl::Standard
        );
        assert_eq!(
            DynamicRangeControl::Standard.gain(words(cut_word, Some(0x10))),
            0.25
        );
        assert_eq!(
            DynamicRangeControl::Disabled.gain(words(cut_word, Some(0x10))),
            1.0
        );
        // Heavy: compr 0x10 (X = 1 → ×4 · 1/2 = 2) имеет приоритет, без compr — dynrng.
        assert_eq!(
            DynamicRangeControl::Heavy.gain(words(cut_word, Some(0x10))),
            2.0
        );
        assert_eq!(DynamicRangeControl::Heavy.gain(words(cut_word, None)), 0.25);
        // Половина ослабления и ноль усиления.
        let half_cut = DynamicRangeControl::Scaled {
            cut: CompressionScale::from_percent(50).unwrap(),
            boost: CompressionScale::NONE,
        };
        assert!((half_cut.gain(words(cut_word, None)) - 0.5).abs() < 1e-6);
        assert_eq!(half_cut.gain(words(boost_word, None)), 1.0);
        let full = DynamicRangeControl::Scaled {
            cut: CompressionScale::FULL,
            boost: CompressionScale::FULL,
        };
        assert_eq!(full.gain(words(boost_word, None)), 2.0);
        assert_eq!(CompressionScale::from_percent(101), None);
    }
}
