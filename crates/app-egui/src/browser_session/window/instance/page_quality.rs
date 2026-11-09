//! Качество страницы в окне браузерного режима: факты от медиа-части сеанса и строка журнала
//! при смене подписи кнопки (сессия 13c).

use super::BrowserWindowInstance;
use crate::page_quality::PageQualityFacts;

impl BrowserWindowInstance {
    /// Факты о качестве от медиа-части (каждый проход окна): кнопка качества получает
    /// битрейт и «можно ли просить страницу», а при смене подписи («1080p» → «720p») в
    /// журнал уходит строка для стенда. Кадр нужен, только если что-то изменилось.
    pub(in crate::browser_session::window) fn apply_page_quality(
        &mut self,
        facts: PageQualityFacts,
    ) {
        let facts_changed = self.app_state.note_page_quality(facts);
        let label_changed = self.app_state.journal_quality_label().was_logged();
        if facts_changed || label_changed {
            self.app_state.note_visible_mutation_for_redraw();
            self.window.request_redraw();
        }
    }
}
