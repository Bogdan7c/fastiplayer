//! Варианты страницы в медиа-части: склад меню, выбор зрителя, следующая серия (14a).
//!
//! ```text
//!  страница: variant_menu_changed / page_navigation ─► склад (`PageVariantShelf`)
//!                                                       │
//!  окно: каждый проход ◄── PageVariantFacts (CoreStep) ─┘
//!  окно: выбор зрителя ─► ViewerTransportIntent ─► viewer_selects_page_variant
//!        ├─ реклама на экране → отказ (решения 206, 283), как перемотка
//!        ├─ склад: последнее меню этого вида и та же ревизия → `variant_id`
//!        ├─ странице `select_variant` (ящик «один на вид», `PageOutbound`)
//!        ├─ возврат на секунду зрителя (`position_carry`, решение 280) — только озвучка и
//!        │  качество: выбор субтитров поколения не меняет (15a2)
//!        └─ окну тост «Audio: …» / «Quality: …» (одно слово за шаг; у субтитров тоста нет)
//!  окно: Next ─► viewer_asks_next_episode ─► `next_episode` (только после `available`, 1.7)
//! ```
//!
//! **Журнал** — только вид меню и номер пункта, без подписей и адреса страницы
//! (инвариант 7): `Page variant selected kind=audio index=1`, `Next episode requested`.

use std::time::Instant;

use tracing::{debug, info, warn};

use super::SessionMedia;
use super::position_carry::{CarryEnd, PositionCarry, PositionCarryPolicy};
use crate::browser_session::outbound::{
    NextEpisodePermission, NextEpisodeSendOutcome, SelectVariantSendOutcome,
};
use crate::browser_session::page_variants::{
    MenuStore, PageVariantShelf, PageVariantUpdate, VariantLookupRefusal, page_variant_kind,
};
use crate::page_variants::{
    NextEpisodeOffer, PageVariantChoice, PageVariantFacts, PageVariantKind, PageVariantNotice,
};
use browser_bridge_protocol::session::NextEpisodeAvailability;

/// Выбор зрителя ушёл странице (строку читают стенд и тесты; дальше `kind=… index=…`).
pub(crate) const PAGE_VARIANT_SELECTED_LINE: &str = "Page variant selected";
/// Просьба о следующей серии ушла странице.
pub(crate) const NEXT_EPISODE_REQUESTED_LINE: &str = "Next episode requested";
/// Выбор варианта отклонён: на экране реклама (решения 206, 283).
pub(crate) const PAGE_VARIANT_REFUSED_DURING_AD_LINE: &str =
    "Page variant selection refused: an ad is on screen";

/// Что вышло с выбором зрителя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageVariantRequestOutcome {
    /// `select_variant` в очереди к странице.
    Queued,
    /// На экране реклама: выбор заблокирован, как перемотка.
    RefusedDuringAd,
    /// Страницы нет (досмотр).
    PageAway,
    /// Страница ещё не подключилась.
    NotAttached,
    /// Меню этого вида нет.
    NoMenu,
    /// Меню сменилось после того, как зритель его видел.
    StaleMenu,
    /// Такого пункта нет.
    NoSuchOption,
}

/// Что вышло с просьбой о следующей серии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NextEpisodeRequestOutcome {
    /// `next_episode` в очереди к странице.
    Queued,
    /// На экране реклама: «Next» пропускает рекламу, а не серию (решение 283).
    RefusedDuringAd,
    /// Не предложена (причина — в предложении).
    NotOffered(NextEpisodeOffer),
}

/// Варианты страницы текущего сеанса: склад, возврат на секунду и тост для окна.
pub(super) struct SessionPageVariants {
    /// Последнее меню каждого вида и навигация страницы.
    pub(super) shelf: PageVariantShelf,
    /// Возврат на секунду после выбора (решение 280).
    pub(super) carry: PositionCarry,
    /// Что сказать зрителю тостом (одно слово за шаг).
    notice: Option<PageVariantNotice>,
}

