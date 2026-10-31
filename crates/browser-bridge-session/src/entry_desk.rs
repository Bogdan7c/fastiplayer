//! «Стойка входа»: один сырой запрос участка «вход» → один ответ (решение 85).
//!
//! Посредник native messaging звонит по D-Bus и передаёт сообщение экстеншена
//! строкой, не разбирая. Здесь оно разбирается, проверяется и превращается в
//! ответ плеера:
//! - `hello` → договор о версии → `hello_accepted` (или отказ «обновите»);
//! - `open_session` → `SessionControl::open_session` → `session_granted`
//!   (порт + одноразовый пропуск) или отказ с причиной;
//! - битое сообщение → `refused{malformed_message}` с ВИДОМ ошибки, без текста
//!   из сообщения (данные врага в журнал и ответ не попадают).
//!
//! Стойка не знает про D-Bus и окна: приложению она возвращает [`EntryEvent`]
//! («сеанс открыт — поставь обычный плеер на паузу, покажи окно»). Свои
//! возможности плеер сообщает через `Hello` из конструктора: стойка их не
//! придумывает.
//!
//! Инвариант 3: стойка нужна только для открытия. Исчезнет вызывающий — сеанс
//! по WebSocket продолжится.

use std::collections::VecDeque;
use std::fmt;
use std::time::Instant;

use browser_bridge_protocol::{
    CURRENT_PROTOCOL_VERSION, ControlRefusal, ExtensionControlMessage, GrantOutcome, Hello,
    HelloAccepted, OpenSessionRequest, PageIdentity, PlaybackHints, PlayerControlMessage,
    ProtocolVersion, RefusalReason, ReplyTarget, SessionGrant, SessionIntent, SessionKey,
    decode_message, encode_message, negotiate_protocol_version,
};

use crate::policy::BrowserSessionPolicy;
use crate::registry::{SessionInstance, SessionRef};
use crate::server::{OpenSessionError, SessionControl};

/// Самое длинное имя звонящего: предел D-Bus для уникальных и известных имён.
const MAX_CALLER_BYTES: usize = 255;

/// Ответ на случай, если не удалось записать обычный (недостижимо: все типы
/// ответов уже проверены, но ответ нужен всегда). Равен записи
/// `refused{unparsed_message, player_unavailable}` — это закреплено тестом.
const FALLBACK_REPLY_JSON: &str = r#"{"type":"refused","in_reply_to":{"kind":"unparsed_message"},"reason":{"kind":"player_unavailable"}}"#;

/// Звонящий не прошёл проверку имени.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EntryCallerError {
    /// Пустое имя.
    #[error("entry caller id is empty")]
    Empty,
    /// Длиннее предела D-Bus.
    #[error("entry caller id is too long")]
    TooLong,
    /// Управляющий символ в имени.
    #[error("entry caller id contains a control character")]
    ControlCharacter,
}

/// Непрозрачный идентификатор звонящего (в D-Bus — уникальное имя соединения).
/// Нужен, чтобы помнить, кто уже сказал `hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryCaller(String);

impl EntryCaller {
    /// Проверяет и запоминает идентификатор.
    ///
    /// # Errors
    /// [`EntryCallerError`] — пустой, слишком длинный или с управляющими символами.
    pub fn new(identifier: impl Into<String>) -> Result<Self, EntryCallerError> {
        let identifier = identifier.into();
        if identifier.is_empty() {
            return Err(EntryCallerError::Empty);
        }
        if identifier.len() > MAX_CALLER_BYTES {
            return Err(EntryCallerError::TooLong);
        }
        if identifier.chars().any(char::is_control) {
            return Err(EntryCallerError::ControlCharacter);
        }
        Ok(Self(identifier))
    }

    /// Идентификатор строкой.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// О чём стойка сообщает приложению сверх ответа.
#[derive(Clone, PartialEq, Eq)]
pub enum EntryEvent {
    /// Сеанс открыт (начат, повторён или заменил прежний).
    SessionOpened {
        /// Ключ сеанса.
        key: SessionKey,
        /// Экземпляр сеанса: по нему отличают события этого сеанса от событий
        /// прежнего сеанса с тем же ключом (`ProcessLifecycle`, приёмник байт).
        instance: SessionInstance,
        /// Страница-источник (заголовок и домен — для окна).
        page: PageIdentity,
        /// Как показывать.
        intent: SessionIntent,
        /// Подсказки (длительность, позиция).
        hints: PlaybackHints,
        /// Что произошло с сеансом.
        outcome: GrantOutcome,
        /// Экземпляр, который это открытие вытеснило (`outcome = Replaced`).
        /// Его вкладка ещё в «коридоре прощания»: приложение может отдать ей
        /// последнее сообщение через `SessionControl::bid_farewell_to_replaced`
        /// (решение 127), иначе сервер закроет её 4001 сам по сроку.
        replaced: Option<SessionRef>,
    },
}

/// Заголовок и домен страницы — данные пользователя: в Debug и журнал не попадают.
impl fmt::Debug for EntryEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionOpened {
                key,
                instance,
                intent,
                outcome,
                replaced,
                page: _,
                hints: _,
            } => formatter
                .debug_struct("SessionOpened")
                .field("key", key)
                .field("instance", instance)
                .field("intent", intent)
                .field("outcome", outcome)
                .field("replaced", replaced)
                .field("page", &"<redacted>")
                .finish_non_exhaustive(),
        }
    }
}

