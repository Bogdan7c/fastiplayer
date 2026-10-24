//! «GPU закончил эту работу»: [`GpuFence`] и очередь освобождений [`GpuCompletionQueue`].
//!
//! Инвариант плана: кадр декодера освобождается только после того, как GPU его дочитал, ровно
//! один раз. Это не зависит от того, нарисует ли UI следующий кадр.
//!
//! Две стратегии:
//! - `EGL_KHR_fence_sync` (EGL 1.5, 3.8): `eglCreateSync(EGL_SYNC_FENCE)` после отправки кадра +
//!   `glFlush`, чтобы fence дошёл до GPU. Ждать можно из любого потока — без текущего контекста;
//! - без расширения — `glFinish` до объявления «готово» (дорого: CPU ждёт GPU). Каждый такой
//!   `glFinish` считается в [`FenceStats::fence_fallback_finishes`], чтобы цена была видна в
//!   замере.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use crate::context::display_registry::DisplayLease;
use crate::context::driver::{EglCallError, SyncId, SyncWaitStatus};
use crate::loss::ContextLoss;

/// Как crate узнаёт, что GPU закончил работу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceStrategy {
    /// `EGL_KHR_fence_sync`.
    EglFenceSync,
    /// `glFinish` перед объявлением «готово».
    FinishFallback,
}

/// Счётчики fence (часть диагностики кадра).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FenceStats {
    /// Создано EGL fence.
    pub egl_fences_created: u64,
    /// Сколько раз пришлось звать `glFinish` вместо fence.
    pub fence_fallback_finishes: u64,
}

/// Отправка команд GL, которая нужна fence. У живого контекста — glow, в тестах — fake.
pub(crate) trait GpuCommandSubmission {
    /// `glFlush`: отправить накопленные команды GPU.
    fn flush(&self);
    /// `glFinish`: дождаться, пока GPU выполнит всё отправленное.
    fn finish(&self);
}

/// Ставит fence по выбранной стратегии и ведёт счётчики.
pub(crate) struct FenceIssuer {
    strategy: FenceStrategy,
    stats: FenceStats,
}

impl FenceIssuer {
    /// Стратегия выбирается по расширениям display.
    pub(crate) fn new(strategy: FenceStrategy) -> Self {
        Self {
            strategy,
            stats: FenceStats::default(),
        }
    }

    /// Текущая стратегия.
    pub(crate) fn strategy(&self) -> FenceStrategy {
        self.strategy
    }

    /// Счётчики.
    pub(crate) fn stats(&self) -> FenceStats {
        self.stats
    }

    /// Fence после уже отправленных команд. Контекст должен быть текущим.
    pub(crate) fn issue(
        &mut self,
        lease: &Arc<DisplayLease>,
        submission: &dyn GpuCommandSubmission,
    ) -> Result<GpuFence, EglCallError> {
        match self.strategy {
            FenceStrategy::EglFenceSync => {
                let sync = lease.driver().create_fence_sync(lease.display())?;
                // Без flush fence может так и не дойти до GPU, и ожидание из другого потока
                // (без флага сброса) зависнет до таймаута.
                submission.flush();
                self.stats.egl_fences_created += 1;
                Ok(GpuFence {
                    state: FenceState::Egl(EglSyncOwner {
                        lease: Arc::clone(lease),
                        sync,
                    }),
                })
            }
            FenceStrategy::FinishFallback => {
                submission.finish();
                self.stats.fence_fallback_finishes += 1;
                Ok(GpuFence {
                    state: FenceState::CompletedByFinish,
                })
            }
        }
    }
}

/// Владелец `EGLSync`: разрушает его ровно один раз. Держит аренду display — display не
/// завершится, пока sync жив (`display_registry`).
struct EglSyncOwner {
    lease: Arc<DisplayLease>,
    sync: SyncId,
}

impl Drop for EglSyncOwner {
    fn drop(&mut self) {
        if let Err(error) = self
            .lease
            .driver()
            .destroy_sync(self.lease.display(), self.sync)
        {
            log::warn!("eglDestroySync failed: {error}");
        }
    }
}

enum FenceState {
    Egl(EglSyncOwner),
    /// Фолбэк: `glFinish` уже дождался GPU при создании.
    CompletedByFinish,
}

/// Состояние fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceStatus {
    /// GPU прошёл точку fence.
    Signaled,
    /// Ещё нет.
    Pending,
}

