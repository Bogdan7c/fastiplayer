//! `fetch_resource` (протокол 1.1, HLS по номеркам): надёжная отправка просьб ресурсов
//! странице и проверка общей версии протокола.
//!
//! Правила те же, что у `fetch_range` файла (см. документацию [`super`]): сообщение
//! надёжное — при полной очереди и переподключении остаётся и уходит позже, с другими
//! не сливается (у каждого свой номер, страница отвечает на каждый), а отказ очереди
//! — явный ([`ResourceQueueOutcome`]), никогда не молчаливая потеря.
//!
//! Предел неотправленных — [`MAX_CONCURRENT_RESOURCE_FETCHES`]: очередь ресурсов
//! `pushed-media-source` сама не выдаёт больше запросов, чем мест у страницы, поэтому
//! при честной работе больше не бывает. Если всё же больше — владелец закрывает сеанс.
//!
//! **Версия.** `fetch_resource` появился в протоколе 1.1. Странице, договорившейся о
//! 1.0, его слать нельзя: её декодер отвергнет незнакомое сообщение. HLS-поколения у
//! такой страницы не бывает (сторож поколений сервера закрывает сеанс на
//! `hls_source_added`), а проверка здесь — страховка: просьба странице 1.0 не уходит
//! ([`ResourceQueueOutcome::ProtocolTooOld`]).

use browser_bridge_protocol::limits::MAX_CONCURRENT_RESOURCE_FETCHES;
use browser_bridge_protocol::{
    ByteOffset, PlayerSessionMessage, ProtocolVersion, RangeRequestId as PageRangeRequestId,
    ResourceByteRange as PageResourceByteRange, ResourceFetchLength, ResourceHandle,
};
use pushed_media_source::{ResourceExtent, ResourceRequest};

use super::PageOutbound;

/// С какой версии протокола есть `fetch_resource` (`PlayerSessionMessage::introduced_in`;
/// равенство закреплено тестом).
pub(crate) const FETCH_RESOURCE_SINCE: ProtocolVersion = ProtocolVersion::V1_1;

/// Что известно об общей версии протокола страницы текущего сеанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageProtocol {
    /// Страница ещё не подключилась (`Attached` не пришёл): версия неизвестна.
    NotAttachedYet,
    /// Общая версия подключения.
    Agreed(ProtocolVersion),
}

/// Можно ли сейчас слать странице просьбы ресурсов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceRequestPermission {
    /// Можно: страница говорит на 1.1 и новее.
    Allowed,
    /// Подождать: страница ещё не подключилась (просьбы остаются в очереди ресурсов).
    NotAttachedYet,
    /// Нельзя: общая версия ниже 1.1 (так быть не должно — сторож не пустил бы HLS).
    ProtocolTooOld,
}

/// Что вышло с просьбой ресурса, поставленной в очередь к странице.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a refused request must be handled: the HLS engine would wait for a resource nobody asked for"]
pub(crate) enum ResourceQueueOutcome {
    /// Поставлена: уйдёт странице, как только она сможет принять.
    Queued,
    /// Не поставлена: неотправленных уже столько, сколько мест у страницы.
    QueueFull,
    /// Не поставлена: смещение, длина или потолок не записываются протоколом.
    NotEncodable,
    /// Не поставлена: страница не говорит на протоколе 1.1 (или ещё не подключилась).
    ProtocolTooOld,
}

/// Что вышло с просьбами ресурсов (итоговая строка журнала: только числа).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResourceRequestCounters {
    /// Просьб `fetch_resource`, ушедших странице.
    pub(crate) sent: u64,
    /// Сумма потолков тела этих просьб (сколько байтов плеер был готов принять).
    pub(crate) bytes_limit_requested: u64,
    /// Отказов из-за переполнения неотправленных.
    pub(crate) refused_queue_full: u64,
    /// Просьб, которых протокол не записывает.
    pub(crate) unencodable: u64,
    /// Просьб, не отправленных странице с протоколом ниже 1.1.
    pub(crate) refused_old_protocol: u64,
}

