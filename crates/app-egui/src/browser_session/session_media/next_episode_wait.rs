//! «Ждём следующую серию» (протокол 1.9, сессия 16a, решения 303, 306): владелец надписи.
//!
//! Серия доиграла у зрителя, страница сняла источник, экстеншен ждёт новый и говорит плееру
//! `awaiting_next_episode`. Окно держит последний кадр и пишет «Waiting for the next
//! episode…», пока не случится первое из двух:
//! - новое поколение показано ([`NextEpisodeWaitEnd::GenerationShown`]): плеер впервые
//!   показывает медиа поколения, загруженного после слова страницы (момент «согласовать с
//!   страницей после установки» в `report_to_page`). Отдельного признака «первый кадр
//!   на экране» у наблюдения плеера нет — используется тот же признак «наблюдение описывает
//!   медиа этого поколения», по которому часы странице переходят на новое поколение;
//! - сеанс кончился любым путём ([`NextEpisodeWaitEnd::SessionEnded`]): конец сеанса,
//!   уход страницы («сирота») или замена вкладки новым экземпляром.
//!
//! Инвариант: пока идёт доигрывание (сеанс кончился), ожидание не начинается — конец сеанса
//! может обогнать данные (`InstanceSealed`), и запоздалое слово страницы иначе повесило бы
//! надпись навсегда. Поэтому надпись ожидания и сообщение о конце сеанса никогда не нужны
//! окну одновременно.
//!
//! Повторное слово страницы ничего не меняет (надпись не «складывается»). Срок
//! `patience_ms` плеер не оспаривает и сам не отсчитывает: закрытие по сроку — забота
//! экстеншена (код 4011). Журнал без данных страницы: здесь только число срока; строки
//! «Browser window tells the viewer: …» пишет окно, когда меняет надпись.

use browser_bridge_protocol::session::NextEpisodePatience;
use tracing::{debug, info};

use super::SessionMedia;

/// Почему надпись ожидания снята.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NextEpisodeWaitEnd {
    /// Новое поколение показано.
    GenerationShown,
    /// Сеанс кончился (любой конец, уход страницы или замена вкладки).
    SessionEnded,
}

impl NextEpisodeWaitEnd {
    /// Стабильная метка для журнала (договор со стендом моста: не переименовывать).
    pub(crate) const fn log_label(self) -> &'static str {
        match self {
            Self::GenerationShown => "generation_shown",
            Self::SessionEnded => "session_ended",
        }
    }
}

/// Что окно должно знать об ожидании следующей серии прямо сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NextEpisodeWaitView {
    /// Страница ни разу не говорила, что ждёт следующую серию.
    #[default]
    NeverWaited,
    /// Ждём: надпись на экране.
    Waiting,
    /// Ожидание кончилось по этой причине (держится до следующего слова страницы).
    Ended(NextEpisodeWaitEnd),
}

/// Итог слова страницы «ждём следующую серию».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AwaitingNote {
    /// Ожидание началось: надпись появится.
    Began,
    /// Уже ждём: повтор ничего не меняет.
    AlreadyWaiting,
}

/// Итог снятия ожидания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WaitClear {
    /// Ожидание было и снято.
    Cleared,
    /// Ожидания не было: ничего не изменилось.
    NotWaiting,
}

/// Владелец ожидания следующей серии: только его состояние, ничего чужого.
#[derive(Debug, Default)]
pub(super) struct NextEpisodeWait {
    view: NextEpisodeWaitView,
}

impl NextEpisodeWait {
    /// Страница сказала «ждём следующую серию».
    pub(super) fn note_awaiting_next_episode(&mut self) -> AwaitingNote {
        if self.view == NextEpisodeWaitView::Waiting {
            return AwaitingNote::AlreadyWaiting;
        }
        self.view = NextEpisodeWaitView::Waiting;
        AwaitingNote::Began
    }

    /// Плеер впервые показывает новое поколение: ожидание (если было) кончилось.
    pub(super) fn clear_on_generation_shown(&mut self) -> WaitClear {
        self.clear(NextEpisodeWaitEnd::GenerationShown)
    }

    /// Сеанс кончился любым путём: ожидание (если было) кончилось.
    pub(super) fn clear_on_session_ended(&mut self) -> WaitClear {
        self.clear(NextEpisodeWaitEnd::SessionEnded)
    }

    /// Что сейчас показывать окну.
    pub(super) const fn view(&self) -> NextEpisodeWaitView {
        self.view
    }

