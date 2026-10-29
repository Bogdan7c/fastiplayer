//! Управление сервером снаружи: узкая ручка для «стойки входа» и приложения.
//!
//! [`SessionControl`] — клонируемая ручка (`Clone + Send`): через неё открывают
//! сеанс, шлют команды странице и закрывают сеанс. Она знает ИМЕНА операций, а не
//! устройство сервера (потоки, очереди, замки).
//!
//! Инвариант 3: после `open_session` и `attach` сеанс живёт без этой ручки —
//! входная сторона (D-Bus, посредник) может исчезнуть, байты продолжат идти.

use std::sync::Arc;
use std::time::Instant;

use browser_bridge_protocol::{
    GrantOutcome, LoopbackPort, MessageErrorKind, OpenSessionRequest, PlayerSessionMessage,
    SessionCloseReason, SessionToken,
};

use super::shared::Shared;
use crate::registry::{SessionInstance, SessionRef};
use crate::token::TokenSourceError;

/// Что получил открывший сеанс.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedSession {
    /// Одноразовый пропуск для `attach`.
    pub token: SessionToken,
    /// Начат, повторён или заменил прежний.
    pub outcome: GrantOutcome,
    /// Экземпляр выданного сеанса (повтор того же сеанса сохраняет экземпляр).
    pub instance: SessionInstance,
    /// Вытесненный сеанс (его соединение закрывается кодом 4001).
    pub replaced: Option<SessionRef>,
}

/// Почему сеанс открыть нельзя.
#[derive(Debug, thiserror::Error)]
pub enum OpenSessionError {
    /// Сервер выключается.
    #[error("browser session server is shutting down")]
    ShuttingDown,
    /// Не удалось выдать пропуск.
    #[error("cannot issue a session token: {0}")]
    TokenSource(TokenSourceError),
}

/// Почему сообщение странице не поставлено в очередь.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendToPageError {
    /// Нет подключённого сеанса.
    #[error("no attached browser session")]
    NoAttachedSession,
    /// Очередь исходящих полна (получатель не успевает).
    #[error("outgoing queue to the page is full")]
    QueueFull,
    /// Сервер выключается.
    #[error("browser session server is shutting down")]
    ShuttingDown,
    /// Сообщение не прошло проверку протокола (баг отправителя).
    #[error("message to the page violates the protocol: {0:?}")]
    Unencodable(MessageErrorKind),
}

/// Ручка управления сервером.
#[derive(Clone)]
pub struct SessionControl {
    shared: Arc<Shared>,
}

impl SessionControl {
    pub(super) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// Порт, на котором сервер слушает 127.0.0.1.
    #[must_use]
    pub fn port(&self) -> LoopbackPort {
        self.shared.port()
    }

    /// Открывает (или повторяет, или заменяет) сеанс. `now` — время для сроков.
    ///
    /// # Errors
    /// Сервер выключается или не выдался пропуск; состояние сеансов не меняется.
    pub fn open_session(
        &self,
        request: &OpenSessionRequest,
        now: Instant,
    ) -> Result<OpenedSession, OpenSessionError> {
        self.shared.open_session(request, now)
    }

    /// Ставит сообщение плеера в очередь подключённого сеанса.
    ///
    /// # Errors
    /// [`SendToPageError`] — нет сеанса, очередь полна, выключение, битое сообщение.
    pub fn send_to_page(&self, message: &PlayerSessionMessage) -> Result<(), SendToPageError> {
        self.shared.send_to_page(message)
    }

    /// Закрывает активный сеанс этим кодом (человек закрыл окно и т. п.).
    /// Нет сеанса — ничего не происходит.
    pub fn close_active_session(&self, reason: SessionCloseReason) {
        self.shared.close_active_session(reason);
    }
}
