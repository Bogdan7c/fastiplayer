//! Общая process-neutral vocabulary для bounded shutdown фоновых владельцев.

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Абсолютный общий deadline process shutdown.
///
/// Абсолютное время не позволяет каждому owner-у заново получать полный timeout:
/// все владельцы расходуют один и тот же ограниченный бюджет.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShutdownDeadline {
    /// Момент, после которого ожидание фоновых потоков запрещено.
    expires_at: Instant,
}

impl ShutdownDeadline {
    /// Создаёт deadline из общего бюджета shutdown.
    #[must_use]
    pub(crate) fn after(timeout: Duration) -> Self {
        Self {
            expires_at: Instant::now() + timeout,
        }
    }

    /// Возвращает общий абсолютный момент для adapter-ов чужих owner API.
    ///
    /// Shell передаёт один и тот же момент всем владельцам, поэтому ни один
    /// adapter не может случайно начать новый полный timeout.
    #[must_use]
    pub(crate) const fn expires_at(self) -> Instant {
        self.expires_at
    }

    /// Возвращает оставшийся общий бюджет без отрицательной длительности.
    #[must_use]
    pub(crate) fn remaining(self) -> Duration {
        self.expires_at.saturating_duration_since(Instant::now())
    }

    /// Сообщает, что общий бюджет уже исчерпан.
    #[must_use]
    pub(crate) fn has_expired(self) -> bool {
        self.remaining().is_zero()
    }
}

/// Типизированный terminal outcome одного process owner-а.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcessOwnerShutdownOutcome {
    /// Все принадлежавшие owner-у потоки завершены и joined в этом вызове.
    Completed,

    /// Owner уже был полностью завершён предыдущим terminal вызовом.
    AlreadyCompleted,

    /// Deadline истёк; незавершённые handles остаются у owner-а.
    TimedOut {
        /// Число всё ещё принадлежащих owner-у потоков.
        pending_threads: usize,
    },

    /// Хотя бы один завершённый поток сообщил Rust panic при join.
    ThreadPanicked {
        /// Число обнаруженных panic-ов за terminal lifecycle owner-а.
        panicked_threads: usize,

        /// Число потоков, которые всё ещё не завершились к deadline.
        pending_threads: usize,
    },
}

/// Результат одной неблокирующей попытки join.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FinishedThreadJoin {
    /// Handle отсутствует: поток уже был joined или никогда не запускался.
    AlreadyJoined,

    /// Поток ещё работает; handle сохранён без detach.
    StillRunning,

    /// Завершённый поток успешно joined.
    Joined,

    /// Завершённый поток joined и сообщил panic.
    Panicked,
}

/// Join-ит только уже завершённый поток и сохраняет работающий handle в slot-е.
pub(crate) fn join_finished_thread(join_handle: &mut Option<JoinHandle<()>>) -> FinishedThreadJoin {
    let Some(handle) = join_handle.as_ref() else {
        return FinishedThreadJoin::AlreadyJoined;
    };
    if !handle.is_finished() {
        return FinishedThreadJoin::StillRunning;
    }

    let handle = join_handle
        .take()
        .expect("проверенный JoinHandle должен оставаться в owner slot");
    if handle.join().is_ok() {
        FinishedThreadJoin::Joined
    } else {
        FinishedThreadJoin::Panicked
    }
}

/// Даёт потоку завершиться до общего deadline, не вызывая blocking join заранее.
pub(crate) fn join_thread_until(
    join_handle: &mut Option<JoinHandle<()>>,
    deadline: ShutdownDeadline,
) -> FinishedThreadJoin {
    loop {
        let join_status = join_finished_thread(join_handle);
        if join_status != FinishedThreadJoin::StillRunning || deadline.has_expired() {
            return join_status;
        }

        let sleep_duration = deadline.remaining().min(Duration::from_millis(1));
        if sleep_duration.is_zero() {
            return FinishedThreadJoin::StillRunning;
        }
        std::thread::sleep(sleep_duration);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;

    #[test]
    fn timeout_retains_handle_and_later_reap_joins_it() {
        let release = Arc::new(AtomicBool::new(false));
        let worker_release = Arc::clone(&release);
        let mut handle = Some(std::thread::spawn(move || {
            while !worker_release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
        }));

        assert_eq!(
            join_thread_until(
                &mut handle,
                ShutdownDeadline::after(Duration::from_millis(1))
            ),
            FinishedThreadJoin::StillRunning
        );
        assert!(handle.is_some(), "timeout обязан сохранить join authority");

        release.store(true, Ordering::Release);
        assert_eq!(
            join_thread_until(&mut handle, ShutdownDeadline::after(Duration::from_secs(1))),
            FinishedThreadJoin::Joined
        );
        assert!(handle.is_none());
    }

    #[test]
    fn finished_panic_is_typed() {
        let mut handle = Some(std::thread::spawn(|| panic!("expected test panic")));

        assert_eq!(
            join_thread_until(&mut handle, ShutdownDeadline::after(Duration::from_secs(1))),
            FinishedThreadJoin::Panicked
        );
        assert!(handle.is_none());
    }
}
