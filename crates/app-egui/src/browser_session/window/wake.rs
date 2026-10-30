//! Пробуждение главного цикла окна: ящик сеанса будит цикл winit.
//!
//! В безоконном процессе ящик будит [`ParkingWake`](super::super::inbox::ParkingWake);
//! с окном — событие `AppWakeOwner::BrowserSession` через тот же `EventLoopProxy`, что
//! у всех владельцев обычного окна (`app_wake`). Флаг «уже разбужен» сливает наводнение
//! пробуждений в одно событие; цикл снимает его прямо перед шагом ядра
//! (`clear_pending_for_drain`), так что пробуждение во время шага не теряется.

use super::super::inbox::BrowserSessionWake;
use crate::app_wake::AppWakePort;

/// Пробуждение цикла окна для ящика, потока приёма и запросов байтов.
pub(super) struct EventLoopSessionWake {
    port: AppWakePort,
}

impl EventLoopSessionWake {
    /// `port` — порт владельца `AppWakeOwner::BrowserSession`.
    pub(super) fn new(port: AppWakePort) -> Self {
        Self { port }
    }
}

impl BrowserSessionWake for EventLoopSessionWake {
    fn wake(&self) {
        // Закрытый цикл (выход) — не ошибка: будить уже некого.
        let _delivery = self.port.request_wake();
    }
}
