//! Время старта контейнера: самый ранний PTS первой эпохи (`StreamStart`).
//!
//! Плеер показывает сырое время контейнера (запись эфира начинается с ~1,4 с),
//! а внешние файлы субтитров считают от нуля. Потребителю нужно знать этот
//! старт, поэтому demuxer публикует его через `Demuxer::media_metadata()`.
//!
//! Инварианты:
//! - учитываются только пакеты эпохи `StreamStart`: после склейки записей
//!   (`StartsAtByte`) шкала уже перебазирована и старт первой записи не меняется;
//! - берётся минимум по всем PID (видео и аудио), а не время первого keyframe;
//! - значение переживает rewind после initial index scan и последующие seek-и
//!   (трекер монотонно только уменьшается и не входит в откатываемое состояние);
//! - при `OwnedByTransport` (HLS) шкалу ведёт transport, старт не публикуется.

use std::time::Duration;

use crate::timeline_continuity::{TimelineEpochId, TimelineOwnership};

/// Тактов PTS в секунде (MPEG-TS 90 кГц).
const TICKS_PER_SECOND: u64 = 90_000;
/// Наносекунд на один такт 90 кГц (100_000_000 / 9 не целое, считаем через секунды).
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Накопитель минимального PTS первой эпохи.
#[derive(Debug, Clone)]
pub(crate) struct PresentationStartTracker {
    ownership: TimelineOwnership,
    /// Минимальный PTS (90 кГц) среди наблюдённых пакетов эпохи `StreamStart`.
    earliest_pts_90khz: Option<i64>,
}

impl PresentationStartTracker {
    pub(crate) const fn new(ownership: TimelineOwnership) -> Self {
        Self {
            ownership,
            earliest_pts_90khz: None,
        }
    }

    /// Учитывает PTS опубликованного пакета, лежащего в эпохе `epoch`.
    pub(crate) fn observe(&mut self, epoch: TimelineEpochId, pts_90khz: i64) {
        if self.ownership != TimelineOwnership::RebasedByDemuxer
            || epoch != TimelineEpochId::StreamStart
        {
            return;
        }
        self.earliest_pts_90khz = Some(
            self.earliest_pts_90khz
                .map_or(pts_90khz, |earliest| earliest.min(pts_90khz)),
        );
    }

    /// Старт контейнера; `None` — не наблюдался, равен нулю или шкалой владеет transport.
    pub(crate) fn presentation_start(&self) -> Option<Duration> {
        let ticks = u64::try_from(self.earliest_pts_90khz?).ok()?;
        if ticks == 0 {
            return None;
        }
        let seconds = ticks / TICKS_PER_SECOND;
        let remainder_ticks = ticks % TICKS_PER_SECOND;
        // remainder < 90_000, умножение на 1e9 помещается в u64 без переполнения.
        let nanos = remainder_ticks * NANOS_PER_SECOND / TICKS_PER_SECOND;
        Some(Duration::new(seconds, u32::try_from(nanos).ok()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimum_pts_of_first_epoch_is_published_across_streams() {
        let mut tracker = PresentationStartTracker::new(TimelineOwnership::RebasedByDemuxer);
        tracker.observe(TimelineEpochId::StreamStart, 138_600);
        tracker.observe(TimelineEpochId::StreamStart, 126_000);
        tracker.observe(TimelineEpochId::StreamStart, 200_000);
        assert_eq!(
            tracker.presentation_start(),
            Some(Duration::from_millis(1_400))
        );
    }

    #[test]
    fn later_epochs_and_transport_owned_timeline_do_not_publish_start() {
        let mut rebased = PresentationStartTracker::new(TimelineOwnership::RebasedByDemuxer);
        rebased.observe(TimelineEpochId::StartsAtByte(188), 90_000);
        assert_eq!(rebased.presentation_start(), None);

        let mut transport = PresentationStartTracker::new(TimelineOwnership::OwnedByTransport);
        transport.observe(TimelineEpochId::StreamStart, 126_000);
        assert_eq!(transport.presentation_start(), None);
    }

    #[test]
    fn zero_start_means_none() {
        let mut tracker = PresentationStartTracker::new(TimelineOwnership::RebasedByDemuxer);
        tracker.observe(TimelineEpochId::StreamStart, 0);
        assert_eq!(tracker.presentation_start(), None);
    }
}
