use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAction {
    Present,
    Wait(Duration),
    Drop,
}

pub struct AvSync {
    threshold_present_ms: f64,
    threshold_drop_ms: f64,
}

impl Default for AvSync {
    fn default() -> Self {
        Self {
            threshold_present_ms: 25.0,
            threshold_drop_ms: 50.0,
        }
    }
}

impl AvSync {
    pub fn new(threshold_present_ms: f64, threshold_drop_ms: f64) -> Self {
        assert!(threshold_drop_ms > threshold_present_ms);
        Self {
            threshold_present_ms,
            threshold_drop_ms,
        }
    }

    pub fn decide(&self, video_pts: Duration, audio_clock: Duration) -> FrameAction {
        let diff_ms = video_pts.as_secs_f64() * 1000.0 - audio_clock.as_secs_f64() * 1000.0;

        if diff_ms < -self.threshold_drop_ms {
            FrameAction::Drop
        } else if diff_ms < -self.threshold_present_ms {
            FrameAction::Present
        } else if diff_ms > self.threshold_present_ms {
            FrameAction::Wait(Duration::from_secs_f64(diff_ms / 1000.0))
        } else {
            FrameAction::Present
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(millis: f64) -> Duration {
        Duration::from_secs_f64(millis / 1000.0)
    }

    #[test]
    fn test_video_ahead_wait() {
        let sync = AvSync::default();
        assert!(matches!(
            sync.decide(ms(100.0), ms(50.0)),
            FrameAction::Wait(_)
        ));
    }

    #[test]
    fn test_video_late_drop() {
        let sync = AvSync::default();
        assert_eq!(sync.decide(ms(0.0), ms(100.0)), FrameAction::Drop);
    }

    #[test]
    fn test_in_sync_present() {
        let sync = AvSync::default();
        assert_eq!(sync.decide(ms(100.0), ms(100.0)), FrameAction::Present);
    }
}
