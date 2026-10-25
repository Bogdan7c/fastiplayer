//! Ожидание, пока кадры, о возврате которых уже попросили, вернутся декодеру.
//!
//! Зачем: «GPU закончил работу» (`RenderShell::wait_for_gpu_idle`) для приложения означает ещё и
//! «кадры, которые GPU дочитал и которые приложение отпустило, вернулись декодеру» — так у wgpu
//! (`device.poll(Wait)` исполняет колбэки `on_submitted_work_done` синхронно). Здесь возврат идёт
//! потоком-ожидателем уже ПОСЛЕ срабатывания fence, и без этого ожидания сразу за `wait` по fence
//! кадр мог бы ещё секунду-другую висеть у ожидателя: пересоздание рендера или проверка «кадр
//! вернулся ровно один раз» увидели бы гонку.

use std::time::{Duration, Instant};

use gles2_egui::{FenceStatus, FenceWaitError};

use super::binding::Gles2FrameReleaseBinding;

/// Шаг опроса: потеря контекста и отвязка рендера опустошают очередь без уведомления ожидателя.
const DRAIN_POLL_STEP: Duration = Duration::from_millis(10);

/// Чем закончилось ожидание возврата кадров.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "таймаут означает, что кадры ещё у ожидателя"]
pub enum PendingReleaseDrain {
    /// Очередь пуста, поток-ожидатель ничего не держит: все запрошенные кадры вернулись.
    Drained,
    /// Время вышло: кадры ждут fence (он ещё не сработал) или ожидатель их возвращает.
    TimedOut {
        /// Сколько записей оставалось в очереди ожидания.
        still_pending: usize,
    },
}

/// Чем закончилось ожидание работы GPU по последнему fence кадра показа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "Pending означает, что GPU ещё не закончил"]
pub enum PresentedWorkWait {
    /// GPU прошёл fence последнего кадра показа: вся работа до него закончена.
    Done,
    /// Ни одного кадра не показано (или привязка отвязана / контекст потерян): ждать нечего.
    NothingPublished,
    /// Срок вышел, fence ещё не сработал.
    Pending,
    /// Ожидание отказало (потеря контекста, отказ драйвера).
    Failed(FenceWaitError),
}

impl Gles2FrameReleaseBinding {
    /// Ждёт не дольше `timeout` fence последнего показанного кадра.
    ///
    /// Нужен рендеру, у которого нет текущего GL-контекста (окно отпущено, surfaceless-контекста
    /// нет): поставить новый fence нечем, но fence уже показанного кадра ждётся из любого потока
    /// (`EGL_KHR_fence_sync`), а он покрывает всю прежнюю работу GPU контекста.
    pub fn wait_for_last_presented_fence(&self, timeout: Duration) -> PresentedWorkWait {
        // Fence клонируется под блокировкой, ждётся — вне её: ожидание не держит привязку.
        let last_fence = self.core().lock_state().last_fence.clone();
        let Some(fence) = last_fence else {
            return PresentedWorkWait::NothingPublished;
        };
        match fence.wait(timeout) {
            Ok(FenceStatus::Signaled) => PresentedWorkWait::Done,
            Ok(FenceStatus::Pending) => PresentedWorkWait::Pending,
            Err(error) => PresentedWorkWait::Failed(error),
        }
    }

    /// Ждёт не дольше `timeout`, пока очередь кадров, ждущих fence, опустеет, а поток-ожидатель
    /// закончит возвращать запись, которую уже взял.
    ///
    /// Ничего не освобождает сам и не меняет режим привязки: кадры возвращает тот же поток
    /// (или потеря контекста / отвязка рендера), а здесь только наблюдение.
    pub fn wait_for_pending_releases(&self, timeout: Duration) -> PendingReleaseDrain {
        let core = self.core();
        let deadline = Instant::now() + timeout;
        let mut state = core.lock_state();
        loop {
            if state.waiting.is_empty() && !state.waiter_holds_entry {
                return PendingReleaseDrain::Drained;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return PendingReleaseDrain::TimedOut {
                    still_pending: state.waiting.len(),
                };
            };
            state = match core
                .wakeup
                .wait_timeout(state, remaining.min(DRAIN_POLL_STEP))
            {
                Ok((guard, _timed_out)) => guard,
                // Отравление не мешает наблюдению: состояние читаем как есть (как `lock_state`).
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }
}
