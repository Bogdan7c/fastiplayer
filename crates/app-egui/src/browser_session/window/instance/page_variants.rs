//! Варианты страницы в окне браузерного режима: факты от медиа-части и тост после принятого
//! выбора или «следующей серии» (сессия 14a).

use tracing::info;

use super::super::variant_test_trigger::VariantTestTrigger;
use super::BrowserWindowInstance;
use crate::page_variants::{
    PageVariantFacts, PageVariantKind, PageVariantNotice, PageVariantRefusal, PageVariantSubject,
};

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
            PageVariantNotice::Switching { kind } => switching_word(*kind),
            PageVariantNotice::Refused { subject, reason } => refusal_word(*subject, *reason),
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

/// Слово журнала для «идёт смена»: `switching_audio`, `switching_quality`.
const fn switching_word(kind: PageVariantKind) -> &'static str {
    match kind {
        PageVariantKind::Audio => "switching_audio",
        PageVariantKind::Quality => "switching_quality",
        PageVariantKind::Subtitles => "switching_subtitles",
    }
}

/// Слово журнала для отказа страницы: `refused_audio_no_such_episode` и т. п. (без подписей).
const fn refusal_word(subject: PageVariantSubject, reason: PageVariantRefusal) -> &'static str {
    match (subject, reason) {
        (PageVariantSubject::Menu(PageVariantKind::Audio), PageVariantRefusal::NoSuchEpisode) => {
            "refused_audio_no_such_episode"
        }
        (PageVariantSubject::Menu(PageVariantKind::Audio), PageVariantRefusal::PersonOnly) => {
            "refused_audio_person_only"
        }
        (PageVariantSubject::Menu(PageVariantKind::Audio), PageVariantRefusal::Failed) => {
            "refused_audio_failed"
        }
        (PageVariantSubject::Menu(PageVariantKind::Quality), PageVariantRefusal::NoSuchEpisode) => {
            "refused_quality_no_such_episode"
        }
        (PageVariantSubject::Menu(PageVariantKind::Quality), PageVariantRefusal::PersonOnly) => {
            "refused_quality_person_only"
        }
        (PageVariantSubject::Menu(PageVariantKind::Quality), PageVariantRefusal::Failed) => {
            "refused_quality_failed"
        }
        (PageVariantSubject::Menu(PageVariantKind::Subtitles), _) => "refused_subtitles",
        (PageVariantSubject::NextEpisode, PageVariantRefusal::NoSuchEpisode) => {
            "refused_next_episode_no_such_episode"
        }
        (PageVariantSubject::NextEpisode, PageVariantRefusal::PersonOnly) => {
            "refused_next_episode_person_only"
        }
        (PageVariantSubject::NextEpisode, PageVariantRefusal::Failed) => {
            "refused_next_episode_failed"
        }
    }
}
