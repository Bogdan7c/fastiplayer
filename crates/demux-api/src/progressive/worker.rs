//! Закрытый protocol и runner blocking progressive worker-а.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use media_core::{
    DemuxActiveReadInterruptionCapability, DemuxReadEvent, DemuxSeekCancellationToken,
    DemuxSeekRequest, DemuxSeekResult, Demuxer,
};
use source_core::CancellationToken;

use super::{
    ProgressiveAsyncSeekLimits, ProgressiveAsyncSeekOutcome, ProgressiveAsyncSeekReceipt,
    ProgressiveDemuxBufferLimits, ProgressiveDemuxPacketTooLargeError,
    ProgressiveRuntimeGeneration, ProgressiveSeekAnchorMismatchError, ProgressiveSeekFence,
};

/// Максимальная пауза worker-а до повторной проверки cancellation при backpressure.
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Одно owned сообщение bounded queue.
///
/// `DemuxReadEvent` намеренно остаётся inline: queue уже ограничена по числу
/// сообщений и encoded bytes, а `Box` добавил бы heap allocation на каждый
/// packet только ради выравнивания размеров двух внутренних вариантов.
#[allow(clippy::large_enum_variant)]
pub(super) enum ProgressiveMessage {
    /// Exact demux event, прочитанный worker-ом.
    Event(DemuxReadEvent),
    /// Downcastable concrete demux/source failure.
    Failure(anyhow::Error),
}

/// Generation envelope не позволяет старому read/seek результату попасть после supersede.
pub(super) struct ProgressiveMessageEnvelope {
    /// Player-visible generation, для которой получено сообщение.
    pub(super) generation: u64,
    /// Exact worker outcome.
    pub(super) message: ProgressiveMessage,
}

/// Latest-only seek command из player-owner в blocking worker.
#[derive(Debug, Clone)]
pub(super) enum ProgressiveSeekCommand {
    /// Legacy path уже синхронно отдал caller-у доказанный preview.
    Previewed {
        /// Generation, которая становится единственной publishable после command-а.
        generation: u64,
        /// Исходная container-neutral цель.
        request: DemuxSeekRequest,
        /// Уже опубликованный player-у доказанный anchor.
        preview: DemuxSeekResult,
        /// Newer preview либо receipted intent отменяет blocking transport этого seek-а.
        cancellation: DemuxSeekCancellationToken,
    },
    /// Opt-in path публикует authoritative result отдельным terminal receipt-ом.
    Receipted {
        /// Generation, которая становится единственной publishable после command-а.
        generation: u64,
        /// Исходная container-neutral цель.
        request: DemuxSeekRequest,
        /// Stable caller identity и runtime fence.
        fence: ProgressiveSeekFence,
        /// Newer accepted request отменяет blocking transport этого seek-а.
        cancellation: DemuxSeekCancellationToken,
    },
}

impl ProgressiveSeekCommand {
    /// Возвращает generation, принадлежащую command-у.
    pub(super) const fn generation(&self) -> u64 {
        match self {
            Self::Previewed { generation, .. } | Self::Receipted { generation, .. } => *generation,
        }
    }

    /// Возвращает fence только для receipted path-а.
    pub(super) const fn receipt_fence(&self) -> Option<ProgressiveSeekFence> {
        match self {
            Self::Previewed { .. } => None,
            Self::Receipted { fence, .. } => Some(*fence),
        }
    }

    /// Возвращает request-scoped cancellation любого worker-owned seek command-а.
    pub(super) fn cancellation(&self) -> Option<DemuxSeekCancellationToken> {
        match self {
            Self::Previewed { cancellation, .. } | Self::Receipted { cancellation, .. } => {
                Some(cancellation.clone())
            }
        }
    }
}

