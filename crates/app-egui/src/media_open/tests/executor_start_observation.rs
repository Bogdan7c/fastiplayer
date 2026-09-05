//! Тестовый rendezvous гарантирует наблюдение ещё не стартовавшего worker-а.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Duration;

pub(super) struct StartLatch {
    started: AtomicBool,
    pending_observed: Mutex<Option<SyncSender<()>>>,
    observation: Mutex<Receiver<()>>,
}

impl StartLatch {
    pub(super) fn new(started: bool) -> Self {
        assert!(!started, "fixture begins before worker start");
        let (sender, receiver) = sync_channel(1);
        Self {
            started: AtomicBool::new(false),
            pending_observed: Mutex::new(Some(sender)),
            observation: Mutex::new(receiver),
        }
    }

    pub(super) fn load(&self, ordering: Ordering) -> bool {
        // Сохраняем false до разрешения worker-у опубликовать start: caller
        // обязательно проверит свой deadline и выполнит pending iteration.
        let started = self.started.load(ordering);
        if !started
            && let Some(sender) = self.pending_observed.lock().expect("observer lock").take()
        {
            sender.send(()).expect("worker observation receiver");
        }
        started
    }

    pub(super) fn store(&self, started: bool, ordering: Ordering) {
        assert!(started, "fixture only publishes worker start");
        self.observation
            .lock()
            .expect("observation receiver lock")
            .recv_timeout(Duration::from_secs(2))
            .expect("caller must observe pending before worker start");
        self.started.store(started, ordering);
    }
}