impl SessionPageVariants {
    /// Пусто; срок возврата — по умолчанию (процесс задаёт свой из config-а).
    pub(super) fn new() -> Self {
        Self {
            shelf: PageVariantShelf::default(),
            carry: PositionCarry::new(PositionCarryPolicy::recommended()),
            notice: None,
        }
    }

    /// Новый сеанс: меню, навигация, возврат и тост прежней страницы забыты.
    pub(super) fn start_new_session(&mut self) {
        self.shelf.start_new_session();
        self.carry.forget(CarryEnd::NewSession);
        self.notice = None;
    }
}

impl SessionMedia {
    /// Слово страницы о вариантах → на склад. Страницы уже нет (досмотр) — слово опоздало.
    pub(super) fn apply_page_variant_update(&mut self, update: PageVariantUpdate) {
        if self.is_draining() {
            debug!("Page variant word of a page that has already left dropped");
            return;
        }
        match update {
            PageVariantUpdate::Menu(menu) => {
                let kind = menu.kind;
                let options = menu.options.len();
                match self.variants.shelf.store_menu(menu) {
                    // Только вид и число пунктов: подписи — текст страницы.
                    MenuStore::Stored(revision) => info!(
                        options,
                        revision = revision.get(),
                        "Page variant menu kind={}",
                        page_variant_kind(kind).log_label()
                    ),
                    outcome @ (MenuStore::Removed(_) | MenuStore::Unchanged) => {
                        debug!(?kind, ?outcome, "Page variant menu");
                    }
                }
            }
            PageVariantUpdate::Navigation(availability) => {
                if self.variants.shelf.note_navigation(availability) {
                    info!(
                        next_episode = availability == NextEpisodeAvailability::Available,
                        "Page navigation changed"
                    );
                }
            }
            // Подсказка субтитров — на склад субтитров сеанса (`page_subtitles.rs`).
            PageVariantUpdate::SubtitlePreference(choice) => {
                self.note_page_subtitle_preference(choice);
            }
        }
    }

    /// Факты о вариантах для окна на этот проход (чистое чтение). Страницы нет — меню нет:
    /// выбирать некому.
    pub(crate) fn page_variant_facts(&self) -> PageVariantFacts {
        if !self.page_listens() {
            return PageVariantFacts {
                next_episode: NextEpisodeOffer::PageAway,
                ..PageVariantFacts::default()
            };
        }
        PageVariantFacts {
            audio: self.variants.shelf.menu_view(PageVariantKind::Audio),
            quality: self.variants.shelf.menu_view(PageVariantKind::Quality),
            subtitles: self.variants.shelf.menu_view(PageVariantKind::Subtitles),
            next_episode: self.next_episode_offer(),
        }
    }

    /// Слово окну за шаг (забирает его): тост показывается один раз.
    pub(crate) fn take_page_variant_notice(&mut self) -> Option<PageVariantNotice> {
        self.variants.notice.take()
    }

