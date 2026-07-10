use std::{fmt, time::Duration};

/// Скорость воспроизведения как проверенный multiplier, а не сырой `f32`.
///
/// Один typed rate связывает audio tempo, media-clock mapping и video scheduler.
/// Значение остаётся runtime-only и не переносится в Settings/history/startup restore.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackRate(f32);

/// Количество бит мантиссы в IEEE-754 `f32`.
const F32_MANTISSA_BITS: u32 = 23;

/// Смещение экспоненты IEEE-754 `f32`.
const F32_EXPONENT_BIAS: i32 = 127;

/// Наносекунд в одной секунде; нужно для saturating сборки `Duration`.
const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// Точная положительная двоичная дробь для уже проверенного `PlaybackRate`.
#[derive(Debug, Clone, Copy)]
struct PlaybackRateRatio {
    /// Числитель дроби после раскрытия `f32` мантиссы.
    numerator: u128,

    /// Степень двойки в знаменателе.
    denominator_shift: u32,
}

impl PlaybackRate {
    /// Минимальная скорость V1, включительно.
    pub const MIN: Self = Self(0.25);

    /// Обычная скорость, которая сохраняет текущее поведение плеера.
    pub const NORMAL: Self = Self(1.0);

    /// Максимальная скорость V1, включительно.
    pub const MAX: Self = Self(4.0);

    /// Проверяет сырой multiplier и возвращает typed rate только для разрешённого диапазона.
    pub fn new(multiplier: f32) -> Result<Self, PlaybackRateValidationError> {
        if !multiplier.is_finite() {
            return Err(PlaybackRateValidationError::NotFinite);
        }

        if multiplier <= 0.0 {
            return Err(PlaybackRateValidationError::NonPositive);
        }

        if multiplier < Self::MIN.as_f32() {
            return Err(PlaybackRateValidationError::BelowMinimum);
        }

        if multiplier > Self::MAX.as_f32() {
            return Err(PlaybackRateValidationError::AboveMaximum);
        }

        Ok(Self(multiplier))
    }

    /// Возвращает multiplier для diagnostics/tests без раскрытия mutable storage.
    #[must_use]
    pub const fn as_f32(self) -> f32 {
        self.0
    }

    /// Проверяет eligibility ускоренного video-overload recovery path-а.
    ///
    /// `1.0x` и замедление сохраняют обычный FIFO без намеренных compressed skips.
    #[must_use]
    pub(crate) fn is_faster_than_normal(self) -> bool {
        self.0 > Self::NORMAL.0
    }

    /// Переводит elapsed wall-time в media delta для no-audio monotonic clock.
    #[must_use]
    pub(crate) fn scale_wall_delta_to_media_delta(self, wall_delta: Duration) -> Duration {
        if self == Self::NORMAL || wall_delta.is_zero() {
            return wall_delta;
        }

        let rate_ratio = self.binary_ratio();
        let scaled_nanos = wall_delta
            .as_nanos()
            .checked_mul(rate_ratio.numerator)
            .map(|nanos| shift_right_or_zero(nanos, rate_ratio.denominator_shift))
            .unwrap_or(u128::MAX);

        duration_from_nanos_saturating(scaled_nanos)
    }

    /// Переводит media delta до frame deadline в wall delay для worker wakeup-а.
    #[must_use]
    pub(crate) fn scale_media_delta_to_wall_delay(self, media_delta: Duration) -> Duration {
        if self == Self::NORMAL || media_delta.is_zero() {
            return media_delta;
        }

        let rate_ratio = self.binary_ratio();
        let shifted_media_nanos =
            shift_left_saturating(media_delta.as_nanos(), rate_ratio.denominator_shift);

        if shifted_media_nanos == u128::MAX {
            return Duration::MAX;
        }

        let rounded_wall_nanos = div_ceil_saturating(shifted_media_nanos, rate_ratio.numerator);

        // Положительный будущий media deadline не должен превращаться в zero-timeout spin.
        duration_from_nanos_saturating(rounded_wall_nanos.max(1))
    }

