//! Надпись «Waiting for the next episode…» в центре окна (протокол 1.9, сессия 16a).
//!
//! Когда ждать, решает медиа-часть (`session_media::next_episode_wait`); здесь — только
//! что окно пишет и одна строка журнала на каждую смену надписи (без данных страницы — её
//! читают сквозные тесты и стенд моста):
//! - `Browser window tells the viewer: waiting for the next episode`;
//! - `Browser window tells the viewer: the waiting notice is cleared reason=<…>`.

use tracing::info;

use crate::browser_session::session_media::{NextEpisodeWaitEnd, NextEpisodeWaitView};

/// Текст надписи. Пользовательский текст — английский.
const WAITING_FOR_THE_NEXT_EPISODE: &str = "Waiting for the next episode…";

/// Какое ожидание окно уже показало (журнал пишется только при смене).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ShownNextEpisodeWait {
    shown: NextEpisodeWaitView,
}

impl ShownNextEpisodeWait {
    /// Ожидание за проход → текст надписи, который сейчас нужен окну (`None` — не нужен).
    /// Смена ожидания пишется в журнал одной строкой.
    pub(super) fn update(&mut self, view: NextEpisodeWaitView) -> Option<&'static str> {
        if view != self.shown {
            log_change(self.shown, view);
            self.shown = view;
        }
        center_message(view)
    }
}

/// Текст в центре окна для этого ожидания.
const fn center_message(view: NextEpisodeWaitView) -> Option<&'static str> {
    match view {
        NextEpisodeWaitView::Waiting => Some(WAITING_FOR_THE_NEXT_EPISODE),
        NextEpisodeWaitView::NeverWaited | NextEpisodeWaitView::Ended(_) => None,
    }
}

/// Строка журнала о смене надписи (формат — договор со стендом моста).
fn log_change(previous: NextEpisodeWaitView, next: NextEpisodeWaitView) {
    match (previous, next) {
        (_, NextEpisodeWaitView::Waiting) => {
            info!("Browser window tells the viewer: waiting for the next episode");
        }
        (NextEpisodeWaitView::Waiting, NextEpisodeWaitView::Ended(end)) => log_cleared(end),
        // Надписи не было и нет (например, окно открылось уже после снятия): сказать нечего.
        (_, NextEpisodeWaitView::NeverWaited | NextEpisodeWaitView::Ended(_)) => {}
    }
}

/// Строка журнала «надпись снята» с причиной (метка — договор со стендом моста).
fn log_cleared(end: NextEpisodeWaitEnd) {
    let reason = end.log_label();
    info!("Browser window tells the viewer: the waiting notice is cleared reason={reason}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_is_shown_only_while_the_page_awaits_the_next_episode() {
        let mut shown = ShownNextEpisodeWait::default();
        assert_eq!(shown.update(NextEpisodeWaitView::NeverWaited), None);
        assert_eq!(
            shown.update(NextEpisodeWaitView::Waiting),
            Some("Waiting for the next episode…")
        );
        // Повтор того же ожидания — та же надпись (окно её не перерисовывает: это решает
        // владелец центра по тексту).
        assert_eq!(
            shown.update(NextEpisodeWaitView::Waiting),
            Some("Waiting for the next episode…")
        );
        for end in [
            NextEpisodeWaitEnd::GenerationShown,
            NextEpisodeWaitEnd::SessionEnded,
        ] {
            assert_eq!(shown.update(NextEpisodeWaitView::Ended(end)), None);
            assert!(shown.update(NextEpisodeWaitView::Waiting).is_some());
        }
    }
}
