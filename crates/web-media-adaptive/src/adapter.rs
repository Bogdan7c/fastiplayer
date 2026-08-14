//! Узкий compatibility adapter existing finite ordered demux factories.

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use demux_api::{
    OrderedSegment, OrderedSegmentReadError, OrderedSegmentSource, ProgressiveDemuxBufferLimits,
    ProgressiveDemuxStartupError, ProgressiveDemuxer,
};
use media_core::{DemuxRetryHint, Demuxer};
use source_core::CancellationToken;

use crate::{AdaptiveOrderedSegmentSource, SegmentPoll};

const CANCELLATION_OBSERVATION_INTERVAL: Duration = Duration::from_millis(25);
const MAX_SAFE_REASON_BYTES: usize = 256;
const READ_AHEAD_WORKER_NAME: &str = "adaptive-segment-read-ahead";

/// Blocking facade, которую разрешено читать только внутри demux worker-а.
pub struct BlockingOrderedSegmentAdapter {
    source: BlockingSegmentSource,
}

/// Вариант владения source-ом отделяет обычный demand-only adapter от
/// activatable read-ahead path-а, который нужен provider-у после commit выбора.
enum BlockingSegmentSource {
    /// Старый контракт: network fetch начинается только по `next_segment`.
    Direct(Box<AdaptiveOrderedSegmentSource>),
    /// Shared control включает producer pump только для выбранного runtime-а.
    Activatable(Arc<ReadAheadControl>),
}

/// Lifecycle owner pump-а отделён от shared source state, чтобы `JoinHandle` не
/// образовывал Arc-cycle через выполняющийся worker.
struct ReadAheadControl {
    /// Source, bounded buffer и wake protocol принадлежат этому shared owner-у.
    shared: Arc<ReadAheadShared>,
    /// Worker создаётся ровно один раз при selected activation.
    worker_started: AtomicBool,
    /// Adapter drop забирает handle и выполняет bounded cooperative join.
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// Mutex + condition variable не держат lock во время network fetch-а:
/// `AdaptiveOrderedSegmentSource::poll_next` только управляет отдельным HTTP worker-ом.
struct ReadAheadShared {
    state: Mutex<ReadAheadState>,
    wake: Condvar,
}

/// Единственное mutable состояние selected read-ahead producer/consumer path-а.
struct ReadAheadState {
    /// Adaptive owner сохраняет retry/cancel/generation и HTTP invariants.
    source: AdaptiveOrderedSegmentSource,
    /// До явной активации catalog probe остаётся demand-only.
    enabled: bool,
    /// Drop adapter-а останавливает pump до освобождения source-а.
    shutdown: bool,
    /// Terminal outcome запрещает повторный poll после EOF/error/cancellation.
    terminal_buffered: bool,
    /// Caller-owned предел готовых successor fragments.
    buffered_segment_capacity: NonZeroUsize,
    /// Строго ordered готовые segments и единственный terminal tail.
    buffered_outcomes: VecDeque<BufferedSegmentOutcome>,
}

/// Результат bounded чтения вперёд без temporary-readiness состояния.
enum BufferedSegmentOutcome {
    /// Следующий ordered segment готов к немедленной выдаче demuxer-у.
    Segment(OrderedSegment),
    /// Snapshot полностью исчерпан.
    EndOfStream,
    /// Cancellation/transport failure сохраняется до consumer read-а.
    Error(OrderedSegmentReadError),
}

/// Provider-owned ключ активации выбранного segmented runtime-а.
///
/// Handle не раскрывает source storage и не разрешает менять порядок, retry,
/// generation или HTTP policy. Повторная активация безопасна и idempotent.
#[derive(Clone)]
pub struct BlockingOrderedSegmentReadAheadHandle {
    control: Arc<ReadAheadControl>,
}

/// Named construction result не смешивает adapter ownership и activation key.
pub struct ActivatableBlockingOrderedSegmentAdapter {
    adapter: BlockingOrderedSegmentAdapter,
    read_ahead_handle: BlockingOrderedSegmentReadAheadHandle,
}

impl BlockingOrderedSegmentAdapter {
    /// Забирает единоличное владение nonblocking adaptive source-ом.
    #[must_use]
    pub fn new(source: AdaptiveOrderedSegmentSource) -> Self {
        Self {
            source: BlockingSegmentSource::Direct(Box::new(source)),
        }
    }

