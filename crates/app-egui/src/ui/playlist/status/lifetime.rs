//! UI-only lifetime/dedup owner проблем Playlist status.

use std::time::Duration;

use animation_core::{Easing, SlideTransition};

use crate::ui::animation::UiMotion;

use super::presentation::{
    PlaylistStatusPresentation, PlaylistStatusProblemIdentity, PlaylistStatusRetention,
    PlaylistStatusRow,
};

/// Одновременно статусная область показывает не больше пяти свежих проблем.
const MAX_VISIBLE_PROBLEMS: usize = 5;
/// Через десять секунд проблема перестаёт быть authoritative.
const PROBLEM_LIFETIME_SECONDS: f64 = 10.0;
/// Весь путь открытия или закрытия занимает ровно 180 мс.
const STATUS_TRANSITION_DURATION: Duration = Duration::from_millis(180);

/// Active запись хранит deadline отдельно от безопасного presentation payload.
#[derive(Debug, Clone)]
struct ActiveProblem {
    row: PlaylistStatusRow,
    deadline: f64,
}

/// Последняя identity одного slot-а подавляет вечный persistent snapshot после expiry.
#[derive(Debug, Clone, Copy)]
struct ObservedProblem {
    identity: PlaylistStatusProblemIdentity,
    retention: PlaylistStatusRetention,
}

/// Status owner живёт внутри `PlaylistUiState` и не меняет runtime/domain state.
#[derive(Debug)]
pub(in crate::ui::playlist) struct PlaylistStatusLifetimeState {
    /// Новые проблемы находятся в начале bounded списка.
    active: Vec<ActiveProblem>,
    /// По одному последнему identity на runtime source достаточно для suppression.
    observed: Vec<ObservedProblem>,
    /// Один общий slide сохраняет существующую geometry-only анимацию.
    transition: SlideTransition,
    /// Residual snapshot живёт только до последнего пикселя закрытия.
    retained_presentation: Option<PlaylistStatusPresentation>,
    /// Frame time нужен для animation delta без поглощения десятисекундного idle.
    previous_frame_time: Option<f64>,
    /// Явный target позволяет начать reverse с нулевым delta в кадре переключения.
    target_open: bool,
}

impl Default for PlaylistStatusLifetimeState {
    fn default() -> Self {
        Self {
            active: Vec::with_capacity(MAX_VISIBLE_PROBLEMS),
            observed: Vec::new(),
            transition: SlideTransition::closed(),
            retained_presentation: None,
            previous_frame_time: None,
            target_open: false,
        }
    }
}

impl PlaylistStatusLifetimeState {
    /// Сопоставляет typed snapshot, применяет deadlines и двигает общий slide.
    pub(super) fn advance(
        &mut self,
        current_presentation: Option<PlaylistStatusPresentation>,
        motion: UiMotion,
        frame_time: f64,
    ) -> PlaylistStatusFrame {
        let current_rows = current_presentation
            .map(PlaylistStatusPresentation::into_rows)
            .unwrap_or_default();

        // Stateful проблема снимается сразу, как только owner явно перестал её публиковать.
        self.remove_resolved_stateful_problems(&current_rows);
        // Граница `now == deadline` уже означает expiry и начало закрытия.
        self.active
            .retain(|active_problem| frame_time < active_problem.deadline);
        // Reverse iteration сохраняет исходный порядок при вставке новых строк в начало.
        for current_row in current_rows.into_iter().rev() {
            self.observe_current_problem(current_row, frame_time);
        }
        // Шестая свежая проблема вытесняет самую старую без синтетического `+N`.
        self.active.truncate(MAX_VISIBLE_PROBLEMS);

        let active_presentation = PlaylistStatusPresentation::from_rows(
            self.active
                .iter()
                .map(|active_problem| active_problem.row.clone())
                .collect(),
        );
        if let Some(presentation) = active_presentation.as_ref() {
            self.retained_presentation = Some(presentation.clone());
        }

        let should_open = active_presentation.is_some();
        let target_changed = self.target_open != should_open;
        self.target_open = should_open;
        self.transition.set_target_open(should_open);

        // Новый target начинает движение сейчас, а не задним числом за весь idle interval.
        let delta_seconds = if target_changed {
            0.0
        } else {
            self.previous_frame_time
                .map_or(0.0, |previous| (frame_time - previous).max(0.0))
        };
        self.previous_frame_time = Some(frame_time);
        let duration_seconds = match motion {
            UiMotion::Standard => STATUS_TRANSITION_DURATION.as_secs_f32(),
            UiMotion::Reduced => 0.0,
        };
        self.transition
            .advance(delta_seconds as f32, duration_seconds);

        let progress = self.transition.eased_progress(Easing::EaseOutCubic);
        if self.transition.is_fully_closed() {
            self.retained_presentation = None;
        }
        let presentation = active_presentation.or_else(|| self.retained_presentation.clone());
        let next_deadline = self
            .active
            .iter()
            .map(|active_problem| active_problem.deadline)
            .min_by(f64::total_cmp);

        PlaylistStatusFrame {
            presentation,
            progress,
            authoritative: !self.active.is_empty(),
            needs_repaint: self.transition.is_animating(),
            repaint_after: next_deadline
                .filter(|deadline| *deadline > frame_time)
                .map(|deadline| Duration::from_secs_f64(deadline - frame_time)),
        }
    }

