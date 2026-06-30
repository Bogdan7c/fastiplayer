//! App-owned runtime state единого timeline hover intent-а.
//!
//! S24 намеренно не строит реальный `TimelineHoverPrepareTarget`: для этого нужны
//! source/backend/track/generation guards, которыми владеют player/source слои.
//! Здесь хранится только coalesced UI intent и placeholder visual slot.

use crate::ui::timeline::{TimelineHoverIntent, TimelineHoverTarget, TimelineHoverVisualTarget};

/// Placeholder состояния visual hover preview slot-а.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) enum TimelineHoverPreviewSlot {
    /// Нет активного visual preview target-а.
    #[default]
    Empty,

    /// Visual preview может показать placeholder для target-а.
    Pending(TimelineHoverVisualTarget),

    /// Hover target активен, но visual presentation выключена config-ом.
    DisabledByConfig,
}

/// Итог применения latest hover intent-а в конце одного egui frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct TimelineHoverFrameOutcome {
    /// Invisible prepare/predecode intent, который должен увидеть app-owned controller.
    pub(crate) invisible_prepare_target: Option<TimelineHoverTarget>,

    /// Hover leave очистил active invisible prepare target.
    pub(crate) invisible_prepare_cleared: bool,

    /// Visual presentation intent для placeholder preview slot-а.
    pub(crate) visual_presentation_target: Option<TimelineHoverVisualTarget>,

    /// Visual preview slot был очищен leave/gesture state-ом.
    pub(crate) visual_presentation_cleared: bool,
}

/// Coalescer одного UI frame-а: latest hover intent wins без задержки/debounce.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct TimelineHoverFrameCoalescer {
    /// Последний hover intent, полученный в текущем app frame.
    latest_intent: Option<TimelineHoverIntent>,
}

impl TimelineHoverFrameCoalescer {
    /// Запоминает новый intent; старый same-frame intent намеренно заменяется.
    pub(crate) fn record(&mut self, intent: TimelineHoverIntent) {
        self.latest_intent = Some(intent);
    }

    /// Применяет latest intent к owner state в конце обработки controls.
    pub(crate) fn finish(
        self,
        state: &mut TimelineHoverIntentState,
        visual_preview_enabled: bool,
    ) -> TimelineHoverFrameOutcome {
        state.apply_frame_intent(self.latest_intent, visual_preview_enabled)
    }
}

/// App-owned состояние hover intent-а, отдельное от visual widget и player.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TimelineHoverIntentState {
    /// Последний hover target, для которого уже опубликован invisible prepare intent.
    active_target: Option<TimelineHoverTarget>,

    /// Placeholder visual preview slot для будущего S25 render path.
    preview_slot: TimelineHoverPreviewSlot,

    /// Test-observable счётчик invisible prepare target intents.
    invisible_prepare_target_count: u64,

    /// Test-observable счётчик clear intents для invisible prepare target-а.
    invisible_prepare_clear_count: u64,

    /// Test-observable счётчик visual presentation intents.
    visual_presentation_target_count: u64,
}

