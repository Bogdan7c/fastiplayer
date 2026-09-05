//! Причинная синхронизация timeout/reap теста без предположений о scheduler-е.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Duration;

/// Release разрешён лишь после того, как worker прочитал pending-состояние.
pub(super) struct ObservedRelease {
    released: AtomicBool,
    first_pending: Mutex<Option<SyncSender<()>>>,
    observed: Mutex<Receiver<()>>,
}

impl ObservedRelease {
    pub(super) fn new() -> Self {
        let (sender, receiver) = sync_channel(1);
        Self {
            released: AtomicBool::new(false),
            first_pending: Mutex::new(Some(sender)),
            observed: Mutex::new(receiver),
        }
    }

    pub(super) fn load(&self, ordering: Ordering) -> bool {
        // Фиксируем false ДО публикации ack: даже при немедленном release worker
        // обязан выполнить тело pending-loop хотя бы один раз.
        let released = self.released.load(ordering);
        if !released && let Some(sender) = self.first_pending.lock().expect("observer lock").take()
        {
            sender.send(()).expect("release owner still alive");
        }
        released
    }

    pub(super) fn store(&self, released: bool, ordering: Ordering) {
        self.observed
            .lock()
            .expect("observer receiver lock")
            .recv_timeout(Duration::from_secs(2))
            .expect("worker must observe pending before release");
        self.released.store(released, ordering);
    }
}
