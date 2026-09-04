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

/// Связывает latest task с той же revision, которую наблюдает runtime worker.
struct VersionedTaskSlot<T> {
    revision: u64,
    task: Option<T>,
}

impl<T> VersionedTaskSlot<T> {
    fn new() -> Self {
        Self {
            revision: 0,
            task: None,
        }
    }

    /// Публикует замену/отмену как одно состояние и сохраняет прежний wrapping contract.
    fn publish(&mut self, task: Option<T>) -> u64 {
        self.revision = self.revision.wrapping_add(1);
        self.task = task;
        self.revision
    }

    /// Старое уведомление не вправе забрать task, принадлежащий более новой revision.
    fn take_for_revision(&mut self, observed_revision: u64) -> VersionedTaskTake<T> {
        if self.revision == observed_revision {
            VersionedTaskTake::Current(self.task.take())
        } else {
            VersionedTaskTake::NewerCommandPending
        }
    }
}

enum VersionedTaskTake<T> {
    Current(Option<T>),
    NewerCommandPending,
}

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
    task_slot: Arc<Mutex<VersionedTaskSlot<AbortableHttpTask<T>>>>,
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
        let task_slot = Arc::new(Mutex::new(VersionedTaskSlot::new()));
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
        self.publish_command(sender, Some(task))
    }

    /// Отменяет current task без остановки executor-а или будущих tasks.
    pub fn cancel_current(&self) -> Result<(), AbortableHttpTaskExecutorError> {
        let Some(sender) = &self.command_sender else {
            return Err(AbortableHttpTaskExecutorError::WorkerStopped);
        };
        if sender.receiver_count() == 0 {
            return Err(AbortableHttpTaskExecutorError::WorkerStopped);
        }
        self.publish_command(sender, None)
    }

    /// Slot и watch value публикуются под одним mutex, поэтому их revisions не расходятся.
    fn publish_command(
        &self,
        sender: &watch::Sender<u64>,
        task: Option<AbortableHttpTask<T>>,
    ) -> Result<(), AbortableHttpTaskExecutorError> {
        let mut task_slot = self
            .task_slot
            .lock()
            .map_err(|_| AbortableHttpTaskExecutorError::WorkerStopped)?;
        let revision = task_slot.publish(task);
        sender.send_replace(revision);
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
        if let Some(sender) = self.command_sender.take()
            && let Ok(mut task_slot) = self.task_slot.lock()
        {
            let revision = task_slot.publish(None);
            sender.send_replace(revision);
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
    task_slot: Arc<Mutex<VersionedTaskSlot<AbortableHttpTask<T>>>>,
    result_slot: Arc<Mutex<Option<T>>>,
) where
    T: Send + 'static,
{
    let mut next_task = match take_observed_task(&mut command_receiver, &task_slot) {
        VersionedTaskTake::Current(task) => task,
        VersionedTaskTake::NewerCommandPending => None,
    };

    loop {
        let Some(mut task) = next_task.take() else {
            if command_receiver.changed().await.is_err() {
                return;
            }
            next_task = match take_observed_task(&mut command_receiver, &task_slot) {
                VersionedTaskTake::Current(task) => task,
                VersionedTaskTake::NewerCommandPending => None,
            };
            continue;
        };

        tokio::select! {
            biased;

            changed = command_receiver.changed() => {
                if changed.is_err() {
                    return;
                }
                next_task = match take_observed_task(&mut command_receiver, &task_slot) {
                    VersionedTaskTake::Current(task) => task,
                    VersionedTaskTake::NewerCommandPending => None,
                };
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

/// Копирует watch revision до slot lock-а: `watch::Ref` нельзя держать при обратном lock order.
fn take_observed_task<T>(
    command_receiver: &mut watch::Receiver<u64>,
    task_slot: &Arc<Mutex<VersionedTaskSlot<AbortableHttpTask<T>>>>,
) -> VersionedTaskTake<AbortableHttpTask<T>> {
    let observed_revision = {
        let watched_revision = command_receiver.borrow_and_update();
        *watched_revision
    };
    task_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take_for_revision(observed_revision)
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

    fn take_result(executor: &AbortableHttpTaskExecutor<usize>) -> usize {
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        // Бесконечный ленивый iterator сохраняет polling, не создавая локальную ветку,
        // покрытие которой зависело от того, успел ли worker завершиться до первого poll-а.
        std::iter::repeat_with(|| {
            // Deadline превращает зависший worker в точный test failure.
            assert!(
                std::time::Instant::now() < deadline,
                "latest task timed out"
            );
            // Yield даёт worker-у CPU без привязки корректности к длительности sleep-а.
            std::thread::yield_now();
            // Ошибка остановленного worker-а не маскируется как отсутствие результата.
            executor.try_take().expect("poll result")
        })
        // Стандартный adapter сам продолжает polling до первого опубликованного результата.
        .find_map(|maybe_result| maybe_result)
        // repeat_with бесконечен, поэтому None возможен только при нарушении std-контракта.
        .expect("infinite polling iterator must yield a result before the deadline")
    }

    /// Публичная ошибка worker-а сохраняет точную и source-free диагностику.
    #[test]
    fn worker_stopped_error_preserves_exact_public_diagnostic() {
        // Создаём typed ошибку без зависимости от timing worker thread-а.
        let error = AbortableHttpTaskExecutorError::WorkerStopped;
        // Display обязан сохранять точную причину для вызывающего transport boundary.
        assert_eq!(error.to_string(), "abortable HTTP task worker stopped");
        // Ошибка не должна объявлять несуществующий вложенный источник.
        assert!(std::error::Error::source(&error).is_none());
    }

    /// Уведомление старой cancellation revision не может украсть более новый task.
    #[test]
    fn stale_observed_revision_leaves_newer_task_for_exact_notification() {
        let mut task_slot = VersionedTaskSlot::new();
        let cancellation_revision = task_slot.publish(None);
        let successor_revision = task_slot.publish(Some(42));

        assert!(matches!(
            task_slot.take_for_revision(cancellation_revision),
            VersionedTaskTake::NewerCommandPending
        ));
        assert!(matches!(
            task_slot.take_for_revision(successor_revision),
            VersionedTaskTake::Current(Some(42))
        ));

        task_slot.revision = u64::MAX;
        let wrapped_cancellation_revision = task_slot.publish(None);
        assert_eq!(wrapped_cancellation_revision, 0);
        assert!(matches!(
            task_slot.take_for_revision(wrapped_cancellation_revision),
            VersionedTaskTake::Current(None)
        ));
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

        assert_eq!(take_result(&executor), 42);
    }

    /// Successor не теряется, когда caller не ждёт acknowledgement cancellation.
    #[test]
    fn immediate_successor_after_cancellation_completes_every_time() {
        let executor = AbortableHttpTaskExecutor::start().expect("start executor");
        for expected_result in 0..32 {
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
            executor
                .replace(Box::pin(async move { expected_result }))
                .expect("submit immediate successor task");

            dropped_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("cancelled future must drop");
            assert_eq!(take_result(&executor), expected_result);
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
