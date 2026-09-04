use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};

/// Защитный предел одновременно живых async consumers одного seek intent-а.
///
/// В штатном transport path одновременно ждут единицы операций. Предел защищает
/// owner от ошибочного fan-out: переполнение явно отменяет seek вместо silent
/// lost wake либо неограниченного удержания task allocations.
const MAX_LIVE_DEMUX_SEEK_CANCELLATION_WAITERS: usize = 64;

/// Нейтральный cooperative-cancellation token одного demux seek intent-а.
///
/// Тип живёт в `media-core`, поэтому transport может ждать отмену асинхронно,
/// не протаскивая HTTP/Tokio API в общий demux contract. Один token принадлежит
/// ровно одному accepted seek; supersede отменяет только предыдущий token.
#[derive(Clone, Debug, Default)]
pub struct DemuxSeekCancellationToken {
    /// Общее состояние видят worker, transport и все transactional components.
    shared: Arc<DemuxSeekCancellationSharedState>,
}

/// Результат попытки завершить request-scoped seek до возможного supersede.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemuxSeekCancellationCompletion {
    /// Request доказал terminal replacement раньше cancellation.
    Completed,
    /// Cancellation уже победила race, поэтому replacement нельзя коммитить.
    CancellationWon,
}

/// Однонаправленное состояние lifecycle одного request-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum DemuxSeekCancellationStatus {
    /// Request ещё можно либо отменить, либо завершить.
    Pending = 0,
    /// Supersede победил и разбудил transport waiters.
    Cancelled = 1,
    /// Request доказан; поздний `cancel` обязан быть no-op.
    Completed = 2,
}

/// Общее состояние и ограниченный фактическим числом consumers набор wake-up handles.
#[derive(Debug, Default)]
struct DemuxSeekCancellationSharedState {
    /// Atomic fast path не требует mutex на обычной проверке parser/transport-а.
    status: AtomicU8,
    /// Waker-ы нужны только ожидающим async body operations; polling отсутствует.
    waiters: Mutex<DemuxSeekCancellationWaiterRegistry>,
    /// Blocking demux implementations могут ждать supersede без sleep/spin.
    cancelled_condition: Condvar,
}

/// Стабильная identity одной регистрации, принадлежащей конкретному future.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DemuxSeekCancellationWaiterId(u64);

/// Живая регистрация и только самый свежий waker соответствующего consumer-а.
#[derive(Debug)]
struct DemuxSeekCancellationWaiter {
    /// Identity позволяет `Drop` удалить ровно собственную регистрацию.
    id: DemuxSeekCancellationWaiterId,
    /// Последний waker заменяется при миграции future между executor tasks.
    waker: Waker,
}

/// Ограниченный registry живых futures; исторические polls здесь не остаются.
#[derive(Debug, Default)]
struct DemuxSeekCancellationWaiterRegistry {
    /// Следующая identity; коллизия с небольшим live set явно пропускается.
    next_id: u64,
    /// Число элементов никогда не превышает защитный named bound.
    live: Vec<DemuxSeekCancellationWaiter>,
}

impl DemuxSeekCancellationWaiterRegistry {
    /// Выделяет identity, не совпадающую с живыми registrations даже после wrap.
    fn allocate_id(&mut self) -> DemuxSeekCancellationWaiterId {
        loop {
            let candidate = DemuxSeekCancellationWaiterId(self.next_id);
            self.next_id = self.next_id.wrapping_add(1);
            if self.live.iter().all(|waiter| waiter.id != candidate) {
                return candidate;
            }
        }
    }

    /// Удаляет регистрацию конкретного уничтожаемого future, если она ещё жива.
    fn remove(&mut self, id: DemuxSeekCancellationWaiterId) {
        if let Some(index) = self.live.iter().position(|waiter| waiter.id == id) {
            self.live.swap_remove(index);
        }
    }

    /// Забирает wakers для пробуждения уже без удержания registry mutex-а.
    fn take_wakers(&mut self) -> Vec<Waker> {
        std::mem::take(&mut self.live)
            .into_iter()
            .map(|waiter| waiter.waker)
            .collect()
    }
}

