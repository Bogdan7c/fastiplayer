//! Выброс с полок: предохранитель памяти и место у жёсткого потолка.
//!
//! Правило полки — «помним, пока помнит страница» (решение владельца 31.10.2026):
//! всё, что страница считает скачанным, есть и у источника, потому что повторно
//! она это не пришлёт (её настоящий MSE `SourceBuffer` этот кусок ещё держит), и
//! перемотка туда обязана садиться с полки. Поэтому в обычной работе с полки
//! ничего не выбрасывается по времени: прошлое уходит, когда страница сама
//! стёрла его у себя (модуль `page_removal`). Держать больше, чем страница,
//! безопасно (браузер чистит свой буфер и молча, без `remove()`), меньше — нельзя.
//!
//! **Предохранитель памяти — «зеркало браузера»** (решение владельца 1.11.2026,
//! по замеру родного YouTube 4K60). Браузер при нехватке памяти выкидывает
//! данные КАЖДОЙ дорожки отдельно, по её лимиту, и в одном порядке: сначала
//! прошлое (с самого раннего, кроме куска у позиции), потом самое далёкое
//! будущее. Звук лёгкий и лежит у него минутами, видео 4K — меньше минуты. Если
//! полка держит по каждой полосе не меньше браузера и выкидывает в том же
//! порядке, то у неё есть всё, что есть у браузера, — и перемотка в любое
//! пройденное место доезжает: чего нет у нас, того нет и у браузера, и страница
//! это перекачает.
//!
//! Когда удерживаемые байты достигают порога «полон», выбрасывается по одному
//! сегменту, пока байты не станут ниже порога:
//! - только у полосы, перевалившей свою норму ([`lane_norm`](super::lane_norm):
//!   полоса без видео — резерв, полосам с видео — остальное); среди таких — у
//!   той, что перевалила сильнее;
//! - в полосе — порядок браузера: самое раннее СЫГРАННОЕ (у полосы по курсору —
//!   кончившееся не позже `min(позиция, курсор)`, у полосы, ждущей цель
//!   перемотки T, — не позже T), затем самое далёкое будущее после непрерывного
//!   куска у места плеера (позиция или цель). Сам этот кусок — то, что сейчас
//!   играет, — не выбрасывается никогда.
//!
//! Выкинуть честно нечего (все полосы в своих нормах или у них остался только
//! играющий кусок) — ничего не теряем: поток остаётся «полным»
//! (`position::buffer_status`), и честная страница ждёт.
//!
//! **Жёсткий потолок** (приём сегмента не помещается) — тот же порядок, но уже
//! у любой полосы, начиная с самой перевалившей норму. Не помогло — страница
//! игнорирует «полон», это ошибка потока.
//!
//! Сегмент, копия которого ещё в трубе эпизода, не снимается никогда: памяти это
//! не освободит, а полка потеряла бы его для перемотки назад. Каждое выселение
//! предохранителем считается, а предупреждение в журнал пишется один раз на
//! поток (только числа).

use bytes::Bytes;

use super::lane_norm::LaneNorms;
use super::seek::LaneSeekState;
use super::{BufferState, BufferedLane, LaneTimeline, StreamBuffer};
use crate::error::PushedStreamError;
use crate::framing::MediaTime;
use crate::intake::ChargedSegment;
use crate::lane::LaneId;
use crate::shelf::{PlayedSoFar, ShelfRules};

/// Счёт выселений предохранителем памяти за жизнь потока.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MemoryGuardTally {
    /// Сколько сегментов выселено.
    pub(crate) evicted_segments: u64,
    /// Сколько байтов они держали.
    pub(crate) evicted_bytes: u64,
    /// Сколько предупреждений записано в журнал (не больше одного на поток).
    pub(crate) warnings_logged: u64,
}

impl MemoryGuardTally {
    /// Отмечает выселение; первое за поток — предупреждение в журнал.
    fn note_eviction(&mut self, segment_bytes: usize, held_bytes: usize, full_at_bytes: usize) {
        self.evicted_segments += 1;
        self.evicted_bytes = self
            .evicted_bytes
            .saturating_add(u64::try_from(segment_bytes).unwrap_or(u64::MAX));
        tracing::debug!(
            segment_bytes,
            held_bytes,
            full_at_bytes,
            evicted_segments = self.evicted_segments,
            "Pushed media buffer evicted a segment under memory pressure"
        );
        if self.warnings_logged == 0 {
            self.warnings_logged += 1;
            // Только числа: ни домена, ни заголовка, ни байтов страницы (инвариант 7).
            tracing::warn!(
                held_bytes,
                full_at_bytes,
                segment_bytes,
                "Pushed media buffer reached its memory threshold; it now evicts like the browser does, lane by lane"
            );
        }
    }
}

/// Насколько остро нужна память.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryPressure {
    /// Порог «полон»: выкидываем только у полос сверх нормы.
    AtThreshold,
    /// Сегмент не помещается в жёсткий потолок: выкидываем у любой полосы.
    AtHardCeiling,
}

