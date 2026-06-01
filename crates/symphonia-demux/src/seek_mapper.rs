use std::collections::HashMap;
use std::time::Duration;

use media_core::{
    DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, MediaDemuxError, MediaTime, TrackId,
    TrackInfo, TrackKind, TrackTimestamp,
};
use symphonia::core::errors::{Error as SymphoniaError, SeekErrorKind};
use symphonia::core::formats::{SeekMode as SymphoniaSeekMode, SeekTo, SeekedTo};

use crate::error::DemuxError;
use crate::symphonia_api::{duration_to_symphonia_time, media_time_base_from_symphonia};
use crate::track_mapper::TrackEntry;

/// Выбирает track для Symphonia seek: video предпочтительнее audio.
pub(crate) fn preferred_seek_track_id(tracks: &[TrackInfo]) -> Option<TrackId> {
    tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .or_else(|| tracks.iter().find(|track| track.kind == TrackKind::Audio))
        .map(|track| track.id)
}

/// Мапит neutral seek mode в Symphonia mode с сохранением demux contract-а.
pub(crate) fn symphonia_seek_mode(mode: DemuxSeekMode) -> SymphoniaSeekMode {
    match mode {
        DemuxSeekMode::Accurate | DemuxSeekMode::DecodePointBefore => SymphoniaSeekMode::Accurate,
        DemuxSeekMode::Preview => SymphoniaSeekMode::Coarse,
    }
}

/// Создаёт Symphonia time seek target из neutral request.
pub(crate) fn symphonia_seek_target(
    request: DemuxSeekRequest,
    seek_track_id: Option<TrackId>,
) -> SeekTo {
    SeekTo::Time {
        time: duration_to_symphonia_time(request.timestamp),
        track_id: seek_track_id.map(TrackId::get),
    }
}

/// Конвертирует `SeekedTo.actual_ts` в нейтральную timeline-позицию.
pub(crate) fn seeked_to_timeline_result(
    requested_position: Duration,
    seeked_to: SeekedTo,
    track_map: &HashMap<u32, TrackEntry>,
) -> DemuxSeekResult {
    let actual_track_id = TrackId::new(seeked_to.track_id);
    let actual_track_timestamp = track_map
        .get(&seeked_to.track_id)
        .and_then(|track_entry| {
            track_entry
                .time_base
                .and_then(media_time_base_from_symphonia)
        })
        .map(|time_base| {
            TrackTimestamp::new(actual_track_id, seeked_to.actual_ts.get(), time_base)
        });
    let actual_position = actual_track_timestamp
        .map(TrackTimestamp::to_media_time)
        .unwrap_or_else(|| MediaTime::from_duration(requested_position));

    DemuxSeekResult {
        requested_position: MediaTime::from_duration(requested_position),
        actual_position,
        actual_track_timestamp,
    }
}

