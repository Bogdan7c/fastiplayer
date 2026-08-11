use std::sync::Arc;

use source_core::{ByteSource, CancellationToken, Seekability, SourceError, SourceResult};

use crate::config::PrefetchConfig;
use crate::shared::{ActivePrefetchFetch, PrefetchShared};

/// Worker владеет единственным inner source и последовательно наполняет RAM-window.
pub(crate) struct PrefetchWorker {
    /// Реальный byte source, включая возможный HTTP Range источник.
    inner: Box<dyn ByteSource>,

    /// Shared state для обмена chunks, seek requests и lifecycle flags.
    shared: Arc<PrefetchShared>,

    /// Token, которым `Drop` останавливает worker loop, когда worker не находится внутри fetch-а.
    cancellation: CancellationToken,

    /// Настройки размера chunk/window, снятые из публичного config.
    config: PrefetchConfig,

    /// Текущий размер следующего чтения; worker-local slow-start не нужен shared state-у.
    current_chunk_len: usize,

    /// Флаг seekability, снятый один раз до передачи source-а в worker thread.
    inner_is_seekable: bool,
}

impl PrefetchWorker {
    /// Собирает worker из source-а, который после этого нельзя читать foreground-ом напрямую.
    #[must_use]
    pub fn new(
        inner: Box<dyn ByteSource>,
        shared: Arc<PrefetchShared>,
        cancellation: CancellationToken,
        config: PrefetchConfig,
        seekability: Seekability,
    ) -> Self {
        let current_chunk_len = usize::try_from(config.initial_chunk_bytes())
            .expect("prefetch initial_chunk_bytes должен помещаться в usize");

        Self {
            inner,
            shared,
            cancellation,
            config,
            current_chunk_len,
            inner_is_seekable: matches!(seekability, Seekability::Seekable),
        }
    }

    /// Основной цикл prefetch-а: lock нужен только для RAM-state, сеть читается без mutex-а.
    pub fn run(mut self) {
        let max_chunk_len = usize::try_from(self.config.chunk_bytes())
            .expect("prefetch chunk_bytes должен помещаться в usize для allocation");
        let mut chunk_buffer = vec![0; max_chunk_len];

        loop {
            if self.cancellation.is_cancelled() {
                return;
            }

            let (fetch_offset, fetch_cancellation) = match self.next_fetch_offset() {
                FetchDecision::Fetch {
                    offset,
                    cancellation,
                } => (offset, cancellation),
                FetchDecision::Shutdown => return,
            };

            if self.inner_is_seekable {
                let seek_result = self.inner.seek(fetch_offset);
                if self.publish_seek_error_if_needed(seek_result) {
                    continue;
                }
            }

            if self.cancellation.is_cancelled() {
                self.clear_active_fetch();
                return;
            }

            let current_chunk_len = self.current_chunk_len;
            tracing::debug!(
                fetch_offset,
                chunk_bytes = current_chunk_len,
                max_chunk_bytes = max_chunk_len,
                window_bytes = self.config.window_bytes(),
                "media prefetch worker читает следующий chunk"
            );
            let read_result = self
                .inner
                .read(&mut chunk_buffer[..current_chunk_len], &fetch_cancellation);
            match read_result {
                Ok(bytes_read) => {
                    self.publish_read_result(Ok(bytes_read), &chunk_buffer);
                    if bytes_read > 0 {
                        // Рост применяется только к следующему fetch: уже опубликованный chunk
                        // остаётся ровно тем размером, который запросил текущий HTTP Range.
                        self.current_chunk_len =
                            self.current_chunk_len.saturating_mul(2).min(max_chunk_len);
                    }
                }
                Err(error) => self.publish_read_result(Err(error), &chunk_buffer),
            }
        }
    }

    /// Выбирает следующий fetch offset или ждёт, пока foreground освободит место/попросит seek.
    fn next_fetch_offset(&mut self) -> FetchDecision {
        let initial_chunk_len = usize::try_from(self.config.initial_chunk_bytes())
            .expect("prefetch initial_chunk_bytes должен помещаться в usize");
        let mut state = self.shared.lock_state();

        loop {
            if state.shutdown || self.cancellation.is_cancelled() {
                return FetchDecision::Shutdown;
            }

            if let Some(offset) = state.seek_request.take() {
                let previous_buffered_end = state.buffer.buffered_end();
                state.buffer.reset_to(offset);
                state.fatal_error = None;
                state.diagnostics.refetches = state.diagnostics.refetches.saturating_add(1);
                tracing::debug!(
                    offset,
                    previous_buffered_end,
                    refetches = state.diagnostics.refetches,
                    "media prefetch worker применяет новое окно после foreground seek"
                );
                // Seek начинает новую серию чтения с маленького Range, чтобы foreground
                // получил первый байт с нового offset-а без ожидания полного большого chunk-а.
                self.current_chunk_len = initial_chunk_len;
            }

            if state.fatal_error.is_none() && state.buffer.needs_fetch() {
                let active_fetch = ActivePrefetchFetch::new(
                    state.buffer.next_fetch_offset(),
                    self.current_chunk_len,
                );
                let fetch_cancellation = active_fetch.cancellation();
                debug_assert!(
                    state.active_fetch.is_none(),
                    "active fetch должен очищаться после каждого завершённого чтения"
                );
                state.active_fetch = Some(active_fetch);
                return FetchDecision::Fetch {
                    offset: state.buffer.next_fetch_offset(),
                    cancellation: fetch_cancellation,
                };
            }

            tracing::debug!(
                buffered_end = state.buffer.buffered_end(),
                next_fetch_offset = state.buffer.next_fetch_offset(),
                window_bytes = self.config.window_bytes(),
                "media prefetch worker ждёт foreground consume/seek"
            );
            state = self.shared.wait_state(state);
        }
    }

