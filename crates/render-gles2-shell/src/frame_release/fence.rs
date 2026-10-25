//! Узкий контракт «fence кадра показа»: всё, что нужно потоку-ожидателю от `GpuFence`.
//!
//! Трейт нужен ради тестов: настоящий [`GpuFence`] создаётся только на живом EGL-контексте, а
//! поведение освобождения (кто, когда, сколько раз) надо проверять детерминированно, сигналя
//! fence вручную. В production единственная реализация — `GpuFence`.

use std::time::Duration;

use gles2_egui::{FenceStatus, FenceWaitError, GpuFence};

/// Fence кадра показа: «GPU прошёл точку, после которой все прежние команды выполнены».
pub(crate) trait PresentedFrameFence: Send + Sync {
    /// Проверка без ожидания.
    fn poll(&self) -> Result<FenceStatus, FenceWaitError>;

    /// Ждёт не дольше `timeout`; `Pending` означает «срок вышел, fence ещё не сработал».
    fn wait(&self, timeout: Duration) -> Result<FenceStatus, FenceWaitError>;
}

impl PresentedFrameFence for GpuFence {
    fn poll(&self) -> Result<FenceStatus, FenceWaitError> {
        GpuFence::poll(self)
    }

    fn wait(&self, timeout: Duration) -> Result<FenceStatus, FenceWaitError> {
        GpuFence::wait(self, timeout)
    }
}
