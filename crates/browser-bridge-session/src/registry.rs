//! Таблица сеансов: единственный активный сеанс и его пропуск (решения 40, 85, 86).
//!
//! Чистая логика без ввода-вывода: время приходит аргументом `Instant`, соединения
//! обозначены номерами [`ConnectionId`]. Сервер вызывает методы под своим замком и
//! сам выполняет то, что решил реестр (закрыть соединение, послать событие).
//!
//! Жизнь сеанса:
//! ```text
//!  open ──► AwaitingAttach ──attach──► Attached ──обрыв──► Disconnected
//!               │  ▲                       │  ▲                │
//!               │  └──── open (тот же ключ)┘  └──── attach ────┘
//!               └─ срок first_attach_deadline / reconnect_grace ─► сеанса нет
//! ```
//! Инварианты:
//! - пропуск одноразовый: успешный `attach` его гасит, повторный `attach` с ним — 4004;
//! - сверка пропуска только через `SessionToken::matches` (постоянное время);
//! - отвергнутый `attach` НЕ меняет состояние (чужой запрос не должен портить сеанс);
//! - ошибка источника пропусков не меняет состояние (замена не происходит наполовину).

use std::time::{Duration, Instant};

use browser_bridge_protocol::{
    GrantOutcome, OpenSessionDecision, SessionCloseReason, SessionKey, SessionToken,
    decide_open_session,
};

use crate::policy::BrowserSessionPolicy;
use crate::sink::SessionEndReason;
use crate::token::{SessionTokenSource, TokenSourceError};

/// Номер TCP-соединения, выданный сервером. Не ключ сеанса: сеанс переживает
/// соединение (обрыв и возврат).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionId(pub u64);

/// Экземпляр сеанса: растущий номер, который реестр выдаёт при `StartNew` и
/// `ReplaceActive` (повтор `ReuseActive` его сохраняет). Ключ сеанса задаёт
/// экстеншен и может повториться (сеанс закончился, открыли снова), поэтому
/// событие «конец сеанса K» без экземпляра нельзя отличить от конца СЛЕДУЮЩЕГО K.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionInstance(pub u64);

/// Сеанс целиком: ключ + экземпляр.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    /// Ключ от экстеншена.
    pub key: SessionKey,
    /// Экземпляр от реестра.
    pub instance: SessionInstance,
}

/// Сроки, которыми живёт реестр (выжимка из [`BrowserSessionPolicy`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryTimings {
    /// Срок выданного сеанса без `attach`.
    pub first_attach_deadline: Duration,
    /// Срок ожидания возврата после обрыва.
    pub reconnect_grace: Duration,
}

impl From<&BrowserSessionPolicy> for RegistryTimings {
    fn from(policy: &BrowserSessionPolicy) -> Self {
        Self {
            first_attach_deadline: policy.first_attach_deadline,
            reconnect_grace: policy.reconnect_grace,
        }
    }
}

/// Вытесненный сеанс: его соединение (если было) надо закрыть кодом 4001.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacedSession {
    /// Вытесненный сеанс.
    pub session: SessionRef,
    /// Подключённое соединение вытесненного сеанса.
    pub connection: Option<ConnectionId>,
}

/// Итог `open_session`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryGrant {
    /// Экземпляр выданного сеанса.
    pub instance: SessionInstance,
    /// Пропуск для `attach` (при повторе в `AwaitingAttach` — тот же самый).
    pub token: SessionToken,
    /// Что произошло: начат, повторён, заменил прежний.
    pub outcome: GrantOutcome,
    /// Вытесненный сеанс, если заменили.
    pub replaced: Option<ReplacedSession>,
}

/// Закрытый по просьбе плеера сеанс.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedSession {
    /// Закрытый сеанс.
    pub session: SessionRef,
    /// Его подключённое соединение, если было.
    pub connection: Option<ConnectionId>,
}

/// Почему `attach` отвергнут. Порядок проверок зафиксирован: ключ, пропуск, занятость.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachRejection {
    /// Сеанса с таким ключом нет.
    UnknownSession,
    /// Пропуск не подошёл или уже использован.
    InvalidToken,
    /// К сеансу уже подключено живое соединение.
    AlreadyAttached,
}

impl AttachRejection {
    /// Код закрытия WebSocket для отказа.
    #[must_use]
    pub const fn close_reason(self) -> SessionCloseReason {
        match self {
            Self::UnknownSession => SessionCloseReason::UnknownSession,
            Self::InvalidToken => SessionCloseReason::InvalidToken,
            Self::AlreadyAttached => SessionCloseReason::AlreadyAttached,
        }
    }
}

