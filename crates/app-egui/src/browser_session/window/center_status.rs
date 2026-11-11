//! Владелец сообщения в центре окна браузерного режима (сессия 16a).
//!
//! В центре окна одно место (`CenterNotice::Status`), а говорить туда хотят двое:
//! - сообщение о конце сеанса ([`ShownEndNotice`], 04b) — главное: после него окно
//!   закрывается, и его нельзя затереть;
//! - надпись «ждём следующую серию» ([`ShownNextEpisodeWait`], 16a) — пока сеанс идёт.
//!
//! Оба пишут свои строки журнала сами, а что ставить в центр, решает только этот модуль:
//! окно меняет текст, только когда меняется итог. Медиа-часть снимает ожидание при любом
//! конце сеанса, поэтому одновременно они не нужны; но если бы понадобились, сообщение о
//! конце всё равно победило бы.

use super::end_notice::{CenterStatusChange, ShownEndNotice};
use super::next_episode_notice::ShownNextEpisodeWait;
use crate::browser_session::session_end::SessionEndView;
use crate::browser_session::session_media::NextEpisodeWaitView;

/// Что сейчас в центре окна и кто туда говорит.
#[derive(Debug, Default)]
pub(super) struct WindowCenterStatus {
    end_notice: ShownEndNotice,
    next_episode: ShownNextEpisodeWait,
    /// Текст, который окно показывает сейчас (`None` — центр пуст).
    shown: Option<&'static str>,
}

impl WindowCenterStatus {
    /// Состояние за проход → что поменять в центре окна (`None` — ничего).
    ///
    /// Ожидание обновляется первым: при конце сеанса строка «надпись снята» идёт в журнал
    /// раньше строки о сообщении конца — в том порядке, в каком это видит зритель.
    pub(super) fn update(
        &mut self,
        end: SessionEndView,
        next_episode: NextEpisodeWaitView,
    ) -> Option<CenterStatusChange> {
        let waiting_message = self.next_episode.update(next_episode);
        if let Some(CenterStatusChange::Show(message)) = self.end_notice.update(end) {
            self.shown = Some(message);
            return Some(CenterStatusChange::Show(message));
        }
        // Пока говорит конец сеанса, ожидание центр не трогает.
        if end.center_message().is_some() {
            return None;
        }
        if waiting_message == self.shown {
            return None;
        }
        self.shown = waiting_message;
        Some(waiting_message.map_or(CenterStatusChange::Clear, CenterStatusChange::Show))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser_session::session_end::{DrainCause, DrainFinish};
    use crate::browser_session::session_media::{DrainEnd, NextEpisodeWaitEnd};

    const WAITING: &str = "Waiting for the next episode…";

    #[test]
    fn nothing_to_say_leaves_the_center_alone() {
        let mut center = WindowCenterStatus::default();
        assert_eq!(
            center.update(SessionEndView::Silent, NextEpisodeWaitView::NeverWaited),
            None
        );
    }

    #[test]
    fn the_waiting_notice_comes_and_goes_with_the_new_generation() {
        let mut center = WindowCenterStatus::default();
        assert_eq!(
            center.update(SessionEndView::Silent, NextEpisodeWaitView::Waiting),
            Some(CenterStatusChange::Show(WAITING))
        );
        // Повтор слова страницы надпись не перерисовывает.
        assert_eq!(
            center.update(SessionEndView::Silent, NextEpisodeWaitView::Waiting),
            None
        );
        assert_eq!(
            center.update(
                SessionEndView::Silent,
                NextEpisodeWaitView::Ended(NextEpisodeWaitEnd::GenerationShown)
            ),
            Some(CenterStatusChange::Clear)
        );
    }

    #[test]
    fn the_end_of_the_session_replaces_the_waiting_notice_and_is_never_wiped() {
        let mut center = WindowCenterStatus::default();
        center.update(SessionEndView::Silent, NextEpisodeWaitView::Waiting);
        let ended = NextEpisodeWaitView::Ended(NextEpisodeWaitEnd::SessionEnded);
        // Код 4011: в одном проходе ожидание снято, а конец сеанса заговорил.
        assert_eq!(
            center.update(SessionEndView::NextEpisodeNotStarted, ended),
            Some(CenterStatusChange::Show(
                "The page didn't start the next episode"
            ))
        );
        assert_eq!(
            center.update(SessionEndView::NextEpisodeNotStarted, ended),
            None
        );
        // Даже если бы ожидание вернулось, сообщение о конце центр не отдаёт.
        assert_eq!(
            center.update(
                SessionEndView::NextEpisodeNotStarted,
                NextEpisodeWaitView::Waiting
            ),
            None
        );
    }

    #[test]
    fn a_drain_that_is_still_playing_clears_the_waiting_notice() {
        let mut center = WindowCenterStatus::default();
        center.update(SessionEndView::Silent, NextEpisodeWaitView::Waiting);
        let ended = NextEpisodeWaitView::Ended(NextEpisodeWaitEnd::SessionEnded);
        // Вкладка ушла: доигрывание ещё идёт (сказать нечего), надпись ожидания снята.
        assert_eq!(
            center.update(SessionEndView::Silent, ended),
            Some(CenterStatusChange::Clear)
        );
        // Доигрывание кончилось: сообщение о конце, как раньше.
        let finished = SessionEndView::Finished(DrainFinish {
            cause: DrainCause::TabClosed,
            end: DrainEnd::NothingToPlay,
        });
        assert_eq!(
            center.update(finished, ended),
            Some(CenterStatusChange::Show("Source tab closed"))
        );
    }

    #[test]
    fn a_cleared_end_notice_gives_the_center_back() {
        let mut center = WindowCenterStatus::default();
        let waiting_for_viewer = SessionEndView::WaitingForViewer(DrainCause::TabClosed);
        center.update(waiting_for_viewer, NextEpisodeWaitView::NeverWaited);
        // Зритель нажал «играть»: сообщение снято, центр пуст.
        assert_eq!(
            center.update(SessionEndView::Silent, NextEpisodeWaitView::NeverWaited),
            Some(CenterStatusChange::Clear)
        );
    }
}
