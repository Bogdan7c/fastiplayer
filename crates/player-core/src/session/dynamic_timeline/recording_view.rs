//! Проекция табло «запись» и «пока неизвестно» в публичную шкалу (мост 16d1, решения 318–320).
//!
//! Табло шкалы источника показывает эфир, запись или «пока неизвестно». Эфир проецирует
//! `apply_live_view` (S31L). Здесь — две другие ветки:
//! - **запись**: шкала `Static` с длиной (край записи) и диапазоном перемотки. Так шкала
//!   растёт вместе с длительностью страницы (YouTube) и становится записью, когда эфир
//!   кончился (hls.js, Shaka, dash.js: бесконечная длительность → конечная в том же источнике);
//! - **пока неизвестно**: шкала `Static` без длины и без перемотки — как media, у которого
//!   demuxer длину не знает.
//!
//! Шкала обновляется ЧАСТИЧНО, а не пересоздаётся (`PlayerSnapshot::set_timeline_duration`
//! сносит всё): позиция, цель идущей перемотки, флаги перемотки/скраба и preview остаются,
//! поэтому смена шкалы не двигает ни картинку, ни звук.

use media_core::{
    DynamicMediaTimelineView, MediaDuration, MediaTime, RecordingTimelineState, TimelineMode,
    TimelineNotSeekableReason, TimelineRange,
};
use tracing::info;

use super::super::PlayerSession;

/// Диапазон, которым табло ограничивает уже принятые цели перемотки.
///
/// У эфира — окно сервера (availability), у записи — её доступный диапазон, у «пока
/// неизвестно» — ничего.
pub(super) fn view_availability_range(view: DynamicMediaTimelineView) -> Option<TimelineRange> {
    match view {
        DynamicMediaTimelineView::Live(live) => live.availability_range(),
        DynamicMediaTimelineView::Recording(recording) => Some(recording.available_range()),
        DynamicMediaTimelineView::Undetermined => None,
    }
}

impl PlayerSession {
    /// Табло показывает запись: длина, режим `Static`, перемотка по доступному диапазону.
    ///
    /// Источник с табло записи (MSE, HLS) перематывается по природе, поэтому шкала
    /// записи всегда seekable; demuxer, который перематываться не умеет, табло записи не
    /// публикует.
    pub(super) fn apply_recording_view(&mut self, recording: RecordingTimelineState) {
        let was_live = self.snapshot.timeline.mode == TimelineMode::Live;
        let duration = recording.duration().as_duration();
        let available_range = recording.available_range();

        // Табло — единственный владелец длины этого media (у media с табло нет CUE-окна,
        // поэтому физическая и публичная длины совпадают).
        self.source_duration = Some(duration);
        self.snapshot.duration = Some(duration);
        let timeline = &mut self.snapshot.timeline;
        timeline.mode = TimelineMode::Static;
        timeline.duration = Some(MediaDuration::from_duration(duration));
        timeline.live_edge = None;
        timeline.live_epoch = None;
        timeline.live_revision = None;
        timeline.seekable_range = Some(available_range);
        timeline.seekable = true;
        timeline.not_seekable_reason = None;

        if was_live {
            // Редкое событие (раз за ролик) — в журнал; рост длины записи — нет, его много.
            info!(
                duration_ms = duration.as_millis(),
                available_from_ms = available_range.start.as_duration().as_millis(),
                "Live timeline became a recording"
            );
        }

        // Уже принятая перемотка за пределами записи (длина укоротилась) получает прежний
        // типизированный отказ; внутри диапазона она идёт дальше как обычно.
        self.expire_dynamic_seek_targets_outside_retention(
            Some(available_range),
            Some(available_range),
        );
    }

    /// Табло пока не знает ни вида потока, ни длины: шкала без длины и без перемотки.
    pub(super) fn apply_undetermined_view(&mut self) {
        self.source_duration = None;
        self.snapshot.duration = None;
        let timeline = &mut self.snapshot.timeline;
        timeline.mode = TimelineMode::Static;
        timeline.duration = None;
        timeline.live_edge = None;
        timeline.live_epoch = None;
        timeline.live_revision = None;
        timeline.seekable_range = None;
        timeline.seekable = false;
        timeline.not_seekable_reason = Some(TimelineNotSeekableReason::UnknownTimeline);
    }
}

/// Позиция в пределах записи: для live same-item restore после того, как эфир стал записью.
pub(super) fn clamp_into_recording(
    recording: RecordingTimelineState,
    position: std::time::Duration,
) -> std::time::Duration {
    MediaTime::from_duration(position)
        .clamp_to(recording.available_range())
        .as_duration()
}
