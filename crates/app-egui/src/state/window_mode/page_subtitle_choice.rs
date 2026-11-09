//! Обмен выбором субтитров со страницей для памяти экстеншена (протокол 1.7, сессия 14a,
//! решение 283): окно — посредник между владельцем субтитров и медиа-частью сеанса.
//!
//! - **Отчёт зрителя.** Владелец субтитров после действия зрителя (меню CC, клавиши C/V) кладёт
//!   итог «дорожка страницы или выключено»; окно забирает его и отдаёт намерением в ящик
//!   ([`ViewerTransportIntent::ReportSubtitleChoice`]), как кнопки транспорта. Обычное окно
//!   итог выбрасывает: страницы у него нет.
//! - **Подсказка страницы.** Она едет в снимке склада субтитров, но действует только в окне,
//!   где транспорт ведёт страница: в остальных она снимается до владельца субтитров.

use tracing::debug;

use super::{TransportRoute, ViewerTransportIntent, WindowMode};
use crate::page_subtitles::{PageSubtitlePreference, PageSubtitleViewerChoice};
use crate::state::AppState;

/// Что окно сделало с выбором зрителя для страницы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubtitleChoiceWindowOutcome {
    /// Намерение ушло в ящик медиа-части сеанса.
    Queued,
    /// Обычное окно: страницы нет, сообщать некому.
    NotPageLed,
}

impl WindowMode {
    /// Выбор зрителя для памяти экстеншена → ящик намерений (только окно страницы).
    pub(super) fn report_subtitle_choice(
        &mut self,
        choice: PageSubtitleViewerChoice,
    ) -> SubtitleChoiceWindowOutcome {
        let TransportRoute::PageLed(outbox) = &mut self.transport else {
            return SubtitleChoiceWindowOutcome::NotPageLed;
        };
        outbox.push(ViewerTransportIntent::ReportSubtitleChoice(choice));
        SubtitleChoiceWindowOutcome::Queued
    }

    /// Подсказка страницы для этого окна: в обычном окне её нет.
    pub(super) fn page_subtitle_preference(
        &self,
        preference: PageSubtitlePreference,
    ) -> PageSubtitlePreference {
        if self.is_page_led() {
            preference
        } else {
            PageSubtitlePreference::NotGiven
        }
    }
}

impl AppState {
    /// Забирает у владельца субтитров итог действия зрителя и отдаёт его странице (если в
    /// этом окне она есть). Зовётся сразу после действия зрителя.
    pub(in crate::state) fn forward_viewer_subtitle_choice_to_page(&mut self) {
        let Some(choice) = self.take_viewer_page_subtitle_choice() else {
            return;
        };
        let outcome = self.window_mode.report_subtitle_choice(choice);
        debug!(
            ?outcome,
            choice = choice.log_label(),
            "Viewer subtitle choice"
        );
    }

    /// Подсказка страницы о субтитрах в этом окне (обычное окно — «не подсказано»).
    pub(in crate::state) fn page_subtitle_preference_for_window(
        &self,
        preference: PageSubtitlePreference,
    ) -> PageSubtitlePreference {
        self.window_mode.page_subtitle_preference(preference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page_subtitles::PageSubtitleTrackNumber;

    fn drained_intents(mode: &mut WindowMode) -> Vec<ViewerTransportIntent> {
        match &mut mode.transport {
            TransportRoute::PageLed(outbox) => outbox.take(),
            TransportRoute::LocalPlayer => Vec::new(),
        }
    }

    #[test]
    fn the_page_led_window_queues_the_viewer_choice_and_the_standard_one_drops_it() {
        let choice = PageSubtitleViewerChoice::PageTrack(PageSubtitleTrackNumber::new(2));
        let mut browser = WindowMode::browser_session();
        assert_eq!(
            browser.report_subtitle_choice(choice),
            SubtitleChoiceWindowOutcome::Queued
        );
        assert_eq!(
            drained_intents(&mut browser),
            vec![ViewerTransportIntent::ReportSubtitleChoice(choice)]
        );

        let mut standard = WindowMode::standard();
        assert_eq!(
            standard.report_subtitle_choice(choice),
            SubtitleChoiceWindowOutcome::NotPageLed
        );
        assert!(drained_intents(&mut standard).is_empty());
    }

    #[test]
    fn the_page_preference_is_ignored_outside_the_page_led_window() {
        let preference = PageSubtitlePreference::Off;
        assert_eq!(
            WindowMode::browser_session().page_subtitle_preference(preference),
            preference
        );
        assert_eq!(
            WindowMode::standard().page_subtitle_preference(preference),
            PageSubtitlePreference::NotGiven
        );
    }
}
