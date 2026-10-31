//! Конец сеанса глазами зрителя окна (мост, сессия 04b, решение 42).
//!
//! Вкладка ушла (или экстеншен пропал и не вернулся) — процесс доигрывает скачанное
//! (03c). Окно должно сказать человеку, что происходит:
//! - пока доигрывание играет — ничего (смотрим видео);
//! - окно на паузе ждёт человека — постоянное сообщение «вкладка закрыта, скачанное
//!   можно досмотреть» ([`SessionEndView::WaitingForViewer`]);
//! - доигрывание кончилось — короткое сообщение по причине и итогу
//!   ([`SessionEndView::Finished`]), после которого окно закрывается само.
//!
//! Здесь только чистые правила «что показать» и стабильные метки журнала; когда
//! показывать и сколько держать — забота окна (`window::end_notice`), а состояние
//! доигрывания — медиа-части (`session_media::ending`). Тексты — только английские
//! (пользовательский текст UI), без домена и заголовка страницы (инвариант 7).

use browser_bridge_protocol::SessionCloseReason;
use browser_bridge_session::SessionEndReason;

use super::session_media::DrainEnd;

/// Почему сеанс доигрывается — из причины конца, которая ведёт к доигрыванию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrainCause {
    /// Страница сказала `tab_gone`: вкладку закрыли, ушли со страницы, закрыли браузер.
    TabClosed,
    /// Экстеншен пропал без `tab_gone` и не вернулся за срок возврата (браузер упал).
    BrowserConnectionLost,
}

impl DrainCause {
    /// Причина конца → причина доигрывания; `None` — этот конец не доигрывается.
    ///
    /// Совпадает с единым правилом `browser_bridge_session::ends_with_playback_drain`
    /// (закреплено тестом): сообщение показывается ровно тогда, когда процесс доигрывает.
    pub(crate) const fn of(reason: SessionEndReason) -> Option<Self> {
        match reason {
            SessionEndReason::ClosedByPlayer(SessionCloseReason::TabGone) => Some(Self::TabClosed),
            SessionEndReason::ReconnectGraceExpired => Some(Self::BrowserConnectionLost),
            SessionEndReason::ClosedByPlayer(_)
            | SessionEndReason::ClosedByPage { .. }
            | SessionEndReason::FirstAttachDeadlineExpired => None,
        }
    }

    /// Стабильная метка для журнала.
    pub(crate) const fn log_label(self) -> &'static str {
        match self {
            Self::TabClosed => "tab_gone",
            Self::BrowserConnectionLost => "reconnect_grace_expired",
        }
    }
}

/// Доигрывание кончилось: почему доигрывали и чем кончилось.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DrainFinish {
    /// Почему доигрывали.
    pub(crate) cause: DrainCause,
    /// Чем кончилось.
    pub(crate) end: DrainEnd,
}

/// Что окно должно сказать о конце сеанса прямо сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionEndView {
    /// Сказать нечего: сеанс идёт, доигрывание играет или конец не доигрывается.
    Silent,
    /// Окно на паузе ждёт человека; скачанное можно досмотреть.
    WaitingForViewer(DrainCause),
    /// Доигрывание кончилось: сообщение и закрытие окна.
    Finished(DrainFinish),
}

/// Тексты. Короткие: окно маленькое, а читать их некогда — через пару секунд оно закроется.
const TAB_CLOSED: &str = "Source tab closed";
const TAB_CLOSED_STALLED: &str = "Source tab closed — playback stalled";
const TAB_CLOSED_FAILED: &str = "Source tab closed — playback failed";
const TAB_CLOSED_WAITING: &str = "Source tab closed — you can still watch what was downloaded";
const BROWSER_LOST: &str = "Lost connection to the browser";
const BROWSER_LOST_STALLED: &str = "Lost connection to the browser — playback stalled";
const BROWSER_LOST_FAILED: &str = "Lost connection to the browser — playback failed";
const BROWSER_LOST_WAITING: &str =
    "Lost connection to the browser — you can still watch what was downloaded";

impl SessionEndView {
    /// Текст сообщения в центре окна (`None` — сообщения нет).
    pub(crate) const fn center_message(self) -> Option<&'static str> {
        match self {
            Self::Silent => None,
            Self::WaitingForViewer(DrainCause::TabClosed) => Some(TAB_CLOSED_WAITING),
            Self::WaitingForViewer(DrainCause::BrowserConnectionLost) => Some(BROWSER_LOST_WAITING),
            Self::Finished(finish) => Some(finished_message(finish)),
        }
    }

    /// Стабильные метки для журнала: вид сообщения, причина, итог (`none` — нет).
    pub(crate) const fn log_labels(self) -> SessionEndLogLabels {
        match self {
            Self::Silent => SessionEndLogLabels {
                notice: "silent",
                cause: "none",
                end: "none",
            },
            Self::WaitingForViewer(cause) => SessionEndLogLabels {
                notice: "waiting_for_viewer",
                cause: cause.log_label(),
                end: "none",
            },
            Self::Finished(finish) => SessionEndLogLabels {
                notice: "finished",
                cause: finish.cause.log_label(),
                end: finish.end.log_label(),
            },
        }
    }
}

