//! Что скачано у показанного поколения — для живого перетаскивания по таймлайну окна.
//!
//! Решение владельца (31.10.2026, «кадры бегут по полке»): пока зритель тянет таймлайн
//! по уже скачанному, кадры показываются вживую, а странице не уходит ничего. Что
//! «скачано», знает только источник — полка `pushed-media-source`: перемотка к цели
//! садится с полки, если её ответ [`ShelfSeekOutlook::FromShelf`] (то же правило, что
//! у настоящей перемотки, включая позднее вступление полосы).
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
//!
//! Во время доигрывания (страница ушла) скачанное по-прежнему известно: перетаскивание
//! по нему — как раз то, что зритель может сделать без страницы.

use std::sync::Arc;
use std::time::Duration;

use pushed_media_source::{PushedStreamMonitor, ShelfSeekOutlook};

use super::SessionMedia;
use crate::browser_session::playback::PlaybackObservation;
use crate::downloaded_range::{DownloadedRange, DownloadedRangeProbe};

/// Какая шкала времени у поколения сегментов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SegmentsTimeline {
    /// Запись (VOD): позиции таймлайна = время источника.
    OnDemand,
    /// Эфир: живая шкала со скользящим окном.
    Live,
}

/// Скачанное поколения сегментов: вопрос к полке источника.
struct ShelfDownloadedRange {
    monitor: PushedStreamMonitor,
}

impl DownloadedRangeProbe for ShelfDownloadedRange {
    fn holds(&self, position: Duration) -> bool {
        self.monitor.seek_outlook(position) == ShelfSeekOutlook::FromShelf
    }
}

/// Скачанное для поколения сегментов (собирается один раз при загрузке поколения:
/// дальше каждый проход отдаёт копию ручки, без новых выделений памяти).
pub(super) fn segments_downloaded_range(
    monitor: &PushedStreamMonitor,
    timeline: SegmentsTimeline,
) -> DownloadedRange {
    match timeline {
        SegmentsTimeline::OnDemand => DownloadedRange::known(Arc::new(ShelfDownloadedRange {
            monitor: monitor.clone(),
        })),
        SegmentsTimeline::Live => DownloadedRange::Unknown,
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
                loaded.feed.downloaded_range()
            }
            _ => DownloadedRange::Unknown,
        }
    }
}