    /// Disabled sidebar copy читает только ещё не истёкшие authoritative проблемы.
    pub(super) fn actionless_copy(&self, frame_time: f64) -> Option<PlaylistStatusPresentation> {
        PlaylistStatusPresentation::from_rows(
            self.active
                .iter()
                .filter(|active_problem| frame_time < active_problem.deadline)
                .map(|active_problem| active_problem.row.clone())
                .collect(),
        )
    }

    fn remove_resolved_stateful_problems(&mut self, current_rows: &[PlaylistStatusRow]) {
        self.active.retain(|active_problem| {
            !matches!(
                active_problem.row.retention(),
                PlaylistStatusRetention::WhilePresent
            ) || contains_identity(current_rows, active_problem.row.identity())
        });
        self.observed.retain(|observed_problem| {
            !matches!(
                observed_problem.retention,
                PlaylistStatusRetention::WhilePresent
            ) || contains_identity(current_rows, observed_problem.identity)
        });
    }

    fn observe_current_problem(&mut self, current_row: PlaylistStatusRow, frame_time: f64) {
        let current_identity = current_row.identity();
        let current_slot = current_identity.slot();
        let observed_index = self
            .observed
            .iter()
            .position(|observed_problem| observed_problem.identity.slot() == current_slot);

        if let Some(observed_index) = observed_index {
            if self.observed[observed_index].identity == current_identity {
                // Persistent snapshot обновляет payload, но не запускает deadline повторно.
                if let Some(active_problem) = self
                    .active
                    .iter_mut()
                    .find(|active_problem| active_problem.row.identity() == current_identity)
                {
                    active_problem.row = current_row;
                }
                return;
            }
            // Новое событие того же owner-а заменяет старое и начинает собственные 10 секунд.
            self.observed[observed_index] = ObservedProblem {
                identity: current_identity,
                retention: current_row.retention(),
            };
        } else {
            self.observed.push(ObservedProblem {
                identity: current_identity,
                retention: current_row.retention(),
            });
        }

        self.active
            .retain(|active_problem| active_problem.row.identity().slot() != current_slot);
        self.active.insert(
            0,
            ActiveProblem {
                row: current_row,
                deadline: frame_time + PROBLEM_LIFETIME_SECONDS,
            },
        );
    }
}

/// Все render/pacing данные одного egui frame-а.
#[derive(Debug)]
pub(super) struct PlaylistStatusFrame {
    pub(super) presentation: Option<PlaylistStatusPresentation>,
    pub(super) progress: f32,
    pub(super) authoritative: bool,
    pub(super) needs_repaint: bool,
    pub(super) repaint_after: Option<Duration>,
}

