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
    /// Вытесненный этим открытием сеанс. Его соединение (если было) уже ничего
    /// не передаёт приёмникам, но ещё ждёт прощания в «коридоре»
    /// ([`SessionControl::bid_farewell_to_replaced`]) не дольше `farewell_grace`,
    /// потом закрывается кодом 4001.
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

/// Итог [`SessionControl::close_session`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    /// Этот экземпляр был активен: сеанс закончен, соединению отправлен код.
    Closed,
    /// Этот экземпляр уже не активен (закончился, заменён, сервер выключается):
    /// ничего не сделано, активный сеанс (если есть) не тронут.
    NotActive,
}

/// Почему прощание не принято (состояние сеансов не изменилось).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FarewellError {
    /// Сообщение не прошло проверку протокола (баг отправителя).
    #[error("farewell message violates the protocol: {0:?}")]
    Unencodable(MessageErrorKind),
}

/// Итог [`SessionControl::close_session_with_farewell`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveFarewellOutcome {
    /// Экземпляр был активен и подключён: сеанс закончен, соединение допишет
    /// очередь исходящих, затем прощание, затем кадр закрытия с кодом.
    FarewellQueued,
    /// Экземпляр был активен, но без подключённого соединения (ждал `attach`
    /// или возврата после обрыва): сеанс закончен, прощание отдать некому.
    ClosedWithoutConnection,
    /// Этот экземпляр уже не активен (закончился, заменён, сервер выключается):
    /// ничего не сделано, активный сеанс (если есть) не тронут.
    NotActive,
}

/// Итог [`SessionControl::bid_farewell_to_replaced`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacedFarewellOutcome {
    /// Коридор был открыт: соединение допишет очередь, прощание и закроется 4001.
    FarewellQueued,
    /// Коридор уже закрыт: истёк срок, прощание уже было, страница ушла сама или
    /// сервер выключается. Ничего не отправлено.
    CorridorClosed,
    /// В момент замены у экземпляра не было подключённого соединения.
    NoConnection,
    /// Это не недавно заменённый экземпляр (неизвестный, слишком давний или
    /// АКТИВНЫЙ): ничего не сделано, активный сеанс не тронут.
    NotReplaced,
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

    /// Закрывает сеанс этим кодом, только если активен именно этот экземпляр
    /// (ключ + номер экземпляра).
    ///
    /// Нужен тем, кто решает закрыть сеанс по его данным не сразу (переходник
    /// протокола заметил нарушение в своём потоке, главный цикл исполняет позже):
    /// за это время сеанс мог смениться, и [`Self::close_active_session`] закрыл бы
    /// уже НОВЫЙ сеанс. Здесь старый `SessionRef` даёт честный
    /// [`CloseOutcome::NotActive`], а новый сеанс живёт дальше.
    #[must_use = "NotActive means the session was not closed"]
    pub fn close_session(&self, session: &SessionRef, reason: SessionCloseReason) -> CloseOutcome {
        self.shared.close_session(session, reason)
    }

    /// Закрывает АКТИВНЫЙ экземпляр (как [`Self::close_session`]: реестр, калитка
    /// доставки, `session_ended`, событие `Ended{ClosedByPlayer(reason)}`), но
    /// перед кадром закрытия странице уходит `farewell`.
    ///
    /// Порядок на проводе гарантирован: всё, что уже стояло в очереди
    /// исходящих → `farewell` → кадр закрытия `reason`. Прощание лежит в своём
    /// месте соединения, а не в ограниченной очереди, поэтому «очередь полна»
    /// здесь не бывает. Пример: зритель закрыл окно — `return_to_page{T, paused}`
    /// и код 4000 (решение 127).
    ///
    /// # Errors
    /// [`FarewellError::Unencodable`] — сообщение не прошло проверку протокола;
    /// сеанс при этом НЕ закрыт.
    pub fn close_session_with_farewell(
        &self,
        session: &SessionRef,
        farewell: &PlayerSessionMessage,
        reason: SessionCloseReason,
    ) -> Result<ActiveFarewellOutcome, FarewellError> {
        self.shared
            .close_session_with_farewell(session, farewell, reason)
    }

    /// Последнее сообщение ЗАМЕНЁННОМУ экземпляру, пока он в «коридоре прощания»
    /// (решение 127): соединение старой вкладки допишет очередь исходящих, затем
    /// `farewell`, затем закроется кодом 4001.
    ///
    /// `replaced` — это [`OpenedSession::replaced`] (или
    /// `EntryEvent::SessionOpened::replaced`). Активный сеанс этот метод не трогает
    /// никогда: для него — [`Self::close_session_with_farewell`].
    ///
    /// # Errors
    /// [`FarewellError::Unencodable`] — сообщение не прошло проверку протокола;
    /// коридор при этом остаётся открытым до своего срока.
    pub fn bid_farewell_to_replaced(
        &self,
        replaced: &SessionRef,
        farewell: &PlayerSessionMessage,
    ) -> Result<ReplacedFarewellOutcome, FarewellError> {
        self.shared.bid_farewell_to_replaced(replaced, farewell)
    }
}
