//! Сторона движка: «дай ресурс» и чтение тела кусками.

use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use source_core::CancellationToken;

use super::core::FetchCore;
use super::error::ResourceFetchError;
use super::request::ResourceFetchSpec;
use crate::range_file::RangeRequestId;

/// Сторона движка (HLS и подобные): просит ресурсы и читает тела.
///
/// Клон делит одну очередь: движок может качать из нескольких потоков сразу.
#[derive(Clone)]
pub struct ResourceFetchClient {
    /// Общее ядро.
    core: Arc<FetchCore>,
}

impl ResourceFetchClient {
    /// Клиент над ядром.
    pub(super) const fn new(core: Arc<FetchCore>) -> Self {
        Self { core }
    }

    /// Ставит запрос в очередь отправки.
    ///
    /// Если в полёте уже столько запросов, сколько разрешает политика, **ждёт**
    /// свободного места (по очереди прихода), а не отказывает: так очередь отправки
    /// приложения никогда не переполнится. Ожидание прерывают `cancellation`
    /// (≤ 10 мс), закрытие источника и нарушение страницы.
    ///
    /// # Errors
    /// [`ResourceFetchError::Cancelled`], [`ResourceFetchError::SourceClosed`],
    /// [`ResourceFetchError::PageViolation`], [`ResourceFetchError::ExceedsResourceCeiling`]
    /// (`max_bytes` больше потолка политики), [`ResourceFetchError::RequestIdsExhausted`].
    pub fn start(
        &self,
        spec: ResourceFetchSpec,
        cancellation: &CancellationToken,
    ) -> Result<ResourceFetch, ResourceFetchError> {
        let id = self.core.start(spec, cancellation)?;
        Ok(ResourceFetch {
            core: Arc::clone(&self.core),
            id,
        })
    }
}

/// Один запрос ресурса, которым владеет движок.
///
/// Тело читается кусками в порядке прихода; после последнего куска — `Ok(None)`
/// (тело целиком) или ошибка итога. Уничтожение хэндла — отказ от запроса: не
/// отправленный запрос исчезает из исходящих, отправленный досылается страницей
/// впустую, его поздние рамки отбрасываются молча.
pub struct ResourceFetch {
    /// Общее ядро.
    core: Arc<FetchCore>,
    /// Номер запроса.
    id: RangeRequestId,
}

impl ResourceFetch {
    /// Номер запроса (для журнала).
    #[must_use]
    pub const fn id(&self) -> RangeRequestId {
        self.id
    }

    /// Сколько байтов тела пришло от внешней стороны (включая ещё не забранные).
    #[must_use]
    pub fn received_bytes(&self) -> u64 {
        self.core.received(self.id)
    }

    /// Следующий кусок, асинхронная форма.
    ///
    /// `Pending` запоминает будильник `context`; его будят новый кусок, итог,
    /// закрытие источника, нарушение страницы и сторож молчания.
    ///
    /// # Errors
    /// Ошибка итога (сеть, HTTP, отказ, слишком большой, короткий диапазон), молчание
    /// страницы, закрытие источника, нарушение страницы.
    pub fn poll_next_chunk(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<Option<Bytes>, ResourceFetchError>> {
        self.core.poll_chunk(self.id, context)
    }

    /// Следующий кусок, блокирующая форма (ожидание прерывает `cancellation`).
    ///
    /// # Errors
    /// Как у [`Self::poll_next_chunk`], плюс [`ResourceFetchError::Cancelled`].
    pub fn next_chunk_blocking(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<Bytes>, ResourceFetchError> {
        self.core.next_chunk_blocking(self.id, cancellation)
    }
}

impl Drop for ResourceFetch {
    fn drop(&mut self) {
        self.core.abandon(self.id);
    }
}

impl std::fmt::Debug for ResourceFetch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResourceFetch")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
