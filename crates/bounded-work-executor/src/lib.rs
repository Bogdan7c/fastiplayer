//! Нейтральный bounded executor для коротких CPU-задач приложения.
//!
//! Crate не знает про UI, playlist, filesystem или конкретный runtime. Он
//! владеет только фиксированными worker threads, bounded admission, typed
//! result slot, cooperative cancellation и обязательным shutdown/join.

use std::fmt;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Именованные limits одного executor instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutorConfig {
    worker_threads: NonZeroUsize,
    queue_capacity: NonZeroUsize,
    thread_name_prefix: Arc<str>,
}

impl ExecutorConfig {
    /// Создаёт конфигурацию без неочевидных positional integer-ов в business code.
    #[must_use]
    pub fn new(
        worker_threads: NonZeroUsize,
        queue_capacity: NonZeroUsize,
        thread_name_prefix: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            worker_threads,
            queue_capacity,
            thread_name_prefix: thread_name_prefix.into(),
        }
    }

    /// Число постоянно живущих worker threads.
    #[must_use]
    pub const fn worker_threads(&self) -> NonZeroUsize {
        self.worker_threads
    }

    /// Максимум задач, ожидающих свободный worker.
    #[must_use]
    pub const fn queue_capacity(&self) -> NonZeroUsize {
        self.queue_capacity
    }
}

/// Ошибка создания фиксированного набора threads.
#[derive(Debug)]
pub struct ExecutorStartError {
    worker_index: usize,
    source: std::io::Error,
}

impl fmt::Display for ExecutorStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to start worker {}: {}",
            self.worker_index, self.source
        )
    }
}

impl std::error::Error for ExecutorStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Typed admission failure без silent drop задачи.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// Bounded queue заполнена; caller решает retry/coalesce policy.
    QueueFull,
    /// Shutdown уже закрыл admission.
    ShuttingDown,
}

/// Почему result уже не может стать пользовательским значением.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskFailure {
    /// Task была отменена до начала выполнения.
    CancelledBeforeStart,
    /// Task closure завершилась panic; worker thread продолжил работу.
    Panicked,
    /// Executor завершился до публикации terminal result.
    ExecutorStopped,
}

/// Неблокирующий typed result poll.
#[derive(Debug, PartialEq, Eq)]
pub enum TaskPoll<T> {
    Pending,
    Completed(T),
    Failed(TaskFailure),
}

/// Cloneable cooperative cancellation view для task closure.
#[derive(Clone, Debug)]
pub struct CancellationToken {
    task_cancelled: Arc<AtomicBool>,
    executor_stopping: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Проверяется между bounded chunks алгоритма.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.task_cancelled.load(Ordering::Acquire)
            || self.executor_stopping.load(Ordering::Acquire)
    }
}

/// Caller-owned typed terminal slot и cancellation handle.
pub struct TaskHandle<T> {
    receiver: Receiver<Result<T, TaskFailure>>,
    task_cancelled: Arc<AtomicBool>,
}

impl<T> TaskHandle<T> {
    /// Линеаризует cooperative cancel request.
    pub fn cancel(&self) {
        self.task_cancelled.store(true, Ordering::Release);
    }

    /// Неблокирующе забирает terminal result exactly once.
    pub fn try_take(&self) -> TaskPoll<T> {
        match self.receiver.try_recv() {
            Ok(Ok(value)) => TaskPoll::Completed(value),
            Ok(Err(failure)) => TaskPoll::Failed(failure),
            Err(TryRecvError::Empty) => TaskPoll::Pending,
            Err(TryRecvError::Disconnected) => TaskPoll::Failed(TaskFailure::ExecutorStopped),
        }
    }
}

trait ErasedTask: Send {
    fn run(self: Box<Self>, executor_stopping: Arc<AtomicBool>);
    fn cancel_before_start(self: Box<Self>);
}

struct TypedTask<F, T> {
    operation: Option<F>,
    terminal: SyncSender<Result<T, TaskFailure>>,
    task_cancelled: Arc<AtomicBool>,
    terminal_notifier: Option<Box<dyn FnOnce() + Send + 'static>>,
}