impl TimelineHoverIntentState {
    /// Возвращает текущий active target invisible prepare stream-а.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn active_target(&self) -> Option<TimelineHoverTarget> {
        self.active_target
    }

    /// Возвращает placeholder state visual preview slot-а.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn preview_slot(&self) -> TimelineHoverPreviewSlot {
        self.preview_slot
    }

    /// Возвращает текущий visual target для render/materialization retry.
    #[must_use]
    pub(crate) const fn pending_visual_target(&self) -> Option<TimelineHoverVisualTarget> {
        match self.preview_slot {
            TimelineHoverPreviewSlot::Pending(visual_target) => Some(visual_target),
            TimelineHoverPreviewSlot::Empty | TimelineHoverPreviewSlot::DisabledByConfig => None,
        }
    }

    /// Возвращает число опубликованных invisible prepare target intents.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn invisible_prepare_target_count(&self) -> u64 {
        self.invisible_prepare_target_count
    }

    /// Возвращает число clear intents для invisible prepare path-а.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn invisible_prepare_clear_count(&self) -> u64 {
        self.invisible_prepare_clear_count
    }

    /// Возвращает число visual presentation target intents.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn visual_presentation_target_count(&self) -> u64 {
        self.visual_presentation_target_count
    }

    /// Применяет latest hover intent одного frame-а.
    fn apply_frame_intent(
        &mut self,
        intent: Option<TimelineHoverIntent>,
        visual_preview_enabled: bool,
    ) -> TimelineHoverFrameOutcome {
        match intent {
            Some(TimelineHoverIntent::Target(visual_target)) => {
                self.apply_target(visual_target, visual_preview_enabled)
            }
            Some(TimelineHoverIntent::Clear) => self.clear_active_hover(),
            None => TimelineHoverFrameOutcome::default(),
        }
    }

    /// Публикует target только при смене позиции, но синхронизирует visual config.
    fn apply_target(
        &mut self,
        visual_target: TimelineHoverVisualTarget,
        visual_preview_enabled: bool,
    ) -> TimelineHoverFrameOutcome {
        let target = visual_target.target();
        let target_changed = self.active_target != Some(target);
        self.active_target = Some(target);

        let mut outcome = TimelineHoverFrameOutcome::default();
        if target_changed {
            self.invisible_prepare_target_count += 1;
            outcome.invisible_prepare_target = Some(target);
        }

        self.apply_visual_preview_target(visual_target, visual_preview_enabled, &mut outcome);
        outcome
    }

    /// Обновляет только visual placeholder slot; invisible intent не зависит от config-а.
    fn apply_visual_preview_target(
        &mut self,
        visual_target: TimelineHoverVisualTarget,
        visual_preview_enabled: bool,
        outcome: &mut TimelineHoverFrameOutcome,
    ) {
        if visual_preview_enabled {
            if self.preview_slot != TimelineHoverPreviewSlot::Pending(visual_target) {
                self.preview_slot = TimelineHoverPreviewSlot::Pending(visual_target);
                self.visual_presentation_target_count += 1;
                outcome.visual_presentation_target = Some(visual_target);
            }
        } else {
            if matches!(self.preview_slot, TimelineHoverPreviewSlot::Pending(_)) {
                outcome.visual_presentation_cleared = true;
            }
            self.preview_slot = TimelineHoverPreviewSlot::DisabledByConfig;
        }
    }

    /// Очищает active hover state без hover leave grace timer-а; S28A владеет grace.
    fn clear_active_hover(&mut self) -> TimelineHoverFrameOutcome {
        let mut outcome = TimelineHoverFrameOutcome::default();

        if self.active_target.take().is_some() {
            self.invisible_prepare_clear_count += 1;
            outcome.invisible_prepare_cleared = true;
        }

        if matches!(self.preview_slot, TimelineHoverPreviewSlot::Pending(_)) {
            outcome.visual_presentation_cleared = true;
        }
        self.preview_slot = TimelineHoverPreviewSlot::Empty;

        outcome
    }
}

#[cfg(test)]
mod tests {
    use media_core::MediaTime;

    use super::*;
    use crate::ui::timeline::{TimelineHoverPreviewPlacement, TimelineHoverVisualTarget};

    fn hover_target(seconds: u64) -> TimelineHoverTarget {
        TimelineHoverTarget::new(MediaTime::from_secs(seconds))
    }

    fn hover_visual_target(seconds: u64) -> TimelineHoverVisualTarget {
        TimelineHoverVisualTarget::new(
            hover_target(seconds),
            TimelineHoverPreviewPlacement::new(
                egui::pos2(50.0, 20.0),
                egui::Rect::from_min_size(egui::pos2(0.0, 10.0), egui::vec2(100.0, 12.0)),
            ),
        )
    }

    fn shifted_hover_visual_target(seconds: u64) -> TimelineHoverVisualTarget {
        TimelineHoverVisualTarget::new(
            hover_target(seconds),
            TimelineHoverPreviewPlacement::new(
                egui::pos2(55.0, 20.0),
                egui::Rect::from_min_size(egui::pos2(5.0, 10.0), egui::vec2(100.0, 12.0)),
            ),
        )
    }

    #[test]
    fn same_frame_hover_updates_coalesce_to_latest_target() {
        let mut state = TimelineHoverIntentState::default();
        let mut coalescer = TimelineHoverFrameCoalescer::default();

        coalescer.record(TimelineHoverIntent::Target(hover_visual_target(10)));
        coalescer.record(TimelineHoverIntent::Target(hover_visual_target(40)));
        let outcome = coalescer.finish(&mut state, true);

        assert_eq!(state.active_target(), Some(hover_target(40)));
        assert_eq!(state.invisible_prepare_target_count(), 1);
        assert_eq!(outcome.invisible_prepare_target, Some(hover_target(40)));
        assert_eq!(
            outcome.visual_presentation_target,
            Some(hover_visual_target(40))
        );
    }

