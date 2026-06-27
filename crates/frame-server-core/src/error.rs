use std::fmt;

/// Ошибки валидации neutral config-а frame-server-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameServerConfigError {
    ZeroMaxFeedAndDrainDriverSteps,
    ZeroStaleOutcomeCancelThreshold,
    ZeroResumePendingEventInterval,
}

impl fmt::Display for FrameServerConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroMaxFeedAndDrainDriverSteps => {
                formatter.write_str("max_feed_and_drain_driver_steps must be greater than zero")
            }
            Self::ZeroStaleOutcomeCancelThreshold => {
                formatter.write_str("stale_outcome_cancel_threshold must be greater than zero")
            }
            Self::ZeroResumePendingEventInterval => {
                formatter.write_str("resume_pending_event_interval must be greater than zero")
            }
        }
    }
}

impl std::error::Error for FrameServerConfigError {}
