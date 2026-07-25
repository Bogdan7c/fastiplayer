//! Exact per-stream clocks без lossy rescale.

use std::cmp::Ordering;
use std::fmt;
use std::num::NonZeroU64;

use thiserror::Error;

/// Ошибка построения exact Smooth Streaming clock value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SmoothTimeError {
    #[error("Smooth Streaming timescale обязан быть ненулевым")]
    ZeroTimescale,
}

/// Ненулевой tick rate одного stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SmoothTimescale(NonZeroU64);

impl SmoothTimescale {
    #[must_use = "валидированный timescale нужен для exact clock values"]
    pub fn new(ticks_per_second: u64) -> Result<Self, SmoothTimeError> {
        NonZeroU64::new(ticks_per_second)
            .map(Self)
            .ok_or(SmoothTimeError::ZeroTimescale)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Exact timestamp как rational `ticks / timescale`.
#[derive(Clone, Copy, Eq)]
pub struct SmoothTime {
    ticks: u64,
    timescale: SmoothTimescale,
}

impl SmoothTime {
    #[must_use]
    pub const fn new(ticks: u64, timescale: SmoothTimescale) -> Self {
        Self { ticks, timescale }
    }

    #[must_use]
    pub const fn ticks(self) -> u64 {
        self.ticks
    }

    #[must_use]
    pub const fn timescale(self) -> SmoothTimescale {
        self.timescale
    }

    fn exact_cross_products(self, other: Self) -> (u128, u128) {
        let left = u128::from(self.ticks)
            .checked_mul(u128::from(other.timescale.get()))
            .expect("произведение двух u64 всегда помещается в u128");
        let right = u128::from(other.ticks)
            .checked_mul(u128::from(self.timescale.get()))
            .expect("произведение двух u64 всегда помещается в u128");
        (left, right)
    }
}

impl PartialEq for SmoothTime {
    fn eq(&self, other: &Self) -> bool {
        let (left, right) = self.exact_cross_products(*other);
        left == right
    }
}

impl PartialOrd for SmoothTime {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SmoothTime {
    fn cmp(&self, other: &Self) -> Ordering {
        let (left, right) = self.exact_cross_products(*other);
        left.cmp(&right)
    }
}

impl fmt::Debug for SmoothTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothTime")
            .field("ticks", &self.ticks)
            .field("timescale", &self.timescale.get())
            .finish()
    }
}