    fn clear(&mut self, end: NextEpisodeWaitEnd) -> WaitClear {
        if self.view != NextEpisodeWaitView::Waiting {
            return WaitClear::NotWaiting;
        }
        self.view = NextEpisodeWaitView::Ended(end);
        WaitClear::Cleared
    }
}

impl SessionMedia {
    /// Слово страницы `awaiting_next_episode` текущего экземпляра.
    pub(super) fn note_page_awaits_next_episode(&mut self, patience: NextEpisodePatience) {
        // Страница сняла источник: отложенная перемотка доигравшей серии больше не повтор.
        self.drop_held_page_seek(super::page_play_after_end::HeldSeekDrop::PageAwaitsNextEpisode);
        // Сеанс уже кончился (конец обогнал данные): ждать нечего, надпись не ставится.
        if self.is_draining() {
            debug!("The page awaits the next episode after the session ended; ignored");
            return;
        }
        match self.next_episode_wait.note_awaiting_next_episode() {
            AwaitingNote::Began => info!(
                patience_ms = patience.as_millis(),
                "The page awaits the next episode"
            ),
            AwaitingNote::AlreadyWaiting => {
                debug!("The page repeated that it awaits the next episode");
            }
        }
    }

    /// Ожидание следующей серии глазами окна (каждый шаг ядра).
    pub(crate) const fn next_episode_wait(&self) -> NextEpisodeWaitView {
        self.next_episode_wait.view()
    }

    /// Новое поколение впервые показано: ожидание следующей серии кончилось.
    pub(super) fn end_next_episode_wait_on_generation_shown(&mut self) {
        if self.next_episode_wait.clear_on_generation_shown() == WaitClear::Cleared {
            debug!("The next episode is shown; the wait for it is over");
        }
    }

    /// Сеанс кончился (конец, уход страницы, замена вкладки): ожидание кончилось.
    pub(super) fn end_next_episode_wait_on_session_end(&mut self) {
        if self.next_episode_wait.clear_on_session_ended() == WaitClear::Cleared {
            debug!("The session ended while the page awaited the next episode");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_wait_is_never_waited_and_clearing_it_changes_nothing() {
        let mut wait = NextEpisodeWait::default();
        assert_eq!(wait.view(), NextEpisodeWaitView::NeverWaited);
        assert_eq!(wait.clear_on_generation_shown(), WaitClear::NotWaiting);
        assert_eq!(wait.clear_on_session_ended(), WaitClear::NotWaiting);
        // Снятие несуществующего ожидания не выдумывает причину конца.
        assert_eq!(wait.view(), NextEpisodeWaitView::NeverWaited);
    }

    #[test]
    fn repeated_words_of_the_page_do_not_stack() {
        let mut wait = NextEpisodeWait::default();
        assert_eq!(wait.note_awaiting_next_episode(), AwaitingNote::Began);
        assert_eq!(
            wait.note_awaiting_next_episode(),
            AwaitingNote::AlreadyWaiting
        );
        assert_eq!(wait.view(), NextEpisodeWaitView::Waiting);
        // Одно снятие снимает всё: второго «слоя» надписи нет.
        assert_eq!(wait.clear_on_generation_shown(), WaitClear::Cleared);
        assert_eq!(
            wait.view(),
            NextEpisodeWaitView::Ended(NextEpisodeWaitEnd::GenerationShown)
        );
    }

    #[test]
    fn the_first_clear_wins_and_keeps_its_reason() {
        let mut wait = NextEpisodeWait::default();
        wait.note_awaiting_next_episode();
        assert_eq!(wait.clear_on_session_ended(), WaitClear::Cleared);
        // Показ поколения после конца сеанса причину не переписывает.
        assert_eq!(wait.clear_on_generation_shown(), WaitClear::NotWaiting);
        assert_eq!(
            wait.view(),
            NextEpisodeWaitView::Ended(NextEpisodeWaitEnd::SessionEnded)
        );
    }

    #[test]
    fn a_new_episode_end_begins_a_new_wait_after_a_cleared_one() {
        let mut wait = NextEpisodeWait::default();
        wait.note_awaiting_next_episode();
        wait.clear_on_generation_shown();
        // Серия 2 тоже доиграла: страница снова ждёт — надпись снова нужна.
        assert_eq!(wait.note_awaiting_next_episode(), AwaitingNote::Began);
        assert_eq!(wait.view(), NextEpisodeWaitView::Waiting);
    }

    #[test]
    fn log_labels_are_the_bench_contract() {
        assert_eq!(
            NextEpisodeWaitEnd::GenerationShown.log_label(),
            "generation_shown"
        );
        assert_eq!(
            NextEpisodeWaitEnd::SessionEnded.log_label(),
            "session_ended"
        );
    }
}