/// Мапит Symphonia seek failures в typed neutral ошибки для player-core.
pub(crate) fn symphonia_seek_error_to_demux_error(error: SymphoniaError) -> anyhow::Error {
    match error {
        SymphoniaError::SeekError(SeekErrorKind::Unseekable)
        | SymphoniaError::SeekError(SeekErrorKind::ForwardOnly) => {
            MediaDemuxError::SeekUnavailable {
                reason: error.to_string(),
            }
            .into()
        }
        SymphoniaError::SeekError(_) => DemuxError::SeekFailed(error.to_string()).into(),
        other_error => DemuxError::Parse(other_error).into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use media_core::{
        DemuxSeekMode, DemuxSeekRequest, MediaDemuxError, TrackId, TrackInfo, TrackKind,
    };
    use symphonia::core::errors::{Error as SymphoniaError, SeekErrorKind};
    use symphonia::core::formats::{SeekMode as SymphoniaSeekMode, SeekTo, SeekedTo};
    use symphonia::core::units::{TimeBase, Timestamp};

    use super::{
        preferred_seek_track_id, seeked_to_timeline_result, symphonia_seek_error_to_demux_error,
        symphonia_seek_mode, symphonia_seek_target,
    };
    use crate::track_mapper::{TrackEntry, TrackEntryKind};

    fn track_info(track_id: u32, kind: TrackKind) -> TrackInfo {
        TrackInfo {
            id: TrackId::new(track_id),
            kind,
            codec_id: "test".to_string(),
            codec_private: None,
            time_base: None,
            duration: None,
            sample_rate: None,
            channels: None,
            video: None,
        }
    }

    fn track_entry(kind: TrackKind) -> TrackEntry {
        TrackEntry {
            kind: TrackEntryKind::Supported(kind),
            codec_id: "test".to_string(),
            codec_private: None,
            time_base: Some(TimeBase::try_new(1, 1_000).expect("valid time base")),
            sample_rate: None,
            channels: None,
        }
    }

    #[test]
    fn preferred_seek_track_uses_video_before_audio() {
        let tracks = vec![
            track_info(2, TrackKind::Audio),
            track_info(1, TrackKind::Video),
        ];

        assert_eq!(preferred_seek_track_id(&tracks), Some(TrackId::new(1)));
    }

    #[test]
    fn preferred_seek_track_falls_back_to_audio_when_video_is_absent() {
        let tracks = vec![track_info(7, TrackKind::Audio)];

        assert_eq!(preferred_seek_track_id(&tracks), Some(TrackId::new(7)));
    }

    #[test]
    fn seek_modes_preserve_decode_before_contract_and_preview_speed() {
        assert_eq!(
            symphonia_seek_mode(DemuxSeekMode::Accurate),
            SymphoniaSeekMode::Accurate
        );
        assert_eq!(
            symphonia_seek_mode(DemuxSeekMode::DecodePointBefore),
            SymphoniaSeekMode::Accurate
        );
        assert_eq!(
            symphonia_seek_mode(DemuxSeekMode::Preview),
            SymphoniaSeekMode::Coarse
        );
    }

    #[test]
    fn seek_target_keeps_selected_track_id_inside_symphonia_boundary() {
        let seek_target = symphonia_seek_target(
            DemuxSeekRequest::preview(Duration::from_millis(420)),
            Some(TrackId::new(9)),
        );

        match seek_target {
            SeekTo::Time { time, track_id } => {
                assert_eq!(time.as_millis(), 420);
                assert_eq!(track_id, Some(9));
            }
            SeekTo::Timestamp { .. } => {
                panic!("seek mapper должен строить time-based seek target")
            }
        }
    }

    #[test]
    fn seek_result_uses_actual_track_timestamp_when_time_base_exists() {
        let track_map = HashMap::from([(1, track_entry(TrackKind::Video))]);

        let result = seeked_to_timeline_result(
            Duration::from_secs(1),
            SeekedTo {
                track_id: 1,
                required_ts: Timestamp::new(1_000),
                actual_ts: Timestamp::new(250),
            },
            &track_map,
        );

        assert_eq!(
            result.requested_position,
            media_core::MediaTime::from_secs(1)
        );
        assert_eq!(
            result.actual_position,
            media_core::MediaTime::from_duration(Duration::from_millis(250))
        );
        assert_eq!(
            result
                .actual_track_timestamp
                .expect("actual timestamp должен быть")
                .units
                .get(),
            250
        );
    }

    #[test]
    fn seek_result_keeps_signed_actual_timestamp_and_normalizes_ui_position() {
        let track_map = HashMap::from([(1, track_entry(TrackKind::Video))]);

        let result = seeked_to_timeline_result(
            Duration::from_secs(1),
            SeekedTo {
                track_id: 1,
                required_ts: Timestamp::new(1_000),
                actual_ts: Timestamp::new(-25),
            },
            &track_map,
        );

        let actual_track_timestamp = result
            .actual_track_timestamp
            .expect("signed actual timestamp должен сохраняться для diagnostics");

        assert_eq!(actual_track_timestamp.units.get(), -25);
        assert_eq!(result.actual_position, media_core::MediaTime::ZERO);
    }

    #[test]
    fn unseekable_symphonia_error_maps_to_media_demux_error() {
        let error = symphonia_seek_error_to_demux_error(SymphoniaError::SeekError(
            SeekErrorKind::Unseekable,
        ));

        assert!(matches!(
            error.downcast_ref::<MediaDemuxError>(),
            Some(MediaDemuxError::SeekUnavailable { .. })
        ));
    }

    #[test]
    fn forward_only_symphonia_error_maps_to_neutral_seek_unavailable() {
        let error = symphonia_seek_error_to_demux_error(SymphoniaError::SeekError(
            SeekErrorKind::ForwardOnly,
        ));

        assert!(matches!(
            error.downcast_ref::<MediaDemuxError>(),
            Some(MediaDemuxError::SeekUnavailable { .. })
        ));
    }
}
