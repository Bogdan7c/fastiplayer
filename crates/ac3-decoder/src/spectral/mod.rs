//! Алгоритмы частотной области, общие для AC-3 и E-AC-3 (A/52 §7.1–§7.4).
//!
//! Здесь нет разбора синтаксиса: модули получают уже прочитанные поля и
//! превращают их в экспоненты, указатели битового распределения, значения
//! мантисс и восстановленные из coupling коэффициенты. E-AC-3 (Annex E)
//! переиспользует эти шаги поверх своего синтаксиса.

pub(crate) mod aht;
pub(crate) mod bit_allocation;
pub(crate) mod coupling;
pub(crate) mod dither;
pub(crate) mod ecpl;
pub(crate) mod exponents;
pub(crate) mod mantissa;
pub(crate) mod spx;

/// Число частотных коэффициентов (бинов) в аудиоблоке на канал (§7.9: 256
/// новых сэмплов на блок).
pub(crate) const COEFFICIENTS_PER_BLOCK: usize = 256;

/// Наибольшее число полнополосных каналов AC-3 (`acmod = 7`, Таблица 5.8).
pub(crate) const MAX_FULL_BANDWIDTH_CHANNELS: usize = 5;

/// Сколько точных степеней двойки держим в таблице: экспоненты 0–24 и сдвиги
/// координат coupling до 15 + 3·3 = 24 помещаются с запасом.
const EXPONENT_SCALE_ENTRIES: usize = 32;

/// `2^−n` для n = 0…31, вычисленные делением пополам (точно в f32).
const EXPONENT_SCALES: [f32; EXPONENT_SCALE_ENTRIES] = build_exponent_scales();

const fn build_exponent_scales() -> [f32; EXPONENT_SCALE_ENTRIES] {
    let mut scales = [1.0f32; EXPONENT_SCALE_ENTRIES];
    let mut index = 1;
    while index < EXPONENT_SCALE_ENTRIES {
        scales[index] = scales[index - 1] * 0.5;
        index += 1;
    }
    scales
}

/// Множитель экспоненты: «мантисса сдвигается вправо на exponent» (§7.3.2).
pub(crate) fn exponent_scale(exponent: u8) -> f32 {
    EXPONENT_SCALES
        .get(usize::from(exponent))
        .copied()
        .unwrap_or_else(|| 0.5f32.powi(i32::from(exponent)))
}

#[cfg(test)]
mod tests {
    use super::exponent_scale;

    #[test]
    fn exponent_scale_is_exact_power_of_two() {
        assert_eq!(exponent_scale(0), 1.0);
        assert_eq!(exponent_scale(1), 0.5);
        assert_eq!(exponent_scale(24), 1.0 / 16_777_216.0);
        assert_eq!(exponent_scale(40), 0.5f32.powi(40));
    }
}
