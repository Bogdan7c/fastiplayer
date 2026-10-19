//! Перенос агрегата tick-телеметрии worker-а в shell `Telemetry`.
//!
//! Worker сливает tick-и, которые UI не успел забрать, в один `PlayerTickResult`
//! (счётчики суммируются), так что одна запись агрегата равна сумме записей
//! отдельных tick-ов.

use player_core::PlayerTickResult;

use super::record_player_tick_result;
use crate::telemetry::Telemetry;

/// Записывает забранный агрегат (если он есть) и сообщает, если списки были усечены.
pub(super) fn record_tick_telemetry(telemetry: &Telemetry, aggregate: Option<PlayerTickResult>) {
    let Some(aggregate) = aggregate else {
        return;
    };
    if aggregate.truncated_list_entries > 0 {
        // Штатная ситуация, а не сбой: на старте окно ~1,5 с не рисует кадры, а worker
        // успевает прочитать тысячи пакетов. Lifecycle-события при этом доходят (у них
        // своя очередь), теряется только детализация телеметрии, поэтому debug, а не warn.
        tracing::debug!(
            truncated_entries = aggregate.truncated_list_entries,
            "Worker tick telemetry lists were truncated while the UI was not draining; \
             per-packet and per-drop counters may under-report"
        );
    }
    record_player_tick_result(telemetry, &aggregate);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use media_core::{PacketKeyframe, TrackId, TrackKind};
    use player_core::{PlayerTickPacket, PlayerVideoDropReason, PlayerVideoFrameDrop};

    use super::*;

    fn tick(index: u64) -> PlayerTickResult {
        PlayerTickResult {
            demuxed_packets: vec![PlayerTickPacket {
                track_id: TrackId::new(1),
                kind: if index.is_multiple_of(2) {
                    TrackKind::Video
                } else {
                    TrackKind::Audio
                },
                pts: Duration::from_millis(index),
                track_pts: None,
                track_dts: None,
                size: 10,
                byte_offset: None,
                keyframe: PacketKeyframe::Unknown,
            }],
            decoded_video_frames: 1,
            video_frames_presented: 1,
            video_frames_repeated: index % 3,
            dropped_video_frames: vec![PlayerVideoFrameDrop {
                pts: Duration::from_millis(index),
                reason: PlayerVideoDropReason::Late,
            }],
            ..PlayerTickResult::default()
        }
    }

    #[test]
    fn aggregated_ticks_record_same_telemetry_as_individual_ticks() {
        let individually = Telemetry::new();
        let mut aggregate = PlayerTickResult::default();
        for index in 0..50 {
            record_player_tick_result(&individually, &tick(index));
            aggregate.absorb(tick(index));
        }

        let aggregated = Telemetry::new();
        record_tick_telemetry(&aggregated, Some(aggregate));

        assert_eq!(aggregated.packets_read(), 50);
        assert_eq!(aggregated.packets_read(), individually.packets_read());
        assert_eq!(aggregated.video_frames_decoded(), 50);
        assert_eq!(
            aggregated.video_frames_decoded(),
            individually.video_frames_decoded()
        );
        assert_eq!(aggregated.video_frames_presented(), 50);
        assert_eq!(
            aggregated.video_frames_repeated(),
            individually.video_frames_repeated()
        );
        assert_eq!(aggregated.video_late_drops(), 50);
        assert_eq!(
            aggregated.video_late_drops(),
            individually.video_late_drops()
        );
    }

    #[test]
    fn missing_aggregate_records_nothing() {
        let telemetry = Telemetry::new();
        record_tick_telemetry(&telemetry, None);
        assert_eq!(telemetry.packets_read(), 0);
        assert_eq!(telemetry.video_frames_presented(), 0);
    }
}
