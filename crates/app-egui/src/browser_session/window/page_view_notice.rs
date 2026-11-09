//! Тост «страница открыта в браузере — переключитесь» после просьбы «Change quality on the
//! page» (сессия 13c, решение 278).
//!
//! Экстеншен делает вкладку сеанса активной и просит окно браузера выйти вперёд, но на KDE
//! Wayland окно браузера поднять не получается (решение 208, сессия 17) — поэтому зрителю
//! нужно прямо сказать, куда смотреть.

use tracing::info;

use super::instance::BrowserWindowInstance;
use crate::browser_session::session_media::PageViewNotice;

/// Текст тоста; `None` — говорить нечего.
pub(super) const fn page_view_notice_message(notice: PageViewNotice) -> Option<&'static str> {
    match notice {
        PageViewNotice::Nothing => None,
        PageViewNotice::Requested => Some("The page is open in your browser — switch to it"),
    }
}

/// Показывает тост (если за шаг просьба была принята) и пишет одну строку журнала — её
/// читают сквозные тесты.
pub(super) fn show_page_view_notice(instance: &mut BrowserWindowInstance, notice: PageViewNotice) {
    let Some(message) = page_view_notice_message(notice) else {
        return;
    };
    info!("Browser window tells the viewer: the page is open in the browser");
    instance.notify_viewer(message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_accepted_request_gives_a_toast() {
        assert_eq!(page_view_notice_message(PageViewNotice::Nothing), None);
        assert_eq!(
            page_view_notice_message(PageViewNotice::Requested),
            Some("The page is open in your browser — switch to it")
        );
    }
}
