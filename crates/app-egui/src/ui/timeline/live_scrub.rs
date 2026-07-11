//! Dispatch policy одного live-scrub gesture-а, независимая от egui.

use std::time::{Duration, Instant};

use frame_server_core::{
    DeferredLiveScrubSettingsChange, LiveScrubDecodeMode, LiveScrubDiagnostics,
    LiveScrubSettingsSnapshot,
};
use media_core::MediaTime;

/// Неизменная верхняя граница ожидания landing frame перед force-dispatch.
const LIVE_SCRUB_LANDING_FALLBACK_BUDGET: Duration = Duration::from_millis(250);

/// Policy state принадлежит UI-dispatch boundary, но не renderer-у и не player-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LiveScrubDispatchState {
    diagnostics: LiveScrubDiagnostics,
    last_observed_settings: LiveScrubSettingsSnapshot,
    last_decode_dispatch_at: Option<Instant>,
    last_dispatched_target: Option<MediaTime>,
    pending_throttled_target: Option<MediaTime>,
    awaiting_landing_for_target: Option<MediaTime>,
}

impl LiveScrubDispatchState {
    pub(super) fn begin(
        settings: LiveScrubSettingsSnapshot,
        now: Instant,
        initial_target: MediaTime,
    ) -> Self {
        Self {
            diagnostics: LiveScrubDiagnostics::from_settings_snapshot(settings),
            last_observed_settings: settings,
            last_decode_dispatch_at: Some(now),
            last_dispatched_target: Some(initial_target),
            pending_throttled_target: None,
            awaiting_landing_for_target: Some(initial_target),
        }
    }

    pub(super) fn note_landing_presented(&mut self, presented_target: MediaTime) {
        if self
            .awaiting_landing_for_target
            .is_some_and(|awaiting| presented_target == awaiting)
        {
            self.awaiting_landing_for_target = None;
        }
    }

    pub(super) const fn diagnostics(self) -> LiveScrubDiagnostics {
        self.diagnostics
    }

    pub(super) fn defer_settings_change(
        &mut self,
        new_snapshot: LiveScrubSettingsSnapshot,
    ) -> LiveScrubDiagnostics {
        if self.last_observed_settings != new_snapshot {
            let change = DeferredLiveScrubSettingsChange {
                old_snapshot: self.last_observed_settings,
                new_snapshot,
            };
            self.last_observed_settings = new_snapshot;
            self.diagnostics.record_deferred_settings_change(change);
        }
        self.diagnostics
    }

    pub(super) fn preview_dispatch_target(
        &mut self,
        now: Instant,
        target: MediaTime,
    ) -> Option<MediaTime> {
        // egui может сообщать drag каждый frame при неподвижном pointer. Повтор
        // exact target не должен перезапускать decode от keyframe.
        if self.last_dispatched_target == Some(target) {
            self.pending_throttled_target = None;
            return None;
        }

        match self.diagnostics.settings_snapshot.decode_mode {
            LiveScrubDecodeMode::EveryDragEvent => {
                self.record_dispatch(now, target, true);
                Some(target)
            }
            LiveScrubDecodeMode::ThrottledLatest => {
                if self.is_waiting_for_landing(now) {
                    self.record_skipped_target(target);
                    return None;
                }
                let min_period = min_dispatch_period(self.diagnostics.settings_snapshot.max_hz);
                let can_dispatch = self.last_decode_dispatch_at.is_none_or(|last_dispatch| {
                    now.saturating_duration_since(last_dispatch) >= min_period
                });
                if can_dispatch {
                    self.record_dispatch(now, target, true);
                    Some(target)
                } else {
                    self.record_skipped_target(target);
                    None
                }
            }
        }
    }

    pub(super) fn release_dispatch_target(
        &mut self,
        now: Instant,
        release_target: MediaTime,
    ) -> Option<MediaTime> {
        // Pending нужен лишь как признак coalescing: release всегда обязан попасть
        // в точную pointer coordinate, а не в более старую pending позицию.
        self.pending_throttled_target = None;
        if self.last_dispatched_target == Some(release_target) {
            return None;
        }
        self.record_dispatch(now, release_target, false);
        Some(release_target)
    }

    fn is_waiting_for_landing(self, now: Instant) -> bool {
        self.awaiting_landing_for_target.is_some()
            && self.last_decode_dispatch_at.is_some_and(|last_dispatch| {
                now.saturating_duration_since(last_dispatch) < LIVE_SCRUB_LANDING_FALLBACK_BUDGET
            })
    }

    fn record_dispatch(&mut self, now: Instant, target: MediaTime, await_landing: bool) {
        self.pending_throttled_target = None;
        self.last_decode_dispatch_at = Some(now);
        self.last_dispatched_target = Some(target);
        self.awaiting_landing_for_target = await_landing.then_some(target);
    }

