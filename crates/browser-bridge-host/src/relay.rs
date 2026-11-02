//! Одно сообщение браузера → один ответ браузеру.
//!
//! Порядок (инвариант 1 «страница — враг» и 3 «посредник недоверчивый»):
//! 1. сообщение строго разбирается как `ExtensionControlMessage` — только `hello`
//!    и `open_session`, предел 128 КиБ, никаких лишних полей. Не разобралось —
//!    отказ `refused{in_reply_to: unparsed_message, reason: malformed_message}`,
//!    в дверь не звоним;
//! 2. исходный JSON уходит в дверь как есть (плеер разбирает его повторно сам);
//! 3. ответ плеера тоже строго разбирается как `PlayerControlMessage`: браузеру
//!    уходит только валидный ответ (он заведомо меньше предела 1 МиБ). Звонок не
//!    удался или ответ битый — свой отказ с точной причиной.
//!
//! Повтор `hello` после перезапуска плеера посредник не делает сам: плеер отвечает
//! `refused{hello_required}`, и экстеншен повторяет `hello` (решение 05 —
//! посредник остаётся тонким).

use browser_bridge_protocol::{
    ControlRefusal, ExtensionControlMessage, MessageError, MessageErrorKind, PlayerControlMessage,
    RefusalReason, ReplyTarget, decode_message, encode_message,
};

use crate::door::{DoorFailure, PlayerDoor};

/// Какое сообщение пришло от браузера (для журнала).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// `hello`.
    Hello,
    /// `open_session`.
    OpenSession,
    /// Сообщение не разобралось.
    Unparsed,
}

impl RequestKind {
    /// Имя вида для журнала.
    #[must_use]
    pub const fn log_name(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::OpenSession => "open_session",
            Self::Unparsed => "unparsed",
        }
    }
}

/// Чем ответили браузеру.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyKind {
    /// Передан ответ плеера (сам он может быть и отказом, например `hello_required`).
    FromPlayer(PlayerReplyKind),
    /// Отказ от имени посредника.
    RefusedByHost(HostRefusalCause),
}

/// Вид ответа плеера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerReplyKind {
    /// `hello_accepted`.
    HelloAccepted,
    /// `session_granted`.
    SessionGranted,
    /// `refused` с этой причиной.
    Refused(RefusalReason),
}

/// Почему посредник отказал сам.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRefusalCause {
    /// Сообщение браузера не разобралось.
    MalformedMessage(MessageErrorKind),
    /// Звонок в дверь не дал ответа.
    Door(DoorFailure),
    /// Плеер ответил тем, что не разбирается как ответ входа.
    InvalidPlayerReply(MessageErrorKind),
}

/// Готовый ответ браузеру и что в нём (для журнала).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayedReply {
    /// JSON ответа: всегда валидный `PlayerControlMessage`.
    pub json: String,
    /// Какое сообщение пришло.
    pub request: RequestKind,
    /// Чем ответили.
    pub reply: ReplyKind,
}

/// Переводчик сообщений браузера в звонки двери.
#[derive(Debug)]
pub struct Relay<Door> {
    door: Door,
}

impl<Door: PlayerDoor> Relay<Door> {
    /// Переводчик поверх этой двери.
    pub const fn new(door: Door) -> Self {
        Self { door }
    }

    /// Отвечает на одно сообщение браузера.
    ///
    /// # Errors
    /// Только если не удалось записать собственный отказ — по устройству типов
    /// это невозможно (отказ из фиксированных полей), но ошибка не глотается.
    pub fn handle(&mut self, message: &[u8]) -> Result<RelayedReply, MessageError> {
        let request = match decode_message::<ExtensionControlMessage>(message) {
            Ok(request) => request,
            Err(error) => {
                let cause = HostRefusalCause::MalformedMessage(error.kind());
                return host_refusal(
                    RequestKind::Unparsed,
                    ReplyTarget::UnparsedMessage {},
                    cause,
                );
            }
        };
        let (request_kind, target) = match &request {
            ExtensionControlMessage::Hello(_) => (RequestKind::Hello, ReplyTarget::Hello {}),
            ExtensionControlMessage::OpenSession(open) => (
                RequestKind::OpenSession,
                ReplyTarget::OpenSession {
                    request_id: open.request_id,
                },
            ),
        };
        // Разбор выше уже проверил UTF-8; повторная проверка — без паники.
        let Ok(message_json) = std::str::from_utf8(message) else {
            let cause = HostRefusalCause::MalformedMessage(MessageErrorKind::NotUtf8);
            return host_refusal(request_kind, ReplyTarget::UnparsedMessage {}, cause);
        };
        match self.door.ring(message_json) {
            Ok(player_reply) => forward_player_reply(request_kind, target, player_reply),
            Err(failure) => host_refusal(request_kind, target, HostRefusalCause::Door(failure)),
        }
    }
}

