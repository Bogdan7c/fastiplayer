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
///
/// Цель запроса — медиа-время; reader ищет во времени контейнера, поэтому сдвиг seek-дорожки
/// (Ogg Opus `pre_skip`) прибавляется здесь, симметрично вычитанию в packet/seek-результате.
pub(crate) fn symphonia_seek_target(
    request: DemuxSeekRequest,
    seek_track_id: Option<TrackId>,
    track_map: &HashMap<u32, TrackEntry>,
) -> SeekTo {
    let container_time = seek_track_id
        .and_then(|track_id| track_map.get(&track_id.get()))
        .map_or(request.timestamp, |track_entry| {
            track_entry
                .timestamp_shift
                .media_seek_time_to_container(request.timestamp, track_entry.time_base)
        });
    SeekTo::Time {
        time: duration_to_symphonia_time(container_time),
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
    let actual_track_timestamp = track_map.get(&seeked_to.track_id).and_then(|track_entry| {
        let time_base = track_entry
            .time_base
            .and_then(media_time_base_from_symphonia)?;
        // Reader сообщает приземление во времени контейнера; наружу — медиа-время, как у packet-ов.
        let media_timestamp = track_entry
            .timestamp_shift
            .container_to_media(seeked_to.actual_ts);
        Some(TrackTimestamp::new(
            actual_track_id,
            media_timestamp.get(),
            time_base,
        ))
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
    use crate::container_timestamp_shift::ContainerTimestampShift;
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
            labels: media_core::TrackLabels::default(),
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
            timestamp_shift: ContainerTimestampShift::NONE,
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
            &HashMap::new(),
        );

        match seek_target {
            SeekTo::Time { time, track_id } => {
                assert_eq!(time.as_millis(), 420);
                assert_eq!(track_id, Some(9));
            }
            SeekTo::Timestamp { .. } => {
                panic!("seek mapper build up time-based seek target")
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
                .expect("The actual timestamp should be")
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
            .expect("signed actual timestamp must be saved for diagnostics");

        assert_eq!(actual_track_timestamp.units.get(), -25);
        assert_eq!(result.actual_position, media_core::MediaTime::ZERO);
    }

    /// Сдвиг контейнера (Ogg Opus `pre_skip`) применяется симметрично: цель уходит в reader
    /// позже на сдвиг, а приземление возвращается в медиа-время. Иначе отчёт о seek-е
    /// расходился бы с pts packet-ов, уже переведённых в медиа-время.
    #[test]
    fn seek_target_and_result_apply_container_shift_symmetrically() {
        let mut shifted_audio = track_entry(TrackKind::Audio);
        shifted_audio.time_base = Some(TimeBase::try_new(1, 48_000).expect("valid time base"));
        shifted_audio.timestamp_shift = ContainerTimestampShift::from_leading_ticks(312);
        let track_map = HashMap::from([(3, shifted_audio)]);

        let seek_target = symphonia_seek_target(
            DemuxSeekRequest::accurate(Duration::from_secs(1)),
            Some(TrackId::new(3)),
            &track_map,
        );
        let SeekTo::Time { time, .. } = seek_target else {
            panic!("seek mapper builds time-based seek target");
        };
        assert_eq!(time.as_nanos(), 1_006_500_000);

        let result = seeked_to_timeline_result(
            Duration::from_secs(1),
            SeekedTo {
                track_id: 3,
                required_ts: Timestamp::new(48_312),
                actual_ts: Timestamp::new(48_312),
            },
            &track_map,
        );
        assert_eq!(
            result.actual_position,
            media_core::MediaTime::from_secs(1),
            "container landing 48312 is media time 1 s"
        );
        assert_eq!(
            result
                .actual_track_timestamp
                .expect("track timestamp")
                .units
                .get(),
            48_000
        );
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
