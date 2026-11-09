//! Варианты страницы в режиме окна: факты сеанса, выбор озвучки или качества, следующая
//! серия (сессия 14a, решения 279–283).
//!
//! Факты ([`PageVariantFacts`]) приносит медиа-часть сеанса каждый проход окна, как факты
//! качества 13c: обычное окно их не берёт, вариантов страницы у него нет. Выбор зрителя идёт
//! тем же путём, что кнопки транспорта: намерением в ящик ([`ViewerTransportIntent`]), а не
//! прямой командой. Окно проверяет выбор по своему последнему слову (тот же вид, та же
//! ревизия, номер в меню); последний рубеж — склад медиа-части, который видит самое свежее
//! меню.
//!
//! **Правила окна (решение 283):**
//! - пока на экране реклама, выбор озвучки и качества заблокирован, как перемотка (206);
//! - «следующая серия» предлагается, только если страница сказала `available` и на экране
//!   нет рекламы: реклама главнее (Next/N/MPRIS Next жмут плашку, 09d).
//!
//! Виджеты (меню ♪, кнопка качества, кнопка Next) читают [`AppState::page_variant_facts`],
//! [`AppState::page_variant_availability`] и [`AppState::next_episode_offered`] и зовут
//! [`AppState::select_page_variant`].

use std::time::Instant;

use tracing::{debug, info};

use super::{AdOfferActivation, TransportRoute, ViewerTransportIntent, WindowMode};
use crate::page_variants::{
    PageVariantAvailability, PageVariantChoice, PageVariantFacts, PageVariantKind,
    PageVariantNotice,
};
use crate::state::AppState;
use crate::state::notifications::TransientReplaceKey;

/// Выбор отклонён окном: на экране реклама (тот же смысл, что у отказа перемотки).
pub(crate) const PAGE_VARIANT_REFUSED_IN_WINDOW_LINE: &str =
    "Viewer page variant selection refused: an ad is on screen";

/// Что вышло с выбором зрителя в окне.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a refused selection must be told to the viewer or logged"]
pub(crate) enum PageVariantSelectOutcome {
    /// Намерение ушло в ящик медиа-части сеанса.
    Queued,
    /// На экране реклама: выбор заблокирован (решения 206, 283).
    RefusedDuringAd,
    /// Меню этого вида окно не знает.
    NoMenu,
    /// Выбор сделан по меню, которое с тех пор сменилось.
    StaleMenu,
    /// Такого пункта в меню нет.
    NoSuchOption,
    /// Обычное окно: страницы нет.
    NotPageLed,
}

/// Что вышло с «следующей серией» в окне.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a refused request falls back to the old Next behaviour"]
pub(crate) enum NextEpisodeWindowOutcome {
    /// Намерение ушло в ящик медиа-части сеанса.
    Queued,
    /// Не предложена: страница не сказала `available`, экстеншен старый, или реклама.
    NotOffered,
    /// Обычное окно: страницы нет.
    NotPageLed,
}

/// Что сделало «следующее» зрителя (кнопка Next, клавиша N, MPRIS Next) в окне страницы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NextActivation {
    /// Нажата кнопка плашки рекламы (реклама главнее серии, 09d).
    AdOfferRequested,
    /// Попрошена следующая серия страницы.
    NextEpisodeRequested,
    /// Делать нечего: ни кнопки плашки, ни следующей серии.
    NothingToDo,
}

impl WindowMode {
    /// «Следующее» зрителя: кнопка плашки рекламы, если она предложена и свободна; иначе
    /// следующая серия, если она предложена (при плашке рекламы — никогда).
    pub(super) fn activate_next(&mut self) -> NextActivation {
        if self.activate_ad_offer() == AdOfferActivation::Requested {
            return NextActivation::AdOfferRequested;
        }
        match self.request_next_episode() {
            NextEpisodeWindowOutcome::Queued => NextActivation::NextEpisodeRequested,
            NextEpisodeWindowOutcome::NotOffered | NextEpisodeWindowOutcome::NotPageLed => {
                NextActivation::NothingToDo
            }
        }
    }

    /// Факты о вариантах от того, кто ведёт транспорт; обычное окно их не берёт.
    /// `true` — факты изменились, окну нужен кадр.
    pub(super) fn note_page_variants(&mut self, facts: PageVariantFacts) -> bool {
        if !self.is_page_led() || self.page_variants == facts {
            return false;
        }
        self.page_variants = facts;
        true
    }

