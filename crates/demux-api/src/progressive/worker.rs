//! Закрытый protocol и runner blocking progressive worker-а.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use media_core::{DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, Demuxer};
use source_core::CancellationToken;

use super::{
    ProgressiveDemuxBufferLimits, ProgressiveDemuxPacketTooLargeError,
    ProgressiveSeekAnchorMismatchError,
};

/// Максимальная пауза worker-а до повторной проверки cancellation при backpressure.
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Одно owned сообщение bounded queue.
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
#[derive(Debug, Clone, Copy)]
pub(super) struct ProgressiveSeekCommand {
    /// Generation, которая становится единственной publishable после command-а.
    pub(super) generation: u64,
    /// Исходная container-neutral цель.
    pub(super) request: DemuxSeekRequest,
    /// Уже опубликованный player-у доказанный anchor.
    pub(super) preview: DemuxSeekResult,
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
}

/// Shared queue + backpressure coordination.
pub(super) struct ProgressiveSharedState {
    /// Caller-owned queue limits.
    limits: ProgressiveDemuxBufferLimits,
    /// Единственная authority mutable queue state.
    queue: Mutex<ProgressiveQueueState>,
    /// Consumer pop/drop будит producer без busy loop-а.
    pub(super) capacity_available: Condvar,
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
        Self {
            limits,
            queue: Mutex::new(ProgressiveQueueState {
                messages: VecDeque::new(),
                queued_encoded_bytes: 0,
                stop_requested: false,
                worker_stopped: false,
                current_generation: 0,
                pending_seek: None,
            }),
            capacity_available: Condvar::new(),
        }
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
        if cancellation.is_cancelled() || shared.lock_queue().stop_requested {
            mark_worker_stopped(&shared);
            return;
        }
        let seek_command = {
            let mut queue = shared.lock_queue();
            queue.pending_seek.take()
        };
        if let Some(command) = seek_command {
            generation = command.generation;
            reached_end = false;
            match inner.seek_with_request(command.request) {
                Ok(worker_result) if worker_result == command.preview => {}
                Ok(worker_result) => {
                    let outcome = push_progressive_message(
                        &shared,
                        &cancellation,
                        generation,
                        ProgressiveMessage::Failure(anyhow::Error::new(
                            ProgressiveSeekAnchorMismatchError {
                                preview_actual: command.preview.actual_position,
                                worker_actual: worker_result.actual_position,
                            },
                        )),
                    );
                    match outcome {
                        ProgressivePushOutcome::Published | ProgressivePushOutcome::Stopped => {
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
                        ProgressivePushOutcome::Published | ProgressivePushOutcome::Stopped => {
                            mark_worker_stopped(&shared);
                            return;
                        }
                        ProgressivePushOutcome::Stale => continue,
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

/// EOF worker ждёт command/cancellation без busy loop-а и без ложного terminal stop.
fn wait_for_seek_command(shared: &ProgressiveSharedState, cancellation: &CancellationToken) {
    let queue = shared.lock_queue();
    if cancellation.is_cancelled() || queue.stop_requested || queue.pending_seek.is_some() {
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
    queue.worker_stopped = true;
    shared.capacity_available.notify_all();
}
