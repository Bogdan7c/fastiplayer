use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

/// Один source request имеет мало независимых async waiters; overflow отменяет
/// операцию fail-closed вместо unbounded накопления или polling spin-а.
const MAX_UNIQUE_CANCELLATION_WAKERS: usize = 8;

/// Один уникальный executor waker может обслуживать несколько borrowed futures.
#[derive(Debug)]
struct RegisteredCancellationWaker {
    /// Последний executor waker, который обязан получить wake после cancel.
    waker: Waker,
    /// Число живых futures с эквивалентным `will_wake` identity.
    registrations: usize,
}

/// Shared state объединяет sampled flag и bounded wake-based ожидание.
#[derive(Debug, Default)]
struct CancellationSharedState {
    /// One-way false -> true transition видят sync и async consumers.
    cancelled: AtomicBool,
    /// Mutex закрывает lost-wake race между повторной проверкой flag-а и register.
    waiters: Mutex<Vec<RegisteredCancellationWaker>>,
}

impl CancellationSharedState {
    /// Возвращает guard даже после чужой panic: простое waker состояние восстанавливаемо.
    fn lock_waiters(&self) -> MutexGuard<'_, Vec<RegisteredCancellationWaker>> {
        self.waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Потокобезопасный token отмены для sync и wake-based async source-операций.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    /// Общий one-way state, который видят все clone-ы token-а.
    shared: Arc<CancellationSharedState>,
}

impl CancellationToken {
    /// Создаёт token без запрошенной отмены.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Создаёт token, который остаётся активным, пока владелец не вызовет `cancel`.
    #[must_use]
    pub fn never_cancelled() -> Self {
        Self::default()
    }

    /// Запрашивает остановку и будит каждый уникальный зарегистрированный executor.
    pub fn cancel(&self) {
        if self
            .shared
            .cancelled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let waiters = {
            let mut registered_waiters = self.shared.lock_waiters();
            mem::take(&mut *registered_waiters)
        };
        for waiter in waiters {
            waiter.waker.wake();
        }
    }

    /// Возвращает borrowed standard future без Tokio/futures dependency.
    #[must_use]
    pub fn cancelled(&self) -> CancellationFuture<'_> {
        CancellationFuture {
            shared: &self.shared,
            registered_waker: None,
        }
    }

    /// Возвращает `true`, если caller уже запросил отмену.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.shared.cancelled.load(Ordering::Acquire)
    }
}

/// Borrowed future завершается только после one-way cancellation transition-а.
pub struct CancellationFuture<'token> {
    /// Borrow не продлевает lifecycle token-а скрытым owned clone-ом.
    shared: &'token CancellationSharedState,
    /// Последняя registration нужна для замены waker-а и cleanup на Drop.
    registered_waker: Option<Waker>,
}

impl Future for CancellationFuture<'_> {
    type Output = ();

    /// Регистрирует executor waker без polling и закрывает lost-wake race.
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let cancellation = self.get_mut();
        if cancellation.shared.cancelled.load(Ordering::Acquire) {
            return Poll::Ready(());
        }

        let mut registered_waiters = cancellation.shared.lock_waiters();
        if cancellation.shared.cancelled.load(Ordering::Acquire) {
            return Poll::Ready(());
        }

        if let Some(previous_waker) = cancellation.registered_waker.take() {
            if previous_waker.will_wake(context.waker()) {
                cancellation.registered_waker = Some(previous_waker);
                return Poll::Pending;
            }
            unregister_waker(&mut registered_waiters, &previous_waker);
        }

        if let Some(existing_waiter) = registered_waiters
            .iter_mut()
            .find(|waiter| waiter.waker.will_wake(context.waker()))
        {
            existing_waiter.registrations = existing_waiter.registrations.saturating_add(1);
            cancellation.registered_waker = Some(context.waker().clone());
            return Poll::Pending;
        }

        if registered_waiters.len() == MAX_UNIQUE_CANCELLATION_WAKERS {
            let cancellation_won = cancellation
                .shared
                .cancelled
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
            let waiters_to_wake = if cancellation_won {
                mem::take(&mut *registered_waiters)
            } else {
                Vec::new()
            };
            drop(registered_waiters);
            for waiter in waiters_to_wake {
                waiter.waker.wake();
            }
            return Poll::Ready(());
        }

        registered_waiters.push(RegisteredCancellationWaker {
            waker: context.waker().clone(),
            registrations: 1,
        });
        cancellation.registered_waker = Some(context.waker().clone());
        Poll::Pending
    }
}

