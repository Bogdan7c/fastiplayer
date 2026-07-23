//! Caller-owned bounds для adaptive HTTP lifecycle.

use std::num::{NonZeroU8, NonZeroUsize};
use std::time::Duration;

/// Единые RAM/work bounds manifest и segment owner-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveTransportLimits {
    /// Максимальный buffered manifest body.
    pub maximum_manifest_bytes: NonZeroUsize,
    /// Максимальный full segment body.
    pub maximum_segment_bytes: NonZeroUsize,
    /// Максимальное число segment descriptors одного published snapshot-а.
    pub maximum_snapshot_segments: NonZeroUsize,
}

impl AdaptiveTransportLimits {
    /// Собирает explicit policy без скрытых network/config defaults.
    #[must_use]
    pub const fn new(
        maximum_manifest_bytes: NonZeroUsize,
        maximum_segment_bytes: NonZeroUsize,
        maximum_snapshot_segments: NonZeroUsize,
    ) -> Self {
        Self {
            maximum_manifest_bytes,
            maximum_segment_bytes,
            maximum_snapshot_segments,
        }
    }
}

/// Bounded retry policy одного manifest/segment resource-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveRetryPolicy {
    maximum_attempts: NonZeroU8,
    initial_backoff: Duration,
    maximum_backoff: Duration,
}

impl AdaptiveRetryPolicy {
    /// Проверяет монотонную ненулевую delay policy.
    pub fn new(
        maximum_attempts: NonZeroU8,
        initial_backoff: Duration,
        maximum_backoff: Duration,
    ) -> Result<Self, AdaptiveRetryPolicyError> {
        if initial_backoff.is_zero() {
            return Err(AdaptiveRetryPolicyError::ZeroInitialBackoff);
        }
        if initial_backoff > maximum_backoff {
            return Err(AdaptiveRetryPolicyError::InitialExceedsMaximum);
        }
        if maximum_backoff > Duration::from_secs(60) {
            return Err(AdaptiveRetryPolicyError::MaximumExceedsReadinessBound);
        }
        Ok(Self {
            maximum_attempts,
            initial_backoff,
            maximum_backoff,
        })
    }

    /// Возвращает exact attempt budget.
    #[must_use]
    pub const fn maximum_attempts(self) -> NonZeroU8 {
        self.maximum_attempts
    }

    /// Считает capped exponential backoff после указанной неудачной попытки.
    #[must_use]
    pub fn backoff_after(self, failed_attempt: NonZeroU8) -> Duration {
        let shift = u32::from(failed_attempt.get().saturating_sub(1)).min(31);
        self.initial_backoff
            .saturating_mul(1_u32 << shift)
            .min(self.maximum_backoff)
    }
}

/// Ошибка caller-owned retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdaptiveRetryPolicyError {
    /// Нулевая задержка создала бы busy loop.
    #[error("initial adaptive retry backoff должен быть ненулевым")]
    ZeroInitialBackoff,
    /// Exponential cap обязан быть не меньше первой задержки.
    #[error("initial adaptive retry backoff превышает maximum backoff")]
    InitialExceedsMaximum,
    /// Readiness hint не может превышать neutral 60-second bound.
    #[error("maximum adaptive retry backoff превышает 60 seconds")]
    MaximumExceedsReadinessBound,
}
