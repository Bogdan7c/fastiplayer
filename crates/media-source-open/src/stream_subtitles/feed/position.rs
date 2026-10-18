//! Общая ячейка позиции воспроизведения: приложение пишет, лента читает и просыпается.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// Что лента видит в ячейке в момент чтения.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PositionSnapshot {
    /// Позиция поиска реплик на шкале плеера.
    pub(super) position: Duration,
    /// Счётчик «меня стоит разбудить»: растёт при прыжке и при закрытии ленты.
    pub(super) wake_epoch: u64,
}

#[derive(Debug)]
struct PositionState {
    position: Duration,
    wake_epoch: u64,
}

#[derive(Debug)]
struct PositionShared {
    state: Mutex<PositionState>,
    changed: Condvar,
}

/// Ячейка позиции, которую приложение обновляет, а лента субтитров читает.
///
/// - [`advance_to`](Self::advance_to) - обычное движение вперёд (каждый тик): ленту НЕ будит,
///   она подхватит позицию в очередной проверке;
/// - [`jump_to`](Self::jump_to) - seek/DVR/смена скорости с разрывом: будит ленту сразу, чтобы
///   она пересчитала окно вокруг новой позиции.
///
/// Клонируется дёшево; все клоны видят одно значение.
#[derive(Clone, Debug)]
pub struct StreamPlaybackPosition {
    shared: Arc<PositionShared>,
}

impl StreamPlaybackPosition {
    /// Создаёт ячейку с начальной позицией.
    #[must_use]
    pub fn new(initial_position: Duration) -> Self {
        Self {
            shared: Arc::new(PositionShared {
                state: Mutex::new(PositionState {
                    position: initial_position,
                    wake_epoch: 0,
                }),
                changed: Condvar::new(),
            }),
        }
    }

    /// Обычное продвижение позиции (без пробуждения ленты).
    pub fn advance_to(&self, position: Duration) {
        self.lock().position = position;
    }

    /// Разрыв позиции (seek): записывает её и будит ленту.
    pub fn jump_to(&self, position: Duration) {
        {
            let mut state = self.lock();
            state.position = position;
            state.wake_epoch = state.wake_epoch.wrapping_add(1);
        }
        self.shared.changed.notify_all();
    }

    /// Текущая позиция.
    #[must_use]
    pub fn position(&self) -> Duration {
        self.lock().position
    }

    /// Будит ожидающую ленту без изменения позиции (закрытие ленты).
    pub(super) fn wake_waiters(&self) {
        {
            let mut state = self.lock();
            state.wake_epoch = state.wake_epoch.wrapping_add(1);
        }
        self.shared.changed.notify_all();
    }

    /// Снимок для планировщика ленты.
    pub(super) fn snapshot(&self) -> PositionSnapshot {
        let state = self.lock();
        PositionSnapshot {
            position: state.position,
            wake_epoch: state.wake_epoch,
        }
    }

    /// Спит до пробуждения (рост `wake_epoch` сверх `seen_epoch`) или до истечения `timeout`.
    ///
    /// Если эпоха уже изменилась после последнего снимка, возвращается сразу: пробуждение
    /// между проверкой и ожиданием не теряется.
    pub(super) fn wait_for_wake(&self, seen_epoch: u64, timeout: Duration) {
        let guard = self.lock();
        // Результат (таймаут или пробуждение) не важен: после ожидания лента всё равно
        // перечитывает состояние; отравленный мьютекс тоже не мешает завершению.
        let _ = self
            .shared
            .changed
            .wait_timeout_while(guard, timeout, |state| state.wake_epoch == seen_epoch);
    }

    fn lock(&self) -> MutexGuard<'_, PositionState> {
        // Состояние - два числа без инвариантов между потоками: отравление безвредно.
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