    /// Раскрывает validated `f32` rate в точную двоичную дробь.
    #[must_use]
    fn binary_ratio(self) -> PlaybackRateRatio {
        let bits = self.0.to_bits();
        let exponent_bits = ((bits >> F32_MANTISSA_BITS) & 0xff) as i32;
        let mantissa_bits = bits & ((1 << F32_MANTISSA_BITS) - 1);
        let significand = if exponent_bits == 0 {
            mantissa_bits
        } else {
            mantissa_bits | (1 << F32_MANTISSA_BITS)
        };
        let exponent = if exponent_bits == 0 {
            1 - F32_EXPONENT_BIAS - F32_MANTISSA_BITS as i32
        } else {
            exponent_bits - F32_EXPONENT_BIAS - F32_MANTISSA_BITS as i32
        };

        if exponent >= 0 {
            let numerator = shift_left_saturating(u128::from(significand), exponent as u32);
            return PlaybackRateRatio {
                numerator,
                denominator_shift: 0,
            };
        }

        PlaybackRateRatio {
            numerator: u128::from(significand),
            denominator_shift: (-exponent) as u32,
        }
    }
}

/// Делит `value` на степень двойки; слишком большой shift означает округление к нулю.
#[must_use]
fn shift_right_or_zero(value: u128, shift: u32) -> u128 {
    value.checked_shr(shift).unwrap_or(0)
}

/// Умножает на `2^shift`, явно saturating при потере старших битов.
#[must_use]
fn shift_left_saturating(value: u128, shift: u32) -> u128 {
    let Some(multiplier) = 1_u128.checked_shl(shift) else {
        return u128::MAX;
    };

    value.saturating_mul(multiplier)
}

/// Делит с округлением вверх и saturating поведением при переполнении округления.
#[must_use]
fn div_ceil_saturating(numerator: u128, denominator: u128) -> u128 {
    if denominator == 0 {
        return u128::MAX;
    }

    let quotient = numerator / denominator;
    let remainder = numerator % denominator;

    if remainder == 0 {
        quotient
    } else {
        quotient.saturating_add(1)
    }
}

/// Собирает `Duration` из наносекунд, saturating на верхней границе std type.
#[must_use]
fn duration_from_nanos_saturating(nanos: u128) -> Duration {
    if nanos >= Duration::MAX.as_nanos() {
        return Duration::MAX;
    }

    let seconds = nanos / NANOS_PER_SECOND;
    let subsecond_nanos = nanos % NANOS_PER_SECOND;

    Duration::new(seconds as u64, subsecond_nanos as u32)
}

impl Default for PlaybackRate {
    /// Default остаётся ровно `1.0x`, чтобы S33 не менял поведение существующего playback.
    fn default() -> Self {
        Self::NORMAL
    }
}

impl fmt::Display for PlaybackRate {
    /// Печатает человекочитаемый multiplier для логов и typed diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}x", self.0)
    }
}

/// Причина, по которой сырой playback-rate multiplier нельзя принять.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackRateValidationError {
    /// `NaN` и бесконечности не являются валидной скоростью.
    NotFinite,

    /// Ноль и отрицательные значения не имеют смысла для V1 playback rate.
    NonPositive,

    /// Значение ниже включительной границы `0.25x`.
    BelowMinimum,

    /// Значение выше включительной границы `4.0x`.
    AboveMaximum,
}

impl fmt::Display for PlaybackRateValidationError {
    /// Даёт короткое объяснение без хранения сырого `f32`, чтобы `NaN` не ломал equality tests.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFinite => write!(formatter, "playback rate must be finite"),
            Self::NonPositive => write!(formatter, "playback rate must be positive"),
            Self::BelowMinimum => write!(
                formatter,
                "playback rate must be at least {}",
                PlaybackRate::MIN
            ),
            Self::AboveMaximum => write!(
                formatter,
                "playback rate must be at most {}",
                PlaybackRate::MAX
            ),
        }
    }
}

impl std::error::Error for PlaybackRateValidationError {}