impl DemuxSeekCancellationToken {
    /// Создаёт активный token для нового accepted seek intent-а.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Отменяет seek и будит все transport/component waiters ровно один раз.
    pub fn cancel(&self) {
        if self
            .shared
            .status
            .compare_exchange(
                DemuxSeekCancellationStatus::Pending as u8,
                DemuxSeekCancellationStatus::Cancelled as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }
        let waiters = {
            let mut waiters = self
                .shared
                .waiters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            waiters.take_wakers()
        };
        for waiter in waiters {
            waiter.wake();
        }
        self.shared.cancelled_condition.notify_all();
    }

    /// Завершает request до commit-а доказанного replacement-а.
    ///
    /// После `Completed` поздний supersede не отменяет уже установленный source.
    /// Повторный вызов идемпотентен, а `CancellationWon` запрещает commit.
    #[must_use]
    pub fn complete(&self) -> DemuxSeekCancellationCompletion {
        match self.shared.status.compare_exchange(
            DemuxSeekCancellationStatus::Pending as u8,
            DemuxSeekCancellationStatus::Completed as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                self.shared
                    .waiters
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .live
                    .clear();
                DemuxSeekCancellationCompletion::Completed
            }
            Err(value) if value == DemuxSeekCancellationStatus::Completed as u8 => {
                DemuxSeekCancellationCompletion::Completed
            }
            Err(value) if value == DemuxSeekCancellationStatus::Cancelled as u8 => {
                DemuxSeekCancellationCompletion::CancellationWon
            }
            Err(value) => unreachable!("недопустимое состояние seek cancellation token: {value}"),
        }
    }

    /// Возвращает `true`, когда newer intent уже supersede-нул эту операцию.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.status() == DemuxSeekCancellationStatus::Cancelled
    }

    /// Возвращает runtime-neutral future без timer/polling нагрузки.
    #[must_use]
    pub fn cancelled(&self) -> DemuxSeekCancelled<'_> {
        DemuxSeekCancelled {
            token: self,
            waiter_id: None,
        }
    }

    /// Блокирует текущий worker без polling до отмены именно этого seek intent-а.
    pub fn wait_cancelled(&self) {
        self.wait_cancelled_with_observer(None);
    }

    /// Реализует blocking wait и допускает owner-local наблюдение точки перед `Condvar::wait`.
    fn wait_cancelled_with_observer(&self, mut before_wait: Option<&mut dyn FnMut()>) {
        let mut waiters = self
            .shared
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !self.is_cancelled() {
            // Observer вызывается под waiter mutex после проверки pending predicate.
            if let Some(observer) = before_wait.as_mut() {
                observer();
            }
            waiters = self
                .shared
                .cancelled_condition
                .wait(waiters)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Читает typed lifecycle state из внутреннего atomic представления.
    fn status(&self) -> DemuxSeekCancellationStatus {
        match self.shared.status.load(Ordering::Acquire) {
            value if value == DemuxSeekCancellationStatus::Pending as u8 => {
                DemuxSeekCancellationStatus::Pending
            }
            value if value == DemuxSeekCancellationStatus::Cancelled as u8 => {
                DemuxSeekCancellationStatus::Cancelled
            }
            value if value == DemuxSeekCancellationStatus::Completed as u8 => {
                DemuxSeekCancellationStatus::Completed
            }
            value => unreachable!("недопустимое состояние seek cancellation token: {value}"),
        }
    }
}

/// Borrowed future завершается после отмены соответствующего seek intent-а.
#[derive(Debug)]
pub struct DemuxSeekCancelled<'token> {
    /// Borrow не позволяет future пережить owner token-а случайно.
    token: &'token DemuxSeekCancellationToken,
    /// `Some` означает ровно одну live registration, удаляемую в `Drop`.
    waiter_id: Option<DemuxSeekCancellationWaiterId>,
}

impl DemuxSeekCancelled<'_> {
    /// Удаляет принадлежащую future регистрацию; terminal transition мог уже забрать её.
    fn unregister(&mut self) {
        let Some(waiter_id) = self.waiter_id.take() else {
            return;
        };
        self.token
            .shared
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(waiter_id);
    }
}

