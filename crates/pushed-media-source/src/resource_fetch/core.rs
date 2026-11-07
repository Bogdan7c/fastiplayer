//! Общее ядро очереди: состояние под замком, ожидания и будильники.
//!
//! Ядро делят четыре стороны: движок (`start` и чтение кусков, блокирующее или
//! асинхронное), приложение (забирает запросы), писатель (внешняя сторона несёт
//! байты) и сторож молчания (поток для асинхронных ожиданий). Правила:
//! - **ничего не ждём и никого не зовём под замком**: ожидание — `Condvar`
//!   (замок отпускается), будильники и отправитель зовутся после снятия замка;
//! - блокирующие ожидания отменяемы `CancellationToken` (опрос раз в 10 мс);
//! - срок молчания проверяется в начале каждой операции (`sweep_silent`), поэтому
//!   все стороны видят одно и то же решение «страница молчит»;
//! - отравленный замок не паникует: состояние читается дальше.
//!
//! # Срок молчания в асинхронной форме
//! Асинхронный потребитель сам не проснётся, если страница молчит: его некому
//! будить. Поэтому у очереди есть **один** поток-сторож на всю очередь (не на
//! запрос): он запускается, когда кто-то впервые ждёт асинхронно, спит до
//! ближайшего срока молчания среди асинхронно ждущих, признаёт молчащих мёртвыми и
//! будит их; когда асинхронно ждущих не осталось, поток заканчивается сам.
//! Блокирующему потребителю сторож не нужен: он просыпается раз в 10 мс и видит
//! срок сам.

use std::num::NonZeroUsize;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use source_core::CancellationToken;

use super::error::{ResourceFetchError, ResourceFetchViolation};
use super::policy::ResourceFetchPolicy;
use super::record::{ChunkStep, WakeList};
use super::request::{ResourceFetchSpec, ResourceRequest};
use super::requests::ResourceSealCut;
use super::state::{IssueStep, QueueState, WatchdogDemand};
use super::stats::ResourceFetchStats;
use super::writer::{ResourceFetchOutcome, ResourceFrameVerdict, ResourceRequestClaim};
use crate::range_file::{RangeRequestId, RangeRequestIds, RangeRequestNotifier};

/// Шаг опроса отмены во время блокирующего ожидания: отмена срабатывает за ≤ 10 мс.
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Имя потока-сторожа (видно в отладчике и `top -H`).
const WATCHDOG_THREAD_NAME: &str = "resource-fetch-watchdog";

/// Ядро очереди.
pub(super) struct FetchCore {
    /// Состояние.
    state: Mutex<QueueState>,
    /// Будит блокирующих ждущих (места и кусков) при каждом изменении.
    changed: Condvar,
    /// Будит сторожа: новый асинхронно ждущий или источник закрыт.
    watchdog_wake: Condvar,
    /// Выдаватель номеров сеанса (общий с файлом по запросам байтов).
    ids: RangeRequestIds,
    /// Будильник отправителя запросов.
    notifier: Arc<dyn RangeRequestNotifier>,
    /// Пределы.
    policy: ResourceFetchPolicy,
}