/// Opt-in bounded receipt runtime state.
pub(super) struct ProgressiveAsyncSeekState {
    /// Generation конкретного progressive runtime-а.
    pub(super) runtime_generation: ProgressiveRuntimeGeneration,
    /// Общий bound pending, in-flight и completed-but-not-drained requests.
    pub(super) limits: ProgressiveAsyncSeekLimits,
    /// Последняя accepted monotonic identity.
    pub(super) last_accepted_request_id: Option<u64>,
    /// Число accepted requests без drained terminal receipt-а.
    pub(super) outstanding_receipts: usize,
    /// FIFO terminal receipts, доступных consumer-у.
    pub(super) completed_receipts: VecDeque<ProgressiveAsyncSeekReceipt>,
    /// Terminal outcomes, которые должен опубликовать только worker.
    pub(super) worker_pending_receipts: VecDeque<ProgressiveAsyncSeekReceipt>,
}

impl ProgressiveMessage {
    /// Считает только encoded packet payload, не Rust allocation overhead.
    pub(super) fn encoded_bytes(&self) -> usize {
        match self {
            Self::Event(DemuxReadEvent::Packet(packet)) => packet.data.len(),
            Self::Event(
                DemuxReadEvent::EndOfStream
                | DemuxReadEvent::TemporarilyUnavailable(_)
                | DemuxReadEvent::TracksChanged(_)
                | DemuxReadEvent::MediaMetadataChanged(_),
            )
            | Self::Failure(_) => 0,
        }
    }
}

/// Mutex-protected queue accounting.
pub(super) struct ProgressiveQueueState {
    /// FIFO сохраняет exact inner event order.
    pub(super) messages: VecDeque<ProgressiveMessageEnvelope>,
    /// Сумма encoded packet bytes в `messages`.
    pub(super) queued_encoded_bytes: usize,
    /// Drop/supersede запрещает worker-у читать следующий event.
    pub(super) stop_requested: bool,
    /// Worker больше не сможет опубликовать message.
    pub(super) worker_stopped: bool,
    /// Generation текущего player intent-а.
    pub(super) current_generation: u64,
    /// Latest-only command; rapid seek заменяет ещё не начатую старую цель.
    pub(super) pending_seek: Option<ProgressiveSeekCommand>,
    /// Fence command-а, который worker уже вынул из pending slot-а.
    pub(super) in_flight_receipt: Option<ProgressiveSeekFence>,
    /// Token принадлежит только выполняющемуся request-у и очищается после terminal receipt.
    pub(super) active_seek_cancellation: Option<DemuxSeekCancellationToken>,
    /// Stable owner port текущего physical read-а либо explicit unsupported capability.
    pub(super) active_read_interruption: DemuxActiveReadInterruptionCapability,
    /// Optional receipt capability; legacy constructors оставляют её absent.
    pub(super) async_seek: Option<ProgressiveAsyncSeekState>,
}

/// Shared queue + backpressure coordination.
pub(super) struct ProgressiveSharedState {
    /// Caller-owned queue limits.
    limits: ProgressiveDemuxBufferLimits,
    /// Единственная authority mutable queue state.
    queue: Mutex<ProgressiveQueueState>,
    /// Consumer pop/drop будит producer без busy loop-а.
    pub(super) capacity_available: Condvar,
    /// Producer publish/terminal state будит readiness consumer-а без polling.
    pub(super) message_available: Condvar,
    /// Только synchronous `CancellationFuture::poll` wake отмечает owner queue mutex-а.
    readiness_poll_owner: Mutex<Option<ThreadId>>,
}

/// RAII-предохранитель публикует terminal worker state даже при panic backend-а.
pub(super) struct ProgressiveWorkerCompletion {
    /// Shared queue, которую player-facing handle продолжает опрашивать.
    shared: Arc<ProgressiveSharedState>,
}

impl ProgressiveWorkerCompletion {
    /// Привязывает terminal notification к lifetime worker closure.
    #[must_use]
    pub(super) fn new(shared: Arc<ProgressiveSharedState>) -> Self {
        Self { shared }
    }
}

impl Drop for ProgressiveWorkerCompletion {
    /// Не позволяет player owner-у бесконечно ждать после неожиданного worker panic-а.
    fn drop(&mut self) {
        mark_worker_stopped(&self.shared);
    }
}

impl ProgressiveSharedState {
    /// Создаёт пустую bounded queue до worker spawn-а.
    pub(super) fn new(limits: ProgressiveDemuxBufferLimits) -> Self {
        Self::new_with_async_seek(limits, None)
    }

