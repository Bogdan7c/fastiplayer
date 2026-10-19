//! Слияние нескольких `PlayerTickResult` в один агрегат.
//!
//! Нужно worker-у: телеметрия tick-ов едет к UI через «почтовый ящик»
//! (`worker/event_stream`), и пока UI не забрал прошлую порцию, новые tick-и
//! вливаются в неё, а не теряются. Владелец семантики слияния — сам тип
//! `PlayerTickResult`: добавляя поле, нужно решить, как оно сливается.

use super::types::PlayerTickResult;

/// Максимум записей в каждом списке агрегата (`demuxed_packets`,
/// `dropped_video_frames`, `pipeline_pauses`).
///
/// Агрегат растёт только пока UI не забирает телеметрию (зависший кадр,
/// свёрнутое окно), поэтому он обязан быть ограниченным. Один обычный tick
/// читает ≤ 12 packet-ов, так что 4096 покрывают сотни tick-ов простоя; всё,
/// что не влезло, не хранится, а считается в `truncated_list_entries`.
pub const MAX_AGGREGATED_LIST_ENTRIES: usize = 4096;

impl PlayerTickResult {
    /// `true`, если в результате нет ни одного наблюдения (пустой tick).
    ///
    /// Пустые tick-и (большинство при ~2 мс опросе decoder-а) публиковать не нужно.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Вливает более поздний tick `later` в этот агрегат.
    ///
    /// Семантика по полям:
    /// - счётчики (`u64`) — насыщающая сумма;
    /// - списки — дописываются в порядке tick-ов, но не дольше
    ///   [`MAX_AGGREGATED_LIST_ENTRIES`]: самые старые записи остаются, лишние
    ///   отбрасываются и учитываются в `truncated_list_entries`;
    /// - `demux_backpressured` — логическое ИЛИ («backpressure был хотя бы раз»);
    /// - `truncated_list_entries` — насыщающая сумма.
    pub fn absorb(&mut self, later: Self) {
        let Self {
            demuxed_packets,
            dropped_seek_audio_preroll_packets,
            staged_video_backlog_recovery_packets,
            decoded_video_frames,
            video_frames_presented,
            video_frames_repeated,
            dropped_video_frames,
            pipeline_pauses,
            demux_backpressured,
            truncated_list_entries,
        } = later;

        let mut truncated = truncated_list_entries;
        truncated =
            truncated.saturating_add(append_bounded(&mut self.demuxed_packets, demuxed_packets));
        truncated = truncated.saturating_add(append_bounded(
            &mut self.dropped_video_frames,
            dropped_video_frames,
        ));
        truncated =
            truncated.saturating_add(append_bounded(&mut self.pipeline_pauses, pipeline_pauses));

        self.dropped_seek_audio_preroll_packets = self
            .dropped_seek_audio_preroll_packets
            .saturating_add(dropped_seek_audio_preroll_packets);
        self.staged_video_backlog_recovery_packets = self
            .staged_video_backlog_recovery_packets
            .saturating_add(staged_video_backlog_recovery_packets);
        self.decoded_video_frames = self
            .decoded_video_frames
            .saturating_add(decoded_video_frames);
        self.video_frames_presented = self
            .video_frames_presented
            .saturating_add(video_frames_presented);
        self.video_frames_repeated = self
            .video_frames_repeated
            .saturating_add(video_frames_repeated);
        self.demux_backpressured |= demux_backpressured;
        self.truncated_list_entries = self.truncated_list_entries.saturating_add(truncated);
    }
}

