//! «Розетка» транспорта одного hop-а для `AdaptiveHttpContext`.
//!
//! Вся сеть adaptive-слоя проходит ровно через три операции: буферный запрос на
//! рабочем потоке, прерываемый async-буферный запрос (манифесты) и async-открытие
//! потокового тела (сегменты до EOF). Трейт [`AdaptiveResourceTransport`] описывает
//! только эти три операции, поэтому над ним остаются ВСЕ слои политики context-а:
//! повторы, кэш завершённых VOD-ресурсов, ручные редиректы с `RedirectPolicy`,
//! секреты/заголовки/query, наблюдатель истечения endpoint-а.
//!
//! Первая реализация — обычный HTTP поверх `HttpSourceSession` ([`http_session`]).
//! Внешний транспорт (мост в браузер) подключается через
//! `AdaptiveHttpContext::with_transport`.
//!
//! Почему futures обязаны быть `Send`: прерываемый буферный запрос манифеста уходит
//! в `source_core::AbortableHttpTaskExecutor`, который принимает только
//! `Pin<Box<dyn Future + Send + 'static>>` и исполняет его на своём потоке.
//! Потоковые futures исполняются однопоточным `CurrentThreadAsyncExecutor` и сами по
//! себе `Send` не требуют, но требование оставлено единым: reqwest-futures и так
//! `Send`, а внешний транспорт получает один понятный контракт на все три операции.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use bytes::Bytes;
use source_core::{
    CancellationToken, HttpBoundedFetchRequest, HttpBoundedResponse, HttpRangeResponseMetadata,
    HttpRequestAttemptId, HttpRequestTarget, SourceResult,
};

pub(crate) mod http_session;

/// Boxed future транспорта: без async-trait и без новых зависимостей.
pub type AdaptiveTransportFuture<'operation, Output> =
    Pin<Box<dyn Future<Output = Output> + Send + 'operation>>;

/// Транспорт ровно одного bounded hop-а без собственной политики.
///
/// Контракт для реализаций:
/// - выполнить ровно один hop; по редиректам НЕ ходить, а вернуть `Redirect`
///   (решение принимает `RedirectPolicy` context-а). Транспорт, который сам ходит по
///   редиректам (браузер), просто никогда не возвращает `Redirect`;
/// - не повторять запрос: повторы делает context по `is_retryable`;
/// - соблюдать лимит тела и точную длину диапазона запроса
///   (`HttpBoundedResponse::from_external_body` делает обе проверки);
/// - сообщать сбои существующими вариантами `SourceError`
///   (`HttpBoundedFetchRequest::external_failure_context`), сохраняя повторяемость;
/// - проверить `cancellation` до побочного эффекта и вернуть `SourceError::Cancelled`.
pub trait AdaptiveResourceTransport: Send + Sync + fmt::Debug {
    /// Блокирующий буферный hop на уже выделенном рабочем потоке.
    fn fetch_buffered_hop_blocking(
        &self,
        request: HttpBoundedFetchRequest,
        cancellation: &CancellationToken,
    ) -> SourceResult<AdaptiveBufferedHop>;

    /// Буферный hop как future: drop future обязан прервать запрос физически.
    fn fetch_buffered_hop_abortable<'operation>(
        &'operation self,
        request: HttpBoundedFetchRequest,
        cancellation: &'operation CancellationToken,
    ) -> AdaptiveTransportFuture<'operation, SourceResult<AdaptiveBufferedHop>>;

    /// Открывает hop и отдаёт тело до его полной загрузки.
    fn open_streaming_hop<'operation>(
        &'operation self,
        request: HttpBoundedFetchRequest,
        cancellation: &'operation CancellationToken,
    ) -> AdaptiveTransportFuture<'operation, SourceResult<AdaptiveStreamingHop>>;
}

/// Итог одного буферного hop-а до решения политики редиректов.
#[derive(Debug)]
pub enum AdaptiveBufferedHop {
    /// Сервер потребовал следующий hop; context проверит его `RedirectPolicy`.
    Redirect {
        /// Абсолютная цель следующего hop-а.
        next_target: HttpRequestTarget,
    },
    /// Тело полностью получено и прошло проверки лимита/диапазона.
    Complete(HttpBoundedResponse),
}

/// Итог одного потокового hop-а до решения политики редиректов.
pub enum AdaptiveStreamingHop {
    /// Сервер потребовал следующий hop; context проверит его `RedirectPolicy`.
    Redirect {
        /// Абсолютная цель следующего hop-а.
        next_target: HttpRequestTarget,
    },
    /// Ответ принят, тело читается кусками.
    Body(Box<dyn AdaptiveStreamingBody>),
}

impl fmt::Debug for AdaptiveStreamingHop {
    /// Цель печатается только redacted-формой `HttpRequestTarget`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Redirect { next_target } => formatter
                .debug_struct("Redirect")
                .field("next_target", next_target)
                .finish(),
            Self::Body(body) => formatter.debug_tuple("Body").field(body).finish(),
        }
    }
}

/// Открытое потоковое тело одного ресурса с точным учётом байтов.
///
/// Контракт: `next_chunk` возвращает `Ok(None)` только на честном конце тела
/// (для диапазона — когда получено ровно столько байтов, сколько просили);
/// после конца повторный вызов снова даёт `Ok(None)`.
pub trait AdaptiveStreamingBody: Send + fmt::Debug {
    /// Читает следующий кусок; `None` означает проверенный конец тела.
    fn next_chunk<'operation>(
        &'operation mut self,
        cancellation: &'operation CancellationToken,
    ) -> AdaptiveTransportFuture<'operation, SourceResult<Option<Bytes>>>;

    /// Сколько байтов уже отдано читателю (прошло лимит).
    fn received_body_bytes(&self) -> usize;

    /// Проверенные Range-метаданные (полная длина, валидаторы), если транспорт их видел.
    ///
    /// Внешний транспорт возвращает `None`: HLS их не читает, а DASH-источник
    /// с произвольным Range (`AdaptiveRangeByteSource`) без них откажется открыться.
    fn range_metadata(&self) -> Option<&HttpRangeResponseMetadata>;

    /// Идентификатор физического запроса для диагностики, если транспорт его ведёт.
    fn request_attempt_id(&self) -> Option<HttpRequestAttemptId>;
}
