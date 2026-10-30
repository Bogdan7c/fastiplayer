//! «Файл по запросам байтов»: обычный MP4 или WebM, который мы сами не качаем.
//!
//! Первый потребитель — `<video src>` в браузерном режиме (сессия 03c моста): каждый
//! диапазон байтов плеер просит у внешней стороны (у браузера — экстеншен), байты
//! приходят асинхронно кусками. Для плеера это обычный файл с перемоткой, как
//! progressive HTTP. Crate о браузере по-прежнему ничего не знает.
//!
//! # Как это устроено
//! ```text
//! страница ──рамки байтов──▶ PushedFileWriter ─┐
//!                                              ▼
//! приложение ◀─take_requests── PushedFileRequests    FileCore (замок + Condvar)
//!                                              ▲
//! демуксер ◀─ PrefetchingByteSource ◀─ RangeFileSource (ByteSource: чтение → запрос)
//! ```
//! - [`pushed_file`] создаёт три руки одного файла: писатель (страница), очередь
//!   запросов (приложение шлёт `fetch_range`) и открывальщик (владелец плеера).
//! - Открытие идёт в своём потоке: первый запрос (смещение 0, начальный кусок
//!   prefetch) → из итога узнаём размер → проверка раскладки MP4 → окно
//!   `media-prefetch` → реестр демуксеров ([`OpenedPushedFile::demuxer`]).
//! - Чтение `RangeFileSource` превращается в запрос диапазона; запросы идут
//!   **последовательно, по одному**. Перемотка (новая позиция чтения) бросает
//!   текущий запрос и просит файл с нового места: запоздавшие рамки брошенного
//!   запроса отбрасываются без сверки ([`RangeBytesVerdict::StaleDropped`]).
//! - Память: окно предзагрузки + один текущий запрос (≤ [`MAX_RANGE_REQUEST_BYTES`]).
//!
//! # Недоверие к странице
//! Рамки сверяются с выданными запросами (см. `state`): строго подряд, не за конец
//! запроса и файла, размер файла не меняется, номер, которого не выдавали, —
//! нарушение. Первая ошибка побеждает; ждущие чтения и открытие её получают.
//!
//! # Конец
//! [`PushedFileWriter::seal`] (или уничтожение писателя): полученное доигрывается,
//! чтение за его пределами — конец файла.

mod core;
mod error;
mod ids;
mod layout;
mod opener;
mod policy;
mod retry;
mod sealed_end;
mod source;
mod state;
mod writer;

#[cfg(test)]
mod core_tests;
#[cfg(test)]
mod state_tests;

use std::sync::Arc;

use self::core::FileCore;

pub use error::{PushedFileError, PushedFileFailureClass, UnsupportedFileLayout};
pub use ids::{
    RangeIdStanding, RangeRequest, RangeRequestId, RangeRequestIds, RangeRequestNotifier,
};
pub use opener::{FileOpenProgress, FileSeekability, OpenedPushedFile, PushedFileOpener};
pub use policy::{MAX_RANGE_REQUEST_BYTES, PushedFilePolicy, PushedFilePolicyError};
pub use retry::RangeRetryPolicy;
pub use writer::{
    PushedFileRequests, PushedFileWriter, RangeBytesVerdict, RangeFailure, RangeOutcome,
    ResourceSizeReport,
};

/// Создаёт файл по запросам байтов: писатель (страница), открывальщик (владелец
/// плеера) и очередь запросов (приложение шлёт их странице).
///
/// `ids` — выдаватель номеров, общий на весь сеанс: у каждого нового файла свой
/// поток запросов, но номера сквозные, и запоздалые рамки прежнего файла узнаются по
/// номеру ниже первого номера этого. `notifier` зовётся при каждом новом запросе (без
/// удержания замков).
#[must_use]
pub fn pushed_file(
    policy: PushedFilePolicy,
    ids: RangeRequestIds,
    notifier: Arc<dyn RangeRequestNotifier>,
) -> (PushedFileWriter, PushedFileOpener, PushedFileRequests) {
    let core = FileCore::new(
        ids,
        notifier,
        policy.max_request_bytes().get(),
        policy.retry(),
    );
    (
        PushedFileWriter::new(Arc::clone(&core)),
        PushedFileOpener::new(Arc::clone(&core), policy),
        PushedFileRequests::new(core),
    )
}
