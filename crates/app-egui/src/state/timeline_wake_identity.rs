//! Что считается «шкала изменилась» для будильника шкалы плеера (`AppWakeOwner::PlayerTimeline`).
//!
//! Будильник приходит на каждую публикацию табло источника. Приложение перерисовывает окно и
//! обновляет MPRIS, только если шкала правда стала другой. Раньше сравнивалась одна ревизия
//! эфира, но у записи с табло (мост 16d1, решения 318–320) ревизии эфира нет: длина записи
//! растёт, эфир становится записью. Поэтому в «отпечаток» шкалы входят и режим, и длина, и
//! диапазон перемотки.

use media_core::{DynamicMediaTimelineRevision, MediaDuration, TimelineMode, TimelineRange};
use player_core::{MediaInstanceId, PlayerSnapshot};

/// Отпечаток шкалы: разные отпечатки — шкала изменилась.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TimelineWakeIdentity {
    media_instance_id: Option<MediaInstanceId>,
    mode: TimelineMode,
    live_revision: Option<DynamicMediaTimelineRevision>,
    duration: Option<MediaDuration>,
    seekable_range: Option<TimelineRange>,
}

/// Отпечаток шкалы снимка плеера.
pub(super) fn timeline_identity(snapshot: &PlayerSnapshot) -> TimelineWakeIdentity {
    TimelineWakeIdentity {
        media_instance_id: snapshot.media_instance_id,
        mode: snapshot.timeline.mode,
        live_revision: snapshot.timeline.live_revision,
        duration: snapshot.timeline.duration,
        seekable_range: snapshot.timeline.seekable_range,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use media_core::{MediaTime, TimelineSnapshot};

    use super::*;

    fn recording_snapshot(duration_secs: u64) -> PlayerSnapshot {
        PlayerSnapshot {
            timeline: TimelineSnapshot::seekable_vod(MediaDuration::from_duration(
                Duration::from_secs(duration_secs),
            )),
            ..PlayerSnapshot::default()
        }
    }

    #[test]
    fn a_longer_recording_is_a_changed_timeline() {
        assert_ne!(
            timeline_identity(&recording_snapshot(3)),
            timeline_identity(&recording_snapshot(6))
        );
    }

    #[test]
    fn a_live_stream_that_became_a_recording_is_a_changed_timeline() {
        let mut live = recording_snapshot(6);
        live.timeline.mode = TimelineMode::Live;
        live.timeline.duration = None;
        assert_ne!(
            timeline_identity(&live),
            timeline_identity(&recording_snapshot(6))
        );
    }

    #[test]
    fn only_the_playback_position_moving_is_not_a_changed_timeline() {
        let before = recording_snapshot(6);
        let mut after = recording_snapshot(6);
        after.timeline.current_position = MediaTime::from_secs(2);
        assert_eq!(timeline_identity(&before), timeline_identity(&after));
    }
}
