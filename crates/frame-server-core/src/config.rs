use std::time::Duration;

use crate::error::FrameServerConfigError;

pub const DEFAULT_MAX_FEED_AND_DRAIN_DRIVER_STEPS: u32 = 256;
pub const DEFAULT_STALE_OUTCOME_CANCEL_THRESHOLD: u32 = 3;
pub const DEFAULT_RESUME_PENDING_EVENT_INTERVAL: Duration = Duration::from_millis(16);

/// Нейтральный config protocol-а. Он не хранит backend/audio timing policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameServerConfig {
    pub max_feed_and_drain_driver_steps: u32,
    pub stale_outcome_cancel_threshold: u32,
    pub resume_pending_event_interval: Duration,
}

impl Default for FrameServerConfig {
    fn default() -> Self {
        Self {
            max_feed_and_drain_driver_steps: DEFAULT_MAX_FEED_AND_DRAIN_DRIVER_STEPS,
            stale_outcome_cancel_threshold: DEFAULT_STALE_OUTCOME_CANCEL_THRESHOLD,
            resume_pending_event_interval: DEFAULT_RESUME_PENDING_EVENT_INTERVAL,
        }
    }
}

impl FrameServerConfig {
    pub fn validate(self) -> Result<ValidatedFrameServerConfig, FrameServerConfigError> {
        if self.max_feed_and_drain_driver_steps == 0 {
            return Err(FrameServerConfigError::ZeroMaxFeedAndDrainDriverSteps);
        }

        if self.stale_outcome_cancel_threshold == 0 {
            return Err(FrameServerConfigError::ZeroStaleOutcomeCancelThreshold);
        }

        if self.resume_pending_event_interval.is_zero() {
            return Err(FrameServerConfigError::ZeroResumePendingEventInterval);
        }

        Ok(ValidatedFrameServerConfig { raw: self })
    }
}

/// Config после явной валидации. Future state machine должна принимать именно этот тип.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValidatedFrameServerConfig {
    raw: FrameServerConfig,
}

impl ValidatedFrameServerConfig {
    #[must_use]
    pub const fn raw(self) -> FrameServerConfig {
        self.raw
    }

    #[must_use]
    pub const fn max_feed_and_drain_driver_steps(self) -> u32 {
        self.raw.max_feed_and_drain_driver_steps
    }

    #[must_use]
    pub const fn stale_outcome_cancel_threshold(self) -> u32 {
        self.raw.stale_outcome_cancel_threshold
    }

    #[must_use]
    pub const fn resume_pending_event_interval(self) -> Duration {
        self.raw.resume_pending_event_interval
    }
}
