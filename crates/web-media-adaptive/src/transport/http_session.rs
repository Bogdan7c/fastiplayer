//! Обычный HTTP-транспорт adaptive-слоя поверх единственной `HttpSourceSession`.
//!
//! Это прежний путь без изменений поведения: каждая операция трейта — ровно тот
//! вызов `HttpSourceSession`, который раньше делался прямо из `fetch`-модуля.
//! Ответ source-core переводится в словарь трейта без копирования тела.

use bytes::Bytes;
use source_core::{
    CancellationToken, HttpBoundedFetchHop, HttpBoundedFetchRequest, HttpBoundedStreamingBody,
    HttpBoundedStreamingFetchHop, HttpRangeResponseMetadata, HttpRequestAttemptId,
    HttpSourceSession, SourceResult,
};

use super::{
    AdaptiveBufferedHop, AdaptiveResourceTransport, AdaptiveStreamingBody, AdaptiveStreamingHop,
    AdaptiveTransportFuture,
};

/// HTTP(S) через reqwest-сессию с scoped cookie jar.
#[derive(Debug)]
pub(crate) struct HttpSessionResourceTransport {
    /// Единственный владелец HTTP-клиентов (blocking и lazy async пулов).
    session: HttpSourceSession,
}

impl HttpSessionResourceTransport {
    /// Оборачивает уже собранную сессию; второй клиент не создаётся.
    pub(crate) const fn new(session: HttpSourceSession) -> Self {
        Self { session }
    }
}

impl AdaptiveResourceTransport for HttpSessionResourceTransport {
    fn fetch_buffered_hop_blocking(
        &self,
        request: HttpBoundedFetchRequest,
        cancellation: &CancellationToken,
    ) -> SourceResult<AdaptiveBufferedHop> {
        self.session
            .fetch_bounded_single_hop(request, cancellation)
            .map(buffered_hop_from_session)
    }

    fn fetch_buffered_hop_abortable<'operation>(
        &'operation self,
        request: HttpBoundedFetchRequest,
        cancellation: &'operation CancellationToken,
    ) -> AdaptiveTransportFuture<'operation, SourceResult<AdaptiveBufferedHop>> {
        Box::pin(async move {
            self.session
                .fetch_bounded_single_hop_abortable(request, cancellation)
                .await
                .map(buffered_hop_from_session)
        })
    }

    fn open_streaming_hop<'operation>(
        &'operation self,
        request: HttpBoundedFetchRequest,
        cancellation: &'operation CancellationToken,
    ) -> AdaptiveTransportFuture<'operation, SourceResult<AdaptiveStreamingHop>> {
        Box::pin(async move {
            let hop = self
                .session
                .open_bounded_single_hop_stream(request, cancellation)
                .await?;
            Ok(match hop {
                HttpBoundedStreamingFetchHop::Redirect(redirect) => {
                    AdaptiveStreamingHop::Redirect {
                        next_target: redirect.target().clone(),
                    }
                }
                HttpBoundedStreamingFetchHop::Body(body) => {
                    AdaptiveStreamingHop::Body(Box::new(HttpSessionStreamingBody { body }))
                }
            })
        })
    }
}

/// Переводит буферный hop source-core в словарь трейта (тело не копируется).
fn buffered_hop_from_session(hop: HttpBoundedFetchHop) -> AdaptiveBufferedHop {
    match hop {
        HttpBoundedFetchHop::Redirect(redirect) => AdaptiveBufferedHop::Redirect {
            next_target: redirect.target().clone(),
        },
        HttpBoundedFetchHop::Complete(response) => AdaptiveBufferedHop::Complete(response),
    }
}

/// Открытое reqwest-тело: учёт байтов, лимит и EOF диапазона ведёт source-core.
#[derive(Debug)]
struct HttpSessionStreamingBody {
    /// Прежний курсор ответа с диагностикой физического запроса.
    body: HttpBoundedStreamingBody,
}

impl AdaptiveStreamingBody for HttpSessionStreamingBody {
    fn next_chunk<'operation>(
        &'operation mut self,
        cancellation: &'operation CancellationToken,
    ) -> AdaptiveTransportFuture<'operation, SourceResult<Option<Bytes>>> {
        Box::pin(self.body.next_chunk(cancellation))
    }

    fn received_body_bytes(&self) -> usize {
        self.body.received_body_bytes()
    }

    fn range_metadata(&self) -> Option<&HttpRangeResponseMetadata> {
        self.body.range_metadata()
    }

    fn request_attempt_id(&self) -> Option<HttpRequestAttemptId> {
        // У обычного HTTP физический запрос есть всегда.
        Some(self.body.request_attempt_id())
    }
}