impl<F, T> ErasedTask for TypedTask<F, T>
where
    F: FnOnce(CancellationToken) -> T + Send + 'static,
    T: Send + 'static,
{
    fn run(mut self: Box<Self>, executor_stopping: Arc<AtomicBool>) {
        if self.task_cancelled.load(Ordering::Acquire) || executor_stopping.load(Ordering::Acquire)
        {
            self.complete(Err(TaskFailure::CancelledBeforeStart));
            return;
        }
        let token = CancellationToken {
            task_cancelled: Arc::clone(&self.task_cancelled),
            executor_stopping,
        };
        let operation = self
            .operation
            .take()
            .expect("executor invokes each queued task exactly once");
        let terminal = match catch_unwind(AssertUnwindSafe(|| operation(token))) {
            Ok(value) => Ok(value),
            Err(_) => Err(TaskFailure::Panicked),
        };
        self.complete(terminal);
    }

    fn cancel_before_start(mut self: Box<Self>) {
        self.complete(Err(TaskFailure::CancelledBeforeStart));
    }
}

impl<F, T> TypedTask<F, T> {
    /// Сначала публикует terminal slot, затем exactly-once будит внешний owner.
    fn complete(&mut self, terminal: Result<T, TaskFailure>) {
        let _result_owner_dropped = self.terminal.send(terminal);
        if let Some(notifier) = self.terminal_notifier.take() {
            let _notifier_panicked = catch_unwind(AssertUnwindSafe(notifier));
        }
    }
}

type WorkQueueReceiver = Arc<Mutex<Receiver<Box<dyn ErasedTask>>>>;

