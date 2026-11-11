//! Заголовок вкладки сменился посреди сеанса (`page_title_changed`, протокол 1.9, 16a).
//!
//! Одностраничник ставит следующую серию тем же документом и меняет заголовок вкладки:
//! окно и Info должны показать новый. Медиа-часть принимает слово только от текущего
//! экземпляра (как все уведомления) и держит последний заголовок, пока ядро его не
//! заберёт (`BrowserSessionCore::step` → подпись окна, без подъёма окна).
//!
//! Журнал (инвариант 7): текста страницы нет — только длина заголовка в байтах и номер
//! смены за жизнь процесса (`revision`), по которым стенд видит, что смена дошла.

use browser_bridge_protocol::PageTitle;
use tracing::info;

use super::SessionMedia;

/// Номер смены заголовка за жизнь процесса (с 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PageTitleRevision(u64);

impl PageTitleRevision {
    /// Число для журнала.
    pub(super) const fn get(self) -> u64 {
        self.0
    }
}

/// Смены заголовка вкладки: счётчик процесса и последний невостребованный заголовок.
#[derive(Debug, Default)]
pub(super) struct PageRetitles {
    /// Сколько смен заголовка было за жизнь процесса.
    changes: u64,
    /// Последний заголовок, который ядро ещё не забрало.
    unclaimed: Option<PageTitle>,
}

impl PageRetitles {
    /// Страница назвала новый заголовок: он вытесняет невостребованный прежний.
    pub(super) fn note_title_change(&mut self, title: PageTitle) -> PageTitleRevision {
        self.changes = self.changes.saturating_add(1);
        self.unclaimed = Some(title);
        PageTitleRevision(self.changes)
    }

    /// Забирает последний заголовок (`None` — с прошлого раза не менялся).
    pub(super) fn take_unclaimed(&mut self) -> Option<PageTitle> {
        self.unclaimed.take()
    }

    /// Новый сеанс: невостребованный заголовок прежней страницы к новой не относится.
    /// Счётчик смен не сбрасывается — он считает за жизнь процесса.
    pub(super) fn forget_unclaimed(&mut self) {
        self.unclaimed = None;
    }
}

impl SessionMedia {
    /// Слово страницы `page_title_changed` текущего экземпляра.
    pub(super) fn note_page_title_change(&mut self, title: PageTitle) {
        let title_bytes = title.as_str().len();
        let revision = self.page_retitles.note_title_change(title).get();
        // Формат — договор со стендом моста: только числа.
        info!("Browser page title changed title_bytes={title_bytes} revision={revision}");
    }

    /// Новый заголовок вкладки с прошлого вызова (`None` — не менялся). Забирает.
    pub(crate) fn take_page_retitle(&mut self) -> Option<PageTitle> {
        self.page_retitles.take_unclaimed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(text: &str) -> PageTitle {
        PageTitle::new(text).expect("valid title")
    }

    #[test]
    fn nothing_is_taken_before_the_page_retitles() {
        let mut retitles = PageRetitles::default();
        assert_eq!(retitles.take_unclaimed(), None);
    }

    #[test]
    fn the_latest_title_wins_and_every_change_counts() {
        let mut retitles = PageRetitles::default();
        assert_eq!(
            retitles.note_title_change(title("Серія 1")),
            PageTitleRevision(1)
        );
        assert_eq!(
            retitles.note_title_change(title("Серія 2")),
            PageTitleRevision(2)
        );
        assert_eq!(retitles.take_unclaimed(), Some(title("Серія 2")));
        // Забранный заголовок второй раз не выдаётся.
        assert_eq!(retitles.take_unclaimed(), None);
    }

    #[test]
    fn a_new_session_forgets_the_unclaimed_title_but_keeps_counting() {
        let mut retitles = PageRetitles::default();
        retitles.note_title_change(title("Old tab"));
        retitles.forget_unclaimed();
        assert_eq!(retitles.take_unclaimed(), None);
        // Номер смены — за жизнь процесса, а не сеанса.
        assert_eq!(retitles.note_title_change(title("")), PageTitleRevision(2));
        assert_eq!(retitles.take_unclaimed(), Some(title("")));
    }
}
