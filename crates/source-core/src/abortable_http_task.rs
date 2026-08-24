//! Runtime-owned latest-task executor для abortable async HTTP lifecycle-а.
//!
//! Public boundary не раскрывает Tokio types: consumer передаёт только boxed
//! standard `Future`, а source-core владеет runtime, заменой current task-а и
//! bounded result slot-ом.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use tokio::runtime::Builder;
use tokio::sync::watch;

/// Boxed HTTP future без зависимости consumer-а от concrete async runtime-а.
pub type AbortableHttpTask<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Typed infrastructure failure без смешивания с HTTP/result semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbortableHttpTaskExecutorError {
    /// Runtime thread не стартовал либо уже остановился.
    WorkerStopped,
}

impl fmt::Display for AbortableHttpTaskExecutorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerStopped => formatter.write_str("abortable HTTP task worker stopped"),
        }
    }
}

impl std::error::Error for AbortableHttpTaskExecutorError {}

/// Один latest-value command slot, один in-flight future и один result slot.
pub struct AbortableHttpTaskExecutor<T> {
    /// Versioned wake-up; concrete task остаётся скрыт в bounded latest slot-е.
    command_sender: Option<watch::Sender<u64>>,
    /// `None` отменяет current future; `Some` заменяет его latest task-ом.
    task_slot: Arc<Mutex<Option<AbortableHttpTask<T>>>>,
    /// Newer completion безопасно вытесняет completion superseded owner-ом task-а.
    result_slot: Arc<Mutex<Option<T>>>,
    /// Отличает временно пустой slot от остановленного runtime worker-а.
    worker_running: Arc<AtomicBool>,
}

impl<T> AbortableHttpTaskExecutor<T>
where
    T: Send + 'static,
{
    /// Создаёт выделенный current-thread runtime без HTTP side effects.
    pub fn start() -> Result<Self, AbortableHttpTaskExecutorError> {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| AbortableHttpTaskExecutorError::WorkerStopped)?;
        let (command_sender, command_receiver) = watch::channel(0_u64);
        let task_slot = Arc::new(Mutex::new(None));
        let result_slot = Arc::new(Mutex::new(None));
        let worker_running = Arc::new(AtomicBool::new(true));
        let worker_result_slot = Arc::clone(&result_slot);
        let worker_task_slot = Arc::clone(&task_slot);
        let worker_running_state = Arc::clone(&worker_running);

        thread::Builder::new()
            .name("source-abortable-http".to_owned())
            .spawn(move || {
                let _running_guard = WorkerRunningGuard(worker_running_state);
                runtime.block_on(run_latest_task(
                    command_receiver,
                    worker_task_slot,
                    worker_result_slot,
                ));
            })
            .map_err(|_| AbortableHttpTaskExecutorError::WorkerStopped)?;

        Ok(Self {
            command_sender: Some(command_sender),
            task_slot,
            result_slot,
            worker_running,
        })
    }

    /// Заменяет current task; runtime drop-ает старый future до poll нового.
    pub fn replace(
        &self,
        task: AbortableHttpTask<T>,
    ) -> Result<(), AbortableHttpTaskExecutorError> {
        let Some(sender) = &self.command_sender else {
            return Err(AbortableHttpTaskExecutorError::WorkerStopped);
        };
        if sender.receiver_count() == 0 {
            return Err(AbortableHttpTaskExecutorError::WorkerStopped);
        }
        let mut task_slot = self
            .task_slot
            .lock()
            .map_err(|_| AbortableHttpTaskExecutorError::WorkerStopped)?;
        *task_slot = Some(task);
        drop(task_slot);
        sender.send_modify(|version| *version = version.wrapping_add(1));
        Ok(())
    }

    /// Отменяет current task без остановки executor-а или будущих tasks.
    pub fn cancel_current(&self) -> Result<(), AbortableHttpTaskExecutorError> {
        let Some(sender) = &self.command_sender else {
            return Err(AbortableHttpTaskExecutorError::WorkerStopped);
        };
        if sender.receiver_count() == 0 {
            return Err(AbortableHttpTaskExecutorError::WorkerStopped);
        }
        let mut task_slot = self
            .task_slot
            .lock()
            .map_err(|_| AbortableHttpTaskExecutorError::WorkerStopped)?;
        *task_slot = None;
        drop(task_slot);
        sender.send_modify(|version| *version = version.wrapping_add(1));
        Ok(())
    }

    /// Забирает не более одного result и никогда не ждёт runtime thread.
    pub fn try_take(&self) -> Result<Option<T>, AbortableHttpTaskExecutorError> {
        let mut result_slot = self
            .result_slot
            .lock()
            .map_err(|_| AbortableHttpTaskExecutorError::WorkerStopped)?;
        if let Some(result) = result_slot.take() {
            return Ok(Some(result));
        }
        if self.worker_running.load(Ordering::Acquire) {
            Ok(None)
        } else {
            Err(AbortableHttpTaskExecutorError::WorkerStopped)
        }
    }
}

