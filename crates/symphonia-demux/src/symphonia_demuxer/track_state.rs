//! Построение neutral track state из текущего Symphonia reader-а.
//!
//! Модуль читает только `FormatReader` и уже собранные подсказки (Matroska video metadata,
//! названия дорожек) и не владеет ни reader-ом, ни состоянием demuxer-а.

use std::collections::HashMap;
use std::time::Duration;

use media_core::{TrackId, TrackInfo};

use super::metadata::{
    display_orientations_from_metadata, video_color_metadata_from_metadata,
    video_packet_framings_from_metadata,
};
use crate::matroska_metadata::MatroskaVideoTrack;
use crate::symphonia_api::{self, FormatReaderBox};
use crate::track_mapper::{TrackEntry, map_tracks_with_video_metadata};

/// Снимок track state-а, который rebuild может проверить перед заменой reader-а.
pub(super) struct SymphoniaTrackState {
    pub(super) tracks: Vec<TrackInfo>,
    pub(super) duration: Option<Duration>,
    pub(super) track_map: HashMap<u32, TrackEntry>,
    pub(super) track_duration: Option<Duration>,
    pub(super) media_info_duration: Option<Duration>,
}

/// Читает container-level duration из Symphonia `MediaInfo`.
///
/// Symphonia 0.6 может хранить duration на media-level, даже если отдельные
/// tracks не несут `Track::duration`. Player timeline использует это только как
/// fallback, чтобы не терять seekable VOD UI при валидном seekable source-е.
fn media_info_duration(media_info: &symphonia::core::formats::MediaInfo) -> Option<Duration> {
    media_info
        .time_base
        .zip(media_info.duration)
        .map(|(time_base, duration)| {
            symphonia_api::symphonia_duration_to_duration(time_base, duration)
        })
        .filter(|duration| !duration.is_zero())
}

/// Строит neutral track state из текущего Symphonia reader-а без изменения public boundary.
pub(super) fn track_state_from_format_reader(
    format: &mut FormatReaderBox<'static>,
    video_tracks_by_track: &HashMap<TrackId, MatroskaVideoTrack>,
    track_titles_by_track: &HashMap<TrackId, String>,
) -> SymphoniaTrackState {
    let mut video_tracks_for_mapping = video_tracks_by_track.clone();
    let display_orientations_by_track = display_orientations_from_metadata(format);
    let color_metadata_by_track = video_color_metadata_from_metadata(format);
    let packet_framings_by_track = video_packet_framings_from_metadata(format);
    let track_mapping = map_tracks_with_video_metadata(
        format.tracks(),
        &mut video_tracks_for_mapping,
        &display_orientations_by_track,
        &color_metadata_by_track,
        &packet_framings_by_track,
        track_titles_by_track,
    );
    let media_info_duration = media_info_duration(format.media_info());
    let duration = track_mapping.duration.or(media_info_duration);

    SymphoniaTrackState {
        tracks: track_mapping.tracks,
        duration,
        track_map: track_mapping.track_map,
        track_duration: track_mapping.duration,
        media_info_duration,
    }
}

/// Формирует компактный diagnostics snapshot для случая, когда reprobe меняет public identity.
pub(super) fn track_layout_snapshot(tracks: &[TrackInfo], duration: Option<Duration>) -> String {
    let track_ids = tracks
        .iter()
        .map(|track| format!("{:?}:{:?}:{}", track.id, track.kind, track.codec_id))
        .collect::<Vec<_>>()
        .join(",");

    format!("duration={duration:?};tracks=[{track_ids}]")
}