    fn record_skipped_target(&mut self, target: MediaTime) {
        if self.pending_throttled_target != Some(target) {
            self.pending_throttled_target = Some(target);
            self.diagnostics.record_throttled_latest_skip();
        }
    }
}

fn min_dispatch_period(max_hz: u16) -> Duration {
    Duration::from_nanos(1_000_000_000u64 / u64::from(max_hz.clamp(1, 240)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn throttled() -> LiveScrubSettingsSnapshot {
        LiveScrubSettingsSnapshot {
            decode_mode: LiveScrubDecodeMode::ThrottledLatest,
            max_hz: 60,
        }
    }

    #[test]
    fn completion_gate_opens_only_for_exact_landing() {
        let started_at = Instant::now();
        let mut dispatch =
            LiveScrubDispatchState::begin(throttled(), started_at, MediaTime::from_secs(30));
        dispatch.note_landing_presented(MediaTime::from_secs(30));
        let period = min_dispatch_period(60);
        assert_eq!(
            dispatch.preview_dispatch_target(started_at + period, MediaTime::from_secs(10)),
            Some(MediaTime::from_secs(10))
        );
        dispatch.note_landing_presented(MediaTime::from_secs(30));
        assert_eq!(
            dispatch.preview_dispatch_target(started_at + period * 2, MediaTime::from_secs(5)),
            None
        );
        dispatch.note_landing_presented(MediaTime::from_secs(10));
        assert_eq!(
            dispatch.preview_dispatch_target(started_at + period * 3, MediaTime::from_secs(5)),
            Some(MediaTime::from_secs(5))
        );
    }

    #[test]
    fn fallback_budget_and_release_preserve_progress_and_exactness() {
        let started_at = Instant::now();
        let mut dispatch =
            LiveScrubDispatchState::begin(throttled(), started_at, MediaTime::from_secs(10));
        assert_eq!(
            dispatch.preview_dispatch_target(
                started_at + min_dispatch_period(60),
                MediaTime::from_secs(40)
            ),
            None
        );
        assert_eq!(
            dispatch.preview_dispatch_target(
                started_at + LIVE_SCRUB_LANDING_FALLBACK_BUDGET + Duration::from_millis(1),
                MediaTime::from_secs(40),
            ),
            Some(MediaTime::from_secs(40))
        );
        assert_eq!(
            dispatch.release_dispatch_target(
                started_at + Duration::from_secs(1),
                MediaTime::from_secs(45)
            ),
            Some(MediaTime::from_secs(45))
        );
    }

    #[test]
    fn stationary_pointer_is_not_redispatched() {
        let started_at = Instant::now();
        let mut dispatch =
            LiveScrubDispatchState::begin(throttled(), started_at, MediaTime::from_secs(10));
        dispatch.note_landing_presented(MediaTime::from_secs(10));
        assert_eq!(
            dispatch.preview_dispatch_target(
                started_at + min_dispatch_period(60),
                MediaTime::from_secs(10)
            ),
            None
        );
        assert_eq!(dispatch.diagnostics().throttled_latest_skip_count, 0);
    }

    #[test]
    fn every_drag_event_attempts_each_distinct_target() {
        let started_at = Instant::now();
        let mut dispatch = LiveScrubDispatchState::begin(
            LiveScrubSettingsSnapshot {
                decode_mode: LiveScrubDecodeMode::EveryDragEvent,
                max_hz: 1,
            },
            started_at,
            MediaTime::from_secs(10),
        );
        assert_eq!(
            dispatch.preview_dispatch_target(started_at, MediaTime::from_secs(20)),
            Some(MediaTime::from_secs(20))
        );
        assert_eq!(
            dispatch.preview_dispatch_target(started_at, MediaTime::from_secs(30)),
            Some(MediaTime::from_secs(30))
        );
        assert_eq!(dispatch.diagnostics().throttled_latest_skip_count, 0);
    }

    #[test]
    fn deferred_settings_do_not_replace_pointer_down_policy() {
        let started_at = Instant::now();
        let pointer_down = throttled();
        let changed = LiveScrubSettingsSnapshot {
            decode_mode: LiveScrubDecodeMode::EveryDragEvent,
            max_hz: 120,
        };
        let mut dispatch =
            LiveScrubDispatchState::begin(pointer_down, started_at, MediaTime::from_secs(10));
        let diagnostics = dispatch.defer_settings_change(changed);
        assert_eq!(diagnostics.settings_snapshot, pointer_down);
        assert_eq!(diagnostics.deferred_live_scrub_settings_change_count, 1);
    }
}
