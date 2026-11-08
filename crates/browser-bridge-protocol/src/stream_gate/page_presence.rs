//! Присутствие страницы-источника у сторожа потока (протокол 1.4, решения 240–241).
//!
//! Страница либо на месте, либо «ушла» (`page_left`: связь открыта, байтов нет), либо вернуть
//! её не вышло (`page_reopen_failed`: экстеншен сейчас закроет соединение). Модуль владеет
//! только таблицей переходов по сообщениям; что делать с поколением потока при уходе страницы,
//! решает сам сторож (`StreamGate::leave_page`): поколение живёт в нём.

use super::StreamViolation;
use crate::session::page::PageSessionMessage;

/// Где сейчас страница-источник.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PagePresence {
    /// Страница на месте — обычные правила потока.
    Present,
    /// Страница ушла (`page_left`), экстеншен может её вернуть.
    Left,
    /// Вернуть не вышло (`page_reopen_failed`): ждём закрытия соединения, говорить больше нельзя.
    ReopenFailed,
}

/// Что делать сторожу с сообщением после проверки присутствия.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PresenceReaction {
    /// Присутствие сообщению не мешает и не меняется — дальше обычные правила потока.
    ContinueWithStreamRules,
    /// Сообщение меняет присутствие (или принято как есть): принять и перейти в это состояние.
    /// Правил потока оно не проходит — ни одно из таких сообщений не привязано к потоку.
    AcceptAndBecome(PagePresence),
}

impl PagePresence {
    /// Решение по текстовому сообщению в этом состоянии.
    ///
    /// # Errors
    /// [`StreamViolation::PageIsPresent`] или [`StreamViolation::PageIsAway`] — порядок
    /// сообщений нарушен, сеанс надо закрыть.
    pub(super) const fn react_to(
        self,
        message: &PageSessionMessage,
    ) -> Result<PresenceReaction, StreamViolation> {
        match (self, message) {
            // Страница на месте: уйти можно, «вернуться» и «не вернулась» — нет.
            (Self::Present, PageSessionMessage::PageLeft { .. }) => {
                Ok(PresenceReaction::AcceptAndBecome(Self::Left))
            }
            (
                Self::Present,
                PageSessionMessage::PageReturned {} | PageSessionMessage::PageReopenFailed { .. },
            ) => Err(StreamViolation::PageIsPresent),
            (Self::Present, _) => Ok(PresenceReaction::ContinueWithStreamRules),
            // Страница ушла: только вернулась / не вернулась / экстеншен сдался (`tab_gone`).
            (Self::Left, PageSessionMessage::PageReturned {}) => {
                Ok(PresenceReaction::AcceptAndBecome(Self::Present))
            }
            (Self::Left, PageSessionMessage::PageReopenFailed { .. }) => {
                Ok(PresenceReaction::AcceptAndBecome(Self::ReopenFailed))
            }
            // Сдался: сервер закончит сеанс сам, состояние не трогаем.
            (Self::Left, PageSessionMessage::TabGone { .. }) => {
                Ok(PresenceReaction::AcceptAndBecome(Self::Left))
            }
            (Self::Left | Self::ReopenFailed, _) => Err(StreamViolation::PageIsAway),
        }
    }

    /// Можно ли принимать байты рамок: только пока страница на месте.
    ///
    /// # Errors
    /// [`StreamViolation::PageIsAway`] — страница ушла или не вернулась.
    pub(super) const fn admit_bytes(self) -> Result<(), StreamViolation> {
        match self {
            Self::Present => Ok(()),
            Self::Left | Self::ReopenFailed => Err(StreamViolation::PageIsAway),
        }
    }
}