/// Фаза активного сеанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Выдан, никто не подключился.
    AwaitingAttach { deadline: Instant },
    /// Подключено соединение.
    Attached { connection: ConnectionId },
    /// Соединение оборвалось, ждём возврата до срока.
    Disconnected { grace_deadline: Instant },
}

/// Активный сеанс.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveSession {
    key: SessionKey,
    instance: SessionInstance,
    /// Пропуск, с которым ещё можно подключиться; `None` — погашен.
    token: Option<SessionToken>,
    phase: Phase,
}

impl ActiveSession {
    fn session_ref(&self) -> SessionRef {
        SessionRef {
            key: self.key.clone(),
            instance: self.instance,
        }
    }
}

/// Реестр сеансов: владеет единственным активным сеансом.
#[derive(Debug)]
pub struct SessionRegistry {
    timings: RegistryTimings,
    active: Option<ActiveSession>,
    /// Номер для следующего нового экземпляра (с 1, только растёт).
    next_instance: u64,
}

impl SessionRegistry {
    /// Пустой реестр.
    #[must_use]
    pub fn new(timings: RegistryTimings) -> Self {
        Self {
            timings,
            active: None,
            next_instance: 1,
        }
    }

    /// Ключ активного сеанса.
    #[must_use]
    pub fn active_key(&self) -> Option<&SessionKey> {
        self.active.as_ref().map(|session| &session.key)
    }

    /// Подключённое соединение активного сеанса.
    #[must_use]
    pub fn attached_connection(&self) -> Option<ConnectionId> {
        self.active
            .as_ref()
            .and_then(|session| match session.phase {
                Phase::Attached { connection } => Some(connection),
                Phase::AwaitingAttach { .. } | Phase::Disconnected { .. } => None,
            })
    }

