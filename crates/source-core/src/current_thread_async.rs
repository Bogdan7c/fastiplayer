//! Neutral current-thread owner для async HTTP futures.
//!
//! Concrete Tokio runtime остаётся внутри `source-core`: transport consumers
//! передают только стандартные [`Future`] и получают типизированный outcome.

use std::fmt;
use std::future::Future;
use std::time::Instant;

use tokio::runtime::{Builder, Runtime};

/// Ошибка создания current-thread async executor-а.
#[derive(Debug, thiserror::Error)]
#[error("не удалось создать current-thread async executor")]
pub struct CurrentThreadAsyncExecutorBuildError {
    /// Системная причина, сохранённая без раскрытия concrete runtime consumer-у.
    #[source]
    source: std::io::Error,
}

/// Результат ожидания work future вместе с отдельным interrupt future.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptibleAsyncExecution<T> {
    /// Основная операция завершилась раньше прерывания.
    Completed(T),
    /// Interrupt выиграл; незавершённый work future был немедленно уничтожен.
    Interrupted,
}

/// Boxed current-thread executor без утечки Tokio типов через public boundary.
pub struct CurrentThreadAsyncExecutor {
    /// Box сохраняет маленький стабильный consumer-facing handle.
    runtime: Box<Runtime>,
}

impl CurrentThreadAsyncExecutor {
    /// Создаёт I/O- и timer-capable runtime, который работает только в caller thread-е.
    pub fn new() -> Result<Self, CurrentThreadAsyncExecutorBuildError> {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|source| CurrentThreadAsyncExecutorBuildError { source })?;
        Ok(Self {
            runtime: Box::new(runtime),
        })
    }

    /// Выполняет произвольный standard future без знания consumer-а о Tokio.
    ///
    /// Метод предназначен для blocking transport/demux worker-а, а не для вызова
    /// из уже работающего async runtime-а или UI/player-owner thread-а.
    pub fn block_on<Operation>(&self, operation: Operation) -> Operation::Output
    where
        Operation: Future,
    {
        self.runtime.block_on(operation)
    }

    /// Ждёт operation либо standard interrupt future на том же caller thread-е.
    ///
    /// Interrupt branch biased намеренно: уже опубликованная отмена не должна
    /// проиграть одновременно готовому body chunk-у. Losing future уничтожается
    /// до возврата, поэтому pending HTTP read физически прерывается без потока.
    pub fn block_on_interruptible<Operation, Interrupt>(
        &self,
        operation: Operation,
        interrupt: Interrupt,
    ) -> InterruptibleAsyncExecution<Operation::Output>
    where
        Operation: Future,
        Interrupt: Future,
    {
        self.runtime.block_on(async move {
            tokio::pin!(operation);
            tokio::pin!(interrupt);
            tokio::select! {
                biased;

                _ = &mut interrupt => InterruptibleAsyncExecution::Interrupted,
                output = &mut operation => InterruptibleAsyncExecution::Completed(output),
            }
        })
    }
}

impl fmt::Debug for CurrentThreadAsyncExecutor {
    /// Не раскрывает concrete runtime state в diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CurrentThreadAsyncExecutor")
            .finish_non_exhaustive()
    }
}

/// Neutral класс HTTP операции для безопасного performance breakdown-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpAsyncOperationKind {
    /// Bounded fetch, результат которого включает validated body целиком.
    BoundedFetch,
    /// Bounded pull-based response, где headers и body EOF разделены.
    BoundedStreamingFetch,
}

impl HttpAsyncOperationKind {
    /// Возвращает стабильное secret-free значение tracing field-а.
    const fn as_str(self) -> &'static str {
        match self {
            Self::BoundedFetch => "bounded_fetch",
            Self::BoundedStreamingFetch => "bounded_streaming_fetch",
        }
    }
}