impl BufferedLane {
    /// Докуда полоса сыграла: у полосы по курсору — не дальше позиции плеера и
    /// курсора подачи (то, что ещё подавать, не сыграно); у полосы, ждущей цель
    /// перемотки, — до цели (плеер встал на цель, раньше неё он не вернётся без
    /// новой перемотки; сегмент с целью кончается позже неё и под правило не
    /// попадает).
    pub(super) fn played_so_far(&self, position: MediaTime) -> PlayedSoFar {
        let limit = match self.seek {
            LaneSeekState::Following => self
                .feed
                .cursor()
                .map(|cursor| position.min(cursor.certain_end())),
            LaneSeekState::AwaitingTarget { target, .. } => Some(target),
        };
        limit.map_or(PlayedSoFar::Nothing, PlayedSoFar::Until)
    }

    /// Место плеера для этой полосы: позиция или цель перемотки, которую полоса ждёт.
    const fn player_place(&self, position: MediaTime) -> MediaTime {
        match self.seek {
            LaneSeekState::Following => position,
            LaneSeekState::AwaitingTarget { target, .. } => target,
        }
    }

    /// Какой сегмент полосы выкинуть первым — в порядке браузера: самое раннее
    /// сыгранное, затем самое далёкое будущее после играющего куска. `None` —
    /// осталось только играющее (или то, что ещё дочитывает труба).
    fn first_to_evict(&self, position: MediaTime, rules: &ShelfRules) -> Option<usize> {
        if let PlayedSoFar::Until(limit) = self.played_so_far(position)
            && let Some((index, _)) = self.shelf.oldest_unshared_ended_by(limit)
        {
            return Some(index);
        }
        let place = self.player_place(position);
        let playing_until = self
            .shelf
            .covered_run_end(place, rules)
            .map_or(place, |run_end| run_end.max(place));
        self.shelf
            .latest_unshared_started_after(playing_until)
            .map(|(index, _)| index)
    }
}

impl StreamBuffer {
    /// Предохранитель памяти (правило — в описании модуля): у порога «полон»
    /// выселяет у полос сверх нормы в порядке браузера, пока удерживаемые байты
    /// не станут ниже порога или честно выселять станет нечего.
    pub(super) fn relieve_memory_pressure(&self, state: &mut BufferState) {
        while self.ledger.held_bytes() >= self.full_at_held_bytes {
            let Some((lane_id, index)) = self.choose_eviction(state, MemoryPressure::AtThreshold)
            else {
                return;
            };
            let Some(evicted) = state
                .lanes
                .get_mut(&lane_id)
                .and_then(|lane| lane.shelf.evict(index))
            else {
                return;
            };
            let segment_bytes = evicted.media_len();
            // Сегмент не в трубе: учёт уменьшится ровно на его байты.
            drop(evicted);
            state.memory_guard.note_eviction(
                segment_bytes,
                self.ledger.held_bytes(),
                self.full_at_held_bytes,
            );
        }
    }

    /// Зачитывает байты; у потолка сначала выбрасывает ненужное (по одному
    /// сегменту, пока не поместится или выбрасывать станет нечего).
    pub(super) fn charge_locked(
        &self,
        state: &mut BufferState,
        bytes: Bytes,
    ) -> Result<ChargedSegment, PushedStreamError> {
        loop {
            match ChargedSegment::charge(&self.ledger, bytes.clone()) {
                Ok(charged) => return Ok(charged),
                Err(exceeded) => {
                    // Нечего выбросить — страница игнорирует «полон»: это ошибка.
                    // Каждый шаг снимает сегмент с полки, поэтому цикл конечен.
                    let removed = self
                        .choose_eviction(state, MemoryPressure::AtHardCeiling)
                        .and_then(|(lane_id, index)| {
                            state.lanes.get_mut(&lane_id)?.shelf.evict(index)
                        });
                    if removed.is_none() {
                        return Err(PushedStreamError::IntakeLimitExceeded {
                            limit: exceeded.limit,
                        });
                    }
                }
            }
        }
    }

    /// Какой сегмент выкинуть: полосы по убыванию превышения нормы (у порога —
    /// только сверх нормы), в полосе — первый в порядке браузера.
    fn choose_eviction(
        &self,
        state: &BufferState,
        pressure: MemoryPressure,
    ) -> Option<(LaneId, usize)> {
        let position = state.position()?;
        let norms = LaneNorms::compute(
            state
                .lanes
                .iter()
                .filter(|(_, lane)| lane.timeline == LaneTimeline::Timed)
                .map(|(lane_id, lane)| (*lane_id, lane.content)),
            self.full_at_held_bytes,
            self.lane_without_video_reserve_bytes,
        );
        let mut candidates: Vec<(LaneId, &BufferedLane, i128)> = state
            .lanes
            .iter()
            .filter(|(_, lane)| lane.timeline == LaneTimeline::Timed)
            .map(|(lane_id, lane)| {
                let excess = norms.excess_of(*lane_id, lane.shelf.held_media_bytes());
                (*lane_id, lane, excess)
            })
            .filter(|(_, _, excess)| pressure == MemoryPressure::AtHardCeiling || *excess > 0)
            .collect();
        // Сильнее перевалившая норму — первой; при равенстве — по номеру полосы
        // (детерминированно).
        candidates.sort_by(|left, right| right.2.cmp(&left.2).then(left.0.cmp(&right.0)));
        candidates.into_iter().find_map(|(lane_id, lane, _)| {
            lane.first_to_evict(position, &self.rules)
                .map(|index| (lane_id, index))
        })
    }

    /// Счёт предохранителя памяти — для тестов.
    #[cfg(test)]
    pub(crate) fn memory_guard_tally(&self) -> MemoryGuardTally {
        self.lock().memory_guard
    }
}
