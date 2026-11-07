//! Сторона приёма: внешняя сторона несёт куски тел и итоги запросов.
//!
//! Типы здесь — нейтральные зеркала протокола моста (`resource_fetch_finished`):
//! crate протокола не знает, приложение переводит рамки и итоги само.

use std::sync::Arc;

use super::core::FetchCore;
use super::error::{ResourceFetchError, ResourceFetchViolation};
use crate::range_file::RangeRequestId;

/// Что очередь сделала с рамкой (куском или итогом).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceFrameVerdict {
    /// Рамка принята.
    Accepted,
    /// Рамка запоздала (запрос брошен движком, признан молчащим, принадлежит другому
    /// владельцу номеров сеанса или источник уже закрыт): отброшена молча. Протокол не
    /// умеет отменять запросы, поэтому честная страница досылает начатое.
    StaleDropped,
}

/// Чей номер (для переводчика приложения, который выбирает адресата рамки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceRequestClaim {
    /// Номер выдала эта очередь: рамку — сюда.
    Ours,
    /// Номер выдал не этот владелец (файл, прежнее поколение) или не выдавал никто.
    NotOurs,
}

/// Отказ, о котором сообщила внешняя сторона.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceFetchFailure {
    /// Сбой сети.
    Network,
    /// Сервер ответил HTTP-ошибкой.
    Http {
        /// Статус HTTP.
        status: u16,
    },
    /// Внешняя сторона отказалась качать ресурс.
    Refused,
    /// Ресурс больше `max_bytes` запроса.
    TooLarge,
}

impl ResourceFetchFailure {
    /// Ошибка, которую получит движок.
    pub(super) const fn into_fetch_error(self) -> ResourceFetchError {
        match self {
            Self::Network => ResourceFetchError::Network,
            Self::Http { status } => ResourceFetchError::Http { status },
            Self::Refused => ResourceFetchError::Refused,
            Self::TooLarge => ResourceFetchError::TooLarge,
        }
    }
}

/// Итог запроса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceFetchOutcome {
    /// Тело отдано; `size_bytes` обязан совпасть с числом пришедших байтов.
    Completed {
        /// Размер тела, байт.
        size_bytes: u64,
    },
    /// Запрос не удался.
    Failed(ResourceFetchFailure),
}

/// Приёмная сторона очереди.
///
/// Уничтожение писателя равно [`Self::seal`]: внешняя сторона ушла.
pub struct ResourceFetchWriter {
    /// Общее ядро.
    core: Arc<FetchCore>,
}

impl ResourceFetchWriter {
    /// Писатель над ядром.
    pub(super) const fn new(core: Arc<FetchCore>) -> Self {
        Self { core }
    }

    /// Чей номер `id`: эта очередь его выдала или нет.
    ///
    /// Номера у очереди и у файла по запросам байтов общие на сеанс, а рамка байтов
    /// вида 2 не говорит, кому она. Переводчик приложения спрашивает здесь и шлёт
    /// рамку владельцу: файл считает чужой номер выше своего последнего нарушением.
    #[must_use]
    pub fn claim_of(&self, id: RangeRequestId) -> ResourceRequestClaim {
        self.core.claim_of(id)
    }

    /// Принимает кусок тела запроса `id`, начинающийся с `offset` тела.
    ///
    /// Тело копируется из `payload` один раз. Размер самой рамки здесь не
    /// проверяется (это делает декодер протокола), общий объём ограничен потолком
    /// тела запроса.
    ///
    /// # Errors
    /// Нарушение договора ([`ResourceFetchViolation`]): кусок не подряд, тело больше
    /// потолка, байты после итога, номер не открыт или не выдан никем. Первое
    /// нарушение убивает очередь, следующие вызовы возвращают его же.
    pub fn accept_bytes(
        &self,
        id: RangeRequestId,
        offset: u64,
        payload: &[u8],
    ) -> Result<ResourceFrameVerdict, ResourceFetchViolation> {
        self.core.accept_bytes(id, offset, payload)
    }

    /// Принимает итог запроса `id`.
    ///
    /// # Errors
    /// Нарушение договора: размер не совпал с пришедшим, второй итог, номер не открыт
    /// или не выдан никем.
    pub fn finish(
        &self,
        id: RangeRequestId,
        outcome: ResourceFetchOutcome,
    ) -> Result<ResourceFrameVerdict, ResourceFetchViolation> {
        self.core.finish(id, outcome)
    }

    /// «Больше ничего не будет» (вкладка ушла, поколение кончилось): ждущие и
    /// будущие запросы получают [`ResourceFetchError::SourceClosed`], уже пришедшие
    /// куски и завершённые тела дочитываются до конца.
    pub fn seal(&self) {
        self.core.seal();
    }
}

impl Drop for ResourceFetchWriter {
    fn drop(&mut self) {
        self.core.seal();
    }
}
