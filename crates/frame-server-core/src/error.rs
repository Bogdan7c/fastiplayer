use std::fmt;

/// Ошибки валидации neutral config-а frame-server-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameServerConfigError {
    ZeroMaxFeedAndDrainDriverSteps,
    ZeroStaleOutcomeCancelThreshold,
    ZeroResumePendingEventInterval,
    ZeroLiveScrubMaxHz,
    LiveScrubMaxHzTooHigh { max_allowed: u16, actual: u16 },
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
            Self::ZeroLiveScrubMaxHz => {
                formatter.write_str("live_scrub_max_hz must be greater than zero")
            }
            Self::LiveScrubMaxHzTooHigh {
                max_allowed,
                actual,
            } => write!(
                formatter,
                "live_scrub_max_hz must be <= {max_allowed}, got {actual}"
            ),
        }
    }
}

impl std::error::Error for FrameServerConfigError {}