/// Метки журнала одного сообщения (без данных страницы).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionEndLogLabels {
    /// `silent` / `waiting_for_viewer` / `finished`.
    pub(crate) notice: &'static str,
    /// Метка [`DrainCause`] или `none`.
    pub(crate) cause: &'static str,
    /// Метка [`DrainEnd`] или `none`.
    pub(crate) end: &'static str,
}

/// Текст конца доигрывания: причина + короткий хвост, если видео не доиграло честно.
///
/// «Нечего играть», «доиграно» и пауза (последняя — только у безоконного процесса,
/// где окна нет) звучат одинаково: зрителю важно, что вкладки больше нет.
const fn finished_message(finish: DrainFinish) -> &'static str {
    match (finish.cause, finish.end) {
        (DrainCause::TabClosed, DrainEnd::Stalled) => TAB_CLOSED_STALLED,
        (DrainCause::TabClosed, DrainEnd::PlayerFault) => TAB_CLOSED_FAILED,
        (
            DrainCause::TabClosed,
            DrainEnd::PlayedToEnd | DrainEnd::NothingToPlay | DrainEnd::PagePaused,
        ) => TAB_CLOSED,
        (DrainCause::BrowserConnectionLost, DrainEnd::Stalled) => BROWSER_LOST_STALLED,
        (DrainCause::BrowserConnectionLost, DrainEnd::PlayerFault) => BROWSER_LOST_FAILED,
        (
            DrainCause::BrowserConnectionLost,
            DrainEnd::PlayedToEnd | DrainEnd::NothingToPlay | DrainEnd::PagePaused,
        ) => BROWSER_LOST,
    }
}

#[cfg(test)]
mod tests {
    use browser_bridge_session::ends_with_playback_drain;

    use super::*;

    /// Все причины конца, которые бывают (для сверки с правилом сервера).
    fn every_end_reason() -> Vec<SessionEndReason> {
        let mut reasons: Vec<SessionEndReason> = [
            SessionCloseReason::PlayerClosed,
            SessionCloseReason::ReplacedByAnotherTab,
            SessionCloseReason::TabGone,
            SessionCloseReason::ProtocolViolation,
            SessionCloseReason::InvalidToken,
            SessionCloseReason::UnknownSession,
            SessionCloseReason::PlayerShuttingDown,
            SessionCloseReason::AttachTimeout,
            SessionCloseReason::AlreadyAttached,
        ]
        .into_iter()
        .map(SessionEndReason::ClosedByPlayer)
        .collect();
        reasons.extend([
            SessionEndReason::ClosedByPage { code: None },
            SessionEndReason::ClosedByPage { code: Some(1000) },
            SessionEndReason::ReconnectGraceExpired,
            SessionEndReason::FirstAttachDeadlineExpired,
        ]);
        reasons
    }

    #[test]
    fn a_message_is_due_exactly_when_the_server_rule_drains_the_playback() {
        for reason in every_end_reason() {
            assert_eq!(
                DrainCause::of(reason).is_some(),
                ends_with_playback_drain(reason),
                "{reason:?}"
            );
        }
    }

    #[test]
    fn the_tab_and_the_lost_browser_get_their_own_words() {
        let finished = |cause, end| SessionEndView::Finished(DrainFinish { cause, end });
        assert_eq!(
            finished(DrainCause::TabClosed, DrainEnd::PlayedToEnd).center_message(),
            Some("Source tab closed")
        );
        assert_eq!(
            finished(DrainCause::TabClosed, DrainEnd::NothingToPlay).center_message(),
            Some("Source tab closed")
        );
        assert_eq!(
            finished(DrainCause::TabClosed, DrainEnd::Stalled).center_message(),
            Some("Source tab closed — playback stalled")
        );
        assert_eq!(
            finished(DrainCause::TabClosed, DrainEnd::PlayerFault).center_message(),
            Some("Source tab closed — playback failed")
        );
        assert_eq!(
            finished(DrainCause::BrowserConnectionLost, DrainEnd::PlayedToEnd).center_message(),
            Some("Lost connection to the browser")
        );
        assert_eq!(
            finished(DrainCause::BrowserConnectionLost, DrainEnd::Stalled).center_message(),
            Some("Lost connection to the browser — playback stalled")
        );
        assert_eq!(
            SessionEndView::WaitingForViewer(DrainCause::TabClosed).center_message(),
            Some("Source tab closed — you can still watch what was downloaded")
        );
        assert_eq!(
            SessionEndView::WaitingForViewer(DrainCause::BrowserConnectionLost).center_message(),
            Some("Lost connection to the browser — you can still watch what was downloaded")
        );
        assert_eq!(SessionEndView::Silent.center_message(), None);
    }

    #[test]
    fn log_labels_are_stable_and_carry_no_page_data() {
        let finish = SessionEndView::Finished(DrainFinish {
            cause: DrainCause::TabClosed,
            end: DrainEnd::PlayedToEnd,
        });
        assert_eq!(
            finish.log_labels(),
            SessionEndLogLabels {
                notice: "finished",
                cause: "tab_gone",
                end: "played_to_end",
            }
        );
        assert_eq!(
            SessionEndView::WaitingForViewer(DrainCause::BrowserConnectionLost).log_labels(),
            SessionEndLogLabels {
                notice: "waiting_for_viewer",
                cause: "reconnect_grace_expired",
                end: "none",
            }
        );
    }
}
