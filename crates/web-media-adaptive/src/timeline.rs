//! Neutral VOD/live-edge/DVR и component-clock vocabulary.

use std::num::NonZeroU32;
use std::time::Duration;

/// Live edge на presentation timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LiveEdge(Duration);

impl LiveEdge {
    /// Создаёт edge из neutral media position.
    #[must_use]
    pub const fn new(position: Duration) -> Self {
        Self(position)
    }

    /// Возвращает текущую позицию edge.
    #[must_use]
    pub const fn position(self) -> Duration {
        self.0
    }
}

/// Проверенное DVR window `[start, end]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DvrWindow {
    start: Duration,
    end: Duration,
}

impl DvrWindow {
    /// Создаёт непустой ordered window.
    pub fn new(start: Duration, end: Duration) -> Result<Self, DvrWindowError> {
        if start >= end {
            return Err(DvrWindowError::EmptyOrReversed);
        }
        Ok(Self { start, end })
    }

    /// Левая доступная граница.
    #[must_use]
    pub const fn start(self) -> Duration {
        self.start
    }

    /// Правая доступная граница.
    #[must_use]
    pub const fn end(self) -> Duration {
        self.end
    }
}

/// Ошибка advertised DVR bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DvrWindowError {
    /// Window обязан иметь положительную ширину.
    #[error("DVR window пуст или имеет обратный порядок")]
    EmptyOrReversed,
}

/// Manifest-level presentation mode без protocol-specific policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptivePresentation {
    /// Конечный media timeline.
    Vod {
        /// Duration может стать известной только после container parse.
        duration: Option<Duration>,
    },
    /// Динамический live timeline.
    Live {
        /// Текущий advertised live edge.
        edge: LiveEdge,
        /// Отсутствие window означает live без доказанного DVR seek.
        dvr: Option<DvrWindow>,
    },
}

/// Точное преобразование component timestamp units в neutral seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentClockMetadata {
    units_per_second: NonZeroU32,
    presentation_origin_units: i64,
}

impl ComponentClockMetadata {
    /// Создаёт отдельный component clock без смешивания audio/video timescale.
    #[must_use]
    pub const fn new(units_per_second: NonZeroU32, presentation_origin_units: i64) -> Self {
        Self {
            units_per_second,
            presentation_origin_units,
        }
    }

    /// Возвращает exact manifest timescale.
    #[must_use]
    pub const fn units_per_second(self) -> NonZeroU32 {
        self.units_per_second
    }

    /// Возвращает component-local origin на его собственной шкале.
    #[must_use]
    pub const fn presentation_origin_units(self) -> i64 {
        self.presentation_origin_units
    }
}
