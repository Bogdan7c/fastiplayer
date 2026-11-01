//! Конец того, что можно сыграть всеми дорожками вместе, — отсечка обрыва
//! скачивания (модуль [`crate::download_cut`]).
//!
//! Правило: от позиции плеера (у ждущей перемотки — её цели: перемотка сама ставит
//! позицию в цель) у каждой полосы с метками времени берётся конец непрерывного
//! куска полки, покрывающего позицию, — то же правило, что «буферизовано» для
//! страницы. Отсечка — самый ранний из этих концов: дальше него хотя бы одной
//! дорожки нет. Дыра посреди полосы обрывает её кусок, поэтому отсечка встаёт на
//! дыре, даже если за дырой у полосы ещё есть данные (их не подать: подача идёт
//! только подряд). Полоса, у которой у позиции данных нет вовсе, даёт отсечку в
//! самой позиции — играть дальше нечем.
//!
//! Полоса без меток времени (MPEG-TS) делает конец неизвестным: её время полка не
//! знает, и такой поток доигрывается без отсечки, как при честном конце.

use super::{LaneTimeline, StreamBuffer};
use crate::download_cut::PlayableEnd;

impl StreamBuffer {
    /// Отсечка от текущей позиции плеера (правило — в описании модуля).
    ///
    /// Ничего не меняет: только читает полки и позицию.
    pub(crate) fn playable_end(&self) -> PlayableEnd {
        let mut state = self.lock();
        let any_untimed = state
            .lanes
            .values()
            .any(|lane| lane.timeline == LaneTimeline::Untimed);
        if any_untimed || state.lanes.is_empty() {
            return PlayableEnd::Unknown;
        }
        let Some(anchor) = state.position() else {
            return PlayableEnd::Unknown;
        };
        let rules = self.rules;
        let earliest_end = state
            .timed_lanes_mut()
            .map(|lane| {
                lane.shelf
                    .covered_run_end(anchor, &rules)
                    // Нет куска у позиции — у этой дорожки играть дальше нечего.
                    .unwrap_or(anchor)
            })
            .min()
            // Полосы есть (проверено выше), так что запасной вариант не нужен на деле.
            .unwrap_or(anchor);
        PlayableEnd::at_shelf_time(earliest_end)
    }
}
