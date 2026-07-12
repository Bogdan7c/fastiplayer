//! App-owned контроллер выбора и анимации левого sidebar.

use animation_core::SlideTransition;

/// Стабильный порядок секций одновременно задаёт направление перелистывания.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SidebarSection {
    Playlist,
    Settings,
    Url,
    Info,
}

/// Направление сдвига нового содержимого.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentSlideDirection {
    FromLeft,
    FromRight,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SidebarContentTransition {
    pub(crate) from: SidebarSection,
    pub(crate) to: SidebarSection,
    pub(crate) progress: f32,
    pub(crate) direction: ContentSlideDirection,
}

/// Результат клика, который boundary настроек использует без догадок.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarSelectionOutcome {
    Opened(SidebarSection),
    Queued(SidebarSection),
    Closed,
}

/// Единственный владелец общего open/selection/transition состояния sidebar.
pub(crate) struct SidebarController {
    displayed: Option<SidebarSection>,
    target: Option<SidebarSection>,
    queued: Option<SidebarSection>,
    content_from: Option<SidebarSection>,
    content_progress: f32,
    open_slide: SlideTransition,
}

impl Default for SidebarController {
    fn default() -> Self {
        Self {
            displayed: None,
            target: None,
            queued: None,
            content_from: None,
            content_progress: 1.0,
            open_slide: SlideTransition::closed(),
        }
    }
}

impl SidebarController {
    /// Скрывает sidebar, не вмешиваясь в lifecycle содержимого секции.
    pub(crate) fn hide(&mut self) {
        self.queued = None;
        self.target = None;
        self.open_slide.set_target_open(false);
    }
    pub(crate) fn reconcile_settings_visibility(&mut self, settings_visible: bool) {
        if !settings_visible && self.target == Some(SidebarSection::Settings) {
            self.queued = None;
            self.target = None;
            self.open_slide.set_target_open(false);
        }
    }
    pub(crate) fn select(&mut self, requested: SidebarSection) -> SidebarSelectionOutcome {
        if self.queued == Some(requested) {
            self.queued = None;
            self.target = None;
            self.open_slide.set_target_open(false);
            return SidebarSelectionOutcome::Closed;
        }
        if self.target == Some(requested) && self.content_from.is_none() {
            self.target = None;
            self.open_slide.set_target_open(false);
            return SidebarSelectionOutcome::Closed;
        }
        if self.target.is_none() && self.displayed == Some(requested) && self.content_from.is_none()
        {
            self.target = Some(requested);
            self.open_slide.set_target_open(true);
            return SidebarSelectionOutcome::Opened(requested);
        }
        if self.content_from.is_some() {
            self.queued = Some(requested);
            return SidebarSelectionOutcome::Queued(requested);
        }

        match self.displayed {
            None => {
                self.displayed = Some(requested);
                self.target = Some(requested);
                self.open_slide.set_target_open(true);
                SidebarSelectionOutcome::Opened(requested)
            }
            Some(current) => {
                self.content_from = Some(current);
                self.target = Some(requested);
                self.content_progress = 0.0;
                SidebarSelectionOutcome::Opened(requested)
            }
        }
    }

    pub(crate) fn advance(&mut self, dt_seconds: f32, duration_seconds: f32) {
        self.open_slide.advance(dt_seconds, duration_seconds);
        let content_duration = duration_seconds * 0.5;
        if self.content_from.is_some() {
            self.content_progress = if content_duration <= f32::EPSILON {
                1.0
            } else {
                (self.content_progress + dt_seconds / content_duration).min(1.0)
            };
            if self.content_progress >= 1.0 {
                self.displayed = self.target;
                self.content_from = None;
                if let Some(next) = self.queued.take()
                    && Some(next) != self.displayed
                {
                    self.content_from = self.displayed;
                    self.target = Some(next);
                    self.content_progress = 0.0;
                }
            }
        }
        if self.open_slide.is_fully_closed() {
            self.displayed = None;
        }
    }

    pub(crate) fn displayed(&self) -> Option<SidebarSection> {
        self.displayed
    }
    pub(crate) fn target(&self) -> Option<SidebarSection> {
        self.target
    }
    pub(crate) fn open_progress(&self) -> f32 {
        self.open_slide
            .eased_progress(animation_core::Easing::EaseInOutCubic)
    }
    #[cfg(test)]
    pub(crate) fn content_progress(&self) -> f32 {
        self.content_progress
    }
    pub(crate) fn direction(&self) -> Option<ContentSlideDirection> {
        Some(if self.target? < self.content_from? {
            ContentSlideDirection::FromLeft
        } else {
            ContentSlideDirection::FromRight
        })
    }
    pub(crate) fn content_transition(&self) -> Option<SidebarContentTransition> {
        Some(SidebarContentTransition {
            from: self.content_from?,
            to: self.target?,
            progress: self.content_progress,
            direction: self.direction()?,
        })
    }
    pub(crate) fn is_animating(&self) -> bool {
        self.open_slide.is_animating() || self.content_from.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_duration_is_half_of_open_duration() {
        let mut controller = SidebarController::default();
        controller.select(SidebarSection::Playlist);
        controller.advance(1.0, 1.0);
        controller.select(SidebarSection::Info);
        controller.advance(0.25, 1.0);
        assert_eq!(controller.content_progress(), 0.5);
    }

    #[test]
    fn direction_follows_stable_section_order_even_across_several_items() {
        let mut controller = SidebarController::default();
        controller.select(SidebarSection::Playlist);
        controller.advance(1.0, 1.0);
        controller.select(SidebarSection::Info);
        assert_eq!(
            controller.direction(),
            Some(ContentSlideDirection::FromRight)
        );
    }

    #[test]
    fn latest_request_replaces_queued_request() {
        let mut controller = SidebarController::default();
        controller.select(SidebarSection::Playlist);
        controller.advance(1.0, 1.0);
        controller.select(SidebarSection::Settings);
        controller.select(SidebarSection::Url);
        controller.select(SidebarSection::Info);
        controller.advance(0.5, 1.0);
        controller.advance(0.5, 1.0);
        assert_eq!(controller.target(), Some(SidebarSection::Info));
    }

    #[test]
    fn zero_duration_finishes_both_transitions_immediately() {
        let mut controller = SidebarController::default();
        controller.select(SidebarSection::Playlist);
        controller.advance(0.0, 0.0);
        controller.select(SidebarSection::Info);
        controller.advance(0.0, 0.0);
        assert_eq!(controller.displayed(), Some(SidebarSection::Info));
        assert!(!controller.is_animating());
    }

    #[test]
    fn repeated_active_click_closes_and_click_during_close_reverses_opening() {
        let mut controller = SidebarController::default();
        controller.select(SidebarSection::Info);
        controller.advance(0.3, 1.0);
        assert_eq!(
            controller.select(SidebarSection::Info),
            SidebarSelectionOutcome::Closed
        );
        controller.advance(0.1, 1.0);
        assert_eq!(
            controller.select(SidebarSection::Info),
            SidebarSelectionOutcome::Opened(SidebarSection::Info)
        );
        controller.advance(1.0, 1.0);
        assert_eq!(controller.displayed(), Some(SidebarSection::Info));
        assert!(!controller.is_animating());
    }
}
