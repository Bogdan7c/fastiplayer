//! Transient gesture state machine и actions без зависимости от egui internals.

use std::time::Instant;

use frame_server_core::{LiveScrubDiagnostics, LiveScrubSettingsSnapshot};
use media_core::{MediaTime, TimelineSnapshot};

use super::geometry::TimelineBounds;
use super::live_scrub::LiveScrubDispatchState;

/// Transient UI-состояние timeline; playback и decoder state здесь не хранятся.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimelineUiState {
    transient_drag_position: Option<MediaTime>,
    live_scrub_gesture_active: bool,
    live_scrub_dispatch: Option<LiveScrubDispatchState>,
}

impl TimelineUiState {
    #[must_use]
    pub const fn has_active_drag(&self) -> bool {
        self.transient_drag_position.is_some()
    }

    #[must_use]
    pub const fn has_active_live_scrub_gesture(&self) -> bool {
        self.live_scrub_gesture_active
    }

    pub fn clear_transient_drag(&mut self) {
        self.transient_drag_position = None;
    }

    pub fn clear_live_scrub_gesture(&mut self) {
        self.live_scrub_gesture_active = false;
    }

    pub fn clear_live_scrub_dispatch(&mut self) {
        self.live_scrub_dispatch = None;
    }

    pub fn begin_live_scrub_dispatch(
        &mut self,
        settings: LiveScrubSettingsSnapshot,
        now: Instant,
        initial_target: MediaTime,
    ) {
        self.live_scrub_dispatch =
            Some(LiveScrubDispatchState::begin(settings, now, initial_target));
    }

    pub fn note_live_scrub_landing_presented(&mut self, presented_target: MediaTime) {
        if let Some(dispatch) = self.live_scrub_dispatch.as_mut() {
            dispatch.note_landing_presented(presented_target);
        }
    }

    #[must_use]
    pub fn live_scrub_diagnostics(&self) -> Option<LiveScrubDiagnostics> {
        self.live_scrub_dispatch
            .map(LiveScrubDispatchState::diagnostics)
    }

    pub fn defer_live_scrub_settings_change(
        &mut self,
        new_snapshot: LiveScrubSettingsSnapshot,
    ) -> Option<LiveScrubDiagnostics> {
        self.live_scrub_dispatch
            .as_mut()
            .map(|dispatch| dispatch.defer_settings_change(new_snapshot))
    }

    pub fn live_scrub_preview_dispatch_target(
        &mut self,
        now: Instant,
        target: MediaTime,
    ) -> Option<MediaTime> {
        self.live_scrub_dispatch
            .as_mut()
            .map_or(Some(target), |dispatch| {
                dispatch.preview_dispatch_target(now, target)
            })
    }

    pub fn live_scrub_release_dispatch_target(
        &mut self,
        now: Instant,
        release_target: MediaTime,
    ) -> Option<MediaTime> {
        self.live_scrub_dispatch
            .as_mut()?
            .release_dispatch_target(now, release_target)
    }

    #[must_use]
    pub fn display_position(&self, timeline: &TimelineSnapshot) -> MediaTime {
        self.transient_drag_position
            .or(timeline.target_position)
            .unwrap_or(timeline.current_position)
    }
}

/// Намерение timeline, которое composition root конвертирует в `PlayerCommand`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineAction {
    ClickSeek(MediaTime),
    CommitDragSeek(MediaTime),
    BeginLiveScrub(MediaTime),
    PreviewLiveScrub(MediaTime),
    EndLiveScrubAtLatestTarget(MediaTime),
    EndLiveScrubAtVisiblePreview(MediaTime),
    CancelLiveScrub,
}

/// Нормализованный frame input gesture mapper-а.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TimelinePointerInput {
    pub clicked: bool,
    pub pointer_down_on_timeline: bool,
    pub drag_started: bool,
    pub dragged: bool,
    pub drag_stopped: bool,
    pub lost_focus: bool,
    pub pointer_fraction: Option<f64>,
}

/// Actions и немедленная display position одного UI frame-а.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineInteraction {
    pub actions: Vec<TimelineAction>,
    pub display_position: MediaTime,
}

