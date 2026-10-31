//! Сигнал «файл config-а мог измениться» между потоком слежения и UI-потоком.
//!
//! Payload здесь — один флаг: что именно изменилось, UI-поток узнаёт сам, перечитав файл.
//! Порядок тот же, что у остальных ящиков `app_wake`: издатель сначала поднимает флаг,
//! потом будит цикл; приёмник сначала гасит флаг пробуждения порта, потом забирает свой.
//! Так новое событие после `take_change` всегда снова разбудит цикл, а событие между
//! гашением и взятием даст лишь лишнее (безвредное) пробуждение.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::app_wake::{AppWakePort, WakeDelivery};

/// Общая часть издателя и приёмника.
struct ChangeSignalShared {
    /// «Файл мог измениться, а UI-поток ещё не смотрел».
    change_pending: AtomicBool,
    /// Порт пробуждения цикла winit владельца «файл config-а».
    wake_port: AppWakePort,
}

/// Сторона потока слежения.
#[derive(Clone)]
pub(crate) struct ConfigFileChangePublisher {
    shared: Arc<ChangeSignalShared>,
}

/// Сторона UI-потока.
pub(crate) struct ConfigFileChangeReceiver {
    shared: Arc<ChangeSignalShared>,
}

/// Что UI-поток узнал, забрав сигнал.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFileChange {
    /// Файл мог измениться: перечитать и сравнить.
    MayHaveChanged,
    /// Нового сигнала не было.
    NoChange,
}

/// Создаёт связанную пару «издатель → приёмник» поверх порта пробуждения.
pub(crate) fn config_file_change_signal(
    wake_port: AppWakePort,
) -> (ConfigFileChangePublisher, ConfigFileChangeReceiver) {
    let shared = Arc::new(ChangeSignalShared {
        change_pending: AtomicBool::new(false),
        wake_port,
    });
    (
        ConfigFileChangePublisher {
            shared: shared.clone(),
        },
        ConfigFileChangeReceiver { shared },
    )
}

impl ConfigFileChangePublisher {
    /// Сообщает «файл мог измениться» и будит цикл (поток наводнения сливается в одно
    /// пробуждение, пока UI-поток не забрал сигнал).
    pub(crate) fn publish_may_have_changed(&self) -> WakeDelivery {
        self.shared.change_pending.store(true, Ordering::Release);
        self.shared.wake_port.request_wake()
    }
}

impl ConfigFileChangeReceiver {
    /// Есть ли не забранный сигнал (для решения «перерисовать окно»), без изменения состояния.
    #[must_use]
    pub(crate) fn has_pending_change(&self) -> bool {
        self.shared.change_pending.load(Ordering::Acquire)
    }

    /// Забирает сигнал ровно один раз.
    pub(crate) fn take_change(&self) -> ConfigFileChange {
        // Сначала гасим флаг пробуждения, потом берём свой: см. порядок в описании модуля.
        self.shared.wake_port.clear_pending_for_drain();
        if self.shared.change_pending.swap(false, Ordering::AcqRel) {
            ConfigFileChange::MayHaveChanged
        } else {
            ConfigFileChange::NoChange
        }
    }

    /// Возвращает сигнал обратно, не будя цикл: разобрать его не получилось (принятие
    /// отменено на границе suspend/выхода), и следующий кадр должен посмотреть снова.
    pub(crate) fn keep_change_for_next_frame(&self) {
        self.shared.change_pending.store(true, Ordering::Release);
    }
}
