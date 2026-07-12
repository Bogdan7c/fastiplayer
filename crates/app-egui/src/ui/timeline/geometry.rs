//! Чистая geometry timeline и форматирование media time.

use std::time::Duration;

use egui::Rect;
use media_core::{MediaDuration, MediaTime, TimelineRange, TimelineSnapshot};

use crate::ui::skin::TimelineStyle;
use ui_artwork_egui::TimelineStyle as ArtworkTimelineStyle;

/// Seekable bounds timeline-а, общие для gesture mapper-а и renderer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineBounds {
    range: TimelineRange,
}

impl TimelineBounds {
    /// Создаёт bounds только для ненулевого диапазона.
    pub fn new(range: TimelineRange) -> Option<Self> {
        (range.duration() > MediaDuration::ZERO).then_some(Self { range })
    }

    /// Переводит нормализованную координату в позицию seekable range.
    pub fn position_from_fraction(self, fraction: f64) -> MediaTime {
        let offset = duration_mul_fraction(self.range.duration(), fraction.clamp(0.0, 1.0));
        self.range.start.saturating_add(offset)
    }

    /// Переводит media position в нормализованную координату renderer-а.
    pub(super) fn fraction_from_position(self, position: MediaTime) -> f32 {
        let clamped_position = self.range.clamp(position);
        let offset = clamped_position
            .as_duration()
            .saturating_sub(self.range.start.as_duration());
        let duration = self.range.duration().as_secs_f64();
        if duration <= 0.0 {
            return 0.0;
        }
        (offset.as_secs_f64() / duration).clamp(0.0, 1.0) as f32
    }
}

/// Возвращает интерактивные bounds только для seekable timeline.
pub(super) fn timeline_bounds(timeline: &TimelineSnapshot) -> Option<TimelineBounds> {
    if !timeline.seekable {
        return None;
    }
    if let Some(range) = timeline.seekable_range {
        return TimelineBounds::new(range);
    }
    timeline.duration.and_then(|duration| {
        TimelineBounds::new(TimelineRange::from_bounds_saturating(
            MediaTime::ZERO,
            MediaTime::from_duration(duration.as_duration()),
        ))
    })
}

/// Форматирует media time в player-style строку.
pub fn format_media_time(position: Option<MediaTime>) -> String {
    format_seconds(position.map(MediaTime::as_secs_f64))
}

/// Форматирует media duration в player-style строку.
pub fn format_media_duration(duration: Option<MediaDuration>) -> String {
    format_seconds(duration.map(MediaDuration::as_secs_f64))
}

pub fn format_seconds(seconds: Option<f64>) -> String {
    let Some(seconds) = seconds else {
        return "--:--".to_string();
    };
    if !seconds.is_finite() || seconds < 0.0 {
        return "--:--".to_string();
    }
    let total_seconds = seconds.floor() as u64;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds / 60) % 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

pub(super) fn timeline_track_rect(rect: Rect, style: TimelineStyle) -> Rect {
    ui_artwork_egui::timeline_track_rect(rect, artwork_timeline_style(style))
}

/// Преобразует skin-стиль приложения в проектно-нейтральный artwork-стиль.
pub(super) fn artwork_timeline_style(style: TimelineStyle) -> ArtworkTimelineStyle {
    ArtworkTimelineStyle {
        track_height: style.track_height,
        thumb_radius: style.thumb_radius,
        horizontal_padding: style.horizontal_padding,
        track_fill: style.track_fill,
        played_fill: style.played_fill,
        target_fill: style.target_fill,
        thumb_fill: style.thumb_fill,
        track_outline_width: style.track_outline_width,
        track_outline_fill: style.track_outline_fill,
        thumb_outline_width: style.thumb_outline_width,
        thumb_outline_fill: style.thumb_outline_fill,
        disabled_fill: style.disabled_fill,
    }
}

fn duration_mul_fraction(duration: MediaDuration, fraction: f64) -> MediaDuration {
    let seconds = duration.as_secs_f64() * fraction.clamp(0.0, 1.0);
    MediaDuration::from_duration(Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_short_long_and_unknown_time() {
        assert_eq!(format_seconds(None), "--:--");
        assert_eq!(format_seconds(Some(f64::NAN)), "--:--");
        assert_eq!(format_seconds(Some(65.9)), "01:05");
        assert_eq!(format_seconds(Some(3_661.0)), "01:01:01");
    }

    #[test]
    fn bounds_round_trip_clamps_to_seekable_range() {
        let bounds = TimelineBounds::new(TimelineRange::from_bounds_saturating(
            MediaTime::from_secs(10),
            MediaTime::from_secs(110),
        ))
        .expect("non-empty range");
        assert_eq!(
            bounds.position_from_fraction(0.25),
            MediaTime::from_secs(35)
        );
        assert_eq!(
            bounds.fraction_from_position(MediaTime::from_secs(35)),
            0.25
        );
    }
}