/// Детерминированно переводит gesture frame в actions, меняя лишь transient state.
#[must_use]
pub fn map_timeline_interaction(
    timeline: &TimelineSnapshot,
    state: &mut TimelineUiState,
    bounds: Option<TimelineBounds>,
    input: TimelinePointerInput,
    live_scrub_enabled: bool,
) -> TimelineInteraction {
    let Some(bounds) = bounds else {
        state.clear_transient_drag();
        state.clear_live_scrub_gesture();
        state.clear_live_scrub_dispatch();
        return interaction(Vec::new(), timeline.current_position);
    };

    let pointer_position = input
        .pointer_fraction
        .map(|fraction| bounds.position_from_fraction(fraction));
    if input.lost_focus && state.has_active_live_scrub_gesture() {
        state.clear_transient_drag();
        state.clear_live_scrub_gesture();
        return interaction(
            vec![TimelineAction::CancelLiveScrub],
            state.display_position(timeline),
        );
    }
    if input.lost_focus && state.has_active_drag() {
        let commit_position = pointer_position.or(state.transient_drag_position);
        state.clear_transient_drag();
        let actions = commit_position
            .map(TimelineAction::CommitDragSeek)
            .into_iter()
            .collect();
        return interaction(actions, state.display_position(timeline));
    }

    let wants_drag_position = input.drag_started
        || input.dragged
        || input.drag_stopped
        || (live_scrub_enabled && (input.pointer_down_on_timeline || input.clicked));
    if wants_drag_position && let Some(position) = pointer_position {
        state.transient_drag_position = Some(position);
    }

    let mut actions = Vec::new();
    let mut began_live_scrub_this_frame = false;
    if live_scrub_enabled
        && input.pointer_down_on_timeline
        && !state.has_active_live_scrub_gesture()
        && let Some(position) = pointer_position
    {
        state.live_scrub_gesture_active = true;
        began_live_scrub_this_frame = true;
        actions.push(TimelineAction::BeginLiveScrub(position));
    }
    if state.has_active_live_scrub_gesture()
        && !began_live_scrub_this_frame
        && input.dragged
        && let Some(position) = pointer_position
    {
        actions.push(TimelineAction::PreviewLiveScrub(position));
    }

    if state.has_active_live_scrub_gesture() && (input.drag_stopped || input.clicked) {
        let release_position = pointer_position.or(state.transient_drag_position);
        state.clear_transient_drag();
        state.clear_live_scrub_gesture();
        actions.push(match release_position {
            Some(position) if input.drag_stopped => {
                TimelineAction::EndLiveScrubAtVisiblePreview(position)
            }
            Some(position) => TimelineAction::EndLiveScrubAtLatestTarget(position),
            None => TimelineAction::CancelLiveScrub,
        });
    } else if input.drag_stopped {
        let commit_position = state.transient_drag_position;
        state.clear_transient_drag();
        actions.extend(commit_position.map(TimelineAction::CommitDragSeek));
    } else if input.clicked && !state.has_active_drag() {
        actions.extend(pointer_position.map(TimelineAction::ClickSeek));
    }

    interaction(actions, state.display_position(timeline))
}

fn interaction(actions: Vec<TimelineAction>, display_position: MediaTime) -> TimelineInteraction {
    TimelineInteraction {
        actions,
        display_position,
    }
}

#[cfg(test)]
mod tests {
    use media_core::{MediaDuration, TimelineRange};

    use super::*;

    fn timeline() -> TimelineSnapshot {
        TimelineSnapshot::seekable_vod(MediaDuration::from_secs(100))
    }

    fn bounds() -> TimelineBounds {
        TimelineBounds::new(TimelineRange::from_bounds_saturating(
            MediaTime::ZERO,
            MediaTime::from_secs(100),
        ))
        .expect("seekable bounds")
    }

    fn frame(
        state: &mut TimelineUiState,
        input: TimelinePointerInput,
        live: bool,
    ) -> Vec<TimelineAction> {
        map_timeline_interaction(&timeline(), state, Some(bounds()), input, live).actions
    }

    #[test]
    fn simple_drag_sequence_emits_only_release_seek() {
        let mut state = TimelineUiState::default();
        assert!(
            frame(
                &mut state,
                TimelinePointerInput {
                    drag_started: true,
                    pointer_fraction: Some(0.2),
                    ..Default::default()
                },
                false
            )
            .is_empty()
        );
        assert!(
            frame(
                &mut state,
                TimelinePointerInput {
                    dragged: true,
                    pointer_fraction: Some(0.7),
                    ..Default::default()
                },
                false
            )
            .is_empty()
        );
        assert_eq!(
            frame(
                &mut state,
                TimelinePointerInput {
                    drag_stopped: true,
                    pointer_fraction: Some(0.7),
                    ..Default::default()
                },
                false
            ),
            vec![TimelineAction::CommitDragSeek(MediaTime::from_secs(70))]
        );
    }

    #[test]
    fn live_drag_sequence_emits_begin_preview_and_visible_release() {
        let mut state = TimelineUiState::default();
        assert_eq!(
            frame(
                &mut state,
                TimelinePointerInput {
                    pointer_down_on_timeline: true,
                    pointer_fraction: Some(0.2),
                    ..Default::default()
                },
                true
            ),
            vec![TimelineAction::BeginLiveScrub(MediaTime::from_secs(20))]
        );
        assert_eq!(
            frame(
                &mut state,
                TimelinePointerInput {
                    dragged: true,
                    pointer_fraction: Some(0.7),
                    ..Default::default()
                },
                true
            ),
            vec![TimelineAction::PreviewLiveScrub(MediaTime::from_secs(70))]
        );
        assert_eq!(
            frame(
                &mut state,
                TimelinePointerInput {
                    drag_stopped: true,
                    pointer_fraction: Some(0.9),
                    ..Default::default()
                },
                true
            ),
            vec![TimelineAction::EndLiveScrubAtVisiblePreview(
                MediaTime::from_secs(90)
            )]
        );
    }