/// Пропускает ответ плеера, только если он строго разбирается.
fn forward_player_reply(
    request: RequestKind,
    target: ReplyTarget,
    player_reply: String,
) -> Result<RelayedReply, MessageError> {
    match decode_message::<PlayerControlMessage>(player_reply.as_bytes()) {
        Ok(decoded) => Ok(RelayedReply {
            json: player_reply,
            request,
            reply: ReplyKind::FromPlayer(player_reply_kind(&decoded)),
        }),
        Err(error) => host_refusal(
            request,
            target,
            HostRefusalCause::InvalidPlayerReply(error.kind()),
        ),
    }
}

/// Вид разобранного ответа плеера.
const fn player_reply_kind(reply: &PlayerControlMessage) -> PlayerReplyKind {
    match reply {
        PlayerControlMessage::HelloAccepted(_) => PlayerReplyKind::HelloAccepted,
        PlayerControlMessage::SessionGranted(_) => PlayerReplyKind::SessionGranted,
        PlayerControlMessage::Refused(refusal) => PlayerReplyKind::Refused(refusal.reason),
    }
}

/// Отказ от имени посредника.
fn host_refusal(
    request: RequestKind,
    in_reply_to: ReplyTarget,
    cause: HostRefusalCause,
) -> Result<RelayedReply, MessageError> {
    let refusal = PlayerControlMessage::Refused(ControlRefusal {
        in_reply_to,
        reason: refusal_reason_for(cause),
    });
    Ok(RelayedReply {
        json: encode_message(&refusal)?,
        request,
        reply: ReplyKind::RefusedByHost(cause),
    })
}

/// Причина отказа браузеру для каждой своей причины (договор сессии 01/02).
#[must_use]
pub const fn refusal_reason_for(cause: HostRefusalCause) -> RefusalReason {
    match cause {
        HostRefusalCause::MalformedMessage(error) => RefusalReason::MalformedMessage { error },
        HostRefusalCause::Door(
            DoorFailure::PlayerUnavailable | DoorFailure::UnsupportedPlatform,
        ) => RefusalReason::PlayerUnavailable {},
        HostRefusalCause::Door(DoorFailure::PlayerNotResponding) => {
            RefusalReason::PlayerNotResponding {}
        }
        HostRefusalCause::Door(DoorFailure::PlayerShuttingDown) => {
            RefusalReason::PlayerShuttingDown {}
        }
        // Транспорт отказался везти сообщение — по смыслу это «слишком большое»
        // (единственная причина отказа клиента до шины); до двери такое сообщение
        // не доходит, потому что разбор раньше проверил тот же предел.
        HostRefusalCause::Door(DoorFailure::MessageRefusedByTransport) => {
            RefusalReason::MalformedMessage {
                error: MessageErrorKind::TooLarge,
            }
        }
        // Плеер ответил непонятным — для браузера он недоступен (несовместимая
        // сборка или сбой); подробность — только в журнале посредника.
        HostRefusalCause::InvalidPlayerReply(_) => RefusalReason::PlayerUnavailable {},
    }
}

/// Имя причины отказа для журнала (snake_case, как в протоколе).
#[must_use]
pub const fn refusal_reason_log_name(reason: &RefusalReason) -> &'static str {
    match reason {
        RefusalReason::PlayerUnavailable {} => "player_unavailable",
        RefusalReason::PlayerNotResponding {} => "player_not_responding",
        RefusalReason::PlayerShuttingDown {} => "player_shutting_down",
        RefusalReason::ProtectedContent {} => "protected_content",
        RefusalReason::IncompatibleVersion { .. } => "incompatible_version",
        RefusalReason::UnsupportedCodec {} => "unsupported_codec",
        RefusalReason::MalformedMessage { .. } => "malformed_message",
        RefusalReason::HelloRequired {} => "hello_required",
    }
}

#[cfg(test)]
mod tests;
