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
//! **Предохранитель памяти** — страховка, а не обычный путь. Когда удерживаемые
//! байты достигают порога «полон» (доля жёсткого потолка), выбрасывается по
//! одному сегменту, пока байты не станут ниже порога:
//! 1. самое старое СЫГРАННОЕ прошлое, но не ближе окна «назад» к позиции
//!    ([`crate::BufferWindow::behind`] — гарантированный минимум прошлого): у
//!    полосы по курсору — кончившееся не позже `min(позиция − назад, курсор)`, у
//!    полосы, ждущей цель перемотки T, — не позже `T − назад`;
//! 2. затем самое далёкое БУДУЩЕЕ — сегменты, начавшиеся позже окна «вперёд» от
//!    позиции (у ждущей полосы — от цели) и не продолжающие непрерывный кусок у
//!    неё: это время прежних мест (перемотка назад далеко от уже скачанного).
//!
//! Без предохранителя удерживаемое прошлое объявило бы поток «полным» по байтам
//! (`position::buffer_status`), и честная страница остановилась бы при пустом
//! запасе вперёд — плеер встал бы навсегда. Шаг 2 нужен по той же причине:
//! старое будущее после перемотки назад тоже держит байты, а «полон» при пустом
//! запасе у цели перемотки — это ожидание до срока перемотки. Каждое такое
//! выселение считается, а предупреждение в журнал пишется один раз на поток
//! (только числа): при постоянной нехватке оно повторялось бы на каждом сегменте.
//!
//! **Жёсткий потолок** (приём сегмента не помещается) — выбрасывается по одному
//! самое ненужное, даже внутри окна «назад»: сначала давно показанное (у полосы,
//! ждущей цель, — всё, что кончилось до цели), затем будущее за окном вперёд от
//! цели у ждущих полос. Не помогло — страница игнорирует «полон», это ошибка.
//!
//! Сегмент, копия которого ещё в трубе эпизода, не снимается никогда: памяти это
//! не освободит, а полка потеряла бы его для перемотки назад.

use bytes::Bytes;

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
                "Pushed media buffer reached its memory threshold; it now evicts old segments the page may still hold"
            );
        }
    }
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

    /// Докуда предохранитель памяти может выселять прошлое: то же «сыграно», но
    /// не ближе окна «назад» к позиции (у ждущей полосы — к цели).
    fn guard_releasable_until(&self, position: MediaTime, rules: &ShelfRules) -> PlayedSoFar {
        let limit = match self.seek {
            LaneSeekState::Following => self
                .feed
                .cursor()
                .map(|cursor| rules.behind_limit(position).min(cursor.certain_end())),
            LaneSeekState::AwaitingTarget { target, .. } => Some(rules.behind_limit(target)),
        };
        limit.map_or(PlayedSoFar::Nothing, PlayedSoFar::Until)
    }

    /// Начиная с какого момента будущее полосы — «далёкое» для предохранителя:
    /// позже окна «вперёд» от места плеера (позиция или цель перемотки) и позже
    /// конца непрерывного куска у этого места (его продолжение ещё подаётся).
    fn far_future_after(&self, position: MediaTime, rules: &ShelfRules) -> MediaTime {
        let anchor = match self.seek {
            LaneSeekState::Following => position,
            LaneSeekState::AwaitingTarget { target, .. } => target,
        };
        let window_end = rules.ahead_limit_from(anchor);
        self.shelf
            .covered_run_end(anchor, rules)
            .map_or(window_end, |run_end| run_end.max(window_end))
    }
}

impl StreamBuffer {
    /// Предохранитель памяти (правило — в описании модуля): у порога «полон»
    /// выселяет старое прошлое за окном «назад», затем далёкое будущее, пока
    /// удерживаемые байты не станут ниже порога или выселять станет нечего.
    pub(super) fn relieve_memory_pressure(&self, state: &mut BufferState) {
        let rules = self.rules;
        while self.ledger.held_bytes() >= self.full_at_held_bytes {
            let Some(position) = state.position() else {
                return;
            };
            let candidate = oldest_played_beyond_behind(state, position, &rules)
                .or_else(|| farthest_future(state, position, &rules));
            let Some((lane_id, index)) = candidate else {
                return;
            };
            let Some(evicted) = state
                .lanes
                .get_mut(&lane_id)
                .and_then(|lane| lane.shelf.remove(index))
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
                    if !evict_one_unneeded(state, &self.rules) {
                        return Err(PushedStreamError::IntakeLimitExceeded {
                            limit: exceeded.limit,
                        });
                    }
                }
            }
        }
    }

    /// Счёт предохранителя памяти — для тестов.
    #[cfg(test)]
    pub(crate) fn memory_guard_tally(&self) -> MemoryGuardTally {
        self.lock().memory_guard
    }
}