    #[test]
    fn duplicate_target_does_not_republish_prepare_or_visual_intents() {
        let mut state = TimelineHoverIntentState::default();

        TimelineHoverFrameCoalescer {
            latest_intent: Some(TimelineHoverIntent::Target(hover_visual_target(25))),
        }
        .finish(&mut state, true);
        let duplicate = TimelineHoverFrameCoalescer {
            latest_intent: Some(TimelineHoverIntent::Target(hover_visual_target(25))),
        }
        .finish(&mut state, true);

        assert_eq!(state.invisible_prepare_target_count(), 1);
        assert_eq!(state.visual_presentation_target_count(), 1);
        assert_eq!(duplicate, TimelineHoverFrameOutcome::default());
    }

    #[test]
    fn placement_change_updates_visual_without_duplicate_prepare_target() {
        let mut state = TimelineHoverIntentState::default();

        TimelineHoverFrameCoalescer {
            latest_intent: Some(TimelineHoverIntent::Target(hover_visual_target(25))),
        }
        .finish(&mut state, true);
        let shifted = TimelineHoverFrameCoalescer {
            latest_intent: Some(TimelineHoverIntent::Target(shifted_hover_visual_target(25))),
        }
        .finish(&mut state, true);

        assert_eq!(state.invisible_prepare_target_count(), 1);
        assert_eq!(state.visual_presentation_target_count(), 2);
        assert_eq!(shifted.invisible_prepare_target, None);
        assert_eq!(
            shifted.visual_presentation_target,
            Some(shifted_hover_visual_target(25))
        );
    }

    #[test]
    fn duplicate_target_keeps_pending_visual_target_for_materialization_retry() {
        let mut state = TimelineHoverIntentState::default();

        TimelineHoverFrameCoalescer {
            latest_intent: Some(TimelineHoverIntent::Target(hover_visual_target(25))),
        }
        .finish(&mut state, true);
        let duplicate = TimelineHoverFrameCoalescer {
            latest_intent: Some(TimelineHoverIntent::Target(hover_visual_target(25))),
        }
        .finish(&mut state, true);

        assert_eq!(duplicate.visual_presentation_target, None);
        assert_eq!(state.pending_visual_target(), Some(hover_visual_target(25)));
    }

    #[test]
    fn visual_preview_disabled_still_emits_invisible_prepare_intent() {
        let mut state = TimelineHoverIntentState::default();

        let outcome = TimelineHoverFrameCoalescer {
            latest_intent: Some(TimelineHoverIntent::Target(hover_visual_target(60))),
        }
        .finish(&mut state, false);

        assert_eq!(state.active_target(), Some(hover_target(60)));
        assert_eq!(
            state.preview_slot(),
            TimelineHoverPreviewSlot::DisabledByConfig
        );
        assert_eq!(outcome.invisible_prepare_target, Some(hover_target(60)));
        assert_eq!(outcome.visual_presentation_target, None);
    }

    #[test]
    fn leave_clears_visual_slot_and_active_prepare_target() {
        let mut state = TimelineHoverIntentState::default();

        TimelineHoverFrameCoalescer {
            latest_intent: Some(TimelineHoverIntent::Target(hover_visual_target(30))),
        }
        .finish(&mut state, true);
        let outcome = TimelineHoverFrameCoalescer {
            latest_intent: Some(TimelineHoverIntent::Clear),
        }
        .finish(&mut state, true);

        assert_eq!(state.active_target(), None);
        assert_eq!(state.preview_slot(), TimelineHoverPreviewSlot::Empty);
        assert_eq!(state.invisible_prepare_clear_count(), 1);
        assert!(outcome.invisible_prepare_cleared);
        assert!(outcome.visual_presentation_cleared);
    }

    #[test]
    fn reenter_after_leave_emits_new_invisible_prepare_target() {
        let mut state = TimelineHoverIntentState::default();

        TimelineHoverFrameCoalescer {
            latest_intent: Some(TimelineHoverIntent::Target(hover_visual_target(30))),
        }
        .finish(&mut state, true);
        TimelineHoverFrameCoalescer {
            latest_intent: Some(TimelineHoverIntent::Clear),
        }
        .finish(&mut state, true);
        let reenter = TimelineHoverFrameCoalescer {
            latest_intent: Some(TimelineHoverIntent::Target(hover_visual_target(30))),
        }
        .finish(&mut state, true);

        assert_eq!(state.active_target(), Some(hover_target(30)));
        assert_eq!(state.invisible_prepare_target_count(), 2);
        assert_eq!(reenter.invisible_prepare_target, Some(hover_target(30)));
    }
}