impl PageOutbound {
    /// Сеанс подключён с этой общей версией протокола (`Attached`).
    pub(crate) const fn note_agreed_protocol(&mut self, version: ProtocolVersion) {
        self.page_protocol = PageProtocol::Agreed(version);
    }

    /// Можно ли сейчас слать просьбы ресурсов.
    pub(crate) const fn resource_request_permission(&self) -> ResourceRequestPermission {
        match self.page_protocol {
            PageProtocol::NotAttachedYet => ResourceRequestPermission::NotAttachedYet,
            PageProtocol::Agreed(version) if version.includes(FETCH_RESOURCE_SINCE) => {
                ResourceRequestPermission::Allowed
            }
            PageProtocol::Agreed(_) => ResourceRequestPermission::ProtocolTooOld,
        }
    }

    /// Счётчики просьб ресурсов за жизнь процесса.
    pub(crate) const fn resource_counters(&self) -> ResourceRequestCounters {
        self.resource_counters
    }

    /// Очереди ресурсов HLS нужен ресурс: ставит `fetch_resource` в отправку.
    ///
    /// Надёжно и без слияния; отказ — явный, владелец обязан его обработать.
    pub(crate) fn request_resource(&mut self, request: ResourceRequest) -> ResourceQueueOutcome {
        if self.resource_request_permission() != ResourceRequestPermission::Allowed {
            self.resource_counters.refused_old_protocol = self
                .resource_counters
                .refused_old_protocol
                .saturating_add(1);
            return ResourceQueueOutcome::ProtocolTooOld;
        }
        let Some(message) = fetch_resource_message(request) else {
            self.resource_counters.unencodable =
                self.resource_counters.unencodable.saturating_add(1);
            return ResourceQueueOutcome::NotEncodable;
        };
        let pending_resources = self
            .pending_events
            .iter()
            .filter(|pending| matches!(pending, PlayerSessionMessage::FetchResource { .. }))
            .count();
        if pending_resources >= MAX_CONCURRENT_RESOURCE_FETCHES {
            self.resource_counters.refused_queue_full =
                self.resource_counters.refused_queue_full.saturating_add(1);
            return ResourceQueueOutcome::QueueFull;
        }
        self.pending_events.push_back(message);
        ResourceQueueOutcome::Queued
    }

    /// Просьба ресурса ушла странице: в счётчики (другие сообщения не считаются).
    pub(super) fn count_sent_resource_request(&mut self, sent: &PlayerSessionMessage) {
        if let PlayerSessionMessage::FetchResource { max_bytes, .. } = sent {
            self.resource_counters.sent = self.resource_counters.sent.saturating_add(1);
            self.resource_counters.bytes_limit_requested = self
                .resource_counters
                .bytes_limit_requested
                .saturating_add(max_bytes.as_bytes());
        }
    }
}

/// Просьба очереди → сообщение протокола; `None` — протокол её не записывает.
fn fetch_resource_message(request: ResourceRequest) -> Option<PlayerSessionMessage> {
    let max_bytes = ResourceFetchLength::new(request.max_bytes.get()).ok()?;
    let byte_range = match request.extent {
        ResourceExtent::Whole => PageResourceByteRange::Whole {},
        ResourceExtent::Range(range) => {
            let length = ResourceFetchLength::new(range.length().get()).ok()?;
            // Протокол: кусок не длиннее потолка тела.
            if length > max_bytes {
                return None;
            }
            PageResourceByteRange::Slice {
                offset: ByteOffset::new(range.offset()).ok()?,
                length,
            }
        }
    };
    Some(PlayerSessionMessage::FetchResource {
        request_id: PageRangeRequestId(request.id.get()),
        resource: ResourceHandle(request.handle.get()),
        byte_range,
        max_bytes,
    })
}

#[cfg(test)]
#[path = "resource_requests_tests.rs"]
mod tests;