/// Fixed-size reusable CPU executor с non-blocking admission.
pub struct BoundedExecutor {
    sender: Mutex<Option<SyncSender<Box<dyn ErasedTask>>>>,
    executor_stopping: Arc<AtomicBool>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl BoundedExecutor {
    /// Стартует все workers или возвращает ошибку без частично живого executor-а.
    pub fn start(config: ExecutorConfig) -> Result<Self, ExecutorStartError> {
        let (sender, receiver) = sync_channel(config.queue_capacity.get());
        let receiver = Arc::new(Mutex::new(receiver));
        let executor_stopping = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::with_capacity(config.worker_threads.get());

        for worker_index in 0..config.worker_threads.get() {
            let worker_receiver = Arc::clone(&receiver);
            let worker_stopping = Arc::clone(&executor_stopping);
            let thread_name = format!("{}-{worker_index}", config.thread_name_prefix);
            match thread::Builder::new()
                .name(thread_name)
                .spawn(move || worker_loop(worker_receiver, worker_stopping))
            {
                Ok(worker) => workers.push(worker),
                Err(source) => {
                    executor_stopping.store(true, Ordering::Release);
                    drop(sender);
                    for worker in workers {
                        let _worker_panicked = worker.join();
                    }
                    return Err(ExecutorStartError {
                        worker_index,
                        source,
                    });
                }
            }
        }

        Ok(Self {
            sender: Mutex::new(Some(sender)),
            executor_stopping,
            workers: Mutex::new(workers),
        })
    }

    /// Пытается принять typed task, никогда не блокируя caller thread.
    pub fn try_submit<F, T>(&self, operation: F) -> Result<TaskHandle<T>, SubmitError>
    where
        F: FnOnce(CancellationToken) -> T + Send + 'static,
        T: Send + 'static,
    {
        self.try_submit_with_terminal_notifier(operation, || {})
    }

    /// Принимает task и exactly-once terminal notifier для event-driven owner-а.
    ///
    /// Notifier запускается после публикации result slot для success, operation
    /// panic и cancellation до старта. Его собственный panic изолируется.
    pub fn try_submit_with_terminal_notifier<F, T, N>(
        &self,
        operation: F,
        terminal_notifier: N,
    ) -> Result<TaskHandle<T>, SubmitError>
    where
        F: FnOnce(CancellationToken) -> T + Send + 'static,
        T: Send + 'static,
        N: FnOnce() + Send + 'static,
    {
        if self.executor_stopping.load(Ordering::Acquire) {
            return Err(SubmitError::ShuttingDown);
        }
        let (terminal, receiver) = sync_channel(1);
        let task_cancelled = Arc::new(AtomicBool::new(false));
        let task: Box<dyn ErasedTask> = Box::new(TypedTask {
            operation: Some(operation),
            terminal,
            task_cancelled: Arc::clone(&task_cancelled),
            terminal_notifier: Some(Box::new(terminal_notifier)),
        });
        let sender_guard = self
            .sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(sender) = sender_guard.as_ref() else {
            return Err(SubmitError::ShuttingDown);
        };
        match sender.try_send(task) {
            Ok(()) => Ok(TaskHandle {
                receiver,
                task_cancelled,
            }),
            Err(TrySendError::Full(_)) => Err(SubmitError::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(SubmitError::ShuttingDown),
        }
    }

    /// Закрывает admission и cooperative-cancel-ит active/queued work.
    pub fn shutdown(&self) {
        self.executor_stopping.store(true, Ordering::Release);
        self.sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    /// Завершает lifecycle и сообщает число panic-нувших worker threads.
    pub fn shutdown_and_join(&self) -> ShutdownReport {
        self.shutdown_and_join_impl(None)
    }

    /// Завершает lifecycle, но не ждёт cooperative worker дольше caller deadline.
    ///
    /// Незавершённые к deadline threads detach-ятся: process owner получает typed
    /// отчёт и сам решает, требуется ли terminal process exit.
    pub fn shutdown_and_join_until(&self, deadline: Instant) -> ShutdownReport {
        self.shutdown_and_join_impl(Some(deadline))
    }

    fn shutdown_and_join_impl(&self, deadline: Option<Instant>) -> ShutdownReport {
        self.shutdown();
        let mut workers = self
            .workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let worker_count = workers.len();
        let current_thread_id = thread::current().id();
        let mut joined_workers = 0usize;
        let mut detached_current_workers = 0usize;
        let mut detached_deadline_workers = 0usize;
        let mut panicked_workers = 0usize;
        for worker in workers.drain(..) {
            if worker.thread().id() == current_thread_id {
                detached_current_workers += 1;
                drop(worker);
                continue;
            }
            if let Some(deadline) = deadline {
                while !worker.is_finished() {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    thread::sleep(
                        deadline
                            .saturating_duration_since(now)
                            .min(Duration::from_millis(1)),
                    );
                }
                if !worker.is_finished() {
                    detached_deadline_workers += 1;
                    drop(worker);
                    continue;
                }
            }
            joined_workers += 1;
            if worker.join().is_err() {
                panicked_workers += 1;
            }
        }
        ShutdownReport {
            worker_count,
            joined_workers,
            detached_current_workers,
            detached_deadline_workers,
            panicked_workers,
        }
    }
}

impl Drop for BoundedExecutor {
    fn drop(&mut self) {
        let _report = self.shutdown_and_join();
    }
}

/// Итог обязательного join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShutdownReport {
    pub worker_count: usize,
    /// Workers, для которых caller дождался полного завершения.
    pub joined_workers: usize,
    /// Текущий worker нельзя join-ить из него самого; handle безопасно detach-ится.
    pub detached_current_workers: usize,
    /// Workers, которые не завершили cooperative cancellation к caller deadline.
    pub detached_deadline_workers: usize,
    pub panicked_workers: usize,
}

fn worker_loop(receiver: WorkQueueReceiver, executor_stopping: Arc<AtomicBool>) {
    loop {
        let received = receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .recv();
        let Ok(task) = received else {
            break;
        };
        if executor_stopping.load(Ordering::Acquire) {
            task.cancel_before_start();
        } else {
            task.run(Arc::clone(&executor_stopping));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    fn config(workers: usize, queued: usize) -> ExecutorConfig {
        ExecutorConfig::new(
            NonZeroUsize::new(workers).unwrap(),
            NonZeroUsize::new(queued).unwrap(),
            "bounded-test",
        )
    }

    fn wait<T>(handle: &TaskHandle<T>) -> TaskPoll<T> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let result = handle.try_take();
            if !matches!(result, TaskPoll::Pending) {
                return result;
            }
            assert!(Instant::now() < deadline, "task did not finish");
            thread::yield_now();
        }
    }

    #[test]
    fn typed_result_and_panic_are_isolated() {
        let executor = BoundedExecutor::start(config(1, 2)).unwrap();
        let value = executor.try_submit(|_| 42_u64).unwrap();
        let panic = executor
            .try_submit::<_, ()>(|_| panic!("expected test panic"))
            .unwrap();
        assert_eq!(wait(&value), TaskPoll::Completed(42));
        assert_eq!(wait(&panic), TaskPoll::Failed(TaskFailure::Panicked));
        assert_eq!(executor.shutdown_and_join().panicked_workers, 0);
    }

    #[test]
    fn bounded_admission_reports_backpressure() {
        let executor = BoundedExecutor::start(config(1, 1)).unwrap();
        let gate = Arc::new(Barrier::new(2));
        let active_gate = Arc::clone(&gate);
        let _active = executor.try_submit(move |_| active_gate.wait()).unwrap();
        while executor.try_submit(|_| ()).is_err() {
            thread::yield_now();
        }
        assert!(matches!(
            executor.try_submit(|_| ()),
            Err(SubmitError::QueueFull)
        ));
        gate.wait();
    }

    #[test]
    fn cancellation_shutdown_and_join_have_typed_outcomes() {
        let executor = BoundedExecutor::start(config(1, 2)).unwrap();
        let gate = Arc::new(Barrier::new(2));
        let (started_sender, started_receiver) = std::sync::mpsc::sync_channel(1);
        let active_gate = Arc::clone(&gate);
        let active = executor
            .try_submit(move |token| {
                started_sender.send(()).unwrap();
                active_gate.wait();
                token.is_cancelled()
            })
            .unwrap();
        let queued = executor.try_submit(|_| 7_u8).unwrap();
        started_receiver.recv().unwrap();
        queued.cancel();
        executor.shutdown();
        gate.wait();
        assert_eq!(wait(&active), TaskPoll::Completed(true));
        assert_eq!(
            wait(&queued),
            TaskPoll::Failed(TaskFailure::CancelledBeforeStart)
        );
        let report = executor.shutdown_and_join();
        assert_eq!(report.worker_count, 1);
        assert_eq!(report.joined_workers, 1);
        assert_eq!(report.detached_current_workers, 0);
        assert_eq!(report.panicked_workers, 0);
        assert!(matches!(
            executor.try_submit(|_| ()),
            Err(SubmitError::ShuttingDown)
        ));
    }

    #[test]
    fn deadline_shutdown_detaches_non_cooperative_worker() {
        let executor = BoundedExecutor::start(config(1, 1)).unwrap();
        let (started_sender, started_receiver) = std::sync::mpsc::sync_channel(1);
        let _active = executor
            .try_submit(move |_| {
                started_sender.send(()).unwrap();
                thread::sleep(Duration::from_millis(100));
            })
            .unwrap();
        started_receiver.recv().unwrap();

        let report = executor.shutdown_and_join_until(Instant::now() + Duration::from_millis(5));

        assert_eq!(report.worker_count, 1);
        assert_eq!(report.joined_workers, 0);
        assert_eq!(report.detached_current_workers, 0);
        assert_eq!(report.detached_deadline_workers, 1);
        assert_eq!(report.panicked_workers, 0);
    }

    #[test]
    fn terminal_notifier_covers_success_panic_and_cancel_before_start_exactly_once() {
        #[derive(Debug, PartialEq, Eq)]
        enum TerminalNotification {
            Success,
            Panic,
            CancelledBeforeStart,
        }

        struct ActiveTaskReleaseGuard(Arc<Barrier>);

        impl Drop for ActiveTaskReleaseGuard {
            fn drop(&mut self) {
                // Guard объявлен после executor-а, поэтому при unwind сначала
                // освобождает worker и лишь затем разрешает Executor::drop join.
                self.0.wait();
            }
        }

        let executor = BoundedExecutor::start(config(1, 4)).unwrap();
        let notification_count = Arc::new(AtomicUsize::new(0));
        // Нулевая ёмкость ратчетит production-order: main сначала обязан увидеть
        // terminal result и только после этого разблокировать соответствующий notifier.
        let (notification_sender, notification_receiver) = std::sync::mpsc::sync_channel(0);

        let success_count = Arc::clone(&notification_count);
        let success_notification_sender = notification_sender.clone();
        let success = executor
            .try_submit_with_terminal_notifier(
                |_| 11_u8,
                move || {
                    success_count.fetch_add(1, Ordering::AcqRel);
                    success_notification_sender
                        .send(TerminalNotification::Success)
                        .unwrap();
                },
            )
            .unwrap();
        assert_eq!(wait(&success), TaskPoll::Completed(11));
        assert_eq!(
            notification_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("success notifier did not follow terminal result"),
            TerminalNotification::Success
        );

        let panic_count = Arc::clone(&notification_count);
        let panic_notification_sender = notification_sender.clone();
        let panicked = executor
            .try_submit_with_terminal_notifier::<_, (), _>(
                |_| panic!("expected operation panic"),
                move || {
                    panic_count.fetch_add(1, Ordering::AcqRel);
                    panic_notification_sender
                        .send(TerminalNotification::Panic)
                        .unwrap();
                },
            )
            .unwrap();
        assert_eq!(wait(&panicked), TaskPoll::Failed(TaskFailure::Panicked));
        assert_eq!(
            notification_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("panic notifier did not follow terminal result"),
            TerminalNotification::Panic
        );

        let gate = Arc::new(Barrier::new(2));
        let (started_sender, started_receiver) = std::sync::mpsc::sync_channel(0);
        let active_gate = Arc::clone(&gate);
        let active = executor
            .try_submit(move |_| {
                // При аварийном исчезновении receiver-а operation выходит, не
                // заходя в barrier, поэтому Executor::drop не зависнет на join.
                if started_sender.send(()).is_err() {
                    return;
                }
                active_gate.wait();
            })
            .unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("active task did not occupy the sole worker");
        let active_release = ActiveTaskReleaseGuard(gate);

        let cancelled_count = Arc::clone(&notification_count);
        let cancelled_notification_sender = notification_sender.clone();
        let cancelled = executor
            .try_submit_with_terminal_notifier(
                |_| 99_u8,
                move || {
                    cancelled_count.fetch_add(1, Ordering::AcqRel);
                    cancelled_notification_sender
                        .send(TerminalNotification::CancelledBeforeStart)
                        .unwrap();
                },
            )
            .unwrap();
        drop(notification_sender);
        cancelled.cancel();
        drop(active_release);
        assert_eq!(wait(&active), TaskPoll::Completed(()));
        assert_eq!(
            wait(&cancelled),
            TaskPoll::Failed(TaskFailure::CancelledBeforeStart)
        );
        assert_eq!(
            notification_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("cancel notifier did not follow terminal result"),
            TerminalNotification::CancelledBeforeStart
        );

        let report = executor.shutdown_and_join();
        assert_eq!(report.joined_workers, 1);
        assert_eq!(
            notification_receiver.try_recv(),
            Err(TryRecvError::Disconnected)
        );
        assert_eq!(notification_count.load(Ordering::Acquire), 3);
    }

    #[test]
    fn notifier_panic_is_isolated_and_last_executor_arc_can_drop_on_worker() {
        let executor = Arc::new(BoundedExecutor::start(config(1, 2)).unwrap());
        let notifier_panics = executor
            .try_submit_with_terminal_notifier(|_| 1_u8, || panic!("expected notifier panic"))
            .unwrap();
        assert_eq!(wait(&notifier_panics), TaskPoll::Completed(1));

        let last_owner = Arc::clone(&executor);
        let self_drop = executor
            .try_submit(move |_| {
                drop(last_owner);
                7_u8
            })
            .unwrap();
        drop(executor);
        assert_eq!(wait(&self_drop), TaskPoll::Completed(7));
    }
}
