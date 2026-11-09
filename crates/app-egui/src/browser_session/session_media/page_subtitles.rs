//! Субтитры страницы в медиа-части: готовые документы ложатся на склад сеанса, окно читает
//! снимок склада каждый проход (`CoreStep::page_subtitles`).
//!
//! Склад принадлежит медиа-части потому, что она знает, какой сеанс текущий: уведомление
//! старого экземпляра сюда не доходит (`apply_notice` проверяет `is_current`), а новый сеанс
//! начинает новый склад (`on_session_opened`). Поколения склад не трогают: документ, пришедший
//! во время рекламы или смены качества, ждёт на складе и достаётся каждому следующему медиа.
//!
//! **Обмен выбором для памяти экстеншена (протокол 1.7, 14a):**
//! - подсказка страницы `subtitle_preference` ложится на тот же склад (одна жизнь — сеанс) и
//!   едет к владельцу субтитров в том же снимке;
//! - выбор зрителя окно отдаёт намерением, а сюда он приходит уже как «дорожка страницы или
//!   выключено» и уходит странице `subtitle_choice` (только с экстеншеном 1.7+).

use browser_bridge_protocol::session::{PageSubtitleTrackId, SubtitleChoice};
use tracing::{debug, info, warn};

use super::SessionMedia;
use crate::browser_session::outbound::SubtitleChoiceSendOutcome;
use crate::browser_session::page_subtitles::shelf::{PreferenceStore, ShelfStore};
use crate::page_subtitles::{
    PAGE_SUBTITLE_DOCUMENT_LINE, PageSubtitleDocument, PageSubtitlePreference,
    PageSubtitleShelfView, PageSubtitleTrackNumber, PageSubtitleViewerChoice,
};

/// Строка журнала: страница подсказала выбор субтитров (дальше `preference=…`, `track=…`).
pub(crate) const PAGE_SUBTITLE_PREFERENCE_LINE: &str = "Page subtitle preference received";
/// Строка журнала: выбор зрителя ушёл странице (дальше `choice=…`, `track=…`).
pub(crate) const SUBTITLE_CHOICE_REPORTED_LINE: &str =
    "Viewer subtitle choice reported to the page";

/// Что вышло с выбором зрителя для памяти экстеншена.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubtitleChoiceReportOutcome {
    /// `subtitle_choice` в очереди к странице.
    Queued,
    /// Страницы нет (досмотр) или сеанса нет: запоминать некому.
    PageAway,
    /// Страница ещё не подключилась (версия неизвестна).
    NotAttached,
    /// Экстеншен старше 1.7: сообщения он не знает.
    ProtocolTooOld,
    /// Номер дорожки не годится протоколу (ноль): так быть не должно — номера дала страница.
    InvalidTrackNumber,
}

impl SessionMedia {
    /// Готовый документ текущего сеанса → на склад (тот же номер дорожки заменяет прежний).
    pub(super) fn shelve_page_subtitle(&mut self, document: PageSubtitleDocument) {
        let track = document.track.get();
        let bytes = document.bytes.len();
        let format_hint = document.format_hint.log_label();
        let page_display = document.display.log_label();
        let replaced = self.page_subtitles.store(document) == ShelfStore::Replaced;
        // Только числа и метки: байты и подпись — содержимое страницы (инвариант 7).
        info!(
            track,
            bytes, format_hint, page_display, replaced, "{PAGE_SUBTITLE_DOCUMENT_LINE}"
        );
    }

    /// Снимок склада текущего сеанса для окна (дешёвая копия `Arc`).
    pub(crate) fn page_subtitle_shelf(&self) -> PageSubtitleShelfView {
        self.page_subtitles.view()
    }

    /// Подсказка страницы `subtitle_preference` → на склад сеанса (последнее слово главнее).
    /// Слово ушедшей страницы сюда не доходит: его отсекает вызывающий (досмотр).
    pub(super) fn note_page_subtitle_preference(&mut self, choice: SubtitleChoice) {
        let preference = preference_of(choice);
        if self.page_subtitles.note_page_preference(preference) == PreferenceStore::Changed {
            info!(
                preference = preference.log_label(),
                track = track_of_preference(preference),
                "{PAGE_SUBTITLE_PREFERENCE_LINE}"
            );
        }
    }

    /// Зритель сам выбрал субтитры (меню CC, клавиши C/V): странице — `subtitle_choice`.
    pub(super) fn viewer_reports_subtitle_choice(
        &mut self,
        choice: PageSubtitleViewerChoice,
    ) -> SubtitleChoiceReportOutcome {
        if !self.page_listens() {
            debug!("Viewer subtitle choice not reported: the page is gone");
            return SubtitleChoiceReportOutcome::PageAway;
        }
        let Some(protocol_choice) = protocol_choice_of(choice) else {
            warn!("Viewer subtitle choice not reported: a page track number of zero");
            return SubtitleChoiceReportOutcome::InvalidTrackNumber;
        };
        match self.outbound.request_subtitle_choice(protocol_choice) {
            SubtitleChoiceSendOutcome::Queued => {}
            SubtitleChoiceSendOutcome::NotAttachedYet => {
                debug!("Viewer subtitle choice not reported: the page has not attached yet");
                return SubtitleChoiceReportOutcome::NotAttached;
            }
            SubtitleChoiceSendOutcome::ProtocolTooOld => {
                debug!("Viewer subtitle choice not reported: the extension is older than 1.7");
                return SubtitleChoiceReportOutcome::ProtocolTooOld;
            }
        }
        info!(
            choice = choice.log_label(),
            track = track_of_viewer_choice(choice),
            "{SUBTITLE_CHOICE_REPORTED_LINE}"
        );
        SubtitleChoiceReportOutcome::Queued
    }
}

/// Протокольная подсказка → нейтральная для склада и владельца субтитров.
fn preference_of(choice: SubtitleChoice) -> PageSubtitlePreference {
    match choice {
        SubtitleChoice::PageTrack { track } => {
            PageSubtitlePreference::PageTrack(PageSubtitleTrackNumber::new(track.get()))
        }
        SubtitleChoice::Off {} => PageSubtitlePreference::Off,
    }
}

/// Нейтральный выбор зрителя → протокольный (`None` — номер ноль, протокол его не пустит).
fn protocol_choice_of(choice: PageSubtitleViewerChoice) -> Option<SubtitleChoice> {
    match choice {
        PageSubtitleViewerChoice::PageTrack(number) => PageSubtitleTrackId::new(number.get())
            .ok()
            .map(|track| SubtitleChoice::PageTrack { track }),
        PageSubtitleViewerChoice::Off => Some(SubtitleChoice::Off {}),
    }
}

/// Номер дорожки для журнала (0 — дорожки нет).
const fn track_of_preference(preference: PageSubtitlePreference) -> u32 {
    match preference {
        PageSubtitlePreference::PageTrack(number) => number.get(),
        PageSubtitlePreference::NotGiven | PageSubtitlePreference::Off => 0,
    }
}

/// Номер дорожки для журнала (0 — дорожки нет).
const fn track_of_viewer_choice(choice: PageSubtitleViewerChoice) -> u32 {
    match choice {
        PageSubtitleViewerChoice::PageTrack(number) => number.get(),
        PageSubtitleViewerChoice::Off => 0,
    }
}
