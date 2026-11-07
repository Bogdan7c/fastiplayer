//! Транспорт моста: «розетка» HLS-движка (`AdaptiveResourceTransport`) поверх очереди
//! запросов ресурсов `pushed-media-source`.
//!
//! ```text
//!  движок HLS ── hop ──► BridgeResourceTransport ── адрес → номерок ──► очередь ──► fetch_resource
//!      ▲                                                                   │
//!      └── тело / ошибка ◄── куски рамок вида 2 + resource_fetch_finished ◄─┘
//! ```
//!
//! Договор «розетки» (rustdoc трейта) соблюдается так:
//! - **один hop, без редиректов**: браузер ходит по редиректам сам, поэтому
//!   `Redirect` не возвращается никогда;
//! - **без повторов**: повторы делает контекст по `is_retryable`; смысл
//!   «повторять ли» сохраняет перевод ошибок ([`super::fetch_plan`]);
//! - **лимит тела и точная длина диапазона**: буферный путь собирает ответ через
//!   `HttpBoundedResponse::from_external_body` (те же проверки, что у обычного HTTP);
//!   потоковый — очередь сама не принимает тело длиннее потолка (нарушение
//!   страницы), а короткий диапазон — ошибка `UnexpectedEof`, не честный конец;
//! - **отмена**: проверяется до побочного эффекта (до выдачи номера); блокирующее
//!   ожидание прерывает `CancellationToken`, асинхронное — тот же токен через его
//!   future и уничтожение future (уничтожение `ResourceFetch` = отказ от запроса).
//!
//! Ограничение: выдача номера ([`ResourceFetchClient::start`]) у очереди только
//! блокирующая — она ждёт свободного места, если у страницы уже 8 запросов. Поэтому
//! асинхронные пути на это время занимают поток своего исполнителя. Место
//! освобождает итог страницы или срок молчания, а не чтение тел, поэтому взаимной
//! блокировки нет; отмена прерывает ожидание за ≤ 10 мс.

use std::fmt;
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::task::Poll;

use bytes::Bytes;
use pushed_media_source::{ResourceFetch, ResourceFetchClient, ResourceFetchError};
use source_core::{
    CancellationToken, HttpBoundedFetchRequest, HttpBoundedResponse, HttpRangeResponseMetadata,
    HttpRequestAttemptId, SourceError, SourceResult,
};
use web_media_adaptive::{
    AdaptiveBufferedHop, AdaptiveResourceTransport, AdaptiveStreamingBody, AdaptiveStreamingHop,
    AdaptiveTransportFuture,
};

use super::fetch_plan::{BridgeFailureContext, BridgeFetchPlan};

/// «Розетка» native HLS, которая качает каждый ресурс у страницы по номерку.
#[derive(Clone)]
pub(crate) struct BridgeResourceTransport {
    /// Сторона движка очереди запросов ресурсов поколения.
    client: ResourceFetchClient,
}

impl BridgeResourceTransport {
    /// Транспорт над очередью поколения.
    pub(crate) const fn new(client: ResourceFetchClient) -> Self {
        Self { client }
    }

    /// Выдаёт номер просьбе (проверив отмену до побочного эффекта).
    fn start(
        &self,
        plan: &BridgeFetchPlan,
        cancellation: &CancellationToken,
    ) -> SourceResult<ResourceFetch> {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        self.client
            .start(plan.spec(), cancellation)
            .map_err(|error| plan.source_error(error))
    }
}

impl fmt::Debug for BridgeResourceTransport {
    /// Ни номерков, ни адресов: только вид транспорта.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BridgeResourceTransport")
            .finish_non_exhaustive()
    }
}

impl AdaptiveResourceTransport for BridgeResourceTransport {
    fn fetch_buffered_hop_blocking(
        &self,
        request: HttpBoundedFetchRequest,
        cancellation: &CancellationToken,
    ) -> SourceResult<AdaptiveBufferedHop> {
        let plan = BridgeFetchPlan::for_request(&request)?;
        let mut fetch = self.start(&plan, cancellation)?;
        let mut body = Vec::new();
        while let Some(chunk) = fetch
            .next_chunk_blocking(cancellation)
            .map_err(|error| plan.source_error(error))?
        {
            body.extend_from_slice(&chunk);
        }
        HttpBoundedResponse::from_external_body(&request, body).map(AdaptiveBufferedHop::Complete)
    }

