use std::fmt;

/// Ошибки валидации neutral config-а frame-server-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameServerConfigError {
    ZeroMaxFeedAndDrainDriverSteps,
    ZeroStaleOutcomeCancelThreshold,
    ZeroResumePendingEventInterval,
    ZeroLiveScrubMaxHz,
    LiveScrubMaxHzTooHigh { max_allowed: u16, actual: u16 },
    ZeroTimelineHoverPrepareSlots,
    TimelineHoverPrepareSlotsTooHigh { max_allowed: u8, actual: u8 },
    RecentSupersededPrepareSlotsTooHigh { max_allowed: u8, actual: u8 },
    SoftwareRecentSupersededPrepareSlotsTooHigh { max_allowed: u8, actual: u8 },
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
            Self::ZeroTimelineHoverPrepareSlots => {
                formatter.write_str("timeline_hover_prepare_slots must be greater than zero")
            }
            Self::TimelineHoverPrepareSlotsTooHigh {
                max_allowed,
                actual,
            } => write!(
                formatter,
                "timeline_hover_prepare_slots must be <= {max_allowed}, got {actual}"
            ),
            Self::RecentSupersededPrepareSlotsTooHigh {
                max_allowed,
                actual,
            } => write!(
                formatter,
                "recent_superseded_prepare_slots must be <= {max_allowed}, got {actual}"
            ),
            Self::SoftwareRecentSupersededPrepareSlotsTooHigh {
                max_allowed,
                actual,
            } => write!(
                formatter,
                "software_recent_superseded_prepare_slots must be <= {max_allowed}, got {actual}"
            ),
        }
    }
}

impl std::error::Error for FrameServerConfigError {}
