//! Чистое отображение player-core причин в пользовательские telemetry категории.

use player_core::PlayerVideoDropReason;

use crate::telemetry::{SeekDiscardReason, VideoDropReason, VideoFrameTelemetryEvent};

/// Классифицирует core-причину удаления кадра для пользовательской telemetry.
pub(super) fn map_video_frame_telemetry_event(
    reason: PlayerVideoDropReason,
) -> VideoFrameTelemetryEvent {
    match reason {
        PlayerVideoDropReason::Late => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::Late)
        }
        PlayerVideoDropReason::QueueOverflow => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::QueueOverflow)
        }
        PlayerVideoDropReason::Paused => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::Paused)
        }
        PlayerVideoDropReason::SeekPreroll => {
            VideoFrameTelemetryEvent::SeekDiscard(SeekDiscardReason::SeekPreroll)
        }
        PlayerVideoDropReason::StaleGeneration => {
            VideoFrameTelemetryEvent::SeekDiscard(SeekDiscardReason::StaleGeneration)
        }
        PlayerVideoDropReason::DecoderStarvation => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::DecoderStarvation)
        }
        PlayerVideoDropReason::RenderAcquisitionTimeout => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::Other)
        }
        PlayerVideoDropReason::PlaybackWindow => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::Other)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seek_reasons_stay_out_of_playback_drop_accounting() {
        assert_eq!(
            map_video_frame_telemetry_event(PlayerVideoDropReason::SeekPreroll),
            VideoFrameTelemetryEvent::SeekDiscard(SeekDiscardReason::SeekPreroll)
        );
        assert_eq!(
            map_video_frame_telemetry_event(PlayerVideoDropReason::StaleGeneration),
            VideoFrameTelemetryEvent::SeekDiscard(SeekDiscardReason::StaleGeneration)
        );
    }

    #[test]
    fn playback_reasons_keep_their_dedicated_categories() {
        assert_eq!(
            map_video_frame_telemetry_event(PlayerVideoDropReason::QueueOverflow),
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::QueueOverflow)
        );
        assert_eq!(
            map_video_frame_telemetry_event(PlayerVideoDropReason::DecoderStarvation),
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::DecoderStarvation)
        );
    }
}
