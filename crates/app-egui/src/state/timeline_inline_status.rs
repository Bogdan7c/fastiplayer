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
    use frame_server_core::{
        BackendRevision, PlaybackGeneration, ScrubDriverOutcomeKind, ScrubEvent,
        ScrubEventDiagnostics, ScrubExactnessPolicy, ScrubFailedEvent, ScrubFailureReason,
        ScrubGeneration, ScrubGenerationToken, ScrubRequestKind, ScrubTarget, ScrubTargetContext,
        ScrubTrackSelection, SourceRevision,
    };
    use media_core::{MediaTime, TimeBase, TrackId, TrackTimestamp};
    use std::time::{Duration, Instant};

    fn scrub_context_for_tests() -> ScrubTargetContext {
        let video_track = TrackId::new(7);
        let time_base = TimeBase::new(1, 1_000).expect("валидная test timebase");

        ScrubTargetContext::new(
            SourceRevision::new(10),
            BackendRevision::new(20),
            ScrubTrackSelection::with_audio(video_track, TrackId::new(8)),
            ScrubTarget::new(
                MediaTime::from_millis(1_250),
                TrackTimestamp::new(video_track, 1_250, time_base),
            ),
            ScrubExactnessPolicy::TargetOrAfter,
            ScrubRequestKind::SeekLanding,
            ScrubGenerationToken::new(PlaybackGeneration::new(30), ScrubGeneration::new(40)),
        )
    }

    fn failed_scrub_event_for_tests(
        reason: ScrubFailureReason,
        driver_outcome: ScrubDriverOutcomeKind,
    ) -> ScrubEvent {
        ScrubEvent::Failed(ScrubFailedEvent {
            context: scrub_context_for_tests(),
            reason,
            diagnostics: ScrubEventDiagnostics::new(driver_outcome),
        })
    }

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

    #[test]
    fn non_audio_frame_server_failure_stays_diagnostics_only() {
        let mut state = TimelineInlineStatusState::default();
        let now = Instant::now();
        let event = failed_scrub_event_for_tests(
            ScrubFailureReason::DecoderBackpressure,
            ScrubDriverOutcomeKind::DecoderBackpressure,
        );

        state.handle_scrub_event(&event, now);

        assert_eq!(state.visible_message(now), None);
    }
}
