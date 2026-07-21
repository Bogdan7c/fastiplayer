//! Selected-track validation, public ID layout и merged static metadata.

use std::time::Duration;

use media_core::{Demuxer, MediaMetadata, TrackId, TrackInfo, TrackKind};

use super::{CompositeAvDemuxerError, CompositeComponent};

/// Возвращает exact selected track и проверяет component kind invariant.
pub(super) fn selected_track(
    demuxer: &dyn Demuxer,
    component: CompositeComponent,
    selected_track_id: TrackId,
    expected_kind: TrackKind,
) -> Result<TrackInfo, CompositeAvDemuxerError> {
    let track = demuxer
        .tracks()
        .iter()
        .find(|track| track.id == selected_track_id)
        .cloned()
        .ok_or(CompositeAvDemuxerError::SelectedTrackMissing {
            component,
            track_id: selected_track_id,
        })?;
    if track.kind != expected_kind {
        return Err(CompositeAvDemuxerError::SelectedTrackKindMismatch {
            component,
            track_id: selected_track_id,
            actual_kind: track.kind,
            expected_kind,
        });
    }
    Ok(track)
}

/// Сохраняет inner video ID и remap-ит только реальную collision audio ID.
pub(super) fn collision_safe_audio_track_id(
    video_track_id: TrackId,
    audio_track_id: TrackId,
) -> TrackId {
    if audio_track_id != video_track_id {
        return audio_track_id;
    }
    TrackId::new(audio_track_id.get().wrapping_add(1))
}

/// Строит visible track list с согласованными public IDs.
pub(super) fn remapped_tracks(
    mut video_track: TrackInfo,
    mut audio_track: TrackInfo,
    public_video_track_id: TrackId,
    public_audio_track_id: TrackId,
) -> Vec<TrackInfo> {
    video_track.id = public_video_track_id;
    audio_track.id = public_audio_track_id;
    vec![video_track, audio_track]
}

/// Сохраняет existing video-primary duration fallback order без behavior regression.
pub(super) fn composite_duration(
    video_track: &TrackInfo,
    audio_track: &TrackInfo,
    video_duration: Option<Duration>,
    audio_duration: Option<Duration>,
) -> Option<Duration> {
    video_track
        .duration
        .or(audio_track.duration)
        .or(video_duration)
        .or(audio_duration)
}

/// Video metadata primary; audio заполняет только отсутствующие container/tags.
pub(super) fn merge_media_metadata(
    video: Option<MediaMetadata>,
    audio: Option<MediaMetadata>,
) -> MediaMetadata {
    let mut merged = video.unwrap_or_default();
    if let Some(audio) = audio {
        if merged.container.is_none() {
            merged.container = audio.container;
        }
        merged.tags.fill_missing_from(audio.tags);
    }
    merged
}
