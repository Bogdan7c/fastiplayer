//! Nonblocking player-facing adapter для blocking progressive demuxer-а.
//!
//! Concrete container продолжает выполнять обычные blocking reads на отдельном
//! worker-е. Player owner видит только bounded очередь готовых events либо
//! neutral `TemporarilyUnavailable`, поэтому parser никогда не прерывается
//! посреди container element-а через небезопасный `WouldBlock`.

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekResult, DemuxSeekability, DemuxTrackListUpdate,
    Demuxer, MediaDemuxError, MediaMetadata, TimelineNotSeekableReason, TrackInfo,
};
use source_core::CancellationToken;

/// Максимальная пауза worker-а до повторной проверки cancellation при backpressure.
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Named bounds player-facing progressive event queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressiveDemuxBufferLimits {
    /// Максимум готовых событий, удерживаемых вне concrete demuxer-а.
    max_pending_events: NonZeroUsize,
    /// Максимум суммарных encoded packet bytes в очереди.
    max_pending_encoded_bytes: NonZeroUsize,
}

impl ProgressiveDemuxBufferLimits {
    /// Создаёт explicit policy без скрытых default literals.
    #[must_use]
    pub const fn new(
        max_pending_events: NonZeroUsize,
        max_pending_encoded_bytes: NonZeroUsize,
    ) -> Self {
        Self {
            max_pending_events,
            max_pending_encoded_bytes,
        }
    }

    /// Возвращает event-count boundary для diagnostics/tests.
    #[must_use]
    pub const fn max_pending_events(self) -> usize {
        self.max_pending_events.get()
    }

    /// Возвращает encoded-byte boundary для diagnostics/tests.
    #[must_use]
    pub const fn max_pending_encoded_bytes(self) -> usize {
        self.max_pending_encoded_bytes.get()
    }
}

/// Ошибка публикации progressive runtime handle до player mutation.
#[derive(Debug, thiserror::Error)]
pub enum ProgressiveDemuxStartupError {
    /// Seekable source должен оставаться на обычном synchronous demux path-е.
    #[error("progressive demux worker принимает только non-seekable input")]
    SeekableInput,
    /// OS не смог создать отдельный demux worker.
    #[error("не удалось создать progressive demux worker: {source}")]
    WorkerSpawn {
        /// Исходная ошибка thread builder-а.
        #[source]
        source: std::io::Error,
    },
}

/// Runtime packet превысил caller-owned bounded queue budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "progressive demux packet ({packet_bytes} bytes) превышает queue budget ({budget_bytes} bytes)"
)]
pub struct ProgressiveDemuxPacketTooLargeError {
    /// Размер concrete encoded packet-а.
    pub packet_bytes: usize,
    /// Разрешённый queue byte budget.
    pub budget_bytes: usize,
}

/// Worker завершился без terminal event/error message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("progressive demux worker завершился без terminal outcome")]
pub struct ProgressiveDemuxWorkerStoppedError;

/// Player-facing nonblocking demuxer поверх отдельного blocking worker-а.
pub struct ProgressiveDemuxer {
    /// Последний опубликованный track snapshot.
    visible_tracks: Vec<TrackInfo>,
    /// Container duration snapshot после open-а.
    visible_duration: Option<Duration>,
    /// Последний опубликованный metadata snapshot.
    visible_metadata: Option<MediaMetadata>,
    /// Exact non-seekable source/container outcome.
    visible_seekability: DemuxSeekability,
    /// Bounded worker-to-player handoff.
    shared: Arc<ProgressiveSharedState>,
    /// Earliest safe retry hint для пустой, но живой очереди.
    retry_hint: DemuxRetryHint,
    /// Shared token останавливает source read при drop/supersede.
    cancellation: CancellationToken,
    /// После опубликованного EOF повторные reads остаются terminal.
    end_of_stream_reached: bool,
}

impl ProgressiveDemuxer {
    /// Снимает immutable snapshots и запускает worker до публикации handle-а.
    pub fn new(
        inner: Box<dyn Demuxer + Send>,
        cancellation: CancellationToken,
        limits: ProgressiveDemuxBufferLimits,
        retry_hint: DemuxRetryHint,
    ) -> std::result::Result<Self, ProgressiveDemuxStartupError> {
        let visible_seekability = inner.seekability();
        if matches!(visible_seekability, DemuxSeekability::Seekable) {
            return Err(ProgressiveDemuxStartupError::SeekableInput);
        }

        let visible_tracks = inner.tracks().to_vec();
        let visible_duration = inner.duration();
        let visible_metadata = inner.media_metadata();
        let shared = Arc::new(ProgressiveSharedState::new(limits));
        let worker_shared = Arc::clone(&shared);
        let worker_cancellation = cancellation.clone();
        thread::Builder::new()
            .name("progressive-demux".to_owned())
            .spawn(move || {
                let _completion = ProgressiveWorkerCompletion::new(Arc::clone(&worker_shared));
                run_progressive_worker(inner, worker_shared, worker_cancellation);
            })
            .map_err(|source| ProgressiveDemuxStartupError::WorkerSpawn { source })?;

        Ok(Self {
            visible_tracks,
            visible_duration,
            visible_metadata,
            visible_seekability,
            shared,
            retry_hint,
            cancellation,
            end_of_stream_reached: false,
        })
    }