impl Drop for CancellationFuture<'_> {
    /// Удаляет registration, чтобы dropped requests не занимали bounded slots.
    fn drop(&mut self) {
        let Some(registered_waker) = self.registered_waker.take() else {
            return;
        };
        let mut registered_waiters = self.shared.lock_waiters();
        unregister_waker(&mut registered_waiters, &registered_waker);
    }
}

impl std::fmt::Debug for CancellationFuture<'_> {
    /// Debug не раскрывает executor internals.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CancellationFuture")
            .field("registered", &self.registered_waker.is_some())
            .finish_non_exhaustive()
    }
}

/// Уменьшает shared registration count и удаляет последний unique waker.
fn unregister_waker(waiters: &mut Vec<RegisteredCancellationWaker>, waker: &Waker) {
    let Some(waiter_index) = waiters
        .iter()
        .position(|waiter| waiter.waker.will_wake(waker))
    else {
        return;
    };
    if waiters[waiter_index].registrations > 1 {
        waiters[waiter_index].registrations -= 1;
    } else {
        waiters.swap_remove(waiter_index);
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    use super::{CancellationToken, MAX_UNIQUE_CANCELLATION_WAKERS};

    /// Считает настоящие wake calls без runtime или polling helper-а.
    #[derive(Default)]
    struct WakeCounter {
        wake_count: AtomicUsize,
    }

    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.wake_count.fetch_add(1, Ordering::AcqRel);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.wake_count.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Создаёт standard waker и оставляет счётчик доступным assertions.
    fn counting_waker() -> (Arc<WakeCounter>, Waker) {
        let counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&counter));
        (counter, waker)
    }

    #[test]
    fn registered_future_is_woken_without_polling_after_cancel() {
        let token = CancellationToken::new();
        let (counter, waker) = counting_waker();
        let mut context = Context::from_waker(&waker);
        let mut cancellation = pin!(token.cancelled());

        assert_eq!(cancellation.as_mut().poll(&mut context), Poll::Pending);
        assert_eq!(counter.wake_count.load(Ordering::Acquire), 0);

        token.cancel();

        assert_eq!(counter.wake_count.load(Ordering::Acquire), 1);
        assert_eq!(cancellation.as_mut().poll(&mut context), Poll::Ready(()));
    }

    #[test]
    fn clones_share_idempotent_cancel_and_equivalent_waker_is_registered_once() {
        let token = CancellationToken::new();
        let token_clone = token.clone();
        let (counter, waker) = counting_waker();
        let mut context = Context::from_waker(&waker);
        let mut first = pin!(token.cancelled());
        let mut second = pin!(token_clone.cancelled());

        assert_eq!(first.as_mut().poll(&mut context), Poll::Pending);
        assert_eq!(second.as_mut().poll(&mut context), Poll::Pending);

        token_clone.cancel();
        token.cancel();

        assert!(token.is_cancelled());
        assert!(token_clone.is_cancelled());
        assert_eq!(counter.wake_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn dropped_future_releases_unique_waker_slot() {
        let token = CancellationToken::new();
        let (counter, waker) = counting_waker();
        let mut context = Context::from_waker(&waker);
        {
            let mut cancellation = pin!(token.cancelled());
            assert_eq!(cancellation.as_mut().poll(&mut context), Poll::Pending);
        }

        token.cancel();

        assert_eq!(counter.wake_count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn unique_waker_capacity_exhaustion_cancels_fail_closed() {
        let token = CancellationToken::new();
        let mut counters_and_wakers = Vec::new();
        let mut cancellations = Vec::new();
        for _ in 0..=MAX_UNIQUE_CANCELLATION_WAKERS {
            counters_and_wakers.push(counting_waker());
            cancellations.push(Box::pin(token.cancelled()));
        }

        for (index, cancellation) in cancellations.iter_mut().enumerate() {
            let mut context = Context::from_waker(&counters_and_wakers[index].1);
            let outcome = cancellation.as_mut().poll(&mut context);
            if index < MAX_UNIQUE_CANCELLATION_WAKERS {
                assert_eq!(outcome, Poll::Pending);
            } else {
                assert_eq!(outcome, Poll::Ready(()));
            }
        }

        assert!(token.is_cancelled());
        for (counter, _) in counters_and_wakers
            .iter()
            .take(MAX_UNIQUE_CANCELLATION_WAKERS)
        {
            assert_eq!(counter.wake_count.load(Ordering::Acquire), 1);
        }
    }
}