    /// Создаёт очередь с opt-in bounded asynchronous seek receipts.
    pub(super) fn new_receipted(
        limits: ProgressiveDemuxBufferLimits,
        runtime_generation: ProgressiveRuntimeGeneration,
        async_limits: ProgressiveAsyncSeekLimits,
    ) -> Self {
        Self::new_with_async_seek(
            limits,
            Some(ProgressiveAsyncSeekState {
                runtime_generation,
                limits: async_limits,
                last_accepted_request_id: None,
                outstanding_receipts: 0,
                completed_receipts: VecDeque::new(),
                worker_pending_receipts: VecDeque::new(),
            }),
        )
    }

    /// Общий constructor сохраняет одну инициализацию queue invariants.
    fn new_with_async_seek(
        limits: ProgressiveDemuxBufferLimits,
        async_seek: Option<ProgressiveAsyncSeekState>,
    ) -> Self {
        Self {
            limits,
            queue: Mutex::new(ProgressiveQueueState {
                messages: VecDeque::new(),
                queued_encoded_bytes: 0,
                stop_requested: false,
                worker_stopped: false,
                current_generation: 0,
                pending_seek: None,
                in_flight_receipt: None,
                active_seek_cancellation: None,
                active_read_interruption: DemuxActiveReadInterruptionCapability::Unsupported,
                async_seek,
            }),
            capacity_available: Condvar::new(),
            message_available: Condvar::new(),
            readiness_poll_owner: Mutex::new(None),
        }
    }

    /// Отмечает единственный queue-serialized poll для deadlock-free synchronous wake-а.
    pub(super) fn begin_readiness_cancellation_poll(&self) {
        *self
            .readiness_poll_owner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(thread::current().id());
    }

