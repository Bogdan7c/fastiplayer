use std::sync::Arc;

use source_core::{ByteSource, CancellationToken, Seekability, SourceResult};

use crate::config::PrefetchConfig;
use crate::shared::PrefetchShared;

/// Worker владеет единственным inner source и последовательно наполняет RAM-window.
pub(crate) struct PrefetchWorker {
    /// Реальный byte source, включая возможный HTTP Range источник.
    inner: Box<dyn ByteSource>,

    /// Shared state для обмена chunks, seek requests и lifecycle flags.
    shared: Arc<PrefetchShared>,

    /// Token, которым `Drop` прерывает блокирующие source operations.
    cancellation: CancellationToken,

    /// Настройки размера chunk/window, снятые из публичного config.
    config: PrefetchConfig,

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
        Self {
            inner,
            shared,
            cancellation,
            config,
            inner_is_seekable: matches!(seekability, Seekability::Seekable),
        }
    }

    /// Основной цикл prefetch-а: lock нужен только для RAM-state, сеть читается без mutex-а.
    pub fn run(mut self) {
        let chunk_len = usize::try_from(self.config.chunk_bytes())
            .expect("prefetch chunk_bytes должен помещаться в usize для allocation");
        let mut chunk_buffer = vec![0; chunk_len];

        loop {
            if self.cancellation.is_cancelled() {
                return;
            }

            let fetch_offset = match self.next_fetch_offset() {
                FetchDecision::Fetch(offset) => offset,
                FetchDecision::Shutdown => return,
            };

            if self.inner_is_seekable {
                let seek_result = self.inner.seek(fetch_offset);
                if self.publish_seek_error_if_needed(seek_result) {
                    continue;
                }
            }

            if self.cancellation.is_cancelled() {
                return;
            }

            tracing::debug!(
                fetch_offset,
                chunk_bytes = chunk_len,
                window_bytes = self.config.window_bytes(),
                "media prefetch worker читает следующий chunk"
            );
            let read_result = self
                .inner
                .read(&mut chunk_buffer[..chunk_len], &self.cancellation);
            self.publish_read_result(read_result, &chunk_buffer);
        }
    }

    /// Выбирает следующий fetch offset или ждёт, пока foreground освободит место/попросит seek.
    fn next_fetch_offset(&self) -> FetchDecision {
        let mut state = self.shared.lock_state();

        loop {
            if state.shutdown || self.cancellation.is_cancelled() {
                return FetchDecision::Shutdown;
            }

            if let Some(offset) = state.seek_request.take() {
                tracing::debug!(
                    offset,
                    "media prefetch worker сбрасывает окно после foreground seek"
                );
                state.buffer.reset_to(offset);
                state.fatal_error = None;
            }

            if state.fatal_error.is_none() && state.buffer.needs_fetch() {
                return FetchDecision::Fetch(state.buffer.next_fetch_offset());
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
        if !state.shutdown && !self.cancellation.is_cancelled() && state.seek_request.is_none() {
            state.fatal_error = Some(error);
            self.shared.notify_all();
        }

        true
    }

    /// Публикует результат blocking read-а, отбрасывая устаревший chunk после concurrent seek.
    fn publish_read_result(&self, read_result: SourceResult<usize>, chunk_buffer: &[u8]) {
        let mut state = self.shared.lock_state();

        if state.shutdown || self.cancellation.is_cancelled() || state.seek_request.is_some() {
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
}

/// Решение worker-а после проверки shared predicates.
enum FetchDecision {
    /// Нужно читать source с указанного absolute offset.
    Fetch(u64),

    /// Source закрывается, worker должен выйти без дополнительных действий.
    Shutdown,
}
