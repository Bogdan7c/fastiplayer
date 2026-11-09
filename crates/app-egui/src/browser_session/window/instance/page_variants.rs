//! Варианты страницы в окне браузерного режима: факты от медиа-части и тост после принятого
//! выбора или «следующей серии» (сессия 14a).

use tracing::info;

use super::super::variant_test_trigger::VariantTestTrigger;
use super::BrowserWindowInstance;
use crate::page_variants::{PageVariantFacts, PageVariantNotice};

impl BrowserWindowInstance {
    /// Факты о вариантах от медиа-части (каждый проход окна): меню ♪, кнопка качества и
    /// кнопка Next рисуются по ним. Кадр нужен, только если что-то изменилось.
    pub(in crate::browser_session::window) fn apply_page_variants(
        &mut self,
        facts: PageVariantFacts,
    ) {
        if self.app_state.note_page_variants(facts) {
            self.app_state.note_visible_mutation_for_redraw();
            self.window.request_redraw();
        }
    }

    /// Тост «Audio: …» / «Quality: …» / «Next episode», если за шаг сеанс принял действие
    /// зрителя. Журнал — без подписи (это текст страницы): только вид слова.
    pub(in crate::browser_session::window) fn show_page_variant_notice(
        &mut self,
        notice: Option<PageVariantNotice>,
    ) {
        let Some(notice) = notice else {
            return;
        };
        let word = match &notice {
            PageVariantNotice::Selected { kind, .. } => kind.log_label(),
            PageVariantNotice::NextEpisodeRequested => "next_episode",
        };
        info!("Browser window tells the viewer about a page variant: {word}");
        self.app_state.announce_page_variant_notice(&notice);
        self.app_state.note_visible_mutation_for_redraw();
        self.window.request_redraw();
    }

    /// Тестовый выбор варианта (только тесты 14a, `variant_test_trigger`): тот же путь, что
    /// пункт меню.
    pub(in crate::browser_session::window) fn poll_variant_test_trigger(
        &mut self,
        trigger: &mut VariantTestTrigger,
    ) {
        trigger.poll(&mut self.app_state);
    }
}
