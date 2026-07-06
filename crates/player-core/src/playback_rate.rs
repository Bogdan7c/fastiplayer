use std::fmt;

/// Скорость воспроизведения как проверенный multiplier, а не сырой `f32`.
///
/// S33 хранит это значение только в snapshot/command boundary. Scheduler, audio clock
/// и tempo backend пока не читают non-1x значение, поэтому это internal groundwork.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackRate(f32);

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