/// Дописывает `incoming` в `target` до лимита и возвращает число отброшенных записей.
fn append_bounded<T>(target: &mut Vec<T>, incoming: Vec<T>) -> u64 {
    let free_slots = MAX_AGGREGATED_LIST_ENTRIES.saturating_sub(target.len());
    let dropped = incoming.len().saturating_sub(free_slots);
    target.extend(incoming.into_iter().take(free_slots));
    u64::try_from(dropped).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use media_core::{PacketKeyframe, TrackId, TrackKind};

    use super::*;
    use crate::PipelinePauseReason;
    use crate::session::tick::types::{
        PlayerPipelinePause, PlayerTickPacket, PlayerVideoDropReason, PlayerVideoFrameDrop,
    };

    fn packet(size: usize) -> PlayerTickPacket {
        PlayerTickPacket {
            track_id: TrackId::new(1),
            kind: TrackKind::Video,
            pts: Duration::ZERO,
            track_pts: None,
            track_dts: None,
            size,
            byte_offset: None,
            keyframe: PacketKeyframe::Unknown,
        }
    }

    fn drop_at(millis: u64) -> PlayerVideoFrameDrop {
        PlayerVideoFrameDrop {
            pts: Duration::from_millis(millis),
            reason: PlayerVideoDropReason::Late,
        }
    }

    #[test]
    fn absorb_sums_counters_appends_lists_and_ors_flag_in_tick_order() {
        let mut aggregate = PlayerTickResult {
            demuxed_packets: vec![packet(1)],
            dropped_seek_audio_preroll_packets: 1,
            staged_video_backlog_recovery_packets: 2,
            decoded_video_frames: 3,
            video_frames_presented: 4,
            video_frames_repeated: 5,
            dropped_video_frames: vec![drop_at(10)],
            pipeline_pauses: vec![PlayerPipelinePause {
                reason: PipelinePauseReason::DemuxBackpressure,
            }],
            demux_backpressured: false,
            truncated_list_entries: 6,
        };
        aggregate.absorb(PlayerTickResult {
            demuxed_packets: vec![packet(2)],
            dropped_seek_audio_preroll_packets: 10,
            staged_video_backlog_recovery_packets: 20,
            decoded_video_frames: 30,
            video_frames_presented: 40,
            video_frames_repeated: 50,
            dropped_video_frames: vec![drop_at(20)],
            pipeline_pauses: vec![PlayerPipelinePause {
                reason: PipelinePauseReason::DemuxBackpressure,
            }],
            demux_backpressured: true,
            truncated_list_entries: 60,
        });

        assert_eq!(
            aggregate
                .demuxed_packets
                .iter()
                .map(|p| p.size)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(aggregate.dropped_seek_audio_preroll_packets, 11);
        assert_eq!(aggregate.staged_video_backlog_recovery_packets, 22);
        assert_eq!(aggregate.decoded_video_frames, 33);
        assert_eq!(aggregate.video_frames_presented, 44);
        assert_eq!(aggregate.video_frames_repeated, 55);
        assert_eq!(aggregate.dropped_video_frames, [drop_at(10), drop_at(20)]);
        assert_eq!(aggregate.pipeline_pauses.len(), 2);
        assert!(aggregate.demux_backpressured);
        assert_eq!(aggregate.truncated_list_entries, 66);
    }

    #[test]
    fn absorb_saturates_counters_instead_of_overflowing() {
        let mut aggregate = PlayerTickResult {
            decoded_video_frames: u64::MAX - 1,
            ..PlayerTickResult::default()
        };
        aggregate.absorb(PlayerTickResult {
            decoded_video_frames: 5,
            ..PlayerTickResult::default()
        });
        assert_eq!(aggregate.decoded_video_frames, u64::MAX);
    }

    #[test]
    fn absorb_caps_every_list_keeps_oldest_and_counts_overflow() {
        let mut aggregate = PlayerTickResult {
            dropped_video_frames: (0..MAX_AGGREGATED_LIST_ENTRIES as u64 - 1)
                .map(drop_at)
                .collect(),
            ..PlayerTickResult::default()
        };
        aggregate.absorb(PlayerTickResult {
            dropped_video_frames: vec![drop_at(9001), drop_at(9002), drop_at(9003)],
            demuxed_packets: (0..MAX_AGGREGATED_LIST_ENTRIES + 7).map(packet).collect(),
            ..PlayerTickResult::default()
        });

        assert_eq!(
            aggregate.dropped_video_frames.len(),
            MAX_AGGREGATED_LIST_ENTRIES
        );
        assert_eq!(aggregate.dropped_video_frames.last(), Some(&drop_at(9001)));
        assert_eq!(aggregate.demuxed_packets.len(), MAX_AGGREGATED_LIST_ENTRIES);
        // 2 лишних drop + 7 лишних packets.
        assert_eq!(aggregate.truncated_list_entries, 9);
    }

    #[test]
    fn is_empty_is_false_for_any_single_observation() {
        assert!(PlayerTickResult::default().is_empty());
        let backpressured = PlayerTickResult {
            demux_backpressured: true,
            ..PlayerTickResult::default()
        };
        assert!(!backpressured.is_empty());
        let presented = PlayerTickResult {
            video_frames_presented: 1,
            ..PlayerTickResult::default()
        };
        assert!(!presented.is_empty());
    }
}
