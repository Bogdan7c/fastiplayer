//! Nonblocking player-facing adapter для blocking progressive demuxer-а.
//!
//! Concrete container продолжает выполнять обычные blocking reads на отдельном
//! worker-е. Player owner видит только bounded очередь готовых events либо
//! neutral `TemporarilyUnavailable`, поэтому parser никогда не прерывается
//! посреди container element-а через небезопасный `WouldBlock`.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability,
    DemuxTrackListUpdate, Demuxer, MediaDemuxError, MediaMetadata, TimelineNotSeekableReason,
    TrackInfo,
};
use source_core::CancellationToken;

mod worker;

use worker::{
    ProgressiveMessage, ProgressivePushOutcome, ProgressiveSeekCommand, ProgressiveSharedState,
    ProgressiveWorkerCompletion, push_progressive_message, run_progressive_worker,
    run_seekable_progressive_worker,
};

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
    /// Seekable constructor требует честно seekable inner contract.
    #[error("seekable progressive demux worker получил non-seekable input")]
    SeekableInputRequired,
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

/// Provider-neutral nonblocking preview seek-а, выполняемого blocking worker-ом позже.
///
/// Controller не выполняет I/O и не меняет parser state. Он обязан вернуть только
/// доказанный container packet boundary, который worker сможет воспроизвести тем же
/// `DemuxSeekRequest`. Это позволяет player-owner-у начать preroll без ожидания сети.
#[derive(Clone)]
pub struct ProgressiveSeekController {
    preview: Arc<dyn Fn(DemuxSeekRequest) -> Result<DemuxSeekResult> + Send + Sync>,
}

impl ProgressiveSeekController {
    /// Создаёт controller из bounded provider-owned seek index lookup.
    pub fn new<F>(preview: F) -> Self
    where
        F: Fn(DemuxSeekRequest) -> Result<DemuxSeekResult> + Send + Sync + 'static,
    {
        Self {
            preview: Arc::new(preview),
        }
    }

    /// Возвращает доказанный результат, не выполняя blocking work.
    fn preview(&self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        (self.preview)(request)
    }
}

impl std::fmt::Debug for ProgressiveSeekController {
    /// Не раскрывает provider-owned index internals.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProgressiveSeekController")
            .finish_non_exhaustive()
    }
}

