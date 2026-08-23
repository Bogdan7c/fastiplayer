//! Владение lifecycle отдельного FFmpeg decoder worker-а.
//!
//! Модуль отделяет shutdown/join protocol от packet/control state machine:
//! frontend всегда может остановить принадлежащий ему worker независимо от
//! заполнения обычных decoder channels и host-frame pool.

use crossbeam_channel::{Sender, TrySendError};

/// Exactly-once owner shutdown signal-а и thread join handle-а.
pub(super) struct FfmpegWorkerLifecycle {
    /// Отдельный signal не делит capacity с packet/control traffic.
    shutdown_tx: Sender<()>,

    /// `Option` позволяет безопасно забрать join ownership ровно один раз.
    worker_thread: Option<std::thread::JoinHandle<()>>,
}

impl FfmpegWorkerLifecycle {
    /// Связывает независимый shutdown sender с конкретным worker thread-ом.
    pub(super) const fn new(
        shutdown_tx: Sender<()>,
        worker_thread: std::thread::JoinHandle<()>,
    ) -> Self {
        Self {
            shutdown_tx,
            worker_thread: Some(worker_thread),
        }
    }

    /// Сигналит teardown и ожидает фактическое завершение worker-а exactly once.
    pub(super) fn shutdown_and_join(&mut self) {
        // Channel содержит только один idempotent teardown signal: Full означает
        // уже доставленный запрос, Disconnected — уже завершившийся worker.
        match self.shutdown_tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
        }

        let Some(worker_thread) = self.worker_thread.take() else {
            // Повторный вызов после успешного join является ожидаемым no-op.
            return;
        };

        // Стандартный panic hook уже публикует worker panic. Не создаём второй
        // production panic из Drop, но debug/test сборка проверяет invariant.
        let worker_join_result = worker_thread.join();
        debug_assert!(
            worker_join_result.is_ok(),
            "FFmpeg decoder worker panicked before lifecycle join"
        );
    }
}

impl Drop for FfmpegWorkerLifecycle {
    fn drop(&mut self) {
        // Fallback сохраняет ownership invariant, если внешний owner изменит Drop path.
        self.shutdown_and_join();
    }
}