    fn fetch_buffered_hop_abortable<'operation>(
        &'operation self,
        request: HttpBoundedFetchRequest,
        cancellation: &'operation CancellationToken,
    ) -> AdaptiveTransportFuture<'operation, SourceResult<AdaptiveBufferedHop>> {
        Box::pin(async move {
            let plan = BridgeFetchPlan::for_request(&request)?;
            let mut fetch = self.start(&plan, cancellation)?;
            let mut body = Vec::new();
            while let Some(chunk) = next_chunk_unless_cancelled(&mut fetch, cancellation)
                .await
                .map_err(|error| plan.source_error(error))?
            {
                body.extend_from_slice(&chunk);
            }
            HttpBoundedResponse::from_external_body(&request, body)
                .map(AdaptiveBufferedHop::Complete)
        })
    }

    fn open_streaming_hop<'operation>(
        &'operation self,
        request: HttpBoundedFetchRequest,
        cancellation: &'operation CancellationToken,
    ) -> AdaptiveTransportFuture<'operation, SourceResult<AdaptiveStreamingHop>> {
        Box::pin(async move {
            let plan = BridgeFetchPlan::for_request(&request)?;
            let mut fetch = self.start(&plan, cancellation)?;
            // Ответ «начался», когда пришёл первый кусок или итог — как заголовки у HTTP.
            // Отказ страницы (сеть, статус, отказ фона) поэтому виден ещё при открытии, и
            // контекст повторяет и классифицирует его так же, как ошибку ответа сервера, а
            // не как обрыв посреди тела.
            let first = next_chunk_unless_cancelled(&mut fetch, cancellation)
                .await
                .map_err(|error| plan.source_error(error))?;
            let received_body_bytes = first.as_ref().map_or(0, Bytes::len);
            let progress = match first {
                Some(chunk) => BodyProgress::FirstChunkWaiting(chunk),
                None => BodyProgress::Ended,
            };
            Ok(AdaptiveStreamingHop::Body(Box::new(BridgeStreamingBody {
                fetch,
                failures: plan.into_failures(),
                progress,
                received_body_bytes,
            })))
        })
    }
}

/// Потоковое тело одного запроса ресурса: куски в порядке прихода от страницы.
///
/// Уничтожение тела уничтожает `ResourceFetch` — запрос брошен (поздние рамки
/// страницы очередь отбросит молча).
struct BridgeStreamingBody {
    /// Запрос в очереди.
    fetch: ResourceFetch,
    /// Как назвать отказ (адрес не хранится в печатаемом виде).
    failures: BridgeFailureContext,
    /// Где чтение тела: первый кусок (прочитан при открытии) ещё не отдан, читаем из
    /// очереди или тело кончилось.
    progress: BodyProgress,
    /// Сколько байтов принято от страницы (первый кусок считается сразу).
    received_body_bytes: usize,
}

/// Где чтение потокового тела.
#[derive(Debug)]
enum BodyProgress {
    /// Первый кусок прочитан при открытии и ждёт читателя.
    FirstChunkWaiting(Bytes),
    /// Дальше — куски из очереди.
    Streaming,
    /// Честный конец тела (повторный вызов — снова конец).
    Ended,
}

impl fmt::Debug for BridgeStreamingBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BridgeStreamingBody")
            .field("received_body_bytes", &self.received_body_bytes)
            .finish_non_exhaustive()
    }
}

impl AdaptiveStreamingBody for BridgeStreamingBody {
    fn next_chunk<'operation>(
        &'operation mut self,
        cancellation: &'operation CancellationToken,
    ) -> AdaptiveTransportFuture<'operation, SourceResult<Option<Bytes>>> {
        Box::pin(async move {
            match std::mem::replace(&mut self.progress, BodyProgress::Streaming) {
                BodyProgress::FirstChunkWaiting(chunk) => return Ok(Some(chunk)),
                BodyProgress::Ended => {
                    self.progress = BodyProgress::Ended;
                    return Ok(None);
                }
                BodyProgress::Streaming => {}
            }
            let chunk = next_chunk_unless_cancelled(&mut self.fetch, cancellation)
                .await
                .map_err(|error| self.failures.source_error(error))?;
            match &chunk {
                Some(chunk) => {
                    self.received_body_bytes = self.received_body_bytes.saturating_add(chunk.len());
                }
                None => self.progress = BodyProgress::Ended,
            }
            Ok(chunk)
        })
    }

    fn received_body_bytes(&self) -> usize {
        self.received_body_bytes
    }

    /// `Content-Range` страница не пересказывает: HLS эти метаданные не читает.
    fn range_metadata(&self) -> Option<&HttpRangeResponseMetadata> {
        None
    }

    /// Физический запрос ведёт браузер; номер запроса протокола — не диагностика hop-а.
    fn request_attempt_id(&self) -> Option<HttpRequestAttemptId> {
        None
    }
}

/// Следующий кусок тела асинхронно; отмена токена побеждает ожидание.
///
/// Будят оба: очередь (кусок, итог, молчание, печать, нарушение) и токен отмены.
async fn next_chunk_unless_cancelled(
    fetch: &mut ResourceFetch,
    cancellation: &CancellationToken,
) -> Result<Option<Bytes>, ResourceFetchError> {
    let mut cancelled = pin!(cancellation.cancelled());
    poll_fn(move |context| {
        if cancelled.as_mut().poll(context).is_ready() {
            return Poll::Ready(Err(ResourceFetchError::Cancelled));
        }
        fetch.poll_next_chunk(context)
    })
    .await
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
