//! Ящик процесса браузерного режима: всё, что приходит из чужих потоков, ждёт здесь
//! главного цикла.
//!
//! Источники два, и оба не должны блокироваться:
//! - служба D-Bus «входа» (`desktop-integration`) кладёт звонок посредника и ждёт
//!   ответа со своим таймаутом — если главный цикл завис, звонящий получит
//!   «не отвечает», как у пересылки второго запуска (UX13);
//! - сервер WebSocket (`browser-bridge-session`) кладёт сообщения страницы и события
//!   жизни сеанса.
//!
//! Правила ёмкости разные по смыслу:
//! - звонков «входа» мало, очередь короткая; переполнение = цикл не успевает →
//!   `Full` (звонящий получит «не отвечает»), а не тихая потеря;
//! - сообщения страницы ограничены; переполнение возвращает `EventSinkFull`, сервер
//!   его считает и пишет в отчёт;
//! - события жизни сеанса не теряются никогда (их единицы на сеанс).
//!
//! После [`BrowserSessionInbox::close`] новые звонки получают `Closed`
//! (звонящий — `ShuttingDown`), а недоставленные слоты ответа уничтожаются, и их
//! звонящие тоже получают `ShuttingDown`: ни один звонок не теряется молча.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Instant;

use browser_bridge_protocol::PageSessionMessage;
use browser_bridge_session::{EventSinkFull, SessionEventSink, SessionLifecycleEvent, SessionRef};
use desktop_integration::{BrowserEntryDelivery, BrowserEntrySink, BrowserEntrySinkError};
use tracing::warn;

/// Разбудить главный цикл: в ящике что-то появилось.
///
/// В сессии 02 это [`ParkingWake`] безоконного цикла; окно (04) подставит свой
/// вариант поверх `EventLoopProxy`, не меняя ящик.
pub(crate) trait BrowserSessionWake: Send + Sync {
    /// Сообщает циклу, что есть работа. Не блокирует.
    fn wake(&self);
}

/// Ёмкости очередей ящика.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InboxCapacity {
    /// Звонков «входа», ждущих цикла. Посредник звонит по одному (`hello`, затем
    /// `open_session`); 8 — запас на повторы и на два браузера сразу, как у ящика
    /// пересылки второго запуска.
    pub(crate) entry_deliveries: usize,
    /// Сообщений страницы, ждущих цикла. Страница шлёт их единицами в секунду
    /// (события MSE, перемотка, меню); 256 — это минуты зависшего цикла.
    pub(crate) page_messages: usize,
}

impl Default for InboxCapacity {
    fn default() -> Self {
        Self {
            entry_deliveries: 8,
            page_messages: 256,
        }
    }
}

/// Сообщение страницы вместе с сеансом (ключ + номер экземпляра), к которому оно относится.
///
/// В сессии 02 цикл сообщения только считает; разбирать их будет источник байтов (03).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PageMessageEnvelope {
    pub(crate) session: SessionRef,
    pub(crate) message: PageSessionMessage,
}

/// Всё, что цикл забирает за один проход.
#[derive(Debug, Default)]
pub(crate) struct InboxBatch {
    pub(crate) entry_deliveries: Vec<BrowserEntryDelivery>,
    pub(crate) lifecycle_events: Vec<SessionLifecycleEvent>,
    pub(crate) page_messages: Vec<PageMessageEnvelope>,
}

impl InboxBatch {
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entry_deliveries.is_empty()
            && self.lifecycle_events.is_empty()
            && self.page_messages.is_empty()
    }
}

#[derive(Debug, Default)]
struct InboxState {
    entry_deliveries: VecDeque<BrowserEntryDelivery>,
    lifecycle_events: Vec<SessionLifecycleEvent>,
    page_messages: VecDeque<PageMessageEnvelope>,
    closed: bool,
}

/// Ящик процесса браузерного режима.
pub(crate) struct BrowserSessionInbox {
    capacity: InboxCapacity,
    state: Mutex<InboxState>,
    wake: Arc<dyn BrowserSessionWake>,
}

