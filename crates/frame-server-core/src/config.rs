use std::time::Duration;

use crate::error::FrameServerConfigError;

pub const DEFAULT_MAX_FEED_AND_DRAIN_DRIVER_STEPS: u32 = 256;
pub const DEFAULT_STALE_OUTCOME_CANCEL_THRESHOLD: u32 = 3;
pub const DEFAULT_RESUME_PENDING_EVENT_INTERVAL: Duration = Duration::from_millis(16);
pub const DEFAULT_LIVE_SCRUB_MAX_HZ: u16 = 60;
pub const MAX_LIVE_SCRUB_MAX_HZ: u16 = 240;
pub const DEFAULT_TIMELINE_HOVER_PREPARE_SLOTS: u8 = 1;
pub const MAX_TIMELINE_HOVER_PREPARE_SLOTS: u8 = 3;
pub const DEFAULT_RECENT_SUPERSEDED_PREPARE_SLOTS: u8 = 1;
pub const MAX_RECENT_SUPERSEDED_PREPARE_SLOTS: u8 = 3;
pub const DEFAULT_SOFTWARE_RECENT_SUPERSEDED_PREPARE_SLOTS: u8 = 1;
pub const MAX_SOFTWARE_RECENT_SUPERSEDED_PREPARE_SLOTS: u8 = 2;

/// Policy запуска decode-work для live scrub. Оба режима остаются latest-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiveScrubDecodeMode {
    /// UI target обновляется на каждый drag, а decode-work стартует не чаще лимита.
    ThrottledLatest,
    /// Каждый drag target допускается к запуску без hz throttle, но старый target отменяется.
    EveryDragEvent,
}

/// Нейтральный config protocol-а. Он не хранит backend/audio timing policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameServerConfig {
    pub max_feed_and_drain_driver_steps: u32,
    pub stale_outcome_cancel_threshold: u32,
    pub resume_pending_event_interval: Duration,
    pub live_scrub_max_hz: u16,
    pub live_scrub_decode_mode: LiveScrubDecodeMode,
    pub timeline_hover_prepare_slots: u8,
    pub recent_superseded_prepare_slots: u8,
    pub software_recent_superseded_prepare_slots: u8,
}

impl Default for FrameServerConfig {
    fn default() -> Self {
        Self {
            max_feed_and_drain_driver_steps: DEFAULT_MAX_FEED_AND_DRAIN_DRIVER_STEPS,
            stale_outcome_cancel_threshold: DEFAULT_STALE_OUTCOME_CANCEL_THRESHOLD,
            resume_pending_event_interval: DEFAULT_RESUME_PENDING_EVENT_INTERVAL,
            live_scrub_max_hz: DEFAULT_LIVE_SCRUB_MAX_HZ,
            live_scrub_decode_mode: LiveScrubDecodeMode::ThrottledLatest,
            timeline_hover_prepare_slots: DEFAULT_TIMELINE_HOVER_PREPARE_SLOTS,
            recent_superseded_prepare_slots: DEFAULT_RECENT_SUPERSEDED_PREPARE_SLOTS,
            software_recent_superseded_prepare_slots:
                DEFAULT_SOFTWARE_RECENT_SUPERSEDED_PREPARE_SLOTS,
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

        if self.live_scrub_max_hz == 0 {
            return Err(FrameServerConfigError::ZeroLiveScrubMaxHz);
        }

        if self.live_scrub_max_hz > MAX_LIVE_SCRUB_MAX_HZ {
            return Err(FrameServerConfigError::LiveScrubMaxHzTooHigh {
                max_allowed: MAX_LIVE_SCRUB_MAX_HZ,
                actual: self.live_scrub_max_hz,
            });
        }

        if self.timeline_hover_prepare_slots == 0 {
            return Err(FrameServerConfigError::ZeroTimelineHoverPrepareSlots);
        }

        if self.timeline_hover_prepare_slots > MAX_TIMELINE_HOVER_PREPARE_SLOTS {
            return Err(FrameServerConfigError::TimelineHoverPrepareSlotsTooHigh {
                max_allowed: MAX_TIMELINE_HOVER_PREPARE_SLOTS,
                actual: self.timeline_hover_prepare_slots,
            });
        }

        if self.recent_superseded_prepare_slots > MAX_RECENT_SUPERSEDED_PREPARE_SLOTS {
            return Err(
                FrameServerConfigError::RecentSupersededPrepareSlotsTooHigh {
                    max_allowed: MAX_RECENT_SUPERSEDED_PREPARE_SLOTS,
                    actual: self.recent_superseded_prepare_slots,
                },
            );
        }

        if self.software_recent_superseded_prepare_slots
            > MAX_SOFTWARE_RECENT_SUPERSEDED_PREPARE_SLOTS
        {
            return Err(
                FrameServerConfigError::SoftwareRecentSupersededPrepareSlotsTooHigh {
                    max_allowed: MAX_SOFTWARE_RECENT_SUPERSEDED_PREPARE_SLOTS,
                    actual: self.software_recent_superseded_prepare_slots,
                },
            );
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

    #[must_use]
    pub const fn live_scrub_max_hz(self) -> u16 {
        self.raw.live_scrub_max_hz
    }

    #[must_use]
    pub const fn live_scrub_decode_mode(self) -> LiveScrubDecodeMode {
        self.raw.live_scrub_decode_mode
    }

    #[must_use]
    pub const fn timeline_hover_prepare_slots(self) -> u8 {
        self.raw.timeline_hover_prepare_slots
    }

    #[must_use]
    pub const fn recent_superseded_prepare_slots(self) -> u8 {
        self.raw.recent_superseded_prepare_slots
    }

    #[must_use]
    pub const fn software_recent_superseded_prepare_slots(self) -> u8 {
        self.raw.software_recent_superseded_prepare_slots
    }
}
