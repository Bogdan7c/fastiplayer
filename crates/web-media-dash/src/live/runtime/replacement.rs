//! Решение о замене immutable live plan до следующего network read.

use media_core::MediaTime;

use crate::live::DashLiveAvailability;

/// Явное состояние чтения не смешивает «packet ещё не читали» с timestamp zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DashLiveReadProgress {
    /// Worker ещё не опубликовал ни одного packet-а текущего demux-а.
    Unread,
    /// Конец последнего фактически прочитанного packet-а в source-native шкале.
    LastPacketEnd(MediaTime),
}

/// Выбирает source-native replacement target только для реально устаревшего reader-а.
pub(super) fn replacement_target_for_expired_reader(
    observed_revision: u64,
    authoritative_revision: u64,
    progress: DashLiveReadProgress,
    availability: &DashLiveAvailability,
) -> Option<MediaTime> {
    if authoritative_revision <= observed_revision {
        return None;
    }
    match progress {
        DashLiveReadProgress::Unread => Some(availability.live_edge),
        DashLiveReadProgress::LastPacketEnd(last_packet_end)
            if last_packet_end < availability.manifest_range.start =>
        {
            Some(
                availability
                    .manifest_range
                    .start
                    .min(availability.live_edge),
            )
        }
        DashLiveReadProgress::LastPacketEnd(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use media_core::TimelineRange;

    use super::*;

    fn availability(window_start: u64, live_edge: u64) -> DashLiveAvailability {
        DashLiveAvailability {
            live_edge: MediaTime::from_duration(Duration::from_secs(live_edge)),
            manifest_range: TimelineRange {
                start: MediaTime::from_duration(Duration::from_secs(window_start)),
                end: MediaTime::from_duration(Duration::from_secs(live_edge)),
            },
        }
    }

    #[test]
    fn replacement_requires_new_revision_and_expired_or_unread_progress() {
        let current = availability(100, 160);
        assert_eq!(
            replacement_target_for_expired_reader(7, 7, DashLiveReadProgress::Unread, &current,),
            None,
            "equal revision must never reopen the same plan"
        );
        assert_eq!(
            replacement_target_for_expired_reader(7, 8, DashLiveReadProgress::Unread, &current,),
            Some(MediaTime::from_duration(Duration::from_secs(160))),
            "unread stale demux starts from the fresh safe edge"
        );
        assert_eq!(
            replacement_target_for_expired_reader(
                7,
                8,
                DashLiveReadProgress::LastPacketEnd(MediaTime::from_duration(Duration::from_secs(
                    99
                ),)),
                &current,
            ),
            Some(MediaTime::from_duration(Duration::from_secs(100))),
            "expired reader resumes no earlier than the fresh sliding head"
        );
        assert_eq!(
            replacement_target_for_expired_reader(
                7,
                8,
                DashLiveReadProgress::LastPacketEnd(MediaTime::from_duration(Duration::from_secs(
                    100
                ),)),
                &current,
            ),
            None,
            "reader still inside DVR keeps its current immutable plan until EOF"
        );
    }
}
