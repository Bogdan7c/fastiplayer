//! Dither для мантисс с нулём бит (A/52 §7.3.4).
//!
//! Спецификация разрешает «любую разумно случайную последовательность» с
//! равномерным распределением в ±0,707. Мы используем детерминированный
//! генератор, чтобы декодирование было воспроизводимым (одинаковый вход и
//! одинаковое зерно → одинаковые сэмплы), и даём выключить dither целиком —
//! это нужно тестам, сравнивающим коэффициенты/PCM с эталоном без шума.

/// Масштаб равномерного dither-а: §7.3.4 называет 0,707 оптимальным.
const DITHER_SCALE: f32 = 0.707;
/// Масштаб равномерного шума коэффициентов AHT с hebap 0 (Annex E §3.4):
/// шум кладётся в область DCT до обратного преобразования §3.4.5, которое
/// усиливает энергию в 6 раз (`MᵀM = 6·I`). Annex E величину шума для AHT
/// не задаёт; ±0,5 выбрано по двум независимым эталонам (ffmpeg n9.0.2 и
/// декодер Nero на потоках Dolby): энергия их шума в таких бинах на
/// 3,0 ± 0,1 дБ ниже, чем дал бы ±0,707 (20·lg(0,5/0,707) = −3,01 дБ),
/// см. Serena-память `ac3-decoder/core`, раздел D4.
const AHT_DITHER_SCALE: f32 = 0.5;

/// Множитель и приращение 32-битного линейного конгруэнтного генератора
/// (общеизвестные константы «Numerical Recipes»; качество для dither-а
/// достаточное, спецификация конкретный генератор не задаёт).
const LCG_MULTIPLIER: u32 = 1_664_525;
const LCG_INCREMENT: u32 = 1_013_904_223;

/// Сколько старших бит состояния превращаются в значение (24 бита — точность f32).
const SAMPLE_BITS: u32 = 24;

/// Зерно генератора dither-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DitherSeed(pub u32);

impl DitherSeed {
    /// Зерно по умолчанию (произвольная фиксированная константа).
    pub const DEFAULT: Self = Self(0x5EED_AC03);
}

impl Default for DitherSeed {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Что подставлять в мантиссы, под которые не выделено ни одного бита.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DitherMode {
    /// Поведение по спецификации: при `dithflag = 1` — псевдослучайный шум
    /// из детерминированного генератора с этим зерном, иначе ноль.
    Pseudorandom(DitherSeed),
    /// Всегда ноль, даже при `dithflag = 1` (тестовый режим «без шума»).
    Disabled,
}

impl Default for DitherMode {
    fn default() -> Self {
        Self::Pseudorandom(DitherSeed::DEFAULT)
    }
}

/// Флаг `dithflag[ch]` текущего блока (§5.4.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelDither {
    /// `dithflag = 1`: кодер просит шум в нулевых мантиссах.
    Requested,
    /// `dithflag = 0`: нулевые мантиссы — настоящие нули.
    Off,
}

impl ChannelDither {
    pub(crate) const fn from_flag(dither_flag: bool) -> Self {
        if dither_flag {
            Self::Requested
        } else {
            Self::Off
        }
    }
}

/// Генератор значений для нулевых мантисс; живёт в декодере между кадрами.
#[derive(Debug, Clone)]
pub(crate) struct DitherGenerator {
    mode: DitherMode,
    state: u32,
}

impl DitherGenerator {
    pub(crate) fn new(mode: DitherMode) -> Self {
        let state = match mode {
            DitherMode::Pseudorandom(seed) => seed.0,
            DitherMode::Disabled => 0,
        };
        Self { mode, state }
    }

    /// Возвращает генератор к зерну (часть `Ac3Decoder::reset`).
    pub(crate) fn reset(&mut self) {
        *self = Self::new(self.mode);
    }

    /// Значение нулевой мантиссы до сдвига на экспоненту (§7.3.4).
    pub(crate) fn zero_bit_mantissa(&mut self, channel: ChannelDither) -> f32 {
        match (self.mode, channel) {
            (DitherMode::Pseudorandom(_), ChannelDither::Requested) => {
                self.next_uniform() * DITHER_SCALE
            }
            _ => 0.0,
        }
    }

    /// Значение коэффициента AHT (область DCT) без бит — hebap 0 (см.
    /// [`AHT_DITHER_SCALE`]); то же правило `dithflag`, что у мантисс AC-3.
    pub(crate) fn zero_bit_aht_coefficient(&mut self, channel: ChannelDither) -> f32 {
        match (self.mode, channel) {
            (DitherMode::Pseudorandom(_), ChannelDither::Requested) => {
                self.next_uniform() * AHT_DITHER_SCALE
            }
            _ => 0.0,
        }
    }

    /// Равномерное значение в [−1, 1).
    fn next_uniform(&mut self) -> f32 {
        self.state = self
            .state
            .wrapping_mul(LCG_MULTIPLIER)
            .wrapping_add(LCG_INCREMENT);
        let sample = (self.state >> (32 - SAMPLE_BITS)) as i32 - (1 << (SAMPLE_BITS - 1));
        sample as f32 / (1 << (SAMPLE_BITS - 1)) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::{ChannelDither, DitherGenerator, DitherMode, DitherSeed};

    #[test]
    fn requested_dither_is_uniform_within_spec_scale() {
        let mut generator = DitherGenerator::new(DitherMode::default());
        let values: Vec<f32> = (0..20_000)
            .map(|_| generator.zero_bit_mantissa(ChannelDither::Requested))
            .collect();
        assert!(values.iter().all(|value| value.abs() <= 0.707));
        let mean = values.iter().sum::<f32>() / values.len() as f32;
        let variance = values.iter().map(|value| value * value).sum::<f32>() / values.len() as f32;
        // Равномерное распределение в ±a: среднее 0, дисперсия a²/3.
        assert!(mean.abs() < 0.02, "mean {mean}");
        assert!(
            (variance - 0.707f32 * 0.707 / 3.0).abs() < 0.01,
            "variance {variance}"
        );
    }

    #[test]
    fn off_flag_and_disabled_mode_give_true_zero() {
        let mut generator = DitherGenerator::new(DitherMode::default());
        assert_eq!(generator.zero_bit_mantissa(ChannelDither::Off), 0.0);
        let mut disabled = DitherGenerator::new(DitherMode::Disabled);
        assert_eq!(disabled.zero_bit_mantissa(ChannelDither::Requested), 0.0);
    }

    #[test]
    fn same_seed_reproduces_sequence_and_reset_restarts_it() {
        let mode = DitherMode::Pseudorandom(DitherSeed(7));
        let mut first = DitherGenerator::new(mode);
        let mut second = DitherGenerator::new(mode);
        let sequence: Vec<f32> = (0..16)
            .map(|_| first.zero_bit_mantissa(ChannelDither::Requested))
            .collect();
        let repeated: Vec<f32> = (0..16)
            .map(|_| second.zero_bit_mantissa(ChannelDither::Requested))
            .collect();
        assert_eq!(sequence, repeated);
        first.reset();
        assert_eq!(
            first.zero_bit_mantissa(ChannelDither::Requested),
            sequence[0]
        );
        let mut other_seed = DitherGenerator::new(DitherMode::Pseudorandom(DitherSeed(8)));
        assert_ne!(
            other_seed.zero_bit_mantissa(ChannelDither::Requested),
            sequence[0]
        );
    }
}