    /// Ближайший срок, к которому надо позвать [`Self::expire`].
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.active
            .as_ref()
            .and_then(|session| match session.phase {
                Phase::AwaitingAttach { deadline } => Some(deadline),
                Phase::Disconnected { grace_deadline } => Some(grace_deadline),
                Phase::Attached { .. } => None,
            })
    }

    /// Обрабатывает `open_session` по правилу `decide_open_session`.
    ///
    /// # Errors
    /// Источник пропусков отказал; состояние реестра не изменилось.
    pub fn open(
        &mut self,
        requested: &SessionKey,
        now: Instant,
        tokens: &mut dyn SessionTokenSource,
    ) -> Result<RegistryGrant, TokenSourceError> {
        let decision = decide_open_session(self.active_key(), requested);
        let outcome = decision.grant_outcome();
        match decision {
            OpenSessionDecision::StartNew => {
                let token = tokens.next_token()?;
                let session = self.awaiting_session(requested, &token, now);
                let instance = session.instance;
                self.active = Some(session);
                Ok(RegistryGrant {
                    instance,
                    token,
                    outcome,
                    replaced: None,
                })
            }
            OpenSessionDecision::ReplaceActive => {
                // Пропуск берём ДО замены: при отказе источника старый сеанс жив.
                let token = tokens.next_token()?;
                let replaced = self.active.take().map(|old| ReplacedSession {
                    connection: attached_connection_of(&old),
                    session: SessionRef {
                        key: old.key,
                        instance: old.instance,
                    },
                });
                let session = self.awaiting_session(requested, &token, now);
                let instance = session.instance;
                self.active = Some(session);
                Ok(RegistryGrant {
                    instance,
                    token,
                    outcome,
                    replaced,
                })
            }
            OpenSessionDecision::ReuseActive => self.reuse_active(now, tokens, outcome),
        }
    }

    /// Повторный `open_session` с ключом активного сеанса (идемпотентность).
    fn reuse_active(
        &mut self,
        now: Instant,
        tokens: &mut dyn SessionTokenSource,
        outcome: GrantOutcome,
    ) -> Result<RegistryGrant, TokenSourceError> {
        let deadline = now + self.timings.first_attach_deadline;
        let Some(session) = self.active.as_mut() else {
            // Недостижимо: ReuseActive значит «сеанс есть». Не паникуем, честно
            // сообщаем отказом источника, состояние не трогаем.
            return Err(TokenSourceError::EntropyUnavailable {
                reason: "reuse requested without an active session".to_owned(),
            });
        };
        let token = match (session.phase, session.token.clone()) {
            // Повтор до attach: тот же пропуск, второго сеанса нет; срок обновляем —
            // раз экстеншен повторяет запрос, он ещё заинтересован.
            (Phase::AwaitingAttach { .. }, Some(existing)) => {
                session.phase = Phase::AwaitingAttach { deadline };
                existing
            }
            // В грейсе: возврат, нужен новый пропуск; ждём подключения заново.
            (Phase::Disconnected { .. }, _) | (Phase::AwaitingAttach { .. }, None) => {
                let fresh = tokens.next_token()?;
                session.token = Some(fresh.clone());
                session.phase = Phase::AwaitingAttach { deadline };
                fresh
            }
            // Подключено: новый пропуск про запас, старое соединение живёт; attach
            // новым пропуском, пока старое живо, получит 4008.
            (Phase::Attached { .. }, _) => {
                let fresh = tokens.next_token()?;
                session.token = Some(fresh.clone());
                fresh
            }
        };
        Ok(RegistryGrant {
            instance: session.instance,
            token,
            outcome,
            replaced: None,
        })
    }

    /// Новый сеанс в ожидании подключения.
    fn awaiting_session(
        &mut self,
        key: &SessionKey,
        token: &SessionToken,
        now: Instant,
    ) -> ActiveSession {
        let instance = SessionInstance(self.next_instance);
        // u64 не кончится за жизнь процесса; saturating — чтобы не паниковать.
        self.next_instance = self.next_instance.saturating_add(1);
        ActiveSession {
            key: key.clone(),
            instance,
            token: Some(token.clone()),
            phase: Phase::AwaitingAttach {
                deadline: now + self.timings.first_attach_deadline,
            },
        }
    }

    /// Проверяет `attach`. Успех гасит пропуск и привязывает соединение;
    /// отказ состояние не меняет.
    ///
    /// # Errors
    /// [`AttachRejection`] — чем закрыть соединение.
    pub fn attach(
        &mut self,
        key: &SessionKey,
        presented: &SessionToken,
        connection: ConnectionId,
    ) -> Result<SessionRef, AttachRejection> {
        let session = match self.active.as_mut() {
            Some(session) if &session.key == key => session,
            _ => return Err(AttachRejection::UnknownSession),
        };
        let token_matches = session
            .token
            .as_ref()
            .is_some_and(|expected| expected.matches(presented));
        if !token_matches {
            return Err(AttachRejection::InvalidToken);
        }
        if matches!(session.phase, Phase::Attached { .. }) {
            return Err(AttachRejection::AlreadyAttached);
        }
        session.token = None;
        session.phase = Phase::Attached { connection };
        Ok(session.session_ref())
    }

    /// Подключённое соединение оборвалось без кадра закрытия: начинается грейс.
    /// `None` — это соединение не было подключённым (вытеснено, чужое).
    pub fn connection_lost(
        &mut self,
        connection: ConnectionId,
        now: Instant,
    ) -> Option<SessionRef> {
        let grace_deadline = now + self.timings.reconnect_grace;
        let session = self.active.as_mut()?;
        if session.phase != (Phase::Attached { connection }) {
            return None;
        }
        session.phase = Phase::Disconnected { grace_deadline };
        Some(session.session_ref())
    }

    /// Сеанс этого соединения закончился (страница закрыла его или мы закрыли
    /// из-за нарушения). `None` — соединение не принадлежит активному сеансу.
    pub fn end_session_of(&mut self, connection: ConnectionId) -> Option<SessionRef> {
        if self.attached_connection() != Some(connection) {
            return None;
        }
        self.active.take().map(|session| session.session_ref())
    }

    /// Завершает активный сеанс по просьбе плеера (выход, `close_active_session`).
    pub fn end_active(&mut self) -> Option<EndedSession> {
        self.active.take().map(|session| EndedSession {
            connection: attached_connection_of(&session),
            session: session.session_ref(),
        })
    }

    /// Применяет истёкшие сроки: возвращает завершённый сеанс и причину.
    pub fn expire(&mut self, now: Instant) -> Option<(SessionRef, SessionEndReason)> {
        let reason = match self.active.as_ref()?.phase {
            Phase::AwaitingAttach { deadline } if deadline <= now => {
                SessionEndReason::FirstAttachDeadlineExpired
            }
            Phase::Disconnected { grace_deadline } if grace_deadline <= now => {
                SessionEndReason::ReconnectGraceExpired
            }
            Phase::AwaitingAttach { .. } | Phase::Disconnected { .. } | Phase::Attached { .. } => {
                return None;
            }
        };
        self.active
            .take()
            .map(|session| (session.session_ref(), reason))
    }
}

/// Соединение сеанса, если он подключён.
fn attached_connection_of(session: &ActiveSession) -> Option<ConnectionId> {
    match session.phase {
        Phase::Attached { connection } => Some(connection),
        Phase::AwaitingAttach { .. } | Phase::Disconnected { .. } => None,
    }
}

#[cfg(test)]
mod tests;