impl BrowserSessionInbox {
    pub(crate) fn new(capacity: InboxCapacity, wake: Arc<dyn BrowserSessionWake>) -> Self {
        Self {
            capacity,
            state: Mutex::new(InboxState::default()),
            wake,
        }
    }

    /// Забирает всё накопленное. Порядок внутри каждой очереди сохраняется.
    pub(crate) fn take_batch(&self) -> InboxBatch {
        let mut state = self.lock_state();
        InboxBatch {
            entry_deliveries: state.entry_deliveries.drain(..).collect(),
            lifecycle_events: std::mem::take(&mut state.lifecycle_events),
            page_messages: state.page_messages.drain(..).collect(),
        }
    }

    /// Закрывает ящик: дальше звонки получают `Closed`, а ждущие слоты ответа
    /// уничтожаются (их звонящие получают `ShuttingDown` от транспорта).
    pub(crate) fn close(&self) {
        let abandoned = {
            let mut state = self.lock_state();
            state.closed = true;
            state.page_messages.clear();
            std::mem::take(&mut state.entry_deliveries)
        };
        // Слоты уничтожаются вне замка: их Drop будит асинхронный обработчик D-Bus.
        drop(abandoned);
    }

    /// Отравленный замок означает панику в другом потоке посреди короткой правки
    /// очередей; сами очереди при этом целы, поэтому продолжаем с ними.
    fn lock_state(&self) -> MutexGuard<'_, InboxState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl BrowserEntrySink for BrowserSessionInbox {
    fn deliver(&self, delivery: BrowserEntryDelivery) -> Result<(), BrowserEntrySinkError> {
        {
            let mut state = self.lock_state();
            if state.closed {
                return Err(BrowserEntrySinkError::Closed);
            }
            if state.entry_deliveries.len() >= self.capacity.entry_deliveries {
                return Err(BrowserEntrySinkError::Full);
            }
            state.entry_deliveries.push_back(delivery);
        }
        self.wake.wake();
        Ok(())
    }
}

impl SessionEventSink for BrowserSessionInbox {
    fn publish_page_message(
        &self,
        session: &SessionRef,
        message: PageSessionMessage,
    ) -> Result<(), EventSinkFull> {
        {
            let mut state = self.lock_state();
            if state.closed {
                // Процесс уже выходит: сообщение никому не нужно, но потеря видна.
                return Err(EventSinkFull);
            }
            if state.page_messages.len() >= self.capacity.page_messages {
                return Err(EventSinkFull);
            }
            state.page_messages.push_back(PageMessageEnvelope {
                session: session.clone(),
                message,
            });
        }
        self.wake.wake();
        Ok(())
    }

    fn publish_lifecycle(&self, event: SessionLifecycleEvent) {
        {
            let mut state = self.lock_state();
            if state.closed {
                warn!(?event, "Session lifecycle event after the inbox closed");
                return;
            }
            state.lifecycle_events.push(event);
        }
        self.wake.wake();
    }
}

/// Пробуждение безоконного цикла: флаг под замком и условная переменная.
///
/// Флаг поднимается ПОСЛЕ записи в ящик и снимается ДО того, как цикл забирает
/// ящик, поэтому событие, пришедшее между «забрал» и «уснул», не теряется: цикл
/// увидит поднятый флаг и сразу проснётся.
#[derive(Debug, Default)]
pub(crate) struct ParkingWake {
    pending: Mutex<bool>,
    signal: Condvar,
}

impl ParkingWake {
    /// Ждёт пробуждения или срока `deadline` (без срока — только пробуждения).
    pub(crate) fn wait(&self, deadline: Option<Instant>) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while !*pending {
            let Some(deadline) = deadline else {
                pending = self
                    .signal
                    .wait(pending)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                continue;
            };
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            pending = self
                .signal
                .wait_timeout(pending, deadline - now)
                .map(|(guard, _timeout)| guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner().0);
        }
        *pending = false;
    }
}

impl BrowserSessionWake for ParkingWake {
    fn wake(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *pending = true;
        self.signal.notify_one();
    }
}

#[cfg(test)]
mod tests;
