//! Два независимых потока worker -> UI: lifecycle-события и агрегат tick-телеметрии.
//!
//! Раньше всё ехало по одному bounded каналу. Tick-телеметрия публикуется каждый
//! tick (при вращающемся decoder-е — раз в ~2 мс), быстро забивала канал, и
//! одноразовые lifecycle-события (`AudioPlaybackResumed`, `MediaOpened`,
//! `SeekCommitted`, `FatalError`, ...) молча выбрасывались как «канал полон».
//!
//! Теперь:
//! - **lifecycle** (`Player`/`Scrub`/`RenderError`) — упорядоченная bounded очередь,
//!   в которую телеметрия больше не пишет. Переполнение — громкий `warn!` с видом
//!   события, а не тихий `debug!`;
//! - **tick-телеметрия** — «почтовый ящик» с одним слотом-агрегатом. Это аддитивные
//!   счётчики (UI их накапливает), поэтому новые tick-и *вливаются* в ещё не
//!   забранный агрегат (`PlayerTickResult::absorb`), а не вытесняют друг друга.
//!
//! Инвариант: worker никогда не ждёт UI. Публикация tick-а использует `try_lock`;
//! если UI в этот момент держит слот (он держит его на одно `Option::take`), порция
//! остаётся в локальном `carry` worker-а и уйдёт со следующим tick-ом.

use std::sync::{Arc, Mutex, PoisonError, TryLockError};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use tracing::{debug, warn};

use super::PlayerWorkerEvent;
use crate::PlayerTickResult;

/// Ёмкость очереди lifecycle-событий. Их мало (единицы на действие пользователя),
/// 512 хватает с большим запасом даже если UI не рисует несколько секунд.
pub(super) const LIFECYCLE_EVENT_CAPACITY: usize = 512;

/// Общий слот агрегата tick-телеметрии.
type TickTelemetrySlot = Arc<Mutex<Option<PlayerTickResult>>>;

/// Создаёт пару publisher (worker) / consumer (UI) с ёмкостью lifecycle-очереди по умолчанию.
pub(super) fn worker_event_stream() -> (WorkerEventPublisher, WorkerEventConsumer) {
    worker_event_stream_with_lifecycle_capacity(LIFECYCLE_EVENT_CAPACITY)
}

/// То же, но с явной ёмкостью lifecycle-очереди (для тестов переполнения).
pub(super) fn worker_event_stream_with_lifecycle_capacity(
    lifecycle_capacity: usize,
) -> (WorkerEventPublisher, WorkerEventConsumer) {
    let (lifecycle_tx, lifecycle_rx) = bounded(lifecycle_capacity);
    let tick_slot: TickTelemetrySlot = Arc::new(Mutex::new(None));
    (
        WorkerEventPublisher {
            lifecycle_tx,
            tick_slot: Arc::clone(&tick_slot),
            tick_carry: PlayerTickResult::default(),
        },
        WorkerEventConsumer {
            lifecycle_rx,
            tick_slot,
        },
    )
}

/// Сторона worker-а: публикует события без блокировки.
pub(super) struct WorkerEventPublisher {
    /// Очередь lifecycle-событий.
    lifecycle_tx: Sender<PlayerWorkerEvent>,

    /// Слот агрегата, общий с consumer-ом.
    tick_slot: TickTelemetrySlot,

    /// Телеметрия, которую не удалось положить в слот (UI держал lock).
    /// Ограничена тем же лимитом списков, что и агрегат (`absorb`).
    tick_carry: PlayerTickResult,
}

impl WorkerEventPublisher {
    /// Публикует lifecycle-событие; при полной очереди пишет `warn!` с видом события.
    pub(super) fn publish_lifecycle(&self, event: PlayerWorkerEvent) {
        let Err(error) = self.lifecycle_tx.try_send(event) else {
            return;
        };
        match error {
            TrySendError::Full(lost_event) => warn!(
                event = ?lost_event,
                capacity = ?self.lifecycle_tx.capacity(),
                "Player worker lifecycle event queue is full; event lost"
            ),
            TrySendError::Disconnected(_) => debug!("Player worker event receiver dropped"),
        }
    }

    /// Вливает tick в ещё не забранный агрегат. Не блокируется: если слот занят UI,
    /// порция копится в `tick_carry` и уходит со следующим вызовом.
    pub(super) fn publish_tick(&mut self, tick_result: PlayerTickResult) {
        self.tick_carry.absorb(tick_result);
        if self.tick_carry.is_empty() {
            return;
        }
        let mut slot = match self.tick_slot.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::WouldBlock) => return,
            // Критические секции слота (`take`/`absorb`) не паникуют и атомарны по
            // инварианту, поэтому данные после poison валидны.
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        let carried = std::mem::take(&mut self.tick_carry);
        slot.get_or_insert_with(PlayerTickResult::default)
            .absorb(carried);
    }
}

/// Сторона UI: забирает события и агрегат.
pub(super) struct WorkerEventConsumer {
    /// Очередь lifecycle-событий.
    lifecycle_rx: Receiver<PlayerWorkerEvent>,

    /// Слот агрегата, общий с publisher-ом.
    tick_slot: TickTelemetrySlot,
}

impl WorkerEventConsumer {
    /// Забирает накопленные lifecycle-события в порядке публикации.
    pub(super) fn drain_lifecycle_events(&self) -> Vec<PlayerWorkerEvent> {
        self.lifecycle_rx.try_iter().collect()
    }

    /// Забирает агрегат tick-телеметрии (или `None`, если с прошлого раза ничего не пришло).
    pub(super) fn take_tick_telemetry(&self) -> Option<PlayerTickResult> {
        self.tick_slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

#[cfg(test)]
impl WorkerEventConsumer {
    /// Удерживает lock слота, имитируя UI в середине `take_tick_telemetry`.
    pub(super) fn hold_tick_slot_for_tests(
        &self,
    ) -> std::sync::MutexGuard<'_, Option<PlayerTickResult>> {
        self.tick_slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}