    /// Запускает blocking registry sniff/open и последующий demux в одном worker-е.
    ///
    /// Это boundary segmented sources: initial segment readiness не блокирует
    /// player-owner и не маскируется под EOF. До завершения open caller видит
    /// пустой track snapshot и `TemporarilyUnavailable`; первый worker event
    /// публикует реальные tracks.
    pub fn new_deferred<F>(
        open_inner: F,
        cancellation: CancellationToken,
        limits: ProgressiveDemuxBufferLimits,
        retry_hint: DemuxRetryHint,
    ) -> std::result::Result<Self, ProgressiveDemuxStartupError>
    where
        F: FnOnce() -> Result<Box<dyn Demuxer + Send>> + Send + 'static,
    {
        let shared = Arc::new(ProgressiveSharedState::new(limits));
        let worker_shared = Arc::clone(&shared);
        let worker_cancellation = cancellation.clone();
        thread::Builder::new()
            .name("adaptive-demux-open".to_owned())
            .spawn(move || {
                let _completion = ProgressiveWorkerCompletion::new(Arc::clone(&worker_shared));
                let inner = match open_inner() {
                    Ok(inner) => inner,
                    Err(source) => {
                        let _ = push_progressive_message(
                            &worker_shared,
                            &worker_cancellation,
                            ProgressiveMessage::Failure(source),
                        );
                        return;
                    }
                };
                if matches!(inner.seekability(), DemuxSeekability::Seekable) {
                    let _ = push_progressive_message(
                        &worker_shared,
                        &worker_cancellation,
                        ProgressiveMessage::Failure(anyhow::Error::new(
                            ProgressiveDemuxStartupError::SeekableInput,
                        )),
                    );
                    return;
                }
                let initial_tracks = DemuxReadEvent::TracksChanged(DemuxTrackListUpdate::new(
                    inner.tracks().to_vec(),
                    inner.duration(),
                ));
                if !push_progressive_message(
                    &worker_shared,
                    &worker_cancellation,
                    ProgressiveMessage::Event(initial_tracks),
                ) {
                    return;
                }
                if let Some(metadata) = inner.media_metadata()
                    && !push_progressive_message(
                        &worker_shared,
                        &worker_cancellation,
                        ProgressiveMessage::Event(DemuxReadEvent::MediaMetadataChanged(metadata)),
                    )
                {
                    return;
                }
                run_progressive_worker(inner, worker_shared, worker_cancellation);
            })
            .map_err(|source| ProgressiveDemuxStartupError::WorkerSpawn { source })?;

        Ok(Self {
            visible_tracks: Vec::new(),
            visible_duration: None,
            visible_metadata: None,
            visible_seekability: DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::UnknownTimeline,
            },
            shared,
            retry_hint,
            cancellation,
            end_of_stream_reached: false,
        })
    }

    /// Применяет lifecycle snapshot ровно при публикации соответствующего event-а.
    fn apply_visible_event(&mut self, event: &DemuxReadEvent) {
        match event {
            DemuxReadEvent::TracksChanged(update) => {
                self.visible_tracks = update.tracks.clone();
            }
            DemuxReadEvent::MediaMetadataChanged(metadata) => {
                self.visible_metadata = Some(metadata.clone());
            }
            DemuxReadEvent::EndOfStream => {
                self.end_of_stream_reached = true;
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::TemporarilyUnavailable(_) => {}
        }
    }
}

impl Demuxer for ProgressiveDemuxer {
    /// Возвращает последний event-ordered track snapshot.
    fn tracks(&self) -> &[TrackInfo] {
        &self.visible_tracks
    }

    /// Возвращает container duration, известную после initial open.
    fn duration(&self) -> Option<Duration> {
        self.visible_duration
    }