/// Отказ ожидания fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FenceWaitError {
    /// Контекст потерян: работа GPU не завершится. Освобождать по
    /// [`GpuCompletionQueue::drain_after_context_lost`].
    #[error("GLES2 context lost while waiting for a fence: {0}")]
    ContextLost(ContextLoss),
    /// Отказ драйвера.
    #[error("eglClientWaitSync failed: {0}")]
    Driver(EglCallError),
}

/// Точка в потоке команд GPU. `Send + Sync`: ждать можно из потока плеера.
pub struct GpuFence {
    state: FenceState,
}

impl GpuFence {
    /// Ждёт не дольше `timeout` (ноль — только проверить).
    pub fn wait(&self, timeout: Duration) -> Result<FenceStatus, FenceWaitError> {
        match &self.state {
            FenceState::CompletedByFinish => Ok(FenceStatus::Signaled),
            FenceState::Egl(owner) => {
                let status = owner
                    .lease
                    .driver()
                    .client_wait_sync(owner.lease.display(), owner.sync, timeout)
                    .map_err(|error| {
                        if error.is_context_lost() {
                            FenceWaitError::ContextLost(ContextLoss::from_egl())
                        } else {
                            FenceWaitError::Driver(error)
                        }
                    })?;
                Ok(match status {
                    SyncWaitStatus::Signaled => FenceStatus::Signaled,
                    SyncWaitStatus::TimedOut => FenceStatus::Pending,
                })
            }
        }
    }

    /// Проверка без ожидания.
    pub fn poll(&self) -> Result<FenceStatus, FenceWaitError> {
        self.wait(Duration::ZERO)
    }
}

impl std::fmt::Debug for GpuFence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.state {
            FenceState::Egl(_) => "EglSync",
            FenceState::CompletedByFinish => "CompletedByFinish",
        };
        formatter.debug_tuple("GpuFence").field(&kind).finish()
    }
}

/// Очередь «отдать `T`, когда GPU дочитал». Порядок FIFO: GPU выполняет команды контекста по
/// порядку, поэтому если не готов первый fence, не готовы и следующие.
///
/// Освобождение ровно один раз держится типами: полезная нагрузка перемещается из очереди
/// наружу и больше в ней не встречается. Размер очереди ограничен вызывающим (пулом кадров
/// декодера), сама она не копит ничего сверх того, что в неё положили.
pub struct GpuCompletionQueue<T> {
    pending: VecDeque<(GpuFence, T)>,
}

impl<T> Default for GpuCompletionQueue<T> {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
        }
    }
}

impl<T> GpuCompletionQueue<T> {
    /// Пустая очередь.
    pub fn new() -> Self {
        Self::default()
    }

    /// Положить нагрузку за fence.
    pub fn push(&mut self, fence: GpuFence, payload: T) {
        self.pending.push_back((fence, payload));
    }

    /// Сколько ждёт.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Пуста ли очередь.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Отдаёт всё, чьи fence уже сигналили, без ожидания. Если опрос fence отказал, отданное
    /// до отказа всё равно возвращается, а причина остановки — в
    /// [`CompletedPayloads::stopped_by`]; остальное остаётся в очереди.
    pub fn drain_completed(&mut self) -> CompletedPayloads<T> {
        let mut released = Vec::new();
        while let Some((fence, _)) = self.pending.front() {
            match fence.poll() {
                Ok(FenceStatus::Signaled) => {
                    if let Some((_, payload)) = self.pending.pop_front() {
                        released.push(payload);
                    }
                }
                Ok(FenceStatus::Pending) => break,
                Err(error) => {
                    return CompletedPayloads {
                        released,
                        stopped_by: Some(error),
                    };
                }
            }
        }
        CompletedPayloads {
            released,
            stopped_by: None,
        }
    }

    /// Контекст потерян: GPU больше ничего не читает, отдаём всё сразу. Повторный вызов
    /// вернёт пусто — каждая нагрузка отдаётся ровно один раз.
    pub fn drain_after_context_lost(&mut self) -> Vec<T> {
        self.pending.drain(..).map(|(_, payload)| payload).collect()
    }
}

/// Итог [`GpuCompletionQueue::drain_completed`].
#[derive(Debug)]
pub struct CompletedPayloads<T> {
    /// Отданное (GPU дочитал).
    pub released: Vec<T>,
    /// Почему опрос остановился раньше (потеря контекста или отказ драйвера).
    pub stopped_by: Option<FenceWaitError>,
}

#[cfg(test)]
#[path = "fence_tests.rs"]
mod tests;
