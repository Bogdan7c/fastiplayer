//! Озвучки страницы в меню «♪» окна браузерного режима (сессия 14a, решение 283).
//!
//! Адаптер сайта сообщает меню озвучек плеера страницы (`variant_menu_changed{audio}`), сеанс
//! приносит его окну фактами ([`PageVariantMenuView`]). Здесь меню страницы превращается в
//! строки «♪»: как web-версии звука (`mem:app-egui/web-audio-versions-menu`), озвучки страницы
//! **заменяют** встроенные дорожки потока — одна отметка «играет» (пункт, выбранный на
//! странице), круг клавиши `B` идёт по озвучкам.
//!
//! Строка несёт [`PageVariantChoice`] — вид меню, его ревизию и номер пункта, а не подпись:
//! выбор по меню, которое сменилось после показа, окно и сеанс отвергнут как устаревший.

use super::menu_view::WebAudioMenuSource;
use crate::page_variants::{PageVariantAvailability, PageVariantChoice, PageVariantMenuView};

/// Одна озвучка страницы для меню «♪».
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PageDubEntry {
    /// Что отдать окну при выборе.
    pub(super) choice: PageVariantChoice,
    /// Подпись страницы (уже очищена сеансом; обрезает интерфейс).
    pub(super) label: String,
    /// Этот пункт выбран на странице (отметка «играет»).
    pub(super) is_active: bool,
}

/// Озвучки страницы и можно ли их сейчас выбирать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PageDubRows {
    /// Пункты в порядке страницы.
    pub(super) entries: Vec<PageDubEntry>,
    /// Во время рекламы выбор заблокирован (пункты неактивны).
    pub(super) availability: PageVariantAvailability,
}

/// Строки «♪» из меню озвучек страницы.
pub(super) fn page_dub_menu_source(
    menu: &PageVariantMenuView,
    availability: PageVariantAvailability,
) -> WebAudioMenuSource {
    let entries = menu
        .options()
        .filter_map(|option| {
            Some(PageDubEntry {
                choice: menu.choice(option.index)?,
                label: option.label.to_owned(),
                is_active: menu.selected() == Some(option.index),
            })
        })
        .collect();
    WebAudioMenuSource::PageDubs(PageDubRows {
        entries,
        availability,
    })
}