    /// Публикует ошибку seek-а, если за время seek-а foreground не запросил новый offset.
    fn publish_seek_error_if_needed(&self, seek_result: SourceResult<()>) -> bool {
        let Err(error) = seek_result else {
            return false;
        };

        let mut state = self.shared.lock_state();
        let active_fetch_was_cancelled = state
            .active_fetch
            .as_ref()
            .is_some_and(ActivePrefetchFetch::is_cancelled);
        state.active_fetch = None;
        if !state.shutdown
            && !self.cancellation.is_cancelled()
            && state.seek_request.is_none()
            && !active_fetch_was_cancelled
        {
            state.fatal_error = Some(error);
            self.shared.notify_all();
        }

        true
    }

    /// Публикует результат blocking read-а, отбрасывая устаревший chunk после concurrent seek.
    fn publish_read_result(&self, read_result: SourceResult<usize>, chunk_buffer: &[u8]) {
        let mut state = self.shared.lock_state();
        let seek_request_pending = state.seek_request.is_some();
        let active_fetch_was_cancelled = state
            .active_fetch
            .as_ref()
            .is_some_and(ActivePrefetchFetch::is_cancelled);
        state.active_fetch = None;

        if state.shutdown || self.cancellation.is_cancelled() {
            return;
        }

        // Pending seek может быть superseded возвратом в сохранённое окно ещё до
        // завершения inner read-а. Token остаётся точным доказательством того,
        // что результат active fetch-а уже нельзя публиковать.
        if seek_request_pending || active_fetch_was_cancelled {
            state.diagnostics.cancelled_fetches =
                state.diagnostics.cancelled_fetches.saturating_add(1);
            match read_result {
                Err(SourceError::Cancelled) => {
                    tracing::debug!(
                        cancelled_fetches = state.diagnostics.cancelled_fetches,
                        seek_request_pending,
                        "media prefetch worker отбросил отменённый fetch после foreground seek"
                    );
                }
                Ok(bytes_read) => {
                    tracing::debug!(
                        bytes_read,
                        cancelled_fetches = state.diagnostics.cancelled_fetches,
                        seek_request_pending,
                        "media prefetch worker отбросил устаревший chunk после foreground seek"
                    );
                }
                Err(error) => {
                    tracing::debug!(
                        error = %error,
                        cancelled_fetches = state.diagnostics.cancelled_fetches,
                        seek_request_pending,
                        "media prefetch worker отбросил ошибку устаревшего fetch-а после foreground seek"
                    );
                }
            }
            self.shared.notify_all();
            return;
        }

        match read_result {
            Ok(0) => {
                state.buffer.mark_eof_at_fetch_offset();
                tracing::debug!(
                    eof_offset = state.buffer.buffered_end(),
                    chunks_fetched = state.diagnostics.chunks_fetched,
                    bytes_prefetched = state.diagnostics.bytes_prefetched,
                    "media prefetch worker дошёл до EOF"
                );
                self.shared.notify_all();
            }
            Ok(bytes_read) => {
                state
                    .buffer
                    .append_chunk(chunk_buffer[..bytes_read].to_vec());
                state.diagnostics.bytes_prefetched = state
                    .diagnostics
                    .bytes_prefetched
                    .saturating_add(bytes_read as u64);
                state.diagnostics.chunks_fetched =
                    state.diagnostics.chunks_fetched.saturating_add(1);
                tracing::debug!(
                    bytes_read,
                    buffered_end = state.buffer.buffered_end(),
                    chunks_fetched = state.diagnostics.chunks_fetched,
                    bytes_prefetched = state.diagnostics.bytes_prefetched,
                    "media prefetch worker добавил chunk в RAM window"
                );
                self.shared.notify_all();
            }
            Err(error) => {
                tracing::debug!(
                    error = %error,
                    "media prefetch worker передаёт ошибку foreground read"
                );
                state.fatal_error = Some(error);
                self.shared.notify_all();
            }
        }
    }

    /// Очищает active fetch, если worker остановили между выбором offset-а и чтением source-а.
    fn clear_active_fetch(&self) {
        let mut state = self.shared.lock_state();
        state.active_fetch = None;
    }
}

/// Решение worker-а после проверки shared predicates.
enum FetchDecision {
    /// Нужно читать source с указанного absolute offset.
    Fetch {
        /// Absolute offset, с которого worker должен начать следующий Range/fetch.
        offset: u64,

        /// Token только для этого fetch-а; foreground seek/drop отменяют именно его.
        cancellation: CancellationToken,
    },

    /// Source закрывается, worker должен выйти без дополнительных действий.
    Shutdown,
}