impl<T> Drop for AbortableHttpTaskExecutor<T> {
    fn drop(&mut self) {
        if let Some(sender) = self.command_sender.take() {
            if let Ok(mut task_slot) = self.task_slot.lock() {
                *task_slot = None;
            }
            sender.send_modify(|version| *version = version.wrapping_add(1));
        }
    }
}

/// Сбрасывает liveness даже при unwind runtime thread-а.
struct WorkerRunningGuard(Arc<AtomicBool>);

impl Drop for WorkerRunningGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Poll-ит ровно один future; command branch biased, чтобы supersede всегда победил race.
async fn run_latest_task<T>(
    mut command_receiver: watch::Receiver<u64>,
    task_slot: Arc<Mutex<Option<AbortableHttpTask<T>>>>,
    result_slot: Arc<Mutex<Option<T>>>,
) where
    T: Send + 'static,
{
    let _ = command_receiver.borrow_and_update();
    let mut next_task = take_latest_task(&task_slot);

    loop {
        let Some(mut task) = next_task.take() else {
            if command_receiver.changed().await.is_err() {
                return;
            }
            let _ = command_receiver.borrow_and_update();
            next_task = take_latest_task(&task_slot);
            continue;
        };

        tokio::select! {
            biased;

            changed = command_receiver.changed() => {
                if changed.is_err() {
                    return;
                }
                let _ = command_receiver.borrow_and_update();
                next_task = take_latest_task(&task_slot);
            }
            result = &mut task => {
                let Ok(mut guarded_result) = result_slot.lock() else {
                    return;
                };
                *guarded_result = Some(result);
                next_task = None;
            }
        }
    }
}

/// Забирает latest task, не удерживая mutex во время async poll-а.
fn take_latest_task<T>(
    task_slot: &Arc<Mutex<Option<AbortableHttpTask<T>>>>,
) -> Option<AbortableHttpTask<T>> {
    task_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

#[cfg(test)]
mod tests {
    use std::future;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// Drop observer доказывает физический lifecycle future, а не только stale fence.
    struct DropSignal(Option<mpsc::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    /// Replacement drop-ает pending future и публикует только новый result.
    #[test]
    fn replacement_drops_current_future_before_latest_completion() {
        let executor = AbortableHttpTaskExecutor::start().expect("start executor");
        let (started_sender, started_receiver) = mpsc::channel();
        let (dropped_sender, dropped_receiver) = mpsc::channel();
        executor
            .replace(Box::pin(async move {
                let _drop_signal = DropSignal(Some(dropped_sender));
                started_sender.send(()).expect("report pending task start");
                future::pending::<usize>().await
            }))
            .expect("submit pending task");
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("pending task must start");

        executor
            .replace(Box::pin(async { 42 }))
            .expect("replace pending task");
        dropped_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("superseded future must drop");

        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            match executor.try_take().expect("poll result") {
                Some(result) => {
                    assert_eq!(result, 42);
                    break;
                }
                None => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "latest task timed out"
                    );
                    std::thread::yield_now();
                }
            }
        }
    }

    /// Explicit cancellation drop-ает current future, но executor принимает successor.
    #[test]
    fn cancellation_drops_current_future_without_stopping_executor() {
        let executor = AbortableHttpTaskExecutor::start().expect("start executor");
        let (started_sender, started_receiver) = mpsc::channel();
        let (dropped_sender, dropped_receiver) = mpsc::channel();
        executor
            .replace(Box::pin(async move {
                let _drop_signal = DropSignal(Some(dropped_sender));
                started_sender.send(()).expect("report pending task start");
                future::pending::<usize>().await
            }))
            .expect("submit pending task");
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("pending task must start");

        executor.cancel_current().expect("cancel current task");
        dropped_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("cancelled future must drop");
        executor
            .replace(Box::pin(async { 7 }))
            .expect("submit successor task");
    }
}