    /// Возвращает последний event-ordered metadata snapshot.
    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.visible_metadata.clone()
    }

    /// Сохраняет исходную typed non-seekable причину.
    fn seekability(&self) -> DemuxSeekability {
        self.visible_seekability
    }

    /// Никогда не ждёт blocking inner demuxer на player owner-е.
    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        if self.end_of_stream_reached {
            return Ok(DemuxReadEvent::EndOfStream);
        }

        let message = {
            let mut queue = self.shared.lock_queue();
            let message = queue.messages.pop_front();
            if let Some(message) = &message {
                queue.queued_encoded_bytes = queue
                    .queued_encoded_bytes
                    .saturating_sub(message.encoded_bytes());
                self.shared.capacity_available.notify_all();
            }
            if message.is_none() && queue.worker_stopped {
                return Err(ProgressiveDemuxWorkerStoppedError.into());
            }
            message
        };

        match message {
            Some(ProgressiveMessage::Event(event)) => {
                self.apply_visible_event(&event);
                Ok(event)
            }
            Some(ProgressiveMessage::Failure(source)) => Err(source),
            None => Ok(DemuxReadEvent::TemporarilyUnavailable(self.retry_hint)),
        }
    }

    /// Progressive non-Range input не публикует ложную seek поддержку.
    fn seek(&mut self, _timestamp: Duration) -> Result<DemuxSeekResult> {
        Err(MediaDemuxError::SeekUnavailable {
            reason: "progressive HTTP source не поддерживает byte seek".to_owned(),
        }
        .into())
    }
}

impl Drop for ProgressiveDemuxer {
    /// Отмена будит backpressure wait и прерывает следующий source read.
    fn drop(&mut self) {
        self.cancellation.cancel();
        let mut queue = self.shared.lock_queue();
        queue.stop_requested = true;
        self.shared.capacity_available.notify_all();
    }
}

/// Одно owned сообщение bounded queue.
enum ProgressiveMessage {
    /// Exact demux event, прочитанный worker-ом.
    Event(DemuxReadEvent),
    /// Downcastable concrete demux/source failure.
    Failure(anyhow::Error),
}

impl ProgressiveMessage {
    /// Считает только encoded packet payload, не Rust allocation overhead.
    fn encoded_bytes(&self) -> usize {
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
struct ProgressiveQueueState {
    /// FIFO сохраняет exact inner event order.
    messages: VecDeque<ProgressiveMessage>,
    /// Сумма encoded packet bytes в `messages`.
    queued_encoded_bytes: usize,
    /// Drop/supersede запрещает worker-у читать следующий event.
    stop_requested: bool,
    /// Worker больше не сможет опубликовать message.
    worker_stopped: bool,
}

/// Shared queue + backpressure coordination.
struct ProgressiveSharedState {
    /// Caller-owned queue limits.
    limits: ProgressiveDemuxBufferLimits,
    /// Единственная authority mutable queue state.
    queue: Mutex<ProgressiveQueueState>,
    /// Consumer pop/drop будит producer без busy loop-а.
    capacity_available: Condvar,
}

/// RAII-предохранитель публикует terminal worker state даже при panic backend-а.
struct ProgressiveWorkerCompletion {
    /// Shared queue, которую player-facing handle продолжает опрашивать.
    shared: Arc<ProgressiveSharedState>,
}

impl ProgressiveWorkerCompletion {
    /// Привязывает terminal notification к lifetime worker closure.
    #[must_use]
    fn new(shared: Arc<ProgressiveSharedState>) -> Self {
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
    fn new(limits: ProgressiveDemuxBufferLimits) -> Self {
        Self {
            limits,
            queue: Mutex::new(ProgressiveQueueState {
                messages: VecDeque::new(),
                queued_encoded_bytes: 0,
                stop_requested: false,
                worker_stopped: false,
            }),
            capacity_available: Condvar::new(),
        }
    }

    /// Poison означает internal invariant failure; восстанавливаем owned state для shutdown.
    fn lock_queue(&self) -> MutexGuard<'_, ProgressiveQueueState> {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Единственный owner blocking inner demuxer-а.
fn run_progressive_worker(
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
                if !push_progressive_message(
                    &shared,
                    &cancellation,
                    ProgressiveMessage::Event(event),
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
                    ProgressiveMessage::Failure(source),
                );
                mark_worker_stopped(&shared);
                return;
            }
        }
    }
}

/// Публикует message только после bounded byte/event admission.
fn push_progressive_message(
    shared: &ProgressiveSharedState,
    cancellation: &CancellationToken,
    mut message: ProgressiveMessage,
) -> bool {
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
            return false;
        }
        let has_event_capacity = queue.messages.len() < shared.limits.max_pending_events();
        let has_byte_capacity = queue
            .queued_encoded_bytes
            .checked_add(admitted_bytes)
            .is_some_and(|total| total <= shared.limits.max_pending_encoded_bytes());
        if has_event_capacity && has_byte_capacity {
            queue.queued_encoded_bytes = queue.queued_encoded_bytes.saturating_add(admitted_bytes);
            queue.messages.push_back(message);
            return !oversized_packet;
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

#[cfg(test)]
mod tests;