/// Причина, по которой lifecycle marker не был опубликован повторно.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpAsyncDiagnosticMarkerOutcome {
    /// Marker опубликован впервые.
    Published,
    /// Этот marker уже был опубликован для текущего request-а.
    AlreadyPublished,
    /// Пустой chunk не является пользовательским first-byte evidence.
    EmptyBodyChunkIgnored,
    /// Validated EOF уже завершил lifecycle, поэтому поздний marker отвергнут.
    OperationAlreadyCompleted,
}

/// One-shot monotonic HTTP diagnostics без locator-а, headers или secret material.
pub struct HttpAsyncOperationDiagnostics {
    /// Класс операции задаётся typed caller intent-ом, а не свободной строкой.
    operation_kind: HttpAsyncOperationKind,
    /// Один monotonic origin используется для всех стадий request-а.
    request_started_at: Instant,
    /// Headers marker публикуется не более одного раза.
    headers_ready: bool,
    /// Первый только непустой body chunk публикуется не более одного раза.
    first_non_empty_body_chunk_ready: bool,
    /// Validated EOF закрывает lifecycle ровно один раз.
    validated_body_complete: bool,
}

impl HttpAsyncOperationDiagnostics {
    /// Начинает новый request lifecycle и публикует secret-free start marker.
    #[must_use]
    pub fn started(operation_kind: HttpAsyncOperationKind) -> Self {
        let request_started_at = Instant::now();
        tracing::debug!(
            operation_kind = operation_kind.as_str(),
            elapsed_milliseconds = request_started_at.elapsed().as_millis(),
            received_body_bytes = 0_usize,
            "Source HTTP request started"
        );
        Self {
            operation_kind,
            request_started_at,
            headers_ready: false,
            first_non_empty_body_chunk_ready: false,
            validated_body_complete: false,
        }
    }

    /// Публикует получение final response headers после redirect traversal-а.
    pub fn record_headers_ready(&mut self) -> HttpAsyncDiagnosticMarkerOutcome {
        if self.validated_body_complete {
            return HttpAsyncDiagnosticMarkerOutcome::OperationAlreadyCompleted;
        }
        if self.headers_ready {
            return HttpAsyncDiagnosticMarkerOutcome::AlreadyPublished;
        }
        self.headers_ready = true;
        tracing::debug!(
            operation_kind = self.operation_kind.as_str(),
            elapsed_milliseconds = self.request_started_at.elapsed().as_millis(),
            received_body_bytes = 0_usize,
            "Source HTTP response headers ready"
        );
        HttpAsyncDiagnosticMarkerOutcome::Published
    }

    /// Публикует только первый непустой body chunk и transport-accounted total.
    pub fn record_first_non_empty_body_chunk(
        &mut self,
        chunk_bytes: usize,
        received_body_bytes: usize,
    ) -> HttpAsyncDiagnosticMarkerOutcome {
        if self.validated_body_complete {
            return HttpAsyncDiagnosticMarkerOutcome::OperationAlreadyCompleted;
        }
        if chunk_bytes == 0 {
            return HttpAsyncDiagnosticMarkerOutcome::EmptyBodyChunkIgnored;
        }
        if self.first_non_empty_body_chunk_ready {
            return HttpAsyncDiagnosticMarkerOutcome::AlreadyPublished;
        }
        self.first_non_empty_body_chunk_ready = true;
        tracing::debug!(
            operation_kind = self.operation_kind.as_str(),
            elapsed_milliseconds = self.request_started_at.elapsed().as_millis(),
            chunk_bytes,
            received_body_bytes,
            "Source HTTP first non-empty body chunk ready"
        );
        HttpAsyncDiagnosticMarkerOutcome::Published
    }

    /// Публикует body completion только после validated transport EOF.
    pub fn record_validated_body_complete(
        &mut self,
        received_body_bytes: usize,
    ) -> HttpAsyncDiagnosticMarkerOutcome {
        if self.validated_body_complete {
            return HttpAsyncDiagnosticMarkerOutcome::AlreadyPublished;
        }
        self.validated_body_complete = true;
        tracing::debug!(
            operation_kind = self.operation_kind.as_str(),
            elapsed_milliseconds = self.request_started_at.elapsed().as_millis(),
            received_body_bytes,
            "Source HTTP validated body complete"
        );
        HttpAsyncDiagnosticMarkerOutcome::Published
    }
}

