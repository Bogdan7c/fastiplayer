//! Участок «сеанс»: WebSocket из background экстеншена прямо в процесс
//! браузерного режима (решения 82 и 85).
//!
//! Одно соединение — один сеанс. По нему идут:
//! - текстовые рамки WebSocket — JSON-сообщения [`PageSessionMessage`] и
//!   [`PlayerSessionMessage`];
//! - двоичные рамки WebSocket — куски байтов (`crate::data_frame`).
//!
//! WebSocket сохраняет порядок рамок обоих видов, поэтому «сначала объявить
//! SourceBuffer, потом слать его байты» выполняется само.
//!
//! Порядок сеанса:
//! 1. экстеншен шлёт `attach` с ключом и пропуском из `session_granted`;
//! 2. плеер отвечает `attached` или закрывает соединение с кодом
//!    [`SessionCloseReason`];
//! 3. дальше обе стороны говорят свободно, пока одна не закроет соединение.

pub mod page;
pub mod player;
pub mod variants;

pub use page::{
    PagePlaybackRequest, PageSessionMessage, RangeFetchFailure, RangeFetchOutcome, RemovalEnd,
    ResourceSize, SeekCause, StreamScope, TabGoneReason,
};
pub use player::{
    BufferIntake, ClockSequence, ClockState, PlayerSessionMessage, ResumeState, StreamRejection,
};
pub use variants::{VariantKind, VariantMenu, VariantOption, VariantSelection};

/// Почему сеанс закрыт — код закрытия WebSocket.
///
/// Коды 4000–4999 стандарт WebSocket (RFC 6455, 7.4.2) оставляет приложениям.
/// Причина передаётся кодом, а не текстом: текст закрытия ограничен 123 байтами
/// и для машин ненадёжен.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionCloseReason {
    /// Человек закрыл окно плеера.
    PlayerClosed,
    /// Видео из другой вкладки заменило этот сеанс (решение 40).
    ReplacedByAnotherTab,
    /// Вкладка ушла (решение 42); закрывает экстеншен.
    TabGone,
    /// Нарушение протокола: битое сообщение, «рамка из будущего», пропуск номера.
    ProtocolViolation,
    /// Пропуск не подошёл.
    InvalidToken,
    /// Сеанса с таким ключом нет (истёк, закрыт).
    UnknownSession,
    /// Плеер завершается.
    PlayerShuttingDown,
    /// `attach` не пришёл вовремя.
    AttachTimeout,
    /// К сеансу уже подключено другое соединение.
    AlreadyAttached,
}

impl SessionCloseReason {
    /// Все причины — для обратного перевода кода.
    const ALL: [Self; 9] = [
        Self::PlayerClosed,
        Self::ReplacedByAnotherTab,
        Self::TabGone,
        Self::ProtocolViolation,
        Self::InvalidToken,
        Self::UnknownSession,
        Self::PlayerShuttingDown,
        Self::AttachTimeout,
        Self::AlreadyAttached,
    ];

    /// Код закрытия WebSocket.
    #[must_use]
    pub const fn close_code(self) -> u16 {
        match self {
            Self::PlayerClosed => 4000,
            Self::ReplacedByAnotherTab => 4001,
            Self::TabGone => 4002,
            Self::ProtocolViolation => 4003,
            Self::InvalidToken => 4004,
            Self::UnknownSession => 4005,
            Self::PlayerShuttingDown => 4006,
            Self::AttachTimeout => 4007,
            Self::AlreadyAttached => 4008,
        }
    }

    /// Причина по коду закрытия; `None` — код не наш (обрыв сети, 1006 и т. п.).
    #[must_use]
    pub fn from_close_code(code: u16) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.close_code() == code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_close_reason_has_a_unique_application_code_that_maps_back() {
        let mut seen_codes = std::collections::BTreeSet::new();
        for reason in SessionCloseReason::ALL {
            let code = reason.close_code();
            assert!((4000..=4999).contains(&code), "{reason:?} → {code}");
            assert!(seen_codes.insert(code), "повтор кода {code}");
            assert_eq!(SessionCloseReason::from_close_code(code), Some(reason));
        }
        assert_eq!(SessionCloseReason::from_close_code(1006), None);
        assert_eq!(SessionCloseReason::from_close_code(4999), None);
    }
}
