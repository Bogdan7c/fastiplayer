//! Production adapters существующих local/direct/YtDlp preparation owners.

use player_core::PreparedMedia;

use super::{
    ActiveMediaSource, MediaOpenSourceRequest, MediaPreparationFailureKind,
    PreparedMediaDescriptor, PreparedMediaOpen, SafeMediaLabel,
};

/// Выполняет ровно один source-specific preparation flow.
pub(super) fn prepare_source(
    source_request: MediaOpenSourceRequest,
    cancellation: &super::executor::PreparationCancellation,
) -> Result<PreparedMediaOpen, MediaPreparationFailureKind> {
    if cancellation.is_cancelled() {
        return Err(MediaPreparationFailureKind::Cancelled);
    }

    match source_request {
        MediaOpenSourceRequest::PlaybackWindow {
            source,
            semantic_identity,
        } => prepare_source(*source, cancellation)
            .map(|prepared| prepared.with_playback_window(semantic_identity)),
        MediaOpenSourceRequest::Local {
            path,
            expected_fingerprint,
            demux_config,
        } => {
            let safe_label = SafeMediaLabel::from_local_path(&path);
            super::local::prepare_local_open(
                &path,
                &demux_config,
                expected_fingerprint,
                cancellation.source_token(),
                || cancellation.is_cancelled(),
            )
            .map(super::local::PreparedLocalOpenResult::into_prepared_open)
            .map_err(|error| {
                tracing::warn!(source = %safe_label, error = %error, "Подготовка локального media завершилась ошибкой");
                match error {
                    super::local::PrepareLocalOpenError::Cancelled => {
                        MediaPreparationFailureKind::Cancelled
                    }
                    super::local::PrepareLocalOpenError::SourceChangedDuringPreparation => {
                        MediaPreparationFailureKind::LocalSourceChanged
                    }
                    _ => MediaPreparationFailureKind::LocalOpen,
                }
            })
        }
        MediaOpenSourceRequest::Direct {
            locator,
            network_config,
            demux_config,
        } => {
            let safe_label = SafeMediaLabel::from_service_safe_label(locator.safe_label());
            let opened = crate::startup_media::resolve_direct_media_startup_media(
                &locator,
                &network_config,
                &demux_config,
            )
            .map_err(|error| {
                tracing::warn!(source = %safe_label, error = %error, "Подготовка direct media завершилась ошибкой");
                MediaPreparationFailureKind::DirectOpen
            })?;
            if cancellation.is_cancelled() {
                return Err(MediaPreparationFailureKind::Cancelled);
            }
            let tracks = opened.tracks().to_vec();
            let duration = opened.duration();
            let metadata = opened.media_metadata().unwrap_or_default().tags;
            let prepared_media =
                PreparedMedia::from_external_label(safe_label.as_str(), opened.into_demuxer());
            Ok(PreparedMediaOpen {
                prepared_media,
                descriptor: PreparedMediaDescriptor::Direct {
                    tracks,
                    duration,
                    metadata,
                    source: ActiveMediaSource::DirectMediaUrl(locator),
                    safe_label,
                },
            })
        }
        MediaOpenSourceRequest::YtDlp {
            locator,
            selection_intent,
            network_config,
            yt_dlp_config,
            demux_config,
            preferred_video_codec_order,
            system_capabilities,
            audio_capabilities,
        } => {
            let safe_label = SafeMediaLabel::from_service_safe_label(locator.safe_label());
            let prepared = crate::web_media_open::prepare_yt_dlp_web_media(
                &locator,
                &network_config,
                &yt_dlp_config,
                &demux_config,
                &preferred_video_codec_order,
                &system_capabilities,
                audio_capabilities,
                selection_intent,
                cancellation.source_token(),
                || cancellation.is_cancelled(),
            )
            .map_err(|error| {
                tracing::warn!(source = %safe_label, error = %error, "Подготовка YtDlp media завершилась ошибкой");
                MediaPreparationFailureKind::YtDlpOpen
            })?;
            if cancellation.is_cancelled() {
                return Err(MediaPreparationFailureKind::Cancelled);
            }
            let tracks = prepared.demuxer.tracks().to_vec();
            let demux_duration = prepared.demuxer.duration();
            let demux_metadata = prepared.demuxer.media_metadata().unwrap_or_default().tags;
            let (duration, metadata) = merge_yt_dlp_playlist_metadata(
                demux_duration,
                demux_metadata,
                prepared.playlist_metadata.title(),
                prepared.playlist_metadata.duration(),
            );
            let prepared_media =
                PreparedMedia::from_external_label(safe_label.as_str(), prepared.demuxer);
            Ok(PreparedMediaOpen {
                prepared_media,
                descriptor: PreparedMediaDescriptor::YtDlp {
                    tracks,
                    duration,
                    metadata,
                    source: ActiveMediaSource::YtDlpUrl {
                        source_locator: locator,
                        candidate_selection: Box::new(prepared.candidate_selection),
                        stream_configuration: Box::new(prepared.stream_configuration),
                    },
                    safe_label,
                },
            })
        }
    }
}

/// Service title/duration заполняют только пробелы demux metadata и не стирают более полный snapshot.
fn merge_yt_dlp_playlist_metadata(
    demux_duration: Option<std::time::Duration>,
    mut demux_metadata: media_core::MediaTagMetadata,
    service_title: Option<&str>,
    service_duration: Option<std::time::Duration>,
) -> (Option<std::time::Duration>, media_core::MediaTagMetadata) {
    if demux_metadata
        .title
        .as_deref()
        .is_none_or(|title| title.trim().is_empty())
    {
        demux_metadata.title = service_title.map(ToOwned::to_owned);
    }
    (demux_duration.or(service_duration), demux_metadata)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use media_core::MediaTagMetadata;

    use super::merge_yt_dlp_playlist_metadata;

    #[test]
    fn yt_dlp_service_metadata_fills_missing_demux_values() {
        let (duration, metadata) = merge_yt_dlp_playlist_metadata(
            None,
            MediaTagMetadata::default(),
            Some("Настоящее YtDlp название"),
            Some(Duration::from_secs(90)),
        );

        assert_eq!(duration, Some(Duration::from_secs(90)));
        assert_eq!(metadata.title.as_deref(), Some("Настоящее YtDlp название"));
    }

    #[test]
    fn demux_metadata_remains_primary_when_already_known() {
        let demux_metadata = MediaTagMetadata {
            title: Some("Название из контейнера".to_string()),
            artists: vec!["Автор".to_string()],
            ..MediaTagMetadata::default()
        };
        let (duration, metadata) = merge_yt_dlp_playlist_metadata(
            Some(Duration::from_secs(91)),
            demux_metadata,
            Some("Название YtDlp"),
            Some(Duration::from_secs(90)),
        );

        assert_eq!(duration, Some(Duration::from_secs(91)));
        assert_eq!(metadata.title.as_deref(), Some("Название из контейнера"));
        assert_eq!(metadata.artists, ["Автор".to_string()]);
    }
}
