//! Качество страницы в режиме окна: факты сеанса, просьба «показать страницу», журнал подписи
//! (сессия 13c).
//!
//! Факты ([`PageQualityFacts`]) приносит тот, кто ведёт транспорт (медиа-часть сеанса, каждый
//! проход окна), — как и «скачанное»: обычное окно их не берёт, и кнопки качества у него нет.
//! Просьба показать страницу идёт тем же путём, что кнопки транспорта: намерением зрителя в
//! ящик ([`ViewerTransportIntent::ShowPage`]), а не прямой командой.

use player_core::PlayerSnapshot;

use super::quality_menu::{QualityMenuView, journal::QualityJournalOutcome};
use super::{TransportRoute, ViewerTransportIntent, WindowMode};
use crate::page_quality::{PageQualityFacts, PageViewOffer};
use crate::page_variants::PageVariantKind;
use crate::state::AppState;
use crate::ui::player_controls::QualityControlAction;

/// Что вышло с просьбой зрителя показать страницу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a refused request must be told to the viewer or logged"]
pub(crate) enum PageViewRequestOutcome {
    /// Намерение ушло в ящик медиа-части сеанса.
    Queued,
    /// Окно ведёт страница, но сейчас просить нельзя (причина — в предложении).
    NotAvailable(PageViewOffer),
    /// Обычное окно: страницы нет, просить некого.
    NotPageLed,
}

impl WindowMode {
    /// Факты о качестве от того, кто ведёт транспорт; обычное окно их не берёт.
    /// `true` — факты изменились, окну нужен кадр.
    pub(super) fn note_page_quality(&mut self, facts: PageQualityFacts) -> bool {
        if !self.is_page_led() || self.page_quality == facts {
            return false;
        }
        self.page_quality = facts;
        true
    }

    /// Зритель просит показать страницу: намерение уходит в ящик, только если окно ведёт
    /// страница и она сейчас доступна для просьбы.
    pub(super) fn request_page_view(&mut self) -> PageViewRequestOutcome {
        let TransportRoute::PageLed(outbox) = &mut self.transport else {
            return PageViewRequestOutcome::NotPageLed;
        };
        match self.page_quality.page_view {
            PageViewOffer::Available => {
                outbox.push(ViewerTransportIntent::ShowPage);
                PageViewRequestOutcome::Queued
            }
            offer @ (PageViewOffer::PageAway
            | PageViewOffer::ExtensionTooOld
            | PageViewOffer::NotAttached) => PageViewRequestOutcome::NotAvailable(offer),
        }
    }
}

impl WindowMode {
    /// Исполняет действие кнопки качества. Кнопки нет в обычном окне, но действие всё равно
    /// проверяется по виду окна: профиль главнее присланного намерения.
    pub(super) fn apply_quality_control_action(&mut self, action: QualityControlAction) {
        if !self.view.offers_page_quality_view() {
            tracing::debug!(
                ?action,
                "Quality action ignored: this window has no quality button"
            );
            return;
        }
        match action {
            QualityControlAction::ShowPage => self.apply_show_page_request(),
            QualityControlAction::SelectPageQuality(choice) => {
                // Качество страницы (14a): те же правила окна, что у озвучки (реклама, ревизия).
                let outcome = self.select_page_variant(choice);
                tracing::debug!(?outcome, "Page quality selection");
            }
        }
    }

    /// «Change quality on the page»: просьба показать страницу.
    fn apply_show_page_request(&mut self) {
        match self.request_page_view() {
            PageViewRequestOutcome::Queued => {}
            outcome @ (PageViewRequestOutcome::NotAvailable(_)
            | PageViewRequestOutcome::NotPageLed) => {
                // Пункт меню показывается только при `Available`, так что отказ означает,
                // что состояние сеанса сменилось между кадром и кликом.
                tracing::debug!(?outcome, "Show page request refused");
            }
        }
    }

    /// Снимок меню качества на кадр: только если вид окна его предлагает.
    pub(super) fn quality_menu_view(&self, snapshot: &PlayerSnapshot) -> QualityMenuView {
        if !self.view.offers_page_quality_view() {
            return QualityMenuView::hidden();
        }
        // Список качеств страницы (14a) — над пунктом «Change quality on the page».
        QualityMenuView::from_snapshot(snapshot, &self.page_quality).with_page_qualities(
            self.page_variants.menu(PageVariantKind::Quality),
            self.page_variant_availability(),
        )
    }
}

impl AppState {
    /// Медиа-часть сеанса принесла факты о качестве (каждый проход окна). `true` — факты
    /// изменились, окну нужен кадр. Обычное окно их игнорирует.
    pub(crate) fn note_page_quality(&mut self, facts: PageQualityFacts) -> bool {
        self.window_mode.note_page_quality(facts)
    }

