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
//! Тип не прячет смысл в `Option`/`bool`: «окна нет», «эфир, край ещё неизвестен»,
//! «эфир без перемотки», «эфир с окном» и «запись» — разные варианты, и отправитель решает
//! по ним явно (`outbound::live_window`).
//!
//! **Запись** (16d1, решения 325–327): шкала с длиной — у ролика с открытия или у эфира,
//! который кончился (`EXT-X-ENDLIST`, MSE с длиной). Её край — длительность, перемотка —
//! опубликованный ядром диапазон (у законченного HLS-эфира он может быть короче длины:
//! перемотка — только туда, где плеер видел ключевой кадр).

use std::time::Duration;

use media_core::{TimelineMode, TimelineSnapshot};

/// Окно шкалы показанного медиа: эфир или запись.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LiveWindowObservation {
    /// Окна нет: медиа нет или у записи ещё нет длины.
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
    /// Запись с длиной `end` (ролик или эфир, который кончился).
    Recording {
        /// Край записи — её длительность.
        end: Duration,
        /// Куда можно перематывать; `None` — ядро перемотку не разрешает.
        seekable: Option<ObservedSeekRange>,
    },
}

/// Диапазон перемотки из снимка ядра (`start ≤ end`, гарантирует `TimelineRange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObservedSeekRange {
    /// Самое раннее место перемотки.
    pub(crate) start: Duration,
    /// Самое позднее место перемотки.
    pub(crate) end: Duration,
}

impl LiveWindowObservation {
    /// Шкала — эфир (с краем или без): время ещё скользит.
    pub(crate) const fn is_live(self) -> bool {
        matches!(
            self,
            Self::LiveEdgeUnknown | Self::LiveWithoutDvr { .. } | Self::LiveWithDvr { .. }
        )
    }
}

impl LiveWindowObservation {
    /// Окно по шкале снимка ядра.
    ///
    /// Окно без перемотки — когда ядро не публикует диапазон (`seekable_range = None`,
    /// причина `LiveWindowUnavailable`): так ядро говорит «эфир без DVR».
    pub(crate) fn from_timeline(timeline: &TimelineSnapshot) -> Self {
        if timeline.mode != TimelineMode::Live {
            return Self::recording_of(timeline);
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

    /// Шкала записи: окно есть, только когда известна длина.
    fn recording_of(timeline: &TimelineSnapshot) -> Self {
        let Some(duration) = timeline.duration else {
            return Self::NotLive;
        };
        let seekable = timeline
            .seekable_range
            .filter(|_| timeline.seekable)
            .map(|range| ObservedSeekRange {
                start: range.start.as_duration(),
                end: range.end.as_duration(),
            });
        Self::Recording {
            end: duration.as_duration(),
            seekable,
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
    fn a_static_timeline_with_a_length_is_a_recording_not_live() {
        let vod =
            TimelineSnapshot::seekable_vod(MediaDuration::from_duration(Duration::from_secs(60)));
        let observed = LiveWindowObservation::from_timeline(&vod);
        assert_eq!(
            observed,
            LiveWindowObservation::Recording {
                end: Duration::from_secs(60),
                seekable: Some(ObservedSeekRange {
                    start: Duration::ZERO,
                    end: Duration::from_secs(60),
                }),
            }
        );
        assert!(!observed.is_live());
        assert_eq!(
            LiveWindowObservation::from_timeline(&TimelineSnapshot::default()),
            LiveWindowObservation::NotLive
        );
    }

    #[test]
    fn an_ended_live_stream_still_being_read_seeks_only_where_it_was_read() {
        // ENDLIST: длина 130 с сразу, перемотка — по прочитанному 100–124 с (решение 325).
        let read = TimelineRange::new(at(100), at(124)).expect("range");
        let ended = TimelineSnapshot {
            mode: TimelineMode::Static,
            duration: Some(MediaDuration::from_duration(Duration::from_secs(130))),
            seekable_range: Some(read),
            seekable: true,
            ..TimelineSnapshot::default()
        };
        assert_eq!(
            LiveWindowObservation::from_timeline(&ended),
            LiveWindowObservation::Recording {
                end: Duration::from_secs(130),
                seekable: Some(ObservedSeekRange {
                    start: Duration::from_secs(100),
                    end: Duration::from_secs(124),
                }),
            }
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
