use std::time::{Duration, Instant};

/// Причина, по которой pending hover leave grace должен освободить prepared entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimelineHoverLeaveGraceReleaseReason {
    /// Config задал `hover_leave_grace_ms = 0`, поэтому release нужен в frame leave-а.
    ImmediateTimelineLeave,

    /// Grace deadline наступил без повторного входа pointer/focus на timeline.
    LeaveGraceExpired,

    /// Пользователь сделал действие вне timeline, и ждать UX grace больше нельзя.
    NonTimelineAction,
}

/// Результат обработки timeline leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimelineHoverLeaveGraceStartOutcome {
    /// Entries временно удерживаются до указанного deadline-а.
    Pending { expires_at: Instant },

    /// Grace равен нулю, поэтому caller должен release-нуть entries сразу.
    ReleaseNow {
        reason: TimelineHoverLeaveGraceReleaseReason,
    },
}

/// App-owned таймер retention после ухода pointer/focus с timeline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct TimelineHoverLeaveGraceState {
    /// Deadline pending cleanup-а; отсутствие значения означает отсутствие grace.
    expires_at: Option<Instant>,
}

impl TimelineHoverLeaveGraceState {
    /// Начинает leave grace или сразу просит release при нулевой длительности.
    pub(super) fn note_timeline_left(
        &mut self,
        now: Instant,
        grace_duration: Duration,
    ) -> TimelineHoverLeaveGraceStartOutcome {
        if grace_duration.is_zero() {
            self.expires_at = None;
            return TimelineHoverLeaveGraceStartOutcome::ReleaseNow {
                reason: TimelineHoverLeaveGraceReleaseReason::ImmediateTimelineLeave,
            };
        }

        let expires_at = now + grace_duration;
        self.expires_at = Some(expires_at);
        TimelineHoverLeaveGraceStartOutcome::Pending { expires_at }
    }

    /// Отменяет pending release, когда timeline hover/focus вернулся до expiry.
    pub(super) fn cancel_for_reenter(&mut self) -> bool {
        self.expires_at.take().is_some()
    }

    /// Отменяет grace и просит немедленный release из-за действия вне timeline.
    pub(super) fn cancel_for_non_timeline_action(
        &mut self,
    ) -> Option<TimelineHoverLeaveGraceReleaseReason> {
        self.expires_at
            .take()
            .map(|_expires_at| TimelineHoverLeaveGraceReleaseReason::NonTimelineAction)
    }

    /// Возвращает release reason ровно один раз, когда deadline наступил.
    pub(super) fn expire_due(
        &mut self,
        now: Instant,
    ) -> Option<TimelineHoverLeaveGraceReleaseReason> {
        let expires_at = self.expires_at?;
        if now < expires_at {
            return None;
        }

        self.expires_at = None;
        Some(TimelineHoverLeaveGraceReleaseReason::LeaveGraceExpired)
    }

    /// Есть ли pending grace, ради которого shell должен продолжить redraw-и.
    pub(super) const fn is_pending(self) -> bool {
        self.expires_at.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_hover_leave_grace_has_no_cleanup_churn() {
        let now = Instant::now();
        let mut state = TimelineHoverLeaveGraceState::default();

        assert_eq!(state.expire_due(now), None);
        assert_eq!(state.cancel_for_non_timeline_action(), None);
        assert!(!state.cancel_for_reenter());
        assert!(!state.is_pending());
    }

    #[test]
    fn leave_starts_grace_without_immediate_release() {
        let now = Instant::now();
        let mut state = TimelineHoverLeaveGraceState::default();

        let outcome = state.note_timeline_left(now, Duration::from_millis(500));

        assert_eq!(
            outcome,
            TimelineHoverLeaveGraceStartOutcome::Pending {
                expires_at: now + Duration::from_millis(500),
            }
        );
        assert_eq!(state.expire_due(now + Duration::from_millis(499)), None);
        assert!(state.is_pending());
    }

    #[test]
    fn reenter_before_expiry_preserves_pending_entries() {
        let now = Instant::now();
        let mut state = TimelineHoverLeaveGraceState::default();
        state.note_timeline_left(now, Duration::from_millis(500));

        assert!(state.cancel_for_reenter());

        assert_eq!(state.expire_due(now + Duration::from_millis(500)), None);
        assert!(!state.is_pending());
    }

    #[test]
    fn expiry_releases_once() {
        let now = Instant::now();
        let mut state = TimelineHoverLeaveGraceState::default();
        state.note_timeline_left(now, Duration::from_millis(500));

        assert_eq!(
            state.expire_due(now + Duration::from_millis(500)),
            Some(TimelineHoverLeaveGraceReleaseReason::LeaveGraceExpired)
        );
        assert_eq!(state.expire_due(now + Duration::from_millis(501)), None);
        assert_eq!(state.cancel_for_non_timeline_action(), None);
    }

    #[test]
    fn zero_grace_releases_immediately_without_disabling_future_hover() {
        let now = Instant::now();
        let mut state = TimelineHoverLeaveGraceState::default();

        let first_leave = state.note_timeline_left(now, Duration::ZERO);
        let second_leave = state.note_timeline_left(now + Duration::from_millis(1), Duration::ZERO);

        assert_eq!(
            first_leave,
            TimelineHoverLeaveGraceStartOutcome::ReleaseNow {
                reason: TimelineHoverLeaveGraceReleaseReason::ImmediateTimelineLeave,
            }
        );
        assert_eq!(
            second_leave,
            TimelineHoverLeaveGraceStartOutcome::ReleaseNow {
                reason: TimelineHoverLeaveGraceReleaseReason::ImmediateTimelineLeave,
            }
        );
        assert!(!state.is_pending());
    }

    #[test]
    fn non_timeline_action_cancels_grace_and_releases_once() {
        let now = Instant::now();
        let mut state = TimelineHoverLeaveGraceState::default();
        state.note_timeline_left(now, Duration::from_millis(500));

        assert_eq!(
            state.cancel_for_non_timeline_action(),
            Some(TimelineHoverLeaveGraceReleaseReason::NonTimelineAction)
        );
        assert_eq!(state.cancel_for_non_timeline_action(), None);
        assert_eq!(state.expire_due(now + Duration::from_millis(500)), None);
    }
}
