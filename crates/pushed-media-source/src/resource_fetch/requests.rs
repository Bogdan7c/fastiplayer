//! Сторона приложения: забрать запросы для отправки и снять учёт для журнала.

use std::sync::Arc;

use super::core::FetchCore;
use super::request::ResourceRequest;
use super::stats::ResourceFetchStats;

/// Очередь отправки: приложение забирает запросы и шлёт их внешней стороне.
#[derive(Clone)]
pub struct ResourceFetchRequests {
    /// Общее ядро.
    core: Arc<FetchCore>,
}

impl ResourceFetchRequests {
    /// Очередь над ядром.
    pub(super) const fn new(core: Arc<FetchCore>) -> Self {
        Self { core }
    }

    /// Новые запросы в порядке выдачи. С этого мгновения у запроса идёт срок
    /// молчания: внешняя сторона должна начать отвечать.
    ///
    /// Брошенные движком до отправки сюда не попадают. После закрытия источника или
    /// нарушения страницы — пусто.
    #[must_use]
    pub fn take_requests(&self) -> Vec<ResourceRequest> {
        self.core.take_requests()
    }

    /// Снимок учёта для журнала.
    #[must_use]
    pub fn stats(&self) -> ResourceFetchStats {
        self.core.stats()
    }

    /// Оборвала ли печать ([`ResourceFetchWriter::seal`](super::ResourceFetchWriter::seal))
    /// хотя бы одно чтение движка.
    ///
    /// Нужна обёртке демуксера, которая после ухода вкладки доигрывает полученное:
    /// ошибка демуксера, вызванная печатью, — это конец полученного, а не порча
    /// (как `read_cut_by_seal` у файла по запросам байтов). Сама печать ничего не
    /// значит: если всё нужное движку пришло до неё, ответ — [`ResourceSealCut::NoReadCut`].
    #[must_use]
    pub fn seal_cut(&self) -> ResourceSealCut {
        self.core.seal_cut()
    }
}

/// Оборвала ли печать чтение движка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceSealCut {
    /// Ни одно чтение не получило «источник закрыт» из-за печати: печати не было,
    /// или всё, что движок читал, пришло до неё.
    NoReadCut,
    /// После печати движок хотел ещё (новый запрос или недочитанное тело) и получил
    /// [`ResourceFetchError::SourceClosed`](super::ResourceFetchError::SourceClosed):
    /// полученное кончилось раньше настоящего конца медиа.
    ReadCutBySeal,
}