    /// Создаёт demand-only adapter с отдельным ключом будущей активации.
    ///
    /// До вызова `activate` поведение полностью совпадает с `new`: это не даёт
    /// catalog discovery незаметно скачать второй fragment каждой rendition.
    #[must_use]
    pub fn new_activatable(
        source: AdaptiveOrderedSegmentSource,
        buffered_segment_capacity: NonZeroUsize,
    ) -> ActivatableBlockingOrderedSegmentAdapter {
        let shared = Arc::new(ReadAheadShared {
            state: Mutex::new(ReadAheadState {
                source,
                enabled: false,
                shutdown: false,
                terminal_buffered: false,
                buffered_segment_capacity,
                buffered_outcomes: VecDeque::with_capacity(buffered_segment_capacity.get()),
            }),
            wake: Condvar::new(),
        });
        let control = Arc::new(ReadAheadControl {
            shared,
            worker_started: AtomicBool::new(false),
            worker: Mutex::new(None),
        });
        ActivatableBlockingOrderedSegmentAdapter {
            adapter: Self {
                source: BlockingSegmentSource::Activatable(Arc::clone(&control)),
            },
            read_ahead_handle: BlockingOrderedSegmentReadAheadHandle { control },
        }
    }

    /// Запускает registry sniff/open и parser reads за player-owner boundary.
    ///
    /// Initial segment fetch выполняется тем же worker-ом. Поэтому registry
    /// никогда не получает fake EOF, а player-facing demuxer до готовности
    /// возвращает существующий `DemuxReadEvent::TemporarilyUnavailable`.
    pub fn open_deferred<F>(
        source: AdaptiveOrderedSegmentSource,
        cancellation: CancellationToken,
        limits: ProgressiveDemuxBufferLimits,
        retry_hint: DemuxRetryHint,
        open_inner: F,
    ) -> Result<ProgressiveDemuxer, ProgressiveDemuxStartupError>
    where
        F: FnOnce(Box<dyn OrderedSegmentSource>) -> anyhow::Result<Box<dyn Demuxer + Send>>
            + Send
            + 'static,
    {
        ProgressiveDemuxer::new_deferred(
            move || open_inner(Box::new(Self::new(source))),
            cancellation,
            limits,
            retry_hint,
        )
    }
}

impl Drop for BlockingOrderedSegmentAdapter {
    /// Останавливает только собственный read-ahead pump; demand-only path пуст.
    fn drop(&mut self) {
        let BlockingSegmentSource::Activatable(control) = &self.source else {
            return;
        };
        if let Ok(mut state) = control.shared.state.lock() {
            state.shutdown = true;
        }
        control.shared.wake.notify_all();
        let worker = control
            .worker
            .lock()
            .ok()
            .and_then(|mut worker| worker.take());
        if let Some(worker) = worker {
            let join_result = worker.join();
            debug_assert!(
                join_result.is_ok(),
                "read-ahead worker catches its own panic before join"
            );
        }
    }
}

impl ActivatableBlockingOrderedSegmentAdapter {
    /// Возвращает cloneable intent-only handle до передачи adapter-а registry.
    #[must_use]
    pub fn read_ahead_handle(&self) -> BlockingOrderedSegmentReadAheadHandle {
        self.read_ahead_handle.clone()
    }

    /// Передаёт единоличное чтение ordered segments container demuxer-у.
    #[must_use]
    pub fn into_adapter(self) -> BlockingOrderedSegmentAdapter {
        self.adapter
    }
}

impl BlockingOrderedSegmentReadAheadHandle {
    /// Включает selected-only producer pump, который независимо от demux packet
    /// queue поддерживает caller-owned число готовых successor fragments.
    pub fn activate(&self) -> Result<(), OrderedSegmentReadError> {
        {
            let mut state = lock_read_ahead_state(&self.control.shared)?;
            if state.shutdown {
                return Err(read_ahead_stopped_error());
            }
            state.source.enable_concurrent_read_ahead();
            state.enabled = true;
        }
        self.start_worker_once()?;
        self.control.shared.wake.notify_all();
        Ok(())
    }

    /// Приостанавливает producer pump без cancellation активного source-а и
    /// без потери уже готового FIFO; повторный `activate` продолжит lifecycle.
    pub fn suspend(&self) -> Result<(), OrderedSegmentReadError> {
        let mut state = lock_read_ahead_state(&self.control.shared)?;
        if state.shutdown {
            return Err(read_ahead_stopped_error());
        }
        state.enabled = false;
        self.control.shared.wake.notify_all();
        Ok(())
    }