    /// Зритель выбрал пункт меню страницы (правила — в описании модуля).
    pub(super) fn select_page_variant(
        &mut self,
        choice: PageVariantChoice,
    ) -> PageVariantSelectOutcome {
        if !self.is_page_led() {
            return PageVariantSelectOutcome::NotPageLed;
        }
        if self.page_variant_availability() == PageVariantAvailability::BlockedDuringAd {
            info!("{PAGE_VARIANT_REFUSED_IN_WINDOW_LINE}");
            return PageVariantSelectOutcome::RefusedDuringAd;
        }
        let Some(menu) = self.page_variants.menu(choice.kind) else {
            return PageVariantSelectOutcome::NoMenu;
        };
        if menu.revision() != choice.revision {
            return PageVariantSelectOutcome::StaleMenu;
        }
        if choice.index.get() >= menu.option_count() {
            return PageVariantSelectOutcome::NoSuchOption;
        }
        let TransportRoute::PageLed(outbox) = &mut self.transport else {
            return PageVariantSelectOutcome::NotPageLed;
        };
        outbox.push(ViewerTransportIntent::SelectPageVariant(choice));
        PageVariantSelectOutcome::Queued
    }

    /// Можно ли сейчас выбирать пункты меню страницы: та же проверка, что у выбора (реклама
    /// запирает транспорт — пункты неактивны, решения 206, 283).
    pub(super) fn page_variant_availability(&self) -> PageVariantAvailability {
        if self
            .ad_gate
            .plate()
            .is_some_and(|plate| plate.locks_transport)
        {
            PageVariantAvailability::BlockedDuringAd
        } else {
            PageVariantAvailability::Selectable
        }
    }

    /// Предлагать ли следующую серию (кнопка Next, N, MPRIS `CanGoNext`): страница сказала
    /// `available`, экстеншен 1.7+, и на экране нет рекламы (реклама главнее).
    pub(super) fn next_episode_offered(&self) -> bool {
        self.is_page_led()
            && self.page_variants.next_episode.is_offered()
            && self.ad_gate.plate().is_none()
    }

    /// Зритель просит следующую серию: намерение уходит, только если она предложена.
    pub(super) fn request_next_episode(&mut self) -> NextEpisodeWindowOutcome {
        if !self.next_episode_offered() {
            return if self.is_page_led() {
                NextEpisodeWindowOutcome::NotOffered
            } else {
                NextEpisodeWindowOutcome::NotPageLed
            };
        }
        let TransportRoute::PageLed(outbox) = &mut self.transport else {
            return NextEpisodeWindowOutcome::NotPageLed;
        };
        outbox.push(ViewerTransportIntent::NextEpisode);
        NextEpisodeWindowOutcome::Queued
    }
}

impl AppState {
    /// Медиа-часть сеанса принесла факты о вариантах (каждый проход окна). `true` — факты
    /// изменились, окну нужен кадр. Обычное окно их игнорирует.
    pub(crate) fn note_page_variants(&mut self, facts: PageVariantFacts) -> bool {
        self.window_mode.note_page_variants(facts)
    }

    /// Последнее слово сеанса о вариантах страницы (обычное окно — пусто): меню ♪ и кнопка
    /// качества рисуют по нему пункты.
    pub(crate) const fn page_variant_facts(&self) -> &PageVariantFacts {
        &self.window_mode.page_variants
    }

    /// Зритель выбрал пункт меню страницы (меню ♪, кнопка качества, клавиша B).
    pub(crate) fn select_page_variant(
        &mut self,
        choice: PageVariantChoice,
    ) -> PageVariantSelectOutcome {
        let outcome = self.window_mode.select_page_variant(choice);
        if outcome != PageVariantSelectOutcome::Queued {
            debug!(?outcome, "Page variant selection not queued");
        }
        outcome
    }

    /// Можно ли сейчас выбирать пункты меню страницы (меню ♪ и кнопка качества рисуют их
    /// неактивными во время рекламы).
    pub(crate) fn page_variant_availability(&self) -> PageVariantAvailability {
        self.window_mode.page_variant_availability()
    }

    /// Предлагать ли следующую серию (кнопка Next видна, MPRIS `CanGoNext`).
    pub(crate) fn next_episode_offered(&self) -> bool {
        self.window_mode.next_episode_offered()
    }

    /// Тост после принятого сеансом действия: «Audio: …» заменяет прежний тост дорожки
    /// звука (клавиша B ходит по озвучкам), качество и следующая серия — обычный тост.
    pub(crate) fn announce_page_variant_notice(&mut self, notice: &PageVariantNotice) {
        let text = notice.toast_text();
        match notice {
            PageVariantNotice::Selected {
                kind: PageVariantKind::Audio,
                ..
            } => {
                self.notifications.notify_transient_replacing(
                    TransientReplaceKey::AudioTrack,
                    text,
                    Instant::now(),
                );
            }
            PageVariantNotice::Selected {
                kind: PageVariantKind::Quality,
                ..
            }
            | PageVariantNotice::NextEpisodeRequested => self.notify_transient(text),
        }
    }
}

#[cfg(test)]
#[path = "page_variants_tests.rs"]
mod tests;
