//! Субтитры страницы в медиа-части: готовые документы ложатся на склад сеанса, окно читает
//! снимок склада каждый проход (`CoreStep::page_subtitles`).
//!
//! Склад принадлежит медиа-части потому, что она знает, какой сеанс текущий: уведомление
//! старого экземпляра сюда не доходит (`apply_notice` проверяет `is_current`), а новый сеанс
//! начинает новый склад (`on_session_opened`). Поколения склад не трогают: документ, пришедший
//! во время рекламы или смены качества, ждёт на складе и достаётся каждому следующему медиа.

use tracing::info;

use super::SessionMedia;
use crate::browser_session::page_subtitles::shelf::ShelfStore;
use crate::page_subtitles::{
    PAGE_SUBTITLE_DOCUMENT_LINE, PageSubtitleDocument, PageSubtitleShelfView,
};

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
}
