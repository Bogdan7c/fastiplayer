//! Очередь запросов ресурсов: «дай ресурс по номерку» у внешней стороны.
//!
//! Первый потребитель — нативный HLS браузерного режима (сессия 10a1 моста): движок
//! HLS читает плейлист и просит каждый ресурс (плейлист, сегмент, init, ключ) по
//! номерку, который дала внешняя сторона. Crate о браузере не знает: «номерок» —
//! [`ExternalResourceHandle`], запросы и итоги — нейтральные зеркала протокола.
//!
//! # Как это устроено
//! ```text
//! движок ──start(spec)──▶ ResourceFetchClient ─┐  (ждёт места, если в полёте max)
//!   ▲                                          ▼
//!   └─куски/итог── ResourceFetch ◀──── FetchCore (замок + Condvar + сторож молчания)
//!                                              ▲            │
//! страница ──куски, итоги──▶ ResourceFetchWriter ┘            ▼
//! приложение ◀──take_requests── ResourceFetchRequests (+ будильник без замков)
//! ```
//! - [`resource_fetch_queue`] создаёт три руки одного ядра.
//! - Номера запросов — общий счётчик сеанса ([`RangeRequestIds`]), тот же, что у
//!   файла по запросам байтов: приложение даёт клон одного счётчика, а переводчик
//!   рамок выбирает адресата по [`ResourceFetchWriter::claim_of`].
//! - Тело приходит кусками строго подряд; потребитель получает те же `Bytes`, что
//!   скопированы из рамки (одна копия).
//!
//! # Недоверие к внешней стороне
//! Куски подряд с нуля, не больше `max_bytes` (и длины диапазона), размер в итоге
//! равен пришедшему, один итог, без байтов после итога, номер выдан. Нарушение —
//! первое и общее: очередь мертва, все ждущие получают [`ResourceFetchError::PageViolation`].
//! Запоздавшие рамки брошенных, молчавших и чужих (файл, прежнее поколение) запросов
//! — [`ResourceFrameVerdict::StaleDropped`] молча: протокол отменять не умеет.
//!
//! # Место в полёте
//! Занято с выдачи номера до итога внешней стороны. Брошенный движком **отправленный**
//! запрос держит место до своего итога или до молчания дольше срока: страница всё
//! ещё качает его, и иначе у неё одновременно оказалось бы больше запросов, чем
//! разрешает договор. Неотправленный брошенный — освобождает сразу.
//!
//! # Память
//! Тело одного запроса не больше его `max_bytes` (а `max_bytes` — не больше потолка
//! политики). Глобального обратного давления в протоколе для рамок байтов нет:
//! внешняя сторона шлёт с той скоростью, с какой качает, поэтому в памяти может
//! лежать до «мест в полёте × потолок» плюс завершённые, но ещё не дочитанные тела,
//! которые держат живые хэндлы движка. Ограничивает их сам движок, читая тела.

mod client;
mod core;
mod error;
mod ledger;
mod policy;
mod record;
mod request;
mod requests;
mod state;
mod stats;
mod writer;

#[cfg(test)]
mod core_tests;
#[cfg(test)]
mod state_tests;

use std::sync::Arc;

use self::core::FetchCore;
use crate::range_file::{RangeRequestIds, RangeRequestNotifier};

pub use client::{ResourceFetch, ResourceFetchClient};
pub use error::{ResourceFetchError, ResourceFetchFailureClass, ResourceFetchViolation};
pub use policy::{ResourceFetchPolicy, ResourceFetchPolicyError};
pub use request::{
    ExternalResourceHandle, ResourceByteRange, ResourceByteRangeOverflow, ResourceExtent,
    ResourceFetchSpec, ResourceRequest,
};
pub use requests::{ResourceFetchRequests, ResourceSealCut};
pub use stats::{ResourceFailureCounts, ResourceFetchStats};
pub use writer::{
    ResourceFetchFailure, ResourceFetchOutcome, ResourceFetchWriter, ResourceFrameVerdict,
    ResourceRequestClaim,
};

/// Создаёт очередь запросов ресурсов: писатель (внешняя сторона несёт байты),
/// клиент (движок просит ресурсы) и очередь отправки (приложение шлёт запросы).
///
/// `ids` — выдаватель номеров, общий на весь сеанс (тот же, что у
/// [`pushed_file`](crate::pushed_file)). `notifier` зовётся при каждом новом запросе
/// без удержания замков: реализация вправе сразу вызвать
/// [`ResourceFetchRequests::take_requests`].
#[must_use]
pub fn resource_fetch_queue(
    policy: ResourceFetchPolicy,
    ids: RangeRequestIds,
    notifier: Arc<dyn RangeRequestNotifier>,
) -> (
    ResourceFetchWriter,
    ResourceFetchClient,
    ResourceFetchRequests,
) {
    let core = FetchCore::new(policy, ids, notifier);
    (
        ResourceFetchWriter::new(Arc::clone(&core)),
        ResourceFetchClient::new(Arc::clone(&core)),
        ResourceFetchRequests::new(core),
    )
}