    #[test]
    fn focus_loss_commits_simple_drag_but_cancels_live_scrub() {
        let mut simple = TimelineUiState {
            transient_drag_position: Some(MediaTime::from_secs(40)),
            ..Default::default()
        };
        assert_eq!(
            frame(
                &mut simple,
                TimelinePointerInput {
                    lost_focus: true,
                    ..Default::default()
                },
                false
            ),
            vec![TimelineAction::CommitDragSeek(MediaTime::from_secs(40))]
        );
        let mut live = TimelineUiState {
            transient_drag_position: Some(MediaTime::from_secs(40)),
            live_scrub_gesture_active: true,
            ..Default::default()
        };
        assert_eq!(
            frame(
                &mut live,
                TimelinePointerInput {
                    lost_focus: true,
                    ..Default::default()
                },
                true
            ),
            vec![TimelineAction::CancelLiveScrub]
        );
    }

    #[test]
    fn pointer_down_and_drag_same_frame_emits_only_begin() {
        let mut state = TimelineUiState::default();
        assert_eq!(
            frame(
                &mut state,
                TimelinePointerInput {
                    pointer_down_on_timeline: true,
                    dragged: true,
                    pointer_fraction: Some(0.3),
                    ..Default::default()
                },
                true
            ),
            vec![TimelineAction::BeginLiveScrub(MediaTime::from_secs(30))]
        );
    }

    #[test]
    fn pointer_down_is_inert_without_live_scrub_and_click_seeks_once() {
        let mut state = TimelineUiState::default();
        assert!(
            frame(
                &mut state,
                TimelinePointerInput {
                    pointer_down_on_timeline: true,
                    pointer_fraction: Some(0.25),
                    ..Default::default()
                },
                false
            )
            .is_empty()
        );
        assert!(!state.has_active_drag());
        assert_eq!(
            frame(
                &mut state,
                TimelinePointerInput {
                    clicked: true,
                    pointer_fraction: Some(0.25),
                    ..Default::default()
                },
                false
            ),
            vec![TimelineAction::ClickSeek(MediaTime::from_secs(25))]
        );
    }

    #[test]
    fn active_live_gesture_keeps_route_when_setting_changes_mid_drag() {
        let mut state = TimelineUiState::default();
        let _ = frame(
            &mut state,
            TimelinePointerInput {
                pointer_down_on_timeline: true,
                pointer_fraction: Some(0.2),
                ..Default::default()
            },
            true,
        );
        assert_eq!(
            frame(
                &mut state,
                TimelinePointerInput {
                    dragged: true,
                    pointer_fraction: Some(0.6),
                    ..Default::default()
                },
                false
            ),
            vec![TimelineAction::PreviewLiveScrub(MediaTime::from_secs(60))]
        );
        assert_eq!(
            frame(
                &mut state,
                TimelinePointerInput {
                    drag_stopped: true,
                    pointer_fraction: Some(0.8),
                    ..Default::default()
                },
                false
            ),
            vec![TimelineAction::EndLiveScrubAtVisiblePreview(
                MediaTime::from_secs(80)
            )]
        );
    }

    #[test]
    fn focus_loss_and_drag_stop_commit_final_pointer_once() {
        let mut state = TimelineUiState {
            transient_drag_position: Some(MediaTime::from_secs(40)),
            ..Default::default()
        };
        assert_eq!(
            frame(
                &mut state,
                TimelinePointerInput {
                    drag_stopped: true,
                    lost_focus: true,
                    pointer_fraction: Some(0.9),
                    ..Default::default()
                },
                false
            ),
            vec![TimelineAction::CommitDragSeek(MediaTime::from_secs(90))]
        );
    }

    #[test]
    fn disabled_timeline_clears_transient_state_without_actions() {
        let timeline = TimelineSnapshot::default();
        let mut state = TimelineUiState {
            transient_drag_position: Some(MediaTime::from_secs(10)),
            ..Default::default()
        };
        let result = map_timeline_interaction(
            &timeline,
            &mut state,
            None,
            TimelinePointerInput {
                clicked: true,
                dragged: true,
                drag_stopped: true,
                pointer_fraction: Some(0.5),
                ..Default::default()
            },
            false,
        );
        assert!(result.actions.is_empty());
        assert!(!state.has_active_drag());
    }
}
