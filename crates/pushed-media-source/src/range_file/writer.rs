//! Сторона приёма: страница несёт байты запрошенных диапазонов.
//!
//! Типы здесь — нейтральные зеркала протокола моста: crate протокола не знает,
//! приложение переводит рамки и итоги в эти типы само.

use std::sync::Arc;

use super::core::FileCore;
use super::error::{PushedFileError, PushedFileFailureClass};
use super::ids::{RangeRequest, RangeRequestId};

/// Что источник сделал с рамкой байтов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeBytesVerdict {
    /// Рамка принята.
    Accepted,
    /// Рамка запоздала (запрос отменён перемоткой, заменён повтором или остался от
    /// прежнего файла): молча отброшена. Это не атака — протокол не умеет отменять
    /// запросы, поэтому страница честно досылает уже начатое.
    StaleDropped,
}

/// Что страница сообщает о размере файла по итогу запроса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceSizeReport {
    /// Размер файла известен (из `Content-Range` или `Content-Length`).
    Known(u64),
    /// Размер неизвестен: играем вперёд без перемотки.
    Unknown,
}

/// Причина неудачи запроса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeFailure {
    /// Сеть оборвалась: повторим по бюджету.
    Network,
    /// Сервер ответил ошибкой HTTP: повторим по бюджету.
    Http,
    /// Файл на сервере изменился: окончательная ошибка.
    ResourceChanged,
    /// Диапазон вне файла (за концом — конец файла, внутри — ошибка).
    OutOfRange,
}

/// Итог запроса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeOutcome {
    /// Страница прислала всё, что смогла.
    Completed {
        /// Размер файла.
        resource_size: ResourceSizeReport,
    },
    /// Запрос не удался.
    Failed(RangeFailure),
}

/// Приёмная сторона файла: рамки байтов, итоги запросов, «больше не будет».
///
/// Уничтожение писателя равно [`Self::seal`]: страница ушла, файл доигрывает
/// полученное.
pub struct PushedFileWriter {
    /// Общее ядро.
    core: Arc<FileCore>,
}

impl PushedFileWriter {
    /// Писатель над ядром.
    pub(super) const fn new(core: Arc<FileCore>) -> Self {
        Self { core }
    }

    /// Принимает рамку байтов запроса `id`, начинающуюся с `offset`.
    ///
    /// Размер самой рамки здесь не проверяется: потолок рамки (1 МиБ) держит декодер
    /// протокола (`decode_data_frame`), а общий объём ограничен длиной запроса.
    /// Рамки отменённых и прежних запросов (в том числе запоздавшие рамки других
    /// файлов сеанса) отбрасываются как [`RangeBytesVerdict::StaleDropped`].
    ///
    /// # Errors
    /// Нарушение порядка рамок (чужой номер, смещение мимо, байты за концом запроса
    /// или файла) или прежняя ошибка файла: первая ошибка побеждает, ждущие чтения
    /// получают её. После [`Self::seal`] байты не принимаются.
    pub fn accept_range_bytes(
        &mut self,
        id: RangeRequestId,
        offset: u64,
        payload: &[u8],
    ) -> Result<RangeBytesVerdict, PushedFileError> {
        self.core.accept_range_bytes(id, offset, payload)
    }

    /// Принимает итог запроса `id`.
    ///
    /// # Errors
    /// Нарушение (чужой номер, недобор не у конца файла, смена размера) или прежняя
    /// ошибка файла; окончательный отказ источника (бюджет повторов, файл изменился).
    pub fn finish_request(
        &mut self,
        id: RangeRequestId,
        outcome: RangeOutcome,
    ) -> Result<(), PushedFileError> {
        self.core.finish_request(id, outcome)
    }

    /// «Больше байтов не будет никогда» (вкладка ушла).
    ///
    /// Ждущие чтения не висят; полученное доигрывается; чтение за пределами
    /// полученного — конец файла; новые запросы не выдаются. Открытие, которое ещё не
    /// узнало размер файла, кончается ошибкой [`PushedFileError::SealedBeforeOpen`].
    /// Повторный вызов безвреден.
    pub fn seal(&mut self) {
        self.core.seal();
    }

    /// Первая ошибка файла, если он уже мёртв.
    #[must_use]
    pub fn failure(&self) -> Option<PushedFileError> {
        self.core.failure()
    }

    /// Класс первой ошибки — что честно сказать странице.
    #[must_use]
    pub fn failure_class(&self) -> Option<PushedFileFailureClass> {
        self.core.failure().map(|error| error.failure_class())
    }
}

impl Drop for PushedFileWriter {
    /// Писатель пропал — байтов больше не будет.
    fn drop(&mut self) {
        self.core.seal();
    }
}

impl std::fmt::Debug for PushedFileWriter {
    /// Содержимое файла во внешнюю диагностику не выводится.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PushedFileWriter")
            .finish_non_exhaustive()
    }
}

/// Очередь запросов к странице: приложение забирает и шлёт `fetch_range`.
#[derive(Clone)]
pub struct PushedFileRequests {
    /// Общее ядро.
    core: Arc<FileCore>,
}

impl PushedFileRequests {
    /// Очередь над ядром.
    pub(super) const fn new(core: Arc<FileCore>) -> Self {
        Self { core }
    }

    /// Ядро для тестов, которым нужны прямые чтения (`RangeFileSource`).
    #[cfg(test)]
    pub(super) fn core_for_tests(&self) -> Arc<FileCore> {
        Arc::clone(&self.core)
    }

    /// Запросы, которые надо отправить странице, в порядке выдачи. Каждый запрос
    /// отдаётся один раз; запрос, отменённый до отправки (перемотка ушла дальше),
    /// не отдаётся вовсе.
    #[must_use]
    pub fn take_requests(&self) -> Vec<RangeRequest> {
        self.core.take_requests()
    }
}

impl std::fmt::Debug for PushedFileRequests {
    /// Содержимое очереди не выводится.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PushedFileRequests")
            .finish_non_exhaustive()
    }
}