    /// Очищает transient owner до atomic Condvar unlock+wait.
    pub(super) fn finish_readiness_cancellation_poll(&self) {
        *self
            .readiness_poll_owner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    /// Waker не пытается рекурсивно взять queue mutex только во время synchronous poll wake-а.
    pub(super) fn readiness_poll_is_owned_by_current_thread(&self) -> bool {
        self.readiness_poll_owner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some_and(|owner| owner == thread::current().id())
    }

    /// Устанавливает stable demux-owned controller до первого physical read-а.
    pub(super) fn install_active_read_interruption(
        &self,
        capability: DemuxActiveReadInterruptionCapability,
    ) {
        self.lock_queue().active_read_interruption = capability;
    }

    /// Poison означает internal invariant failure; восстанавливаем owned state для shutdown.
    pub(super) fn lock_queue(&self) -> MutexGuard<'_, ProgressiveQueueState> {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Единственный owner blocking inner demuxer-а.
pub(super) fn run_progressive_worker(
    mut inner: Box<dyn Demuxer + Send>,
    shared: Arc<ProgressiveSharedState>,
    cancellation: CancellationToken,
) {
    loop {
        if cancellation.is_cancelled() || shared.lock_queue().stop_requested {
            mark_worker_stopped(&shared);
            return;
        }

        match inner.next_event() {
            Ok(DemuxReadEvent::TemporarilyUnavailable(hint)) => {
                wait_for_inner_retry(&shared, &cancellation, hint.retry_after());
            }
            Ok(event) => {
                let terminal = matches!(event, DemuxReadEvent::EndOfStream);
                if !matches!(
                    push_progressive_message(
                        &shared,
                        &cancellation,
                        0,
                        ProgressiveMessage::Event(event),
                    ),
                    ProgressivePushOutcome::Published
                ) {
                    mark_worker_stopped(&shared);
                    return;
                }
                if terminal {
                    mark_worker_stopped(&shared);
                    return;
                }
            }
            Err(source) => {
                let _ = push_progressive_message(
                    &shared,
                    &cancellation,
                    0,
                    ProgressiveMessage::Failure(source),
                );
                mark_worker_stopped(&shared);
                return;
            }
        }
    }
}

/// Seekable worker сохраняет parser ownership и принимает latest-only commands.
pub(super) fn run_seekable_progressive_worker(
    mut inner: Box<dyn Demuxer + Send>,
    shared: Arc<ProgressiveSharedState>,
    cancellation: CancellationToken,
) {
    let mut generation = 0_u64;
    let mut reached_end = false;
    loop {
        publish_worker_pending_receipts(&shared);
        if cancellation.is_cancelled() || shared.lock_queue().stop_requested {
            mark_worker_stopped(&shared);
            return;
        }
        let seek_command = {
            let mut queue = shared.lock_queue();
            let command = queue.pending_seek.take();
            queue.in_flight_receipt = command
                .as_ref()
                .and_then(ProgressiveSeekCommand::receipt_fence);
            if let Some(command_cancellation) = command
                .as_ref()
                .and_then(ProgressiveSeekCommand::cancellation)
            {
                queue.active_seek_cancellation = Some(command_cancellation);
            }
            command
        };
        if let Some(command) = seek_command {
            generation = command.generation();
            reached_end = false;
            match command {
                ProgressiveSeekCommand::Previewed {
                    request,
                    preview,
                    cancellation: seek_cancellation,
                    ..
                } => {
                    match inner.seek_with_cancellable_preview_request(request, seek_cancellation) {
                        Ok(worker_result) if worker_result == preview => {}
                        Ok(worker_result) => {
                            let outcome = push_progressive_message(
                                &shared,
                                &cancellation,
                                generation,
                                ProgressiveMessage::Failure(anyhow::Error::new(
                                    ProgressiveSeekAnchorMismatchError {
                                        preview_actual: preview.actual_position,
                                        worker_actual: worker_result.actual_position,
                                    },
                                )),
                            );
                            match outcome {
                                ProgressivePushOutcome::Published
                                | ProgressivePushOutcome::Stopped => {
                                    mark_worker_stopped(&shared);
                                    return;
                                }
                                ProgressivePushOutcome::Stale => continue,
                            }
                        }
                        Err(source) => {
                            let outcome = push_progressive_message(
                                &shared,
                                &cancellation,
                                generation,
                                ProgressiveMessage::Failure(source),
                            );
                            match outcome {
                                ProgressivePushOutcome::Published
                                | ProgressivePushOutcome::Stopped => {
                                    mark_worker_stopped(&shared);
                                    return;
                                }
                                ProgressivePushOutcome::Stale => continue,
                            }
                        }
                    }
                }
                ProgressiveSeekCommand::Receipted {
                    request,
                    fence,
                    cancellation: seek_cancellation,
                    ..
                } => {
                    let runtime_is_current = shared
                        .lock_queue()
                        .async_seek
                        .as_ref()
                        .is_some_and(|state| state.runtime_generation == fence.runtime_generation);
                    let worker_result =
                        if runtime_is_current {
                            // Receipted command не публикует предварительный anchor: concrete demuxer
                            // вправе доказать более точную позицию внутри blocking worker-а.
                            Some(inner.seek_with_cancellable_receipted_request(
                                request,
                                seek_cancellation,
                            ))
                        } else {
                            None
                        };
                    let outcome = receipted_seek_outcome(
                        &shared,
                        &cancellation,
                        generation,
                        runtime_is_current,
                        worker_result,
                    );
                    publish_async_seek_receipt(
                        &shared,
                        ProgressiveAsyncSeekReceipt { fence, outcome },
                    );
                    if matches!(
                        outcome,
                        ProgressiveAsyncSeekOutcome::Cancelled
                            | ProgressiveAsyncSeekOutcome::Superseded
                            | ProgressiveAsyncSeekOutcome::Stale
                            | ProgressiveAsyncSeekOutcome::Failed
                    ) {
                        continue;
                    }
                }
            }
        }
        if reached_end {
            wait_for_seek_command(&shared, &cancellation);
            continue;
        }
        match inner.next_event() {
            Ok(DemuxReadEvent::TemporarilyUnavailable(hint)) => {
                wait_for_inner_retry(&shared, &cancellation, hint.retry_after());
            }
            Ok(event) => {
                reached_end = matches!(event, DemuxReadEvent::EndOfStream);
                match push_progressive_message(
                    &shared,
                    &cancellation,
                    generation,
                    ProgressiveMessage::Event(event),
                ) {
                    ProgressivePushOutcome::Published => {}
                    ProgressivePushOutcome::Stale => reached_end = false,
                    ProgressivePushOutcome::Stopped => {
                        mark_worker_stopped(&shared);
                        return;
                    }
                }
            }
            Err(source) => {
                let outcome = push_progressive_message(
                    &shared,
                    &cancellation,
                    generation,
                    ProgressiveMessage::Failure(source),
                );
                match outcome {
                    ProgressivePushOutcome::Published | ProgressivePushOutcome::Stopped => {
                        mark_worker_stopped(&shared);
                        return;
                    }
                    ProgressivePushOutcome::Stale => {}
                }
            }
        }
    }
}

/// Выбирает ровно один terminal outcome после возврата blocking inner seek-а.
fn receipted_seek_outcome(
    shared: &ProgressiveSharedState,
    cancellation: &CancellationToken,
    generation: u64,
    runtime_is_current: bool,
    worker_result: Option<anyhow::Result<DemuxSeekResult>>,
) -> ProgressiveAsyncSeekOutcome {
    if !runtime_is_current {
        return ProgressiveAsyncSeekOutcome::Stale;
    }
    if cancellation.is_cancelled() || shared.lock_queue().stop_requested {
        return ProgressiveAsyncSeekOutcome::Cancelled;
    }
    if shared.lock_queue().current_generation != generation {
        return ProgressiveAsyncSeekOutcome::Superseded;
    }
    match worker_result.expect("current runtime всегда выполняет inner seek") {
        Ok(result) => ProgressiveAsyncSeekOutcome::Succeeded(result),
        Err(source) => {
            tracing::debug!(
                error = ?source,
                "Progressive worker receipted seek завершился typed demux failure"
            );
            ProgressiveAsyncSeekOutcome::Failed
        }
    }
}

/// Переносит terminal receipts из worker-owned staging в consumer FIFO.
fn publish_worker_pending_receipts(shared: &ProgressiveSharedState) {
    let mut queue = shared.lock_queue();
    let Some(async_seek) = queue.async_seek.as_mut() else {
        return;
    };
    while let Some(receipt) = async_seek.worker_pending_receipts.pop_front() {
        async_seek.completed_receipts.push_back(receipt);
    }
}

/// Публикует worker-computed receipt без blocking и без отдельного unbounded channel-а.
fn publish_async_seek_receipt(
    shared: &ProgressiveSharedState,
    receipt: ProgressiveAsyncSeekReceipt,
) {
    let mut queue = shared.lock_queue();
    if queue.in_flight_receipt == Some(receipt.fence) {
        queue.in_flight_receipt = None;
        if let Some(cancellation) = queue.active_seek_cancellation.take()
            && !matches!(receipt.outcome, ProgressiveAsyncSeekOutcome::Succeeded(_))
        {
            cancellation.cancel();
        }
    }
    let Some(async_seek) = queue.async_seek.as_mut() else {
        return;
    };
    async_seek.completed_receipts.push_back(receipt);
    shared.capacity_available.notify_all();
}

/// EOF worker ждёт command/cancellation без busy loop-а и без ложного terminal stop.
pub(super) fn wait_for_seek_command(
    shared: &ProgressiveSharedState,
    cancellation: &CancellationToken,
) {
    let queue = shared.lock_queue();
    let has_pending_receipt = queue
        .async_seek
        .as_ref()
        .is_some_and(|state| !state.worker_pending_receipts.is_empty());
    if cancellation.is_cancelled()
        || queue.stop_requested
        || queue.pending_seek.is_some()
        || has_pending_receipt
    {
        return;
    }
    let _wait_result = shared
        .capacity_available
        .wait_timeout(queue, CANCELLATION_POLL_INTERVAL);
}

/// Публикует message только после bounded byte/event admission.
pub(super) fn push_progressive_message(
    shared: &ProgressiveSharedState,
    cancellation: &CancellationToken,
    generation: u64,
    mut message: ProgressiveMessage,
) -> ProgressivePushOutcome {
    let encoded_bytes = message.encoded_bytes();
    let oversized_packet = encoded_bytes > shared.limits.max_pending_encoded_bytes();
    if oversized_packet {
        message = ProgressiveMessage::Failure(
            ProgressiveDemuxPacketTooLargeError {
                packet_bytes: encoded_bytes,
                budget_bytes: shared.limits.max_pending_encoded_bytes(),
            }
            .into(),
        );
    }
    let admitted_bytes = message.encoded_bytes();
    let mut queue = shared.lock_queue();
    loop {
        if cancellation.is_cancelled() || queue.stop_requested {
            return ProgressivePushOutcome::Stopped;
        }
        if queue.current_generation != generation {
            return ProgressivePushOutcome::Stale;
        }
        let has_event_capacity = queue.messages.len() < shared.limits.max_pending_events();
        let has_byte_capacity = queue
            .queued_encoded_bytes
            .checked_add(admitted_bytes)
            .is_some_and(|total| total <= shared.limits.max_pending_encoded_bytes());
        if has_event_capacity && has_byte_capacity {
            queue.queued_encoded_bytes = queue.queued_encoded_bytes.saturating_add(admitted_bytes);
            queue.messages.push_back(ProgressiveMessageEnvelope {
                generation,
                message,
            });
            // Predicate публикуется под queue mutex-ом до notification: readiness waiter
            // не может увидеть wake без уже доступного current-generation сообщения.
            shared.message_available.notify_all();
            return if oversized_packet {
                ProgressivePushOutcome::Stopped
            } else {
                ProgressivePushOutcome::Published
            };
        }
        let wait_result = shared
            .capacity_available
            .wait_timeout(queue, CANCELLATION_POLL_INTERVAL);
        queue = match wait_result {
            Ok((queue, _)) => queue,
            Err(poisoned) => poisoned.into_inner().0,
        };
    }
}

/// Результат bounded worker publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProgressivePushOutcome {
    /// Message опубликован для active generation.
    Published,
    /// Новый seek supersede сделал старый результат невидимым.
    Stale,
    /// Runtime отменён, dropped или завершён oversized failure.
    Stopped,
}

/// Ждёт provider hint либо lifecycle cancellation без packet queue mutation.
fn wait_for_inner_retry(
    shared: &ProgressiveSharedState,
    cancellation: &CancellationToken,
    retry_after: Duration,
) {
    let started_at = Instant::now();
    let mut queue = shared.lock_queue();
    while started_at.elapsed() < retry_after {
        if cancellation.is_cancelled() || queue.stop_requested {
            return;
        }
        let remaining = retry_after.saturating_sub(started_at.elapsed());
        let wait_duration = remaining.min(CANCELLATION_POLL_INTERVAL);
        let wait_result = shared.capacity_available.wait_timeout(queue, wait_duration);
        queue = match wait_result {
            Ok((queue, _)) => queue,
            Err(poisoned) => poisoned.into_inner().0,
        };
    }
}

/// Публикует worker terminal state после последнего message-а.
fn mark_worker_stopped(shared: &ProgressiveSharedState) {
    let mut queue = shared.lock_queue();
    let stop_outcome = if queue.stop_requested {
        ProgressiveAsyncSeekOutcome::Cancelled
    } else {
        ProgressiveAsyncSeekOutcome::Failed
    };
    let pending_receipt_fence = queue
        .pending_seek
        .take()
        .as_ref()
        .and_then(ProgressiveSeekCommand::receipt_fence);
    if let Some(cancellation) = queue.active_seek_cancellation.take() {
        cancellation.cancel();
    }
    let in_flight_receipt_fence = queue.in_flight_receipt.take();
    if let Some(async_seek) = queue.async_seek.as_mut() {
        while let Some(receipt) = async_seek.worker_pending_receipts.pop_front() {
            async_seek.completed_receipts.push_back(receipt);
        }
        if let Some(fence) = pending_receipt_fence {
            async_seek
                .completed_receipts
                .push_back(ProgressiveAsyncSeekReceipt {
                    fence,
                    outcome: stop_outcome,
                });
        }
        if let Some(fence) = in_flight_receipt_fence {
            async_seek
                .completed_receipts
                .push_back(ProgressiveAsyncSeekReceipt {
                    fence,
                    outcome: stop_outcome,
                });
        }
    }
    queue.worker_stopped = true;
    shared.capacity_available.notify_all();
    shared.message_available.notify_all();
}
