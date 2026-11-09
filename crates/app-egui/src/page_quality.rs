//! Факты о качестве страницы для окна браузерного режима (сессия 13c, решения 267, 277, 278).
//!
//! Нейтральные типы между медиа-частью сеанса (`browser_session::session_media::page_quality`)
//! и окном (`state::window_mode`, меню кнопки качества): ни моста, ни протокола, ни egui.
//! Медиа-часть каждый проход собирает [`PageQualityFacts`], окно хранит последнее слово и
//! рисует меню; разрешение и кодек окно берёт у плеера само, здесь — только то, что знает
//! один сеанс: битрейт потока и можно ли попросить страницу показаться.

use pushed_media_source::MediaBitrate;

/// Можно ли предложить зрителю «Change quality on the page» (просьба `show_page`, 1.6).
///
/// Четыре причины разведены, а не слиты в `bool`: окно по-разному говорит про «устарел
/// экстеншен» (пункт виден, но выключен, с подсказкой) и про «страницы нет» (пункта нет).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PageViewOffer {
    /// Можно: страница слушает, экстеншен знает `show_page`.
    Available,
    /// Страницы сейчас нет (досмотр после ухода вкладки): просить некого.
    PageAway,
    /// Страница слушает, но экстеншен старее 1.6 и `show_page` не знает.
    ExtensionTooOld,
    /// Страница ещё не подключилась: версия неизвестна.
    #[default]
    NotAttached,
}

/// Что сеанс знает о качестве для окна на этот проход.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PageQualityFacts {
    /// «Вес» видео около показанного места (решение 277); `None` — неизвестно (HLS,
    /// файл `<video src>`, поколение ещё открывается или не приведено к странице).
    pub(crate) bitrate: Option<MediaBitrate>,
    /// Можно ли просить страницу показаться.
    pub(crate) page_view: PageViewOffer,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_facts_know_nothing_and_offer_nothing() {
        let facts = PageQualityFacts::default();
        assert_eq!(facts.bitrate, None);
        assert_eq!(facts.page_view, PageViewOffer::NotAttached);
    }
}
