//! Стирание страницей на стороне буфера: какая полоса, что она сыграла, что
//! отпустить (правило — `crate::page_removal` и `crate::shelf::removal`).
//!
//! Стирание приходит от писателя в порядке провода вместе с байтами: если
//! страница стёрла кусок и тут же прислала его заново, повтор ляжет на уже
//! освобождённое место, а не накроет старую копию.

use super::{LaneTimeline, StreamBuffer};
use crate::error::PushedStreamError;
use crate::lane::LaneId;
use crate::page_removal::{PageRemovalOutcome, PageRemovedRange};
use crate::shelf::{PlayedSoFar, ShelfRemoval};

impl StreamBuffer {
    /// Страница стёрла у себя `range` полосы `lane`: с полки уходят только
    /// сыгранные сегменты, целиком лежащие в диапазоне.
    pub(crate) fn note_page_removed(
        &self,
        lane: LaneId,
        range: PageRemovedRange,
    ) -> Result<PageRemovalOutcome, PushedStreamError> {
        let rules = self.rules;
        let released = {
            let mut state = self.lock();
            let position = state.position();
            let buffered = state.lane_mut(lane)?;
            if buffered.timeline == LaneTimeline::Untimed {
                // MPEG-TS: полки нет, сегменты живут только в трубе.
                return Ok(PageRemovalOutcome::UntimedLane);
            }
            // Без позиции плеера ещё ничего не сыграно — и ничего не отпускается.
            let played = position.map_or(PlayedSoFar::Nothing, |position| {
                buffered.played_so_far(position)
            });
            let released = buffered.shelf.release_removed_by_page(
                ShelfRemoval::from_page(range),
                played,
                &rules,
            );
            // Окно живого эфира — непрерывный кусок полки у позиции: стёртое
            // прошлое сужает его, как сузило у страницы.
            self.publish_live_timeline(&mut state);
            released
        };
        let report = released.report;
        // Учёт освобождается вне замка буфера (как при `changeType` в MPEG-TS).
        drop(released.released);
        tracing::debug!(
            lane = lane.index(),
            released_segments = report.released_segments,
            released_bytes = report.released_bytes,
            kept_not_played = report.kept_not_played,
            kept_in_pipe = report.kept_in_pipe,
            kept_partially_removed = report.kept_partially_removed,
            "Pushed media buffer applied a removal by the page"
        );
        Ok(PageRemovalOutcome::Applied(report))
    }
}