impl Drop for DemuxSeekCancelled<'_> {
    fn drop(&mut self) {
        self.unregister();
    }
}

impl Future for DemuxSeekCancelled<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let future = self.as_mut().get_mut();
        match future.token.status() {
            DemuxSeekCancellationStatus::Cancelled => {
                future.unregister();
                return Poll::Ready(());
            }
            DemuxSeekCancellationStatus::Completed => {
                future.unregister();
                return Poll::Pending;
            }
            DemuxSeekCancellationStatus::Pending => {}
        }
        let mut waiters = future
            .token
            .shared
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match future.token.status() {
            DemuxSeekCancellationStatus::Cancelled => {
                future.waiter_id = None;
                return Poll::Ready(());
            }
            DemuxSeekCancellationStatus::Completed => {
                future.waiter_id = None;
                return Poll::Pending;
            }
            DemuxSeekCancellationStatus::Pending => {}
        }

        if let Some(waiter_id) = future.waiter_id
            && let Some(registered) = waiters
                .live
                .iter_mut()
                .find(|waiter| waiter.id == waiter_id)
        {
            if !registered.waker.will_wake(context.waker()) {
                registered.waker.clone_from(context.waker());
            }
            return Poll::Pending;
        }

        future.waiter_id = None;
        if waiters.live.len() >= MAX_LIVE_DEMUX_SEEK_CANCELLATION_WAITERS {
            drop(waiters);
            future.token.cancel();
            return match future.token.status() {
                DemuxSeekCancellationStatus::Cancelled => Poll::Ready(()),
                DemuxSeekCancellationStatus::Completed => Poll::Pending,
                DemuxSeekCancellationStatus::Pending => {
                    unreachable!("fail-closed cancellation обязана завершить pending seek intent")
                }
            };
        }

        let waiter_id = waiters.allocate_id();
        waiters.live.push(DemuxSeekCancellationWaiter {
            id: waiter_id,
            waker: context.waker().clone(),
        });
        future.waiter_id = Some(waiter_id);
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};
    use std::task::{Wake, Waker};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{
        DemuxSeekCancellationCompletion, DemuxSeekCancellationToken,
        MAX_LIVE_DEMUX_SEEK_CANCELLATION_WAITERS,
    };

    /// Test waker фиксирует wake без runtime либо wall-clock ожиданий.
    struct RecordingWake {
        /// Atomic marker делает assertion deterministic.
        woke: AtomicBool,
    }

    impl Wake for RecordingWake {
        fn wake(self: Arc<Self>) {
            self.woke.store(true, Ordering::Release);
        }
    }

    /// Counting waker доказывает точное число wake без scheduler-а.
    struct CountingWake {
        /// Счётчик отличает live waiters от уже уничтоженной истории polls.
        wake_count: AtomicUsize,
    }

    impl Wake for CountingWake {
        fn wake(self: Arc<Self>) {
            self.wake_count.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Возвращает число живых registrations для owner-local invariant assertions.
    fn live_waiter_count(token: &DemuxSeekCancellationToken) -> usize {
        token
            .shared
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live
            .len()
    }

    #[test]
    fn cancellation_wakes_registered_future_without_polling() {
        let token = DemuxSeekCancellationToken::new();
        let recording = Arc::new(RecordingWake {
            woke: AtomicBool::new(false),
        });
        let waker = Waker::from(Arc::clone(&recording));
        let mut context = std::task::Context::from_waker(&waker);
        let mut cancelled = std::pin::pin!(token.cancelled());

        assert!(cancelled.as_mut().poll(&mut context).is_pending());
        token.cancel();

        assert!(recording.woke.load(Ordering::Acquire));
        assert!(cancelled.as_mut().poll(&mut context).is_ready());
    }

    /// Доказывает реальный blocking wait и condvar wake, а не удачный pre-cancel schedule.
    #[test]
    fn blocking_wait_enters_condvar_before_cancellation_wakes_worker() {
        // Оба sync_channel(0) являются точными rendezvous без буферизованного опережения.
        let (entered_sender, entered_receiver) = mpsc::sync_channel(0);
        let (proceed_sender, proceed_receiver) = mpsc::sync_channel(0);
        // Token остаётся production owner-ом status, waiter mutex и condvar.
        let token = DemuxSeekCancellationToken::new();

        // Worker использует тот же owner-local implementation, что и публичный boundary.
        let waiter_token = token.clone();
        let waiter = thread::spawn(move || {
            // Observer выполняет только rendezvous и не меняет cancellation state.
            let mut before_wait = || {
                // Entered подтверждает уже выполненную pending-проверку под waiter mutex.
                entered_sender
                    .send(())
                    .expect("blocking wait test owner должен принять entered signal");
                // Owner сначала запускает cancel и только затем разрешает mutex release.
                proceed_receiver
                    .recv()
                    .expect("blocking wait test owner должен разрешить condvar wait");
            };
            waiter_token.wait_cancelled_with_observer(Some(&mut before_wait));
        });
        // Entered приходит после pending predicate, пока worker ещё держит waiter mutex.
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("blocking wait worker должен войти в test rendezvous");

        // Cancel сначала меняет atomic status, затем блокируется на waiter mutex.
        let cancellation_token = token.clone();
        let canceller = thread::spawn(move || cancellation_token.cancel());
        // Deadline превращает невозможную смену lifecycle в явный test failure.
        let cancellation_deadline = Instant::now() + Duration::from_secs(1);
        while !token.is_cancelled() {
            assert!(
                Instant::now() < cancellation_deadline,
                "cancel должен опубликовать atomic status до ожидания waiter mutex"
            );
            thread::yield_now();
        }
        // Теперь worker гарантированно вызывает Condvar::wait с уже истинной отменой.
        proceed_sender
            .send(())
            .expect("blocking wait worker должен принять proceed signal");

        // Cancel получает атомарно освобождённый mutex и будит уже спящий worker.
        canceller
            .join()
            .expect("cancellation worker не должен panic");
        waiter
            .join()
            .expect("blocking wait worker должен проснуться");
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancellation_is_shared_and_idempotent() {
        let token = DemuxSeekCancellationToken::new();
        let observer = token.clone();

        token.cancel();
        token.cancel();

        assert!(observer.is_cancelled());
    }

    #[test]
    fn dropped_ready_operation_interrupts_do_not_retain_historical_wakers() {
        let token = DemuxSeekCancellationToken::new();

        for _ in 0..MAX_LIVE_DEMUX_SEEK_CANCELLATION_WAITERS * 4 {
            let recording = Arc::new(CountingWake {
                wake_count: AtomicUsize::new(0),
            });
            let waker = Waker::from(Arc::clone(&recording));
            let mut context = std::task::Context::from_waker(&waker);
            let mut cancelled = Box::pin(token.cancelled());

            assert!(cancelled.as_mut().poll(&mut context).is_pending());
            assert_eq!(live_waiter_count(&token), 1);
            drop(cancelled);
            assert_eq!(live_waiter_count(&token), 0);
            assert_eq!(recording.wake_count.load(Ordering::Acquire), 0);
        }

        assert!(!token.is_cancelled());
    }

    #[test]
    fn repoll_keeps_only_latest_waker_for_one_live_future() {
        let token = DemuxSeekCancellationToken::new();
        let first_recording = Arc::new(CountingWake {
            wake_count: AtomicUsize::new(0),
        });
        let second_recording = Arc::new(CountingWake {
            wake_count: AtomicUsize::new(0),
        });
        let first_waker = Waker::from(Arc::clone(&first_recording));
        let second_waker = Waker::from(Arc::clone(&second_recording));
        let mut first_context = std::task::Context::from_waker(&first_waker);
        let mut second_context = std::task::Context::from_waker(&second_waker);
        let mut cancelled = Box::pin(token.cancelled());

        assert!(cancelled.as_mut().poll(&mut first_context).is_pending());
        assert!(cancelled.as_mut().poll(&mut second_context).is_pending());
        assert_eq!(live_waiter_count(&token), 1);

        token.cancel();

        assert_eq!(first_recording.wake_count.load(Ordering::Acquire), 0);
        assert_eq!(second_recording.wake_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn cancellation_wakes_each_live_waiter_once_and_clears_registry() {
        let token = DemuxSeekCancellationToken::new();
        let mut recordings = Vec::new();
        let mut cancellations = Vec::new();

        for _ in 0..4 {
            let recording = Arc::new(CountingWake {
                wake_count: AtomicUsize::new(0),
            });
            let waker = Waker::from(Arc::clone(&recording));
            let mut context = std::task::Context::from_waker(&waker);
            let mut cancelled = Box::pin(token.cancelled());
            assert!(cancelled.as_mut().poll(&mut context).is_pending());
            recordings.push(recording);
            cancellations.push(cancelled);
        }
        assert_eq!(live_waiter_count(&token), recordings.len());

        token.cancel();
        token.cancel();

        assert_eq!(live_waiter_count(&token), 0);
        assert!(
            recordings
                .iter()
                .all(|recording| recording.wake_count.load(Ordering::Acquire) == 1)
        );
        assert!(cancellations.iter_mut().all(|cancelled| {
            cancelled
                .as_mut()
                .poll(&mut std::task::Context::from_waker(Waker::noop()))
                .is_ready()
        }));
    }

    #[test]
    fn waiter_bound_overflow_fails_closed_without_lost_wake() {
        let token = DemuxSeekCancellationToken::new();
        let mut recordings = Vec::new();
        let mut cancellations = Vec::new();

        for _ in 0..MAX_LIVE_DEMUX_SEEK_CANCELLATION_WAITERS {
            let recording = Arc::new(CountingWake {
                wake_count: AtomicUsize::new(0),
            });
            let waker = Waker::from(Arc::clone(&recording));
            let mut context = std::task::Context::from_waker(&waker);
            let mut cancelled = Box::pin(token.cancelled());
            assert!(cancelled.as_mut().poll(&mut context).is_pending());
            recordings.push(recording);
            cancellations.push(cancelled);
        }
        assert_eq!(
            live_waiter_count(&token),
            MAX_LIVE_DEMUX_SEEK_CANCELLATION_WAITERS
        );

        let overflow_recording = Arc::new(CountingWake {
            wake_count: AtomicUsize::new(0),
        });
        let overflow_waker = Waker::from(Arc::clone(&overflow_recording));
        let mut overflow_context = std::task::Context::from_waker(&overflow_waker);
        let mut overflow = Box::pin(token.cancelled());

        assert!(overflow.as_mut().poll(&mut overflow_context).is_ready());
        assert!(token.is_cancelled());
        assert_eq!(live_waiter_count(&token), 0);
        assert_eq!(overflow_recording.wake_count.load(Ordering::Acquire), 0);
        assert!(
            recordings
                .iter()
                .all(|recording| recording.wake_count.load(Ordering::Acquire) == 1)
        );
    }

    #[test]
    fn completion_wins_before_late_cancellation_without_waking_waiters() {
        let token = DemuxSeekCancellationToken::new();
        let recording = Arc::new(RecordingWake {
            woke: AtomicBool::new(false),
        });
        let waker = Waker::from(Arc::clone(&recording));
        let mut context = std::task::Context::from_waker(&waker);
        let mut cancelled = std::pin::pin!(token.cancelled());

        assert!(cancelled.as_mut().poll(&mut context).is_pending());
        assert_eq!(token.complete(), DemuxSeekCancellationCompletion::Completed);
        token.cancel();

        assert!(!token.is_cancelled());
        assert!(!recording.woke.load(Ordering::Acquire));
        assert!(cancelled.as_mut().poll(&mut context).is_pending());
        assert_eq!(token.complete(), DemuxSeekCancellationCompletion::Completed);
    }

    #[test]
    fn cancellation_wins_before_completion() {
        let token = DemuxSeekCancellationToken::new();

        token.cancel();

        assert_eq!(
            token.complete(),
            DemuxSeekCancellationCompletion::CancellationWon
        );
        assert!(token.is_cancelled());
    }
}