impl FetchCore {
    /// Ядро новой очереди.
    pub(super) fn new(
        policy: ResourceFetchPolicy,
        ids: RangeRequestIds,
        notifier: Arc<dyn RangeRequestNotifier>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(QueueState::new()),
            changed: Condvar::new(),
            watchdog_wake: Condvar::new(),
            ids,
            notifier,
            policy,
        })
    }

    /// Захватывает состояние; отравленный замок не мешает читать.
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Захватывает состояние и сразу признаёт молчащих мёртвыми.
    fn lock_swept(&self, wake: &mut WakeList) -> MutexGuard<'_, QueueState> {
        let mut guard = self.lock();
        guard.sweep_silent(Instant::now(), self.idle_timeout(), wake);
        guard
    }

    /// Снимает замок, будит асинхронных и блокирующих ждущих.
    fn release(&self, guard: MutexGuard<'_, QueueState>, wake: WakeList) {
        drop(guard);
        wake.wake_all();
        self.changed.notify_all();
    }

    /// Шаг блокирующего ожидания: если молчание кого-то признало мёртвым, сначала
    /// будим их (без замка) и сразу смотрим снова; иначе ждём изменения не дольше
    /// шага опроса отмены. В конце — свежая проверка молчания в `wake`.
    fn wait_or_wake_others<'guard>(
        &'guard self,
        guard: MutexGuard<'guard, QueueState>,
        wake: &mut WakeList,
    ) -> MutexGuard<'guard, QueueState> {
        let mut guard = if wake.is_empty() {
            self.changed
                .wait_timeout(guard, CANCELLATION_POLL_INTERVAL)
                .unwrap_or_else(PoisonError::into_inner)
                .0
        } else {
            self.release(guard, std::mem::take(wake));
            self.lock()
        };
        guard.sweep_silent(Instant::now(), self.idle_timeout(), wake);
        guard
    }

    /// Срок молчания политики.
    const fn idle_timeout(&self) -> Duration {
        self.policy.idle_timeout()
    }

    /// Сколько мест в полёте.
    const fn max_in_flight(&self) -> NonZeroUsize {
        self.policy.max_in_flight()
    }

    // ------------------------------------------------------------- движок: старт

    /// Выдаёт номер новому запросу; ждёт места, если все заняты.
    pub(super) fn start(
        &self,
        spec: ResourceFetchSpec,
        cancellation: &CancellationToken,
    ) -> Result<RangeRequestId, ResourceFetchError> {
        let ceiling = self.policy.max_resource_bytes().get();
        if spec.max_bytes.get() > ceiling {
            return Err(ResourceFetchError::ExceedsResourceCeiling {
                requested: spec.max_bytes.get(),
                ceiling,
            });
        }
        let mut wake = WakeList::default();
        let mut guard = self.lock_swept(&mut wake);
        let ticket = guard.join_starters();
        loop {
            if cancellation.is_cancelled() {
                guard.leave_starters(ticket);
                // Ушедший из очереди мог держать первого в очереди: будим остальных.
                self.release(guard, wake);
                return Err(ResourceFetchError::Cancelled);
            }
            match guard.try_issue(ticket, spec, &self.ids, self.max_in_flight()) {
                IssueStep::Issued(id) => {
                    self.release(guard, wake);
                    // Отправитель зовётся строго без замка: он вправе сразу забрать запрос.
                    self.notifier.range_requested();
                    return Ok(id);
                }
                IssueStep::Failed(error) => {
                    self.release(guard, wake);
                    return Err(error);
                }
                IssueStep::Wait => {}
            }
            guard = self.wait_or_wake_others(guard, &mut wake);
        }
    }

    // ------------------------------------------------------------ движок: чтение

    /// Следующий кусок, блокирующее ожидание.
    pub(super) fn next_chunk_blocking(
        &self,
        id: RangeRequestId,
        cancellation: &CancellationToken,
    ) -> Result<Option<Bytes>, ResourceFetchError> {
        let mut wake = WakeList::default();
        let mut guard = self.lock_swept(&mut wake);
        loop {
            if cancellation.is_cancelled() {
                self.release(guard, wake);
                return Err(ResourceFetchError::Cancelled);
            }
            match guard.next_chunk(id, self.idle_timeout()) {
                ChunkStep::Chunk(chunk) => {
                    self.release(guard, wake);
                    return Ok(Some(chunk));
                }
                ChunkStep::End => {
                    self.release(guard, wake);
                    return Ok(None);
                }
                ChunkStep::Failed(error) => {
                    self.release(guard, wake);
                    return Err(error);
                }
                ChunkStep::Pending => {}
            }
            guard = self.wait_or_wake_others(guard, &mut wake);
        }
    }

    /// Следующий кусок, асинхронная форма: `Pending` регистрирует будильник.
    pub(super) fn poll_chunk(
        self: &Arc<Self>,
        id: RangeRequestId,
        context: &Context<'_>,
    ) -> Poll<Result<Option<Bytes>, ResourceFetchError>> {
        let mut wake = WakeList::default();
        let mut guard = self.lock_swept(&mut wake);
        let ready = match guard.next_chunk(id, self.idle_timeout()) {
            ChunkStep::Chunk(chunk) => Ok(Some(chunk)),
            ChunkStep::End => Ok(None),
            ChunkStep::Failed(error) => Err(error),
            ChunkStep::Pending => {
                let demand = guard.register_waker(id, context.waker());
                self.release(guard, wake);
                return match self.ensure_watchdog(demand) {
                    Ok(()) => Poll::Pending,
                    Err(error) => Poll::Ready(Err(error)),
                };
            }
        };
        self.release(guard, wake);
        Poll::Ready(ready)
    }

    /// Запускает сторожа молчания, если он не работает, иначе будит его: новый
    /// ждущий мог прийти с более ранним сроком.
    fn ensure_watchdog(self: &Arc<Self>, demand: WatchdogDemand) -> Result<(), ResourceFetchError> {
        match demand {
            WatchdogDemand::AlreadyRunning => {
                self.watchdog_wake.notify_all();
                Ok(())
            }
            WatchdogDemand::StartNow => {
                let core = Arc::clone(self);
                let spawned = thread::Builder::new()
                    .name(WATCHDOG_THREAD_NAME.to_owned())
                    .spawn(move || core.run_watchdog());
                match spawned {
                    // Поток отсоединён: он кончится сам, когда ждать станет некого.
                    Ok(_detached) => Ok(()),
                    Err(error) => {
                        // Без сторожа асинхронное ожидание могло бы висеть вечно: честный отказ.
                        self.lock().watchdog_failed_to_start();
                        tracing::warn!(%error, "resource fetch silence watchdog could not start");
                        Err(ResourceFetchError::SilenceWatchdogUnavailable {
                            reason: error.to_string(),
                        })
                    }
                }
            }
        }
    }

    /// Тело потока-сторожа: спит до ближайшего срока, будит молчащих, кончается,
    /// когда асинхронно ждущих не осталось.
    fn run_watchdog(&self) {
        let idle_timeout = self.idle_timeout();
        let mut guard = self.lock();
        loop {
            let now = Instant::now();
            let mut wake = WakeList::default();
            guard.sweep_silent(now, idle_timeout, &mut wake);
            if !wake.is_empty() {
                self.release(guard, wake);
                guard = self.lock();
                continue;
            }
            let Some(deadline) = guard.next_async_deadline_or_stop(idle_timeout) else {
                // Отметка «сторож остановлен» поставлена под замком: новый ждущий
                // после этого запустит свежего сторожа.
                return;
            };
            let pause = deadline.saturating_duration_since(now);
            guard = self
                .watchdog_wake
                .wait_timeout(guard, pause)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Сколько байтов пришло по запросу.
    pub(super) fn received(&self, id: RangeRequestId) -> u64 {
        self.lock().received(id)
    }

    /// Потребитель ушёл (уничтожил хэндл запроса).
    pub(super) fn abandon(&self, id: RangeRequestId) {
        let mut wake = WakeList::default();
        let mut guard = self.lock_swept(&mut wake);
        guard.abandon(id, Instant::now());
        // Брошенный до отправки запрос освободил место: ждущий `start` проснётся.
        self.release(guard, wake);
    }

    // --------------------------------------------------------------- приложение

    /// Запросы для отправки в порядке выдачи.
    pub(super) fn take_requests(&self) -> Vec<ResourceRequest> {
        let mut wake = WakeList::default();
        let mut guard = self.lock();
        let requests = guard.take_requests(Instant::now(), &mut wake);
        // Будильники ждавших до отправки — строго без замка (см. `mark_sent`).
        self.release(guard, wake);
        requests
    }

    /// Снимок учёта.
    pub(super) fn stats(&self) -> ResourceFetchStats {
        self.lock().stats()
    }

    /// Оборвала ли печать чтение движка.
    pub(super) fn seal_cut(&self) -> ResourceSealCut {
        self.lock().seal_cut()
    }

    // ---------------------------------------------------------------- писатель

    /// Чей номер.
    pub(super) fn claim_of(&self, id: RangeRequestId) -> ResourceRequestClaim {
        self.lock().claim_of(id)
    }

    /// Кусок тела от внешней стороны.
    pub(super) fn accept_bytes(
        &self,
        id: RangeRequestId,
        offset: u64,
        payload: &[u8],
    ) -> Result<ResourceFrameVerdict, ResourceFetchViolation> {
        let mut wake = WakeList::default();
        let mut guard = self.lock_swept(&mut wake);
        let verdict = guard.accept_bytes(id, offset, payload, Instant::now(), &self.ids, &mut wake);
        self.release(guard, wake);
        verdict
    }

    /// Итог запроса от внешней стороны.
    pub(super) fn finish(
        &self,
        id: RangeRequestId,
        outcome: ResourceFetchOutcome,
    ) -> Result<ResourceFrameVerdict, ResourceFetchViolation> {
        let mut wake = WakeList::default();
        let mut guard = self.lock_swept(&mut wake);
        let verdict = guard.finish(id, outcome, &self.ids, &mut wake);
        self.release(guard, wake);
        verdict
    }

    /// Источник закрыт.
    pub(super) fn seal(&self) {
        let mut wake = WakeList::default();
        let mut guard = self.lock();
        guard.seal(&mut wake);
        self.release(guard, wake);
        // Сторож увидит, что ждать больше некого, и закончится сразу, а не по сроку.
        self.watchdog_wake.notify_all();
    }
}