/// Итог одного запроса.
#[derive(Clone, PartialEq, Eq)]
pub struct EntryOutcome {
    /// Ответ — JSON `PlayerControlMessage`, готовый к отправке посреднику.
    pub reply_json: String,
    /// Событие для приложения, если оно есть.
    pub event: Option<EntryEvent>,
}

/// Ответ содержит одноразовый пропуск сеанса: в Debug и журнал не попадает.
impl fmt::Debug for EntryOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EntryOutcome")
            .field("reply_json_bytes", &self.reply_json.len())
            .field("event", &self.event)
            .finish()
    }
}

/// Звонящий, прошедший `hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RememberedCaller {
    caller: EntryCaller,
    /// Договорённая версия — пригодится, когда minor начнут расти (1.1+).
    agreed_protocol: ProtocolVersion,
}

/// Стойка входа.
pub struct EntryDesk {
    player_hello: Hello,
    control: SessionControl,
    /// Прошедшие `hello`; самый свежий в конце. Ограничено: давнишних забываем.
    remembered: VecDeque<RememberedCaller>,
    max_remembered_callers: usize,
}

impl EntryDesk {
    /// Стойка с готовым `Hello` плеера (версия ПО и возможности).
    #[must_use]
    pub fn new(
        player_hello: Hello,
        control: SessionControl,
        policy: &BrowserSessionPolicy,
    ) -> Self {
        Self {
            player_hello,
            control,
            remembered: VecDeque::new(),
            max_remembered_callers: policy.max_remembered_callers,
        }
    }

    /// Превращает одно сырое сообщение в ответ.
    pub fn handle(&mut self, caller: &EntryCaller, message: &[u8], now: Instant) -> EntryOutcome {
        match decode_message::<ExtensionControlMessage>(message) {
            Err(error) => {
                tracing::warn!(kind = ?error.kind(), "entry request is malformed");
                refuse(
                    ReplyTarget::UnparsedMessage {},
                    RefusalReason::MalformedMessage {
                        error: error.kind(),
                    },
                )
            }
            Ok(ExtensionControlMessage::Hello(hello)) => self.on_hello(caller, &hello),
            Ok(ExtensionControlMessage::OpenSession(request)) => {
                self.on_open_session(caller, request, now)
            }
        }
    }

    fn on_hello(&mut self, caller: &EntryCaller, extension: &Hello) -> EntryOutcome {
        // Старая запись звонящего заменяется в любом случае: итог последнего hello главный.
        self.forget(caller);
        match negotiate_protocol_version(CURRENT_PROTOCOL_VERSION, extension.protocol) {
            Ok(agreed_protocol) => {
                self.remember(caller, agreed_protocol);
                reply(&PlayerControlMessage::HelloAccepted(HelloAccepted {
                    agreed_protocol,
                    player: self.player_hello.clone(),
                }))
            }
            Err(incompatible) => refuse(
                ReplyTarget::Hello {},
                RefusalReason::IncompatibleVersion {
                    update: incompatible.update,
                },
            ),
        }
    }

    fn on_open_session(
        &mut self,
        caller: &EntryCaller,
        request: OpenSessionRequest,
        now: Instant,
    ) -> EntryOutcome {
        let target = ReplyTarget::OpenSession {
            request_id: request.request_id,
        };
        if !self.is_remembered(caller) {
            return refuse(target, RefusalReason::HelloRequired {});
        }
        match self.control.open_session(&request, now) {
            Ok(opened) => {
                let grant = SessionGrant {
                    request_id: request.request_id,
                    session_key: request.session_key.clone(),
                    port: self.control.port(),
                    token: opened.token,
                    outcome: opened.outcome,
                };
                let mut outcome = reply(&PlayerControlMessage::SessionGranted(grant));
                outcome.event = Some(EntryEvent::SessionOpened {
                    key: request.session_key,
                    instance: opened.instance,
                    page: request.page,
                    intent: request.intent,
                    hints: request.hints,
                    outcome: opened.outcome,
                    replaced: opened.replaced,
                });
                outcome
            }
            Err(OpenSessionError::ShuttingDown) => {
                refuse(target, RefusalReason::PlayerShuttingDown {})
            }
            Err(OpenSessionError::TokenSource(error)) => {
                tracing::error!(%error, "cannot open a browser session");
                refuse(target, RefusalReason::PlayerUnavailable {})
            }
        }
    }

    fn is_remembered(&self, caller: &EntryCaller) -> bool {
        self.remembered.iter().any(|known| &known.caller == caller)
    }

    fn forget(&mut self, caller: &EntryCaller) {
        self.remembered.retain(|known| &known.caller != caller);
    }

    /// Запоминает звонящего; при переполнении забывает самого давнего.
    fn remember(&mut self, caller: &EntryCaller, agreed_protocol: ProtocolVersion) {
        while self.remembered.len() >= self.max_remembered_callers.max(1) {
            self.remembered.pop_front();
        }
        self.remembered.push_back(RememberedCaller {
            caller: caller.clone(),
            agreed_protocol,
        });
    }
}

/// Ответ без события.
fn reply(message: &PlayerControlMessage) -> EntryOutcome {
    let reply_json = encode_message(message).unwrap_or_else(|error| {
        tracing::error!(%error, "entry reply failed protocol validation");
        FALLBACK_REPLY_JSON.to_owned()
    });
    EntryOutcome {
        reply_json,
        event: None,
    }
}

/// Отказ с причиной.
fn refuse(in_reply_to: ReplyTarget, reason: RefusalReason) -> EntryOutcome {
    reply(&PlayerControlMessage::Refused(ControlRefusal {
        in_reply_to,
        reason,
    }))
}

#[cfg(test)]
mod tests;
