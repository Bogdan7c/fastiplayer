//! Будильник запросов байтов: источник файла просит байты → главный цикл просыпается.
//!
//! Источник (`pushed-media-source`) зовёт [`RangeRequestNotifier::range_requested`]
//! из потока открытия или из потока чтения демуксера — без внутренних замков. Здесь
//! вызов превращается в обычное пробуждение главного цикла (то же, что будит ящик):
//! цикл забирает запросы в ближайшем проходе, а не ждёт тика часов. Задержка в
//! 250 мс на каждый запрос заметна на старте и на перемотке, поэтому опрос по
//! таймеру тут не годится.

use std::sync::Arc;

use pushed_media_source::RangeRequestNotifier;

use super::inbox::BrowserSessionWake;

/// Будильник главного цикла для источника файла.
pub(crate) struct RangeRequestWake {
    /// То, что будит цикл (в процессе — `ParkingWake`).
    wake: Arc<dyn BrowserSessionWake>,
}

impl RangeRequestWake {
    /// Будильник над пробуждением цикла.
    pub(crate) fn new(wake: Arc<dyn BrowserSessionWake>) -> Self {
        Self { wake }
    }
}

impl RangeRequestNotifier for RangeRequestWake {
    fn range_requested(&self) {
        self.wake.wake();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct CountingWake(AtomicUsize);

    impl BrowserSessionWake for CountingWake {
        fn wake(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Каждый новый запрос байтов будит главный цикл ровно одним пробуждением.
    #[test]
    fn a_range_request_wakes_the_main_loop() {
        let wake = Arc::new(CountingWake::default());
        let notifier = RangeRequestWake::new(wake.clone());

        notifier.range_requested();
        notifier.range_requested();

        assert_eq!(wake.0.load(Ordering::SeqCst), 2);
    }
}
