use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Потокобезопасный флаг отмены для блокирующих source-операций.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    /// Общий атомарный флаг, который видят все clone-ы token-а.
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Создаёт token без запрошенной отмены.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Создаёт token, который никогда не отменяется.
    #[must_use]
    pub fn never_cancelled() -> Self {
        Self::default()
    }

    /// Запрашивает остановку текущей или следующей source-операции.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Возвращает `true`, если caller уже запросил отмену.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}