    /// Активирует pump и ждёт первый готовый successor либо terminal outcome.
    ///
    /// HDS использует эту границу до публикации playback runtime-а, чтобы
    /// начальный fragment transition не соревновался с network latency.
    pub fn activate_and_wait_for_ready_segment(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), OrderedSegmentReadError> {
        self.activate()?;
        loop {
            if cancellation.is_cancelled() {
                return Err(OrderedSegmentReadError::Cancelled);
            }
            let state = lock_read_ahead_state(&self.control.shared)?;
            if let Some(outcome) = state.buffered_outcomes.front() {
                return match outcome {
                    BufferedSegmentOutcome::Segment(_) | BufferedSegmentOutcome::EndOfStream => {
                        Ok(())
                    }
                    BufferedSegmentOutcome::Error(error) => Err(error.clone()),
                };
            }
            let (_state, _wait_result) = self
                .control
                .shared
                .wake
                .wait_timeout(state, CANCELLATION_OBSERVATION_INTERVAL)
                .map_err(|_| read_ahead_poisoned_error())?;
        }
    }

    /// OS thread startup остаётся fallible и не маскируется под readiness wait.
    fn start_worker_once(&self) -> Result<(), OrderedSegmentReadError> {
        if self
            .control
            .worker_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        let shared = Arc::clone(&self.control.shared);
        let worker = thread::Builder::new()
            .name(READ_AHEAD_WORKER_NAME.to_owned())
            .spawn(move || run_read_ahead_worker_catching_panic(shared))
            .map_err(|_| {
                self.control.worker_started.store(false, Ordering::Release);
                read_ahead_worker_start_error()
            })?;
        let mut worker_slot = self
            .control
            .worker
            .lock()
            .map_err(|_| read_ahead_poisoned_error())?;
        *worker_slot = Some(worker);
        Ok(())
    }
}

impl BufferedSegmentOutcome {
    /// Восстанавливает прежний typed `OrderedSegmentSource` результат.
    fn into_read_result(self) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        match self {
            Self::Segment(segment) => Ok(Some(segment)),
            Self::EndOfStream => Ok(None),
            Self::Error(error) => Err(error),
        }
    }
}

impl OrderedSegmentSource for BlockingOrderedSegmentAdapter {
    /// Ждёт readiness только на выделенном demux worker-е.
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        match &mut self.source {
            BlockingSegmentSource::Direct(source) => next_segment_on_demand(source, cancellation),
            BlockingSegmentSource::Activatable(control) => {
                next_segment_activatable(control, cancellation)
            }
        }
    }
}

/// Сохраняет прежний demand-only path для всех существующих consumers.
fn next_segment_on_demand(
    source: &mut AdaptiveOrderedSegmentSource,
    cancellation: &CancellationToken,
) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
    loop {
        if cancellation.is_cancelled() {
            return Err(OrderedSegmentReadError::Cancelled);
        }
        match source.poll_next(Instant::now()) {
            SegmentPoll::Segment(segment) => return Ok(Some(segment)),
            SegmentPoll::EndOfStream => return Ok(None),
            SegmentPoll::Cancelled => return Err(OrderedSegmentReadError::Cancelled),
            SegmentPoll::Failed(error) => return Err(ordered_segment_failure(&error.to_string())),
            SegmentPoll::TemporarilyUnavailable { retry_after } => {
                thread::park_timeout(retry_after.min(CANCELLATION_OBSERVATION_INTERVAL));
            }
        }
    }
}

/// До активации probe читает source по требованию; после активации единственным
/// producer-ом становится pump, а demuxer только забирает готовый FIFO tail.
fn next_segment_activatable(
    control: &Arc<ReadAheadControl>,
    cancellation: &CancellationToken,
) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
    loop {
        if cancellation.is_cancelled() {
            return Err(OrderedSegmentReadError::Cancelled);
        }
        let mut state = lock_read_ahead_state(&control.shared)?;
        if let Some(outcome) = state.buffered_outcomes.pop_front() {
            control.shared.wake.notify_all();
            return outcome.into_read_result();
        }
        if !state.enabled {
            match state.source.poll_next(Instant::now()) {
                SegmentPoll::Segment(segment) => return Ok(Some(segment)),
                SegmentPoll::EndOfStream => return Ok(None),
                SegmentPoll::Cancelled => return Err(OrderedSegmentReadError::Cancelled),
                SegmentPoll::Failed(error) => {
                    return Err(ordered_segment_failure(&error.to_string()));
                }
                SegmentPoll::TemporarilyUnavailable { retry_after } => {
                    drop(state);
                    thread::park_timeout(retry_after.min(CANCELLATION_OBSERVATION_INTERVAL));
                    continue;
                }
            }
        }
        let (_state, _wait_result) = control
            .shared
            .wake
            .wait_timeout(state, CANCELLATION_OBSERVATION_INTERVAL)
            .map_err(|_| read_ahead_poisoned_error())?;
    }
}

