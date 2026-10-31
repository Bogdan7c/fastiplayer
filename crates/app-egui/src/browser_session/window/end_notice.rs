//! Конец сеанса в окне (мост, сессия 04b, решение 42): сообщение и закрытие после него.
//!
//! Что сказать, решает [`SessionEndView`] (`browser_session::session_end`); здесь — как
//! окно это показывает:
//! - [`ShownEndNotice`] ставит и снимает сообщение в центре окна только при смене (и
//!   пишет одну строку журнала без данных страницы — её читают сквозные тесты);
//! - [`exit_plan`] — как выходить, когда машина выхода решила «выход»: доигрывание
//!   кончилось → сообщение висит [`END_NOTICE_PERIOD`], окно рисует последний кадр, потом
//!   закрывается; любой другой выход — сразу, как раньше.
//!
//! Безоконный процесс этим модулем не пользуется: у него выход сразу (правило 103).

use std::time::{Duration, Instant};

use tracing::info;

use crate::browser_session::session_end::SessionEndView;

/// Сколько окно показывает сообщение о конце сеанса, прежде чем закрыться.
///
/// Решение 42 и файл сессии 04b: «закрыть окно через 2–3 с». 2,5 с — середина: короткую
/// фразу («Source tab closed») человек успевает прочитать, а окно не висит без дела.
/// Это поведение окна, а не настройка пользователя.
pub(super) const END_NOTICE_PERIOD: Duration = Duration::from_millis(2_500);

/// Как процессу с окном выйти по решению машины выхода.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WindowExit {
    /// Сразу (как до 04b).
    Now,
    /// Ядро закрыть сейчас, окно — в этот момент, после сообщения о конце сеанса.
    AfterNotice {
        /// Когда закрыть окно.
        close_at: Instant,
    },
}

/// Выход по решению машины: после доигрывания — сообщение и закрытие через
/// [`END_NOTICE_PERIOD`]; иначе (окно закрыто сеансом, нарушение протокола, никто не
/// открыл сеанс…) — сразу.
pub(super) fn exit_plan(view: SessionEndView, now: Instant) -> WindowExit {
    match view {
        SessionEndView::Finished(_) => WindowExit::AfterNotice {
            close_at: now + END_NOTICE_PERIOD,
        },
        SessionEndView::Silent | SessionEndView::WaitingForViewer(_) => WindowExit::Now,
    }
}

/// Что сделать с сообщением состояния окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CenterStatusChange {
    /// Показать (или заменить) этот текст.
    Show(&'static str),
    /// Снять.
    Clear,
}

/// Сообщение о конце сеанса, которое окно показывает сейчас.
#[derive(Debug, Clone, Copy)]
pub(super) struct ShownEndNotice {
    shown: SessionEndView,
}

impl Default for ShownEndNotice {
    fn default() -> Self {
        Self {
            shown: SessionEndView::Silent,
        }
    }
}

impl ShownEndNotice {
    /// Новое состояние конца сеанса за проход → что поменять в окне (`None` — ничего:
    /// сообщение уже такое). Смена пишется в журнал одной строкой.
    pub(super) fn update(&mut self, view: SessionEndView) -> Option<CenterStatusChange> {
        if view == self.shown {
            return None;
        }
        self.shown = view;
        let labels = view.log_labels();
        match view.center_message() {
            Some(message) => {
                info!(
                    notice = %labels.notice,
                    cause = %labels.cause,
                    end = %labels.end,
                    "Browser session end notice shown"
                );
                Some(CenterStatusChange::Show(message))
            }
            None => {
                info!("Browser session end notice cleared");
                Some(CenterStatusChange::Clear)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser_session::session_end::{DrainCause, DrainFinish};
    use crate::browser_session::session_media::DrainEnd;

    fn finished(end: DrainEnd) -> SessionEndView {
        SessionEndView::Finished(DrainFinish {
            cause: DrainCause::TabClosed,
            end,
        })
    }

    #[test]
    fn a_finished_drain_closes_the_window_after_the_notice_period() {
        let now = Instant::now();
        for end in [
            DrainEnd::PlayedToEnd,
            DrainEnd::NothingToPlay,
            DrainEnd::Stalled,
            DrainEnd::PlayerFault,
        ] {
            assert_eq!(
                exit_plan(finished(end), now),
                WindowExit::AfterNotice {
                    close_at: now + END_NOTICE_PERIOD
                },
                "{end:?}"
            );
        }
    }

    #[test]
    fn any_other_exit_closes_the_window_at_once() {
        let now = Instant::now();
        assert_eq!(exit_plan(SessionEndView::Silent, now), WindowExit::Now);
        assert_eq!(
            exit_plan(SessionEndView::WaitingForViewer(DrainCause::TabClosed), now),
            WindowExit::Now
        );
    }

    #[test]
    fn the_notice_period_is_within_the_owner_window_of_two_to_three_seconds() {
        assert!(END_NOTICE_PERIOD >= Duration::from_secs(2));
        assert!(END_NOTICE_PERIOD <= Duration::from_secs(3));
    }

    #[test]
    fn the_window_changes_its_message_only_when_the_view_changes() {
        let mut notice = ShownEndNotice::default();
        assert_eq!(notice.update(SessionEndView::Silent), None);
        let waiting = SessionEndView::WaitingForViewer(DrainCause::TabClosed);
        assert_eq!(
            notice.update(waiting),
            Some(CenterStatusChange::Show(
                "Source tab closed — you can still watch what was downloaded"
            ))
        );
        assert_eq!(notice.update(waiting), None);
        // Зритель нажал «играть»: доигрывание играет — сообщение снимается.
        assert_eq!(
            notice.update(SessionEndView::Silent),
            Some(CenterStatusChange::Clear)
        );
        assert_eq!(
            notice.update(finished(DrainEnd::PlayedToEnd)),
            Some(CenterStatusChange::Show("Source tab closed"))
        );
    }
}
