//! Окно эфира показанного медиа — глазами медиа-части браузерного режима (сессия 10a3,
//! решение 232).
//!
//! Ядро плеера само ведёт live-шкалу и публикует её в снимке (`TimelineSnapshot`):
//! режим `Live`, край эфира `live_edge` и честное окно перемотки `seekable_range`
//! (пересечение удержанных сегментов, реально наблюдённых пакетов и безопасных якорей —
//! `web-media-hls::live`, `media-core::dynamic_timeline`). Тот же снимок рисует строку
//! `DVR mm:ss–mm:ss · LIVE` обычного таймлайна (`ui::timeline::render`).
//!
//! **Одно время с часами страницы.** Позиция, которую часы `clock` говорят странице
//! ([`super::PlaybackObservation::page_clock_position`]), — `PlayerSnapshot::current_position`
//! или цель перемотки `timeline.target_position`. Ядро пишет позицию в обе копии снимка
//! одним шагом (`PlayerSnapshot::set_timeline_position`), а окно и край — в ту же шкалу
//! «относительного» времени. У эфира относительное время совпадает со временем
//! источника: окно воспроизведения (CUE) и живая шкала взаимоисключающие
//! (`PreparedMediaTimelineModeError`). Поэтому окно отсюда и позиция часов — одно время
//! плеера, без перевода.
//!
//! Тип не прячет смысл в `Option`/`bool`: «не эфир», «эфир, край ещё неизвестен»,
//! «эфир без перемотки» и «эфир с окном» — разные варианты, и отправитель решает по ним
//! явно (`outbound::live_window`).

use std::time::Duration;

use media_core::{TimelineMode, TimelineSnapshot};

/// Окно эфира показанного медиа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LiveWindowObservation {
    /// Медиа не эфир (ролик, файл) или медиа нет: окна нет.
    #[default]
    NotLive,
    /// Эфир, но ядро ещё не знает край (шкала только поставлена): сказать нечего.
    LiveEdgeUnknown,
    /// Эфир без DVR: перемотки нет, только край.
    LiveWithoutDvr {
        /// Край эфира — самое новое время, которое есть у плеера.
        edge: Duration,
    },
    /// Эфир с окном перемотки `[start, end]` (ядро гарантирует `start ≤ end ≤ edge`:
    /// `TimelineRange::new` и `DynamicMediaTimelineState::with_dvr`).
    LiveWithDvr {
        /// Край эфира.
        edge: Duration,
        /// Самое раннее место, куда плеер честно может встать.
        start: Duration,
        /// Самое позднее место перемотки.
        end: Duration,
    },
}

impl LiveWindowObservation {
    /// Окно по шкале снимка ядра.
    ///
    /// Окно без перемотки — когда ядро не публикует диапазон (`seekable_range = None`,
    /// причина `LiveWindowUnavailable`): так ядро говорит «эфир без DVR».
    pub(crate) fn from_timeline(timeline: &TimelineSnapshot) -> Self {
        if timeline.mode != TimelineMode::Live {
            return Self::NotLive;
        }
        let Some(edge) = timeline.live_edge else {
            return Self::LiveEdgeUnknown;
        };
        let edge = edge.as_duration();
        match timeline.seekable_range {
            Some(range) => Self::LiveWithDvr {
                edge,
                start: range.start.as_duration(),
                end: range.end.as_duration(),
            },
            None => Self::LiveWithoutDvr { edge },
        }
    }
}

#[cfg(test)]
mod tests {
    use media_core::{MediaDuration, MediaTime, TimelineRange};

    use super::*;

    fn at(seconds: u64) -> MediaTime {
        MediaTime::from_duration(Duration::from_secs(seconds))
    }

    fn live(edge: Option<MediaTime>, range: Option<TimelineRange>) -> TimelineSnapshot {
        TimelineSnapshot {
            mode: TimelineMode::Live,
            live_edge: edge,
            seekable_range: range,
            duration: None,
            ..TimelineSnapshot::default()
        }
    }

    #[test]
    fn a_static_timeline_is_not_live_even_with_a_seekable_range() {
        let vod =
            TimelineSnapshot::seekable_vod(MediaDuration::from_duration(Duration::from_secs(60)));
        assert_eq!(
            LiveWindowObservation::from_timeline(&vod),
            LiveWindowObservation::NotLive
        );
        assert_eq!(
            LiveWindowObservation::from_timeline(&TimelineSnapshot::default()),
            LiveWindowObservation::NotLive
        );
    }

    #[test]
    fn a_live_timeline_without_an_edge_says_nothing_yet() {
        assert_eq!(
            LiveWindowObservation::from_timeline(&live(None, None)),
            LiveWindowObservation::LiveEdgeUnknown
        );
    }

    #[test]
    fn a_live_timeline_without_a_range_has_no_dvr() {
        assert_eq!(
            LiveWindowObservation::from_timeline(&live(Some(at(61)), None)),
            LiveWindowObservation::LiveWithoutDvr {
                edge: Duration::from_secs(61)
            }
        );
    }

    #[test]
    fn a_live_timeline_with_a_range_carries_edge_and_window() {
        let range = TimelineRange::new(at(40), at(60)).expect("range");
        assert_eq!(
            LiveWindowObservation::from_timeline(&live(Some(at(61)), Some(range))),
            LiveWindowObservation::LiveWithDvr {
                edge: Duration::from_secs(61),
                start: Duration::from_secs(40),
                end: Duration::from_secs(60),
            }
        );
    }
}