impl fmt::Debug for HttpAsyncOperationDiagnostics {
    /// Не включает raw request material или нестабильный monotonic timestamp.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpAsyncOperationDiagnostics")
            .field("operation_kind", &self.operation_kind)
            .field("headers_ready", &self.headers_ready)
            .field(
                "first_non_empty_body_chunk_ready",
                &self.first_non_empty_body_chunk_ready,
            )
            .field("validated_body_complete", &self.validated_body_complete)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::future::{pending, ready};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{
        CurrentThreadAsyncExecutor, HttpAsyncDiagnosticMarkerOutcome,
        HttpAsyncOperationDiagnostics, HttpAsyncOperationKind, InterruptibleAsyncExecution,
    };

    /// Drop probe доказывает физическое уничтожение losing future.
    struct DropProbe {
        dropped: Arc<AtomicBool>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    #[test]
    fn generic_block_on_returns_operation_output() {
        let executor = CurrentThreadAsyncExecutor::new().expect("create current-thread executor");

        assert_eq!(executor.block_on(async { 42_u8 }), 42);
    }

    #[test]
    fn interrupt_wins_and_drops_pending_operation_before_return() {
        let executor = CurrentThreadAsyncExecutor::new().expect("create current-thread executor");
        let operation_dropped = Arc::new(AtomicBool::new(false));
        let operation_drop_probe = DropProbe {
            dropped: Arc::clone(&operation_dropped),
        };

        let outcome = executor.block_on_interruptible(
            async move {
                let _drop_probe = operation_drop_probe;
                pending::<()>().await;
            },
            ready(()),
        );

        assert_eq!(outcome, InterruptibleAsyncExecution::Interrupted);
        assert!(operation_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn completion_wins_and_drops_pending_interrupt_before_return() {
        let executor = CurrentThreadAsyncExecutor::new().expect("create current-thread executor");
        let interrupt_dropped = Arc::new(AtomicBool::new(false));
        let interrupt_drop_probe = DropProbe {
            dropped: Arc::clone(&interrupt_dropped),
        };

        let outcome = executor.block_on_interruptible(ready(7_u8), async move {
            let _drop_probe = interrupt_drop_probe;
            pending::<()>().await;
        });

        assert_eq!(outcome, InterruptibleAsyncExecution::Completed(7));
        assert!(interrupt_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn diagnostics_publish_each_stage_once_and_ignore_empty_first_chunk() {
        let mut diagnostics =
            HttpAsyncOperationDiagnostics::started(HttpAsyncOperationKind::BoundedStreamingFetch);

        assert_eq!(
            diagnostics.record_headers_ready(),
            HttpAsyncDiagnosticMarkerOutcome::Published
        );
        assert_eq!(
            diagnostics.record_headers_ready(),
            HttpAsyncDiagnosticMarkerOutcome::AlreadyPublished
        );
        assert_eq!(
            diagnostics.record_first_non_empty_body_chunk(0, 0),
            HttpAsyncDiagnosticMarkerOutcome::EmptyBodyChunkIgnored
        );
        assert_eq!(
            diagnostics.record_first_non_empty_body_chunk(4, 4),
            HttpAsyncDiagnosticMarkerOutcome::Published
        );
        assert_eq!(
            diagnostics.record_first_non_empty_body_chunk(8, 12),
            HttpAsyncDiagnosticMarkerOutcome::AlreadyPublished
        );
        assert_eq!(
            diagnostics.record_validated_body_complete(12),
            HttpAsyncDiagnosticMarkerOutcome::Published
        );
        assert_eq!(
            diagnostics.record_validated_body_complete(12),
            HttpAsyncDiagnosticMarkerOutcome::AlreadyPublished
        );
        assert_eq!(
            diagnostics.record_first_non_empty_body_chunk(1, 13),
            HttpAsyncDiagnosticMarkerOutcome::OperationAlreadyCompleted
        );
    }
}
