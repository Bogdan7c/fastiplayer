//! Что страница принесла для данных, которых ждёт плеер (решение владельца 151).
//!
//! Перемотка браузерного режима ждёт страницу без предела, пока та несёт данные, и
//! честно сдаётся, если за `player.seek.live_source_stall_timeout_ms` (60 с) не пришло
//! ни одного нового байта для ожидаемого места. Плеер видит только счётчик
//! ([`media_core::LiveProducerProgress`]), а что в нём считать — решает источник.
//!
//! **Определение.** Счётчик растёт на размер каждого куска (`append`), пришедшего на
//! полосу, которой плееру больше нечего подать:
//! - полоса ждёт цель перемотки (`AwaitingTarget`) — любой её байт может оказаться
//!   сегментом с целью (YouTube несёт 4K-сегмент секундами, кусок за куском);
//! - или полоса подала плееру всё, что у неё лежит подряд от курсора (на полке нет
//!   сегмента, продолжающего поданное) — её новый байт и есть продолжение, которого
//!   плеер ждёт (стенд 04e: звук после цели).
//!
//! Куски полосы, у которой продолжение уже лежит на полке и просто ещё не подано,
//! не считаются: плеер ждёт не их. Считаются байты куска целиком, ещё до того, как
//! сегмент собран: иначе медленная страница с большим сегментом выглядела бы молчащей.
//! Счётчик только растёт (насыщается, а не переполняется).

use super::{BufferedLane, LaneSeekState, StreamBuffer};
use crate::lane::LaneId;
use crate::shelf::ShelfRules;

impl StreamBuffer {
    /// Страница принесла `bytes` байтов полосе `lane` (до сборки сегмента).
    pub(crate) fn note_incoming_bytes(&self, lane: LaneId, bytes: usize) {
        let rules = self.rules;
        let mut state = self.lock();
        let awaits = state
            .lanes
            .get(&lane)
            .is_some_and(|buffered| buffered.awaits_data(&rules));
        if awaits {
            let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
            state.awaited_delivery_bytes = state.awaited_delivery_bytes.saturating_add(bytes);
        }
    }

    /// Сколько байтов страница принесла для ожидаемых данных за жизнь потока.
    pub(crate) fn awaited_delivery_bytes(&self) -> u64 {
        self.lock().awaited_delivery_bytes
    }
}

impl BufferedLane {
    /// Нечего больше подать плееру: ждёт цель перемотки или подала всё подряд лежащее.
    fn awaits_data(&self, rules: &ShelfRules) -> bool {
        matches!(self.seek, LaneSeekState::AwaitingTarget { .. })
            || self.shelf.next_to_feed(self.feed.cursor(), rules).is_none()
    }
}