/// Worker воспроизвёл иной anchor, чем controller уже отдал player-у.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("progressive seek worker вернул anchor {worker_actual:?}, ожидался {preview_actual:?}")]
pub struct ProgressiveSeekAnchorMismatchError {
    /// Доказанный preview, по которому player начал preroll.
    pub preview_actual: media_core::MediaTime,
    /// Фактический anchor replacement parser-а.
    pub worker_actual: media_core::MediaTime,
}

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
    /// Optional target-specific preview для seekable blocking inner-а.
    seek_controller: Option<ProgressiveSeekController>,
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
            seek_controller: None,
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
                            0,
                            ProgressiveMessage::Failure(source),
                        );
                        return;
                    }
                };
                if matches!(inner.seekability(), DemuxSeekability::Seekable) {
                    let _ = push_progressive_message(
                        &worker_shared,
                        &worker_cancellation,
                        0,
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
                if !matches!(
                    push_progressive_message(
                        &worker_shared,
                        &worker_cancellation,
                        0,
                        ProgressiveMessage::Event(initial_tracks),
                    ),
                    ProgressivePushOutcome::Published
                ) {
                    return;
                }
                if let Some(metadata) = inner.media_metadata()
                    && !matches!(
                        push_progressive_message(
                            &worker_shared,
                            &worker_cancellation,
                            0,
                            ProgressiveMessage::Event(DemuxReadEvent::MediaMetadataChanged(
                                metadata,
                            )),
                        ),
                        ProgressivePushOutcome::Published
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
            seek_controller: None,
            shared,
            retry_hint,
            cancellation,
            end_of_stream_reached: false,
        })
    }

    /// Запускает seekable blocking demuxer с nonblocking command/result boundary.
    ///
    /// `open_inner` и все последующие `seek_with_request` выполняются только на
    /// worker-е. Player-facing `seek_with_request` вызывает лишь controller preview,
    /// очищает старую generation queue и публикует latest seek command.
    pub fn new_deferred_seekable<F>(
        open_inner: F,
        seek_controller: ProgressiveSeekController,
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
            .name("adaptive-seekable-demux-open".to_owned())
            .spawn(move || {
                let _completion = ProgressiveWorkerCompletion::new(Arc::clone(&worker_shared));
                let inner = match open_inner() {
                    Ok(inner) => inner,
                    Err(source) => {
                        let _ = push_progressive_message(
                            &worker_shared,
                            &worker_cancellation,
                            0,
                            ProgressiveMessage::Failure(source),
                        );
                        return;
                    }
                };
                if !matches!(inner.seekability(), DemuxSeekability::Seekable) {
                    let _ = push_progressive_message(
                        &worker_shared,
                        &worker_cancellation,
                        0,
                        ProgressiveMessage::Failure(anyhow::Error::new(
                            ProgressiveDemuxStartupError::SeekableInputRequired,
                        )),
                    );
                    return;
                }
                let initial_tracks = DemuxReadEvent::TracksChanged(DemuxTrackListUpdate::new(
                    inner.tracks().to_vec(),
                    inner.duration(),
                ));
                if !matches!(
                    push_progressive_message(
                        &worker_shared,
                        &worker_cancellation,
                        0,
                        ProgressiveMessage::Event(initial_tracks),
                    ),
                    ProgressivePushOutcome::Published
                ) {
                    return;
                }
                if let Some(metadata) = inner.media_metadata()
                    && !matches!(
                        push_progressive_message(
                            &worker_shared,
                            &worker_cancellation,
                            0,
                            ProgressiveMessage::Event(DemuxReadEvent::MediaMetadataChanged(
                                metadata,
                            )),
                        ),
                        ProgressivePushOutcome::Published
                    )
                {
                    return;
                }
                run_seekable_progressive_worker(inner, worker_shared, worker_cancellation);
            })
            .map_err(|source| ProgressiveDemuxStartupError::WorkerSpawn { source })?;

        Ok(Self {
            visible_tracks: Vec::new(),
            visible_duration: None,
            visible_metadata: None,
            visible_seekability: DemuxSeekability::Seekable,
            seek_controller: Some(seek_controller),
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
                self.visible_duration = update.duration;
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
            while queue
                .messages
                .front()
                .is_some_and(|envelope| envelope.generation != queue.current_generation)
            {
                if let Some(stale) = queue.messages.pop_front() {
                    queue.queued_encoded_bytes = queue
                        .queued_encoded_bytes
                        .saturating_sub(stale.message.encoded_bytes());
                }
            }
            let message = queue.messages.pop_front();
            if let Some(message) = &message {
                queue.queued_encoded_bytes = queue
                    .queued_encoded_bytes
                    .saturating_sub(message.message.encoded_bytes());
                self.shared.capacity_available.notify_all();
            }
            if message.is_none() && queue.worker_stopped {
                return Err(ProgressiveDemuxWorkerStoppedError.into());
            }
            message
        };

        match message.map(|envelope| envelope.message) {
            Some(ProgressiveMessage::Event(event)) => {
                self.apply_visible_event(&event);
                Ok(event)
            }
            Some(ProgressiveMessage::Failure(source)) => Err(source),
            None => Ok(DemuxReadEvent::TemporarilyUnavailable(self.retry_hint)),
        }
    }

    /// Progressive non-Range input не публикует ложную seek поддержку.
    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    /// Публикует command без ожидания network/parser worker-а.
    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        let Some(controller) = &self.seek_controller else {
            return Err(MediaDemuxError::SeekUnavailable {
                reason: "progressive HTTP source не поддерживает byte seek".to_owned(),
            }
            .into());
        };
        let preview = controller.preview(request)?;
        let mut queue = self.shared.lock_queue();
        if queue.worker_stopped {
            return Err(ProgressiveDemuxWorkerStoppedError.into());
        }
        queue.current_generation = queue.current_generation.wrapping_add(1);
        queue.messages.clear();
        queue.queued_encoded_bytes = 0;
        queue.pending_seek = Some(ProgressiveSeekCommand {
            generation: queue.current_generation,
            request,
            preview,
        });
        self.end_of_stream_reached = false;
        self.shared.capacity_available.notify_all();
        Ok(preview)
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

#[cfg(test)]
mod tests;
