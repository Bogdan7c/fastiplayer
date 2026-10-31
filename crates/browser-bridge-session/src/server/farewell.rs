//! «Прощание» с соединением (решение 127): последнее сообщение плеера странице
//! перед кадром закрытия.
//!
//! Два владельца, оба живут под замком состояния сервера:
//! - [`FarewellSlot`] — отдельное место для ОДНОГО прощального сообщения на
//!   соединение. Не очередь исходящих: прощание не может получить «очередь
//!   полна», а поток соединения пишет его строго после всей очереди и прямо
//!   перед кадром закрытия;
//! - [`FarewellCorridors`] — память о недавно ЗАМЕНЁННЫХ экземплярах сеанса.
//!   Пока экземпляр «в коридоре», его соединение ещё открыто и ждёт прощания
//!   (не дольше `farewell_grace`); от страницы в это время ничего не принимается
//!   (калитка доставки уже закрыта концом сеанса).
//!
//! Таблица коридоров ограничена `max_farewell_corridors`: при переполнении
//! забывается самый старый экземпляр, а если его соединение ещё ждёт прощания —
//! вызывающий обязан закрыть его кодом 4001 сразу (возвращаемый номер
//! соединения). Так поток замен не копит потоки соединений.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::policy::BrowserSessionPolicy;
use crate::registry::{ConnectionId, SessionRef};

/// Место для прощального сообщения одного соединения (готовый JSON).
///
/// Кладёт сервер под своим замком ДО просьбы закрыть соединение; забирает поток
/// соединения, когда закрывается (после очереди исходящих, перед кадром
/// закрытия). Порядок «положили → попросили закрыть» вместе с чтением просьбы
/// (`Acquire`) гарантирует, что поток увидит прощание.
#[derive(Debug, Default)]
pub(super) struct FarewellSlot {
    message_json: Mutex<Option<String>>,
}

impl FarewellSlot {
    /// Кладёт прощание. Сервер зовёт это не больше раза на соединение: сразу
    /// следом идёт просьба закрыть, а после неё прощание уже не принимается.
    pub(super) fn leave(&self, message_json: String) {
        *self
            .message_json
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(message_json);
    }

    /// Забирает прощание, если оно есть (поток соединения при закрытии).
    pub(super) fn take(&self) -> Option<String> {
        self.message_json
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/// Что сейчас с заменённым экземпляром.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CorridorState {
    /// Соединение открыто и ждёт прощания до срока.
    Open {
        connection: ConnectionId,
        deadline: Instant,
    },
    /// Коридор закрыт: прощание принято, срок истёк, соединение ушло само или
    /// сервер выключается.
    Closed,
    /// В момент замены у экземпляра не было подключённого соединения.
    NoConnection,
}

/// Запись о заменённом экземпляре.
#[derive(Debug, Clone)]
struct ReplacedInstance {
    session: SessionRef,
    state: CorridorState,
}

/// Что нашлось по просьбе попрощаться с экземпляром.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CorridorClaim {
    /// Коридор был открыт и теперь занят этим прощанием: вызывающий кладёт
    /// прощание в слот соединения и просит закрыть его кодом 4001.
    Claimed(ConnectionId),
    /// Экземпляр помним, но коридор уже закрыт.
    AlreadyClosed,
    /// Экземпляр помним, но соединения у него не было.
    NoConnection,
    /// Такого заменённого экземпляра не помним (чужой, активный, слишком старый).
    Unknown,
}

/// Таблица коридоров прощания.
#[derive(Debug)]
pub(super) struct FarewellCorridors {
    /// Заменённые экземпляры; самый старый — в начале.
    remembered: VecDeque<ReplacedInstance>,
    capacity: usize,
    grace: Duration,
}

impl FarewellCorridors {
    pub(super) fn new(policy: &BrowserSessionPolicy) -> Self {
        Self {
            remembered: VecDeque::new(),
            // Хотя бы один: иначе заменённый экземпляр забывался бы сразу.
            capacity: policy.max_farewell_corridors.max(1),
            grace: policy.farewell_grace,
        }
    }

    /// Запоминает заменённый экземпляр; с соединением — открывает коридор до
    /// `now + farewell_grace`.
    ///
    /// Возвращает соединение, которое надо закрыть кодом 4001 СЕЙЧАС: самый
    /// старый коридор, вытесненный пределом таблицы.
    #[must_use = "an evicted open corridor must be closed by the caller"]
    pub(super) fn remember_replaced(
        &mut self,
        session: SessionRef,
        connection: Option<ConnectionId>,
        now: Instant,
    ) -> Option<ConnectionId> {
        let mut evicted = None;
        while self.remembered.len() >= self.capacity {
            if let Some(oldest) = self.remembered.pop_front()
                && let CorridorState::Open { connection, .. } = oldest.state
            {
                evicted = Some(connection);
            }
        }
        let state = match connection {
            Some(connection) => CorridorState::Open {
                connection,
                deadline: now + self.grace,
            },
            None => CorridorState::NoConnection,
        };
        self.remembered
            .push_back(ReplacedInstance { session, state });
        evicted
    }

    /// Занимает коридор этого экземпляра под прощание (открытый → закрытый).
    pub(super) fn claim(&mut self, session: &SessionRef) -> CorridorClaim {
        let Some(record) = self
            .remembered
            .iter_mut()
            .find(|record| &record.session == session)
        else {
            return CorridorClaim::Unknown;
        };
        match record.state {
            CorridorState::Open { connection, .. } => {
                record.state = CorridorState::Closed;
                CorridorClaim::Claimed(connection)
            }
            CorridorState::Closed => CorridorClaim::AlreadyClosed,
            CorridorState::NoConnection => CorridorClaim::NoConnection,
        }
    }

    /// Закрывает коридоры, чей срок наступил; возвращает их соединения
    /// (вызывающий закрывает их кодом 4001 без прощания).
    pub(super) fn expire(&mut self, now: Instant) -> Vec<ConnectionId> {
        let mut expired = Vec::new();
        for record in &mut self.remembered {
            if let CorridorState::Open {
                connection,
                deadline,
            } = record.state
                && deadline <= now
            {
                record.state = CorridorState::Closed;
                expired.push(connection);
            }
        }
        expired
    }

    /// Соединение завершилось (страница закрыла сама, обрыв): его коридор закрыт.
    pub(super) fn connection_released(&mut self, released: ConnectionId) {
        for record in &mut self.remembered {
            if matches!(record.state, CorridorState::Open { connection, .. } if connection == released)
            {
                record.state = CorridorState::Closed;
            }
        }
    }

    /// Выключение сервера: закрывает все открытые коридоры и возвращает их
    /// соединения (вызывающий закрывает их кодом 4001 — см. `begin_shutdown`).
    pub(super) fn close_all(&mut self) -> Vec<ConnectionId> {
        let mut closed = Vec::new();
        for record in &mut self.remembered {
            if let CorridorState::Open { connection, .. } = record.state {
                record.state = CorridorState::Closed;
                closed.push(connection);
            }
        }
        closed
    }
}

#[cfg(test)]
mod tests;