    /// Зритель выбрал пункт меню страницы (правила — в описании модуля).
    pub(super) fn viewer_selects_page_variant(
        &mut self,
        choice: PageVariantChoice,
        now: Instant,
    ) -> PageVariantRequestOutcome {
        if self.ad.locks_transport(now) {
            info!("{PAGE_VARIANT_REFUSED_DURING_AD_LINE}");
            return PageVariantRequestOutcome::RefusedDuringAd;
        }
        if !self.page_listens() {
            debug!("Page variant selection ignored: the page is gone");
            return PageVariantRequestOutcome::PageAway;
        }
        let found = match self.variants.shelf.lookup(choice) {
            Ok(found) => found,
            Err(refusal) => {
                debug!(?refusal, "Page variant selection not sent to the page");
                return match refusal {
                    VariantLookupRefusal::NoMenu => PageVariantRequestOutcome::NoMenu,
                    VariantLookupRefusal::StaleMenu => PageVariantRequestOutcome::StaleMenu,
                    VariantLookupRefusal::NoSuchOption => PageVariantRequestOutcome::NoSuchOption,
                };
            }
        };
        match self
            .outbound
            .request_select_variant(found.kind, found.variant_id)
        {
            SelectVariantSendOutcome::Queued => {}
            SelectVariantSendOutcome::NotAttachedYet => {
                debug!("Page variant selection not sent: the page has not attached yet");
                return PageVariantRequestOutcome::NotAttached;
            }
        }
        info!(
            "{PAGE_VARIANT_SELECTED_LINE} kind={} index={}",
            choice.kind.log_label(),
            choice.index.get()
        );
        // Субтитры поколения не меняют (страница лишь скачивает ещё один документ): ни
        // возврата секунды, ни тоста о смене варианта, иначе взведённый возврат дождался бы
        // чужого поколения (например, после рекламы) и сдвинул бы видео.
        if choice.kind.selection_restarts_media() {
            // Решение 280: новое поколение после смены встанет на секунду зрителя.
            self.arm_position_carry(now);
            self.variants.notice = Some(PageVariantNotice::Selected {
                kind: choice.kind,
                label: found.label,
            });
        }
        PageVariantRequestOutcome::Queued
    }

    /// Зритель просит следующую серию (Next, N, MPRIS Next вне рекламы).
    pub(super) fn viewer_asks_next_episode(&mut self, now: Instant) -> NextEpisodeRequestOutcome {
        // Последний рубеж: окно отдаёт «Next» рекламе раньше (решение 283).
        if self.ad.plate(now).is_some() {
            info!("Next episode refused: an ad is on screen");
            return NextEpisodeRequestOutcome::RefusedDuringAd;
        }
        let offer = self.next_episode_offer();
        if offer != NextEpisodeOffer::Offered {
            debug!(?offer, "Next episode not sent to the page");
            return NextEpisodeRequestOutcome::NotOffered(offer);
        }
        match self.outbound.request_next_episode() {
            NextEpisodeSendOutcome::Queued => {}
            NextEpisodeSendOutcome::NotAttachedYet => {
                return NextEpisodeRequestOutcome::NotOffered(NextEpisodeOffer::NotAttached);
            }
            NextEpisodeSendOutcome::ProtocolTooOld => {
                warn!("Next episode not sent: the extension is older than protocol 1.7");
                return NextEpisodeRequestOutcome::NotOffered(NextEpisodeOffer::ExtensionTooOld);
            }
        }
        info!("{NEXT_EPISODE_REQUESTED_LINE}");
        // Следующая серия начинается сначала: позицию прежней не переносим.
        self.forget_position_carry(CarryEnd::NextEpisode);
        self.variants.notice = Some(PageVariantNotice::NextEpisodeRequested);
        NextEpisodeRequestOutcome::Queued
    }

    /// Можно ли сейчас предложить следующую серию.
    fn next_episode_offer(&self) -> NextEpisodeOffer {
        if !self.page_listens() {
            return NextEpisodeOffer::PageAway;
        }
        match self.outbound.next_episode_permission() {
            NextEpisodePermission::NotAttachedYet => NextEpisodeOffer::NotAttached,
            NextEpisodePermission::ProtocolTooOld => NextEpisodeOffer::ExtensionTooOld,
            NextEpisodePermission::Allowed => match self.variants.shelf.next_episode() {
                NextEpisodeAvailability::Available => NextEpisodeOffer::Offered,
                NextEpisodeAvailability::Unavailable => NextEpisodeOffer::PageSaysUnavailable,
            },
        }
    }

    /// Страница слушает: сеанс есть и досмотра нет.
    pub(super) fn page_listens(&self) -> bool {
        self.drain.is_none() && self.current_session.is_some()
    }
}
