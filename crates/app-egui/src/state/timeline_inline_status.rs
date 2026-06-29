use std::time::{Duration, Instant};

use player_core::{ScrubDriverOutcomeKind, ScrubEvent};

use super::AppState;

/// Фиксированное время показа inline failure у timeline.
pub(super) const TIMELINE_INLINE_FAILURE_DURATION: Duration = Duration::from_millis(2_500);

/// Локальный app-egui статус timeline, не попадающий в player-core snapshot.
#[derive(Debug, Default)]
pub(super) struct TimelineInlineStatusState {
    active_failure: Option<TimelineInlineFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimelineInlineFailure {
    message: &'static str,
    expires_at: Instant,
}

impl TimelineInlineStatusState {
    /// Слушает scrub events и включает только audio resume failure/timeout.
    pub(super) fn handle_scrub_event(&mut self, event: &ScrubEvent, now: Instant) {
        let ScrubEvent::Failed(event) = event else {
            return;
        };

        let message = match event.diagnostics.driver_outcome {
            ScrubDriverOutcomeKind::AudioResumeTimedOut => "Audio resume timed out",
            ScrubDriverOutcomeKind::AudioResumeFailed => "Audio resume failed",
            _ => return,
        };

        self.active_failure = Some(TimelineInlineFailure {
            message,
            expires_at: now + TIMELINE_INLINE_FAILURE_DURATION,
        });
    }

    /// Возвращает видимый текст и сам очищает истёкший статус.
    pub(super) fn visible_message(&mut self, now: Instant) -> Option<&'static str> {
        if self
            .active_failure
            .is_some_and(|failure| now >= failure.expires_at)
        {
            self.active_failure = None;
        }

        self.active_failure.map(|failure| failure.message)
    }

    /// Следующее явное timeline действие снимает старый inline failure.
    pub(super) fn clear_for_timeline_action(&mut self) {
        self.active_failure = None;
    }

    #[cfg(test)]
    fn show_failure_for_tests(&mut self, message: &'static str, now: Instant) {
        self.active_failure = Some(TimelineInlineFailure {
            message,
            expires_at: now + TIMELINE_INLINE_FAILURE_DURATION,
        });
    }
}

impl AppState {
    /// Переносит scrub failure stream в локальный timeline inline status.
    pub(crate) fn handle_timeline_inline_status_scrub_event(&mut self, event: &ScrubEvent) {
        self.timeline_inline_status
            .handle_scrub_event(event, Instant::now());
    }

    /// Возвращает видимый timeline inline status для текущего UI frame-а.
    pub(super) fn timeline_inline_status_message(&mut self, now: Instant) -> Option<&'static str> {
        self.timeline_inline_status.visible_message(now)
    }

    /// Следующее timeline действие явно снимает прошлый failure status.
    pub(super) fn clear_timeline_inline_status_for_action(&mut self) {
        self.timeline_inline_status.clear_for_timeline_action();
    }
}

#[cfg(test)]
mod tests {
    use super::{TIMELINE_INLINE_FAILURE_DURATION, TimelineInlineStatusState};
    use std::time::{Duration, Instant};

    #[test]
    fn audio_failure_status_lives_for_fixed_2500_ms() {
        let mut state = TimelineInlineStatusState::default();
        let now = Instant::now();

        state.show_failure_for_tests("Audio resume timed out", now);

        assert_eq!(state.visible_message(now), Some("Audio resume timed out"));
        assert_eq!(
            state
                .visible_message(now + TIMELINE_INLINE_FAILURE_DURATION - Duration::from_millis(1)),
            Some("Audio resume timed out")
        );
        assert_eq!(
            state.visible_message(now + TIMELINE_INLINE_FAILURE_DURATION),
            None
        );
    }

    #[test]
    fn timeline_action_clears_inline_failure() {
        let mut state = TimelineInlineStatusState::default();
        let now = Instant::now();

        state.show_failure_for_tests("Audio resume failed", now);
        state.clear_for_timeline_action();

        assert_eq!(state.visible_message(now), None);
    }
}