fn contains_identity(rows: &[PlaylistStatusRow], identity: PlaylistStatusProblemIdentity) -> bool {
    rows.iter().any(|row| row.identity() == identity)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use std::num::NonZeroU64;

    use crate::playlist_runtime::{
        PlaylistManualAddEventId, PlaylistProbeView, PlaylistSafeFeedbackGeneration,
        PlaylistSaveView, SiblingDiscoveryScopeId,
    };
    use crate::ui::playlist::status::presentation::{
        PlaylistNavigationProblemIdentity, PlaylistStatusProblemIdentity, StatusRowKind, StatusTone,
    };

    fn event(generation: u64, message: &'static str) -> PlaylistStatusPresentation {
        PlaylistStatusPresentation::from_rows(vec![PlaylistStatusRow::new(
            PlaylistStatusProblemIdentity::SafeFeedback(PlaylistSafeFeedbackGeneration(generation)),
            PlaylistStatusRetention::Event,
            Arc::<str>::from(message),
            StatusTone::Warning,
            StatusRowKind::Normal,
            None,
        )])
        .expect("fixture contains one row")
    }

    fn stateful_startup() -> PlaylistStatusPresentation {
        PlaylistStatusPresentation::from_rows(vec![PlaylistStatusRow::new(
            PlaylistStatusProblemIdentity::StartupWarning,
            PlaylistStatusRetention::WhilePresent,
            "startup warning",
            StatusTone::Warning,
            StatusRowKind::Normal,
            None,
        )])
        .expect("fixture contains one row")
    }

    fn stateful_save() -> PlaylistStatusPresentation {
        PlaylistStatusPresentation::from_rows(vec![PlaylistStatusRow::new(
            PlaylistStatusProblemIdentity::Save(PlaylistSaveView::Blocked),
            PlaylistStatusRetention::WhilePresent,
            "save blocked",
            StatusTone::Warning,
            StatusRowKind::Weak,
            None,
        )])
        .expect("fixture contains one row")
    }

    fn scope(value: u64) -> SiblingDiscoveryScopeId {
        SiblingDiscoveryScopeId::from_non_zero(
            NonZeroU64::new(value).expect("fixture scope is non-zero"),
        )
    }

    fn event_row(
        identity: PlaylistStatusProblemIdentity,
        message: &'static str,
    ) -> PlaylistStatusPresentation {
        PlaylistStatusPresentation::from_rows(vec![PlaylistStatusRow::new(
            identity,
            PlaylistStatusRetention::Event,
            message,
            StatusTone::Warning,
            StatusRowKind::Normal,
            None,
        )])
        .expect("fixture contains one row")
    }

    #[test]
    fn exact_ten_second_boundary_expires_without_reopening_persistent_snapshot() {
        let mut state = PlaylistStatusLifetimeState::default();
        let persistent = event(1, "problem");

        let opened = state.advance(Some(persistent.clone()), UiMotion::Reduced, 0.0);
        assert!(opened.presentation.is_some());
        let before = state.advance(Some(persistent.clone()), UiMotion::Reduced, 9.999);
        assert!(before.presentation.is_some());
        let expired = state.advance(Some(persistent.clone()), UiMotion::Reduced, 10.0);
        assert!(expired.presentation.is_none());
        let still_suppressed = state.advance(Some(persistent), UiMotion::Reduced, 40.0);
        assert!(still_suppressed.presentation.is_none());
    }

    #[test]
    fn independent_deadlines_and_repeated_source_refresh_only_its_problem() {
        let mut state = PlaylistStatusLifetimeState::default();
        let startup_row = stateful_startup().into_rows().remove(0);
        let safe_row = event(1, "safe").into_rows().remove(0);
        let both =
            PlaylistStatusPresentation::from_rows(vec![startup_row.clone(), safe_row.clone()])
                .expect("two independent fixtures");
        let _ = state.advance(Some(stateful_startup()), UiMotion::Reduced, 0.0);
        let _ = state.advance(Some(both.clone()), UiMotion::Reduced, 5.0);

        let at_old_deadline = state.advance(Some(both.clone()), UiMotion::Reduced, 10.0);
        let rows = at_old_deadline
            .presentation
            .expect("second problem owns an independent deadline");
        assert_eq!(rows.rows().len(), 1);
        assert_eq!(rows.rows()[0].text(), "safe");
        assert!(
            state
                .advance(Some(both), UiMotion::Reduced, 15.0)
                .presentation
                .is_none()
        );

        let refreshed = state.advance(Some(event(2, "refreshed")), UiMotion::Reduced, 16.0);
        assert_eq!(
            refreshed
                .presentation
                .expect("new generation reopens the slot")
                .rows()[0]
                .text(),
            "refreshed"
        );
    }

    #[test]
    fn resolved_state_disappears_early_and_can_be_reported_again() {
        let mut state = PlaylistStatusLifetimeState::default();
        let _ = state.advance(Some(stateful_save()), UiMotion::Reduced, 0.0);
        assert!(
            state
                .advance(None, UiMotion::Reduced, 1.0)
                .presentation
                .is_none()
        );
        assert!(
            state
                .advance(Some(stateful_save()), UiMotion::Reduced, 2.0)
                .presentation
                .is_some()
        );
    }

    #[test]
    fn sixth_distinct_problem_evicts_oldest_without_summary_row() {
        let mut state = PlaylistStatusLifetimeState::default();
        let presentations = [
            event_row(
                PlaylistStatusProblemIdentity::StartupWarning,
                "oldest startup",
            ),
            event_row(
                PlaylistStatusProblemIdentity::Probe(PlaylistProbeView::Warning {
                    scope_id: scope(1),
                }),
                "probe",
            ),
            event_row(
                PlaylistStatusProblemIdentity::ManualAdd(PlaylistManualAddEventId(2)),
                "manual",
            ),
            event_row(
                PlaylistStatusProblemIdentity::SafeFeedback(PlaylistSafeFeedbackGeneration(3)),
                "feedback",
            ),
            event_row(
                PlaylistStatusProblemIdentity::Save(PlaylistSaveView::Blocked),
                "save",
            ),
            event_row(
                PlaylistStatusProblemIdentity::Navigation(
                    PlaylistNavigationProblemIdentity::Fatal { scope_id: scope(4) },
                ),
                "newest navigation",
            ),
        ];
        for (index, presentation) in presentations.into_iter().enumerate() {
            let _ = state.advance(Some(presentation), UiMotion::Reduced, index as f64 * 0.1);
        }

        let visible = state
            .advance(None, UiMotion::Reduced, 1.0)
            .presentation
            .expect("five newest problems remain");
        assert_eq!(visible.rows().len(), MAX_VISIBLE_PROBLEMS);
        assert!(
            visible
                .rows()
                .iter()
                .all(|row| row.text() != "oldest startup")
        );
        assert!(visible.rows().iter().all(|row| !row.text().contains("+")));
    }
}