    /// Снимок меню качества для кадра (обычное окно — всегда скрыт).
    pub(crate) fn quality_menu_view(&self, snapshot: &PlayerSnapshot) -> QualityMenuView {
        self.window_mode.quality_menu_view(snapshot)
    }

    /// Исполняет действие кнопки качества (см. [`WindowMode::apply_quality_control_action`]).
    pub(crate) fn apply_quality_control_action(&mut self, action: QualityControlAction) {
        self.window_mode.apply_quality_control_action(action);
    }

    /// Пишет в журнал строку о качестве, если подпись кнопки сменилась (зовётся каждый
    /// проход окна браузерного режима). `Logged` — подпись сменилась.
    pub(crate) fn journal_quality_label(&mut self) -> QualityJournalOutcome {
        let snapshot = self.refresh_player_snapshot();
        let view = self.window_mode.quality_menu_view(&snapshot);
        self.window_mode.quality_journal.note(&view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(page_view: PageViewOffer) -> PageQualityFacts {
        PageQualityFacts {
            bitrate: None,
            page_view,
        }
    }

    fn drained_intents(mode: &mut WindowMode) -> Vec<ViewerTransportIntent> {
        match &mut mode.transport {
            TransportRoute::PageLed(outbox) => outbox.take(),
            TransportRoute::LocalPlayer => Vec::new(),
        }
    }

    /// Факты берёт только окно, которое ведёт страница; повтор тех же фактов — не изменение.
    #[test]
    fn only_the_page_led_window_keeps_the_page_quality_facts() {
        let mut standard = WindowMode::standard();
        assert!(!standard.note_page_quality(facts(PageViewOffer::Available)));
        assert_eq!(standard.page_quality, PageQualityFacts::default());

        let mut browser = WindowMode::browser_session();
        assert!(browser.note_page_quality(facts(PageViewOffer::Available)));
        assert!(!browser.note_page_quality(facts(PageViewOffer::Available)));
        assert_eq!(browser.page_quality.page_view, PageViewOffer::Available);
        assert!(browser.note_page_quality(facts(PageViewOffer::PageAway)));
    }

    /// Просьба идёт в ящик только когда страница доступна; иначе — типизированный отказ и
    /// ящик пуст.
    #[test]
    fn the_request_is_queued_only_when_the_page_view_is_available() {
        let mut browser = WindowMode::browser_session();
        assert_eq!(
            browser.request_page_view(),
            PageViewRequestOutcome::NotAvailable(PageViewOffer::NotAttached),
            "before any fact the page is not attached"
        );
        for refused in [
            PageViewOffer::PageAway,
            PageViewOffer::ExtensionTooOld,
            PageViewOffer::NotAttached,
        ] {
            browser.note_page_quality(facts(refused));
            assert_eq!(
                browser.request_page_view(),
                PageViewRequestOutcome::NotAvailable(refused)
            );
        }
        assert_eq!(drained_intents(&mut browser), vec![]);

        browser.note_page_quality(facts(PageViewOffer::Available));
        assert_eq!(browser.request_page_view(), PageViewRequestOutcome::Queued);
        assert_eq!(
            drained_intents(&mut browser),
            vec![ViewerTransportIntent::ShowPage]
        );
    }

    /// Действие кнопки качества: в браузерном окне доходит до ящика намерением `ShowPage`,
    /// пока страницу можно просить; в обычном окне и при недоступной странице — ничего.
    #[test]
    fn the_quality_action_reaches_the_outbox_only_in_the_browser_window() {
        let mut standard = WindowMode::standard();
        standard.apply_quality_control_action(QualityControlAction::ShowPage);
        assert_eq!(drained_intents(&mut standard), vec![]);

        let mut browser = WindowMode::browser_session();
        browser.apply_quality_control_action(QualityControlAction::ShowPage);
        assert_eq!(
            drained_intents(&mut browser),
            vec![],
            "the page is not attached yet"
        );
        browser.note_page_quality(facts(PageViewOffer::Available));
        browser.apply_quality_control_action(QualityControlAction::ShowPage);
        assert_eq!(
            drained_intents(&mut browser),
            vec![ViewerTransportIntent::ShowPage]
        );
    }

    /// Обычное окно: просить некого, ящика нет.
    #[test]
    fn the_standard_window_has_nobody_to_ask() {
        let mut standard = WindowMode::standard();
        assert_eq!(
            standard.request_page_view(),
            PageViewRequestOutcome::NotPageLed
        );
        assert_eq!(drained_intents(&mut standard), vec![]);
    }
}
