//! Выброс с полок: окно позади позиции и место у жёсткого потолка.
//!
//! Что полосе больше не нужно, зависит от того, как она подаёт:
//! - **по курсору** — сегменты, кончившиеся позади позиции и курсора подачи
//!   (то, что ещё подавать, не трогаем);
//! - **ждёт цель перемотки T** — курсора нет, а плееру нужно только окно вокруг
//!   цели: сегменты, кончившиеся до `T − назад`, и начавшиеся после `T + вперёд`
//!   (страница досылала старое время или прислала будущее до перемотки).
//!
//! У потолка (приём сегмента не помещается) выбрасывается по одному самое
//! ненужное, даже внутри окна «назад»: сначала давно показанное (у полосы,
//! ждущей цель, — всё, что кончилось до цели: плеер туда больше не вернётся без
//! новой перемотки), затем будущее за окном вперёд от цели. Сегмент, копия
//! которого ещё в трубе эпизода, не снимается: памяти это не освободит, а полка
//! потеряла бы его для перемотки назад.

use bytes::Bytes;

use super::seek::LaneSeekState;
use super::{BufferState, LaneTimeline, StreamBuffer};
use crate::error::PushedStreamError;
use crate::framing::MediaTime;
use crate::intake::ChargedSegment;
use crate::lane::LaneId;
use crate::shelf::ShelfRules;

impl StreamBuffer {
    /// Выбрасывает сегменты за окном (правила — в описании модуля).
    pub(super) fn evict_behind_window(&self, state: &mut BufferState) {
        let Some(position) = state.position() else {
            return;
        };
        let rules = self.rules;
        for lane in state.timed_lanes_mut() {
            match lane.seek {
                LaneSeekState::Following => {
                    if let Some(cursor) = lane.feed.cursor() {
                        let limit = rules.behind_limit(position).min(cursor.certain_end());
                        lane.shelf.evict_ended_by(limit);
                    }
                }
                LaneSeekState::AwaitingTarget { target, .. } => {
                    lane.shelf.evict_ended_by(rules.behind_limit(target));
                    lane.shelf
                        .evict_started_after(rules.ahead_limit_from(target));
                }
            }
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
}

/// Снимает с полок один самый ненужный сегмент; `false` — таких нет.
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
    state
        .lanes
        .iter()
        .filter(|(_, lane)| lane.timeline == LaneTimeline::Timed)
        .filter_map(|(lane_id, lane)| {
            let limit = match lane.seek {
                LaneSeekState::Following => position.min(lane.feed.cursor()?.certain_end()),
                LaneSeekState::AwaitingTarget { target, .. } => target,
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
