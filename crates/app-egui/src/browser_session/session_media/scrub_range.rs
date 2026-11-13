//! Что скачано у показанного поколения — для живого перетаскивания по таймлайну окна.
//!
//! Решение владельца (31.10.2026, «кадры бегут по полке»): пока зритель тянет таймлайн
//! по уже скачанному, кадры показываются вживую, а странице не уходит ничего. Что
//! «скачано», знает только источник — полка `pushed-media-source`
//! ([`PushedStreamMonitor::holds_downloaded`]): цель внутри сегмента полки у всех полос
//! или полоса вступает позже по другим (то же правило, что у настоящей перемотки), но
//! БЕЗ посадки за концом законченного входа — такое место не скачано (09a2, Д-5).
//!
//! Медиа-часть каждый проход отдаёт плееру окна [`DownloadedRange`] ТОЛЬКО того
//! поколения, которое плеер показывает и которое уже приведено к странице. Всё
//! остальное — `Unknown` (окно тогда перематывает только на отпускании):
//! - поколение ещё открывается или плеер ещё ставит загруженное — скраб достался бы
//!   прежнему медиа, а полка — новому;
//! - файл `<video src>` — полки нет, байты идут по запросам (`fetch_range`), и любая
//!   промежуточная цель стала бы запросом странице;
//! - эфир — шкала эфира скользит (DVR), а позиции таймлайна у живой шкалы плеер
//!   переводит в время источника сам; не смешиваем две шкалы — отпускание, как раньше.
//!   Эфир, который страница закончила (длина + `endOfStream`), — уже запись (16d1): шкала
//!   плеера — время источника, и скачанное снова известно. Поэтому ответ зависит не от
//!   того, как поколение открылось, а от того, что шкала плеера показывает сейчас.
//!
//! Во время доигрывания (страница ушла) скачанное по-прежнему известно: перетаскивание
//! по нему — как раз то, что зритель может сделать без страницы.

use std::sync::Arc;
use std::time::Duration;

use pushed_media_source::PushedStreamMonitor;

use super::SessionMedia;
use crate::browser_session::playback::PlaybackObservation;
use crate::browser_session::playback::live_window::LiveWindowObservation;
use crate::downloaded_range::{DownloadedRange, DownloadedRangeProbe};

/// Какая шкала времени у поколения сегментов при открытии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SegmentsTimeline {
    /// Запись (VOD): позиции таймлайна = время источника.
    OnDemand,
    /// Эфир: живая шкала со скользящим окном (может стать записью, 16d1).
    Live,
}

/// Что шкала плеера показывает сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShownTimeline {
    /// Эфир (или ничего ещё не показано у поколения, открытого эфиром).
    Live,
    /// Запись: позиции таймлайна = время источника.
    Recording,
}

impl ShownTimeline {
    /// Вид шкалы по последнему наблюдению плеера; наблюдения ещё нет — эфир.
    pub(super) fn of(observation: Option<&PlaybackObservation>) -> Self {
        observation.map_or(Self::Live, |observation| {
            Self::of_window(observation.live_window)
        })
    }

    /// Вид шкалы по окну из снимка плеера.
    ///
    /// Запись — только явная запись с длиной: так ядро показывает и ролик, и эфир, который
    /// кончился. «Окна нет» (`NotLive`) записью не считается: у поколения, открытого эфиром,
    /// оно значит, что плеер ещё показывает чужое медиа (или ничего), и сравнивать цель с
    /// полкой этого поколения нельзя — шкалы разные. Поколению, открытому записью, вид
    /// шкалы не важен (см. [`SegmentsDownloadedRange::while_showing`]).
    pub(super) const fn of_window(window: LiveWindowObservation) -> Self {
        match window {
            LiveWindowObservation::Recording { .. } => Self::Recording,
            LiveWindowObservation::NotLive
            | LiveWindowObservation::LiveEdgeUnknown
            | LiveWindowObservation::LiveWithoutDvr { .. }
            | LiveWindowObservation::LiveWithDvr { .. } => Self::Live,
        }
    }
}

/// Скачанное поколения сегментов: вопрос к полке источника.
struct ShelfDownloadedRange {
    monitor: PushedStreamMonitor,
}

impl DownloadedRangeProbe for ShelfDownloadedRange {
    /// «Скачано» — а не «сядет ли перемотка» (09a2, Д-5): после конца входа (обрыв
    /// скачивания, когда страница ушла) перемотка за конец полки «садится» на последний
    /// сегмент, и клик за скачанное во время досмотра давал конец ролика вместо отказа.
    /// Пока вход открыт, оба вопроса отвечают одинаково.
    fn holds(&self, position: Duration) -> bool {
        self.monitor.holds_downloaded(position)
    }
}

/// Скачанное поколения сегментов (ручка полки собирается один раз при загрузке поколения:
/// дальше каждый проход отдаёт копию ручки, без новых выделений памяти).
#[derive(Clone)]
pub(super) struct SegmentsDownloadedRange {
    shelf: DownloadedRange,
    opened_as: SegmentsTimeline,
}

impl SegmentsDownloadedRange {
    pub(super) fn new(monitor: &PushedStreamMonitor, opened_as: SegmentsTimeline) -> Self {
        Self {
            shelf: DownloadedRange::known(Arc::new(ShelfDownloadedRange {
                monitor: monitor.clone(),
            })),
            opened_as,
        }
    }

    /// Запись (с открытия или ставшая записью) — полка; эфир — неизвестно.
    pub(super) fn while_showing(&self, shown: ShownTimeline) -> DownloadedRange {
        match (self.opened_as, shown) {
            (SegmentsTimeline::OnDemand, _)
            | (SegmentsTimeline::Live, ShownTimeline::Recording) => self.shelf.clone(),
            (SegmentsTimeline::Live, ShownTimeline::Live) => DownloadedRange::Unknown,
        }
    }
}

impl SessionMedia {
    /// Отдаёт плееру окна, что скачано у показанного поколения (правила — в описании
    /// модуля). Зовётся каждый проход после ответов странице.
    pub(super) fn offer_downloaded_range(&mut self, observation: &PlaybackObservation) {
        let range = self.downloaded_range_for_viewer(observation);
        self.playback.offer_downloaded_range(range);
    }

    /// Скачанное поколения, которое видит зритель, или `Unknown`.
    fn downloaded_range_for_viewer(&self, observation: &PlaybackObservation) -> DownloadedRange {
        if self.pending.is_some() {
            return DownloadedRange::Unknown;
        }
        match &self.loaded {
            Some(loaded) if loaded.is_shown_by(observation) && loaded.page_state_reconciled => {
                loaded
                    .feed
                    .downloaded_range(ShownTimeline::of(Some(observation)))
            }
            _ => DownloadedRange::Unknown,
        }
    }
}