/// Самый давний сыгранный сегмент за окном «назад» среди полос (шаг 1
/// предохранителя), чьи байты держит только полка.
fn oldest_played_beyond_behind(
    state: &BufferState,
    position: MediaTime,
    rules: &ShelfRules,
) -> Option<(LaneId, usize)> {
    oldest_unshared_among(state, |lane| lane.guard_releasable_until(position, rules))
}

/// Самый далёкий сегмент будущего среди полос (шаг 2 предохранителя).
fn farthest_future(
    state: &BufferState,
    position: MediaTime,
    rules: &ShelfRules,
) -> Option<(LaneId, usize)> {
    state
        .lanes
        .iter()
        .filter(|(_, lane)| lane.timeline == LaneTimeline::Timed)
        .filter_map(|(lane_id, lane)| {
            lane.shelf
                .latest_unshared_started_after(lane.far_future_after(position, rules))
                .map(|(index, start)| (*lane_id, index, start))
        })
        .max_by_key(|(_, _, start)| *start)
        .map(|(lane_id, index, _)| (lane_id, index))
}

/// Снимает с полок один самый ненужный сегмент у жёсткого потолка; `false` —
/// таких нет.
fn evict_one_unneeded(state: &mut BufferState, rules: &ShelfRules) -> bool {
    let Some(position) = state.position() else {
        return false;
    };
    let candidate = oldest_played(state, position).or_else(|| farthest_ahead(state, rules));
    let Some((lane_id, index)) = candidate else {
        return false;
    };
    state
        .lanes
        .get_mut(&lane_id)
        .and_then(|lane| lane.shelf.remove(index))
        .is_some()
}

/// Самый давний показанный сегмент среди полос (только полка держит его байты).
///
/// Показанный — кончился не позже позиции плеера и курсора подачи своей полосы;
/// у полосы, ждущей цель перемотки, — не позже цели.
fn oldest_played(state: &BufferState, position: MediaTime) -> Option<(LaneId, usize)> {
    oldest_unshared_among(state, |lane| lane.played_so_far(position))
}

/// Самый давний сегмент среди полос с временем, кончившийся не позже своей
/// границы `limit_of(полоса)`, чьи байты держит только полка.
fn oldest_unshared_among(
    state: &BufferState,
    limit_of: impl Fn(&BufferedLane) -> PlayedSoFar,
) -> Option<(LaneId, usize)> {
    state
        .lanes
        .iter()
        .filter(|(_, lane)| lane.timeline == LaneTimeline::Timed)
        .filter_map(|(lane_id, lane)| {
            let PlayedSoFar::Until(limit) = limit_of(lane) else {
                return None;
            };
            lane.shelf
                .oldest_unshared_ended_by(limit)
                .map(|(index, end)| (*lane_id, index, end))
        })
        .min_by_key(|(_, _, end)| *end)
        .map(|(lane_id, index, _)| (lane_id, index))
}

/// Самый далёкий сегмент за окном вперёд от цели у полос, ждущих цель перемотки.
fn farthest_ahead(state: &BufferState, rules: &ShelfRules) -> Option<(LaneId, usize)> {
    state
        .lanes
        .iter()
        .filter(|(_, lane)| lane.timeline == LaneTimeline::Timed)
        .filter_map(|(lane_id, lane)| match lane.seek {
            LaneSeekState::Following => None,
            LaneSeekState::AwaitingTarget { target, .. } => lane
                .shelf
                .latest_unshared_started_after(rules.ahead_limit_from(target))
                .map(|(index, start)| (*lane_id, index, start)),
        })
        .max_by_key(|(_, _, start)| *start)
        .map(|(lane_id, index, _)| (lane_id, index))
}
