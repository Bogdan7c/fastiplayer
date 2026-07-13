//! Объединение metadata двух независимых adaptive stream-ов.

use media_core::MediaMetadata;

/// Собирает единый snapshot: video является primary, audio заполняет только пропуски.
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

#[cfg(test)]
mod tests {
    use media_core::{
        DiscNumber, MediaContainerMetadata, MediaTagMetadata, TrackNumber, TvEpisodeNumber,
        TvSeasonNumber,
    };

    use super::*;

    #[test]
    fn video_metadata_remains_primary_and_audio_fills_sequence_gaps() {
        let video = MediaMetadata {
            container: Some(MediaContainerMetadata {
                format_name: Some("video/webm".into()),
            }),
            tags: MediaTagMetadata {
                title: Some("Video title".into()),
                disc_number: Some(DiscNumber::new(1)),
                tv_season_number: Some(TvSeasonNumber::new(2)),
                ..Default::default()
            },
        };
        let audio = MediaMetadata {
            container: Some(MediaContainerMetadata {
                format_name: Some("audio/webm".into()),
            }),
            tags: MediaTagMetadata {
                title: Some("Audio title".into()),
                artists: vec!["Audio artist".into()],
                album: Some("Audio album".into()),
                disc_number: Some(DiscNumber::new(99)),
                track_number: Some(TrackNumber::new(7)),
                tv_season_number: Some(TvSeasonNumber::new(99)),
                tv_episode_number: Some(TvEpisodeNumber::new(5)),
            },
        };

        let merged = merge_media_metadata(Some(video), Some(audio));

        assert_eq!(
            merged.container.and_then(|container| container.format_name),
            Some("video/webm".into())
        );
        assert_eq!(merged.tags.title.as_deref(), Some("Video title"));
        assert_eq!(merged.tags.artists, ["Audio artist"]);
        assert_eq!(merged.tags.album.as_deref(), Some("Audio album"));
        assert_eq!(merged.tags.disc_number, Some(DiscNumber::new(1)));
        assert_eq!(merged.tags.track_number, Some(TrackNumber::new(7)));
        assert_eq!(merged.tags.tv_season_number, Some(TvSeasonNumber::new(2)));
        assert_eq!(merged.tags.tv_episode_number, Some(TvEpisodeNumber::new(5)));
    }
}