/// Catch boundary превращает неожиданный worker panic в consumer-visible poison
/// или typed terminal failure вместо process abort-а и зависшего ожидания.
fn run_read_ahead_worker_catching_panic(shared: Arc<ReadAheadShared>) {
    let worker_result = panic::catch_unwind(AssertUnwindSafe(|| {
        run_read_ahead_worker(Arc::clone(&shared));
    }));
    if worker_result.is_err()
        && let Ok(mut state) = shared.state.lock()
    {
        state.buffered_outcomes.clear();
        state
            .buffered_outcomes
            .push_back(BufferedSegmentOutcome::Error(
                OrderedSegmentReadError::Failed {
                    reason: "adaptive ordered segment read-ahead worker panicked".to_owned(),
                },
            ));
        state.terminal_buffered = true;
        shared.wake.notify_all();
    }
}

/// Producer pump последовательно poll-ит bounded HTTP pool и заполняет только
/// caller-owned bounded FIFO, не читая container packets и не меняя player state.
fn run_read_ahead_worker(shared: Arc<ReadAheadShared>) {
    loop {
        let retry_after = {
            let Ok(mut state) = shared.state.lock() else {
                return;
            };
            while !state.shutdown
                && (!state.enabled
                    || state.terminal_buffered
                    || state.buffered_outcomes.len() >= state.buffered_segment_capacity.get())
            {
                let Ok(next_state) = shared.wake.wait(state) else {
                    return;
                };
                state = next_state;
            }
            if state.shutdown {
                return;
            }
            match state.source.poll_next(Instant::now()) {
                SegmentPoll::Segment(segment) => {
                    state
                        .buffered_outcomes
                        .push_back(BufferedSegmentOutcome::Segment(segment));
                    shared.wake.notify_all();
                    Duration::ZERO
                }
                SegmentPoll::EndOfStream => {
                    state
                        .buffered_outcomes
                        .push_back(BufferedSegmentOutcome::EndOfStream);
                    state.terminal_buffered = true;
                    shared.wake.notify_all();
                    Duration::ZERO
                }
                SegmentPoll::Cancelled => {
                    state
                        .buffered_outcomes
                        .push_back(BufferedSegmentOutcome::Error(
                            OrderedSegmentReadError::Cancelled,
                        ));
                    state.terminal_buffered = true;
                    shared.wake.notify_all();
                    Duration::ZERO
                }
                SegmentPoll::Failed(error) => {
                    state
                        .buffered_outcomes
                        .push_back(BufferedSegmentOutcome::Error(ordered_segment_failure(
                            &error.to_string(),
                        )));
                    state.terminal_buffered = true;
                    shared.wake.notify_all();
                    Duration::ZERO
                }
                SegmentPoll::TemporarilyUnavailable { retry_after } => retry_after,
            }
        };
        if !retry_after.is_zero() {
            let Ok(state) = shared.state.lock() else {
                return;
            };
            let _wait_result = shared
                .wake
                .wait_timeout(state, retry_after.min(CANCELLATION_OBSERVATION_INTERVAL));
        }
    }
}

/// Poisoned synchronization state становится typed operational failure.
fn lock_read_ahead_state(
    shared: &ReadAheadShared,
) -> Result<std::sync::MutexGuard<'_, ReadAheadState>, OrderedSegmentReadError> {
    shared.state.lock().map_err(|_| read_ahead_poisoned_error())
}

/// Общая secret-safe проекция adaptive transport failure-а.
fn ordered_segment_failure(reason: &str) -> OrderedSegmentReadError {
    OrderedSegmentReadError::Failed {
        reason: bounded_reason(reason),
    }
}

/// OS thread startup failure не смешивается с transport cancellation.
fn read_ahead_worker_start_error() -> OrderedSegmentReadError {
    OrderedSegmentReadError::Failed {
        reason: "adaptive ordered segment read-ahead worker failed to start".to_owned(),
    }
}

/// Handle после adapter drop больше не может возродить source lifecycle.
fn read_ahead_stopped_error() -> OrderedSegmentReadError {
    OrderedSegmentReadError::Failed {
        reason: "adaptive ordered segment read-ahead adapter is stopped".to_owned(),
    }
}

/// Poison закрывает boundary fail-closed, потому что FIFO/source invariants неизвестны.
fn read_ahead_poisoned_error() -> OrderedSegmentReadError {
    OrderedSegmentReadError::Failed {
        reason: "adaptive ordered segment read-ahead state poisoned".to_owned(),
    }
}

fn bounded_reason(reason: &str) -> String {
    let mut boundary = reason.len().min(MAX_SAFE_REASON_BYTES);
    while !reason.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    reason[..boundary].to_owned()
}
