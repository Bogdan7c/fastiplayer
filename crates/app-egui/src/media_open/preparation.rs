//! Production adapters существующих local/direct/YouTube preparation owners.

use player_core::PreparedMedia;

use super::{
    ActiveMediaSource, MediaOpenSourceRequest, MediaPreparationFailureKind,
    PreparedMediaDescriptor, PreparedMediaOpen, SafeMediaLabel,
};

/// Выполняет ровно один source-specific preparation flow.
pub(super) fn prepare_source(
    source_request: MediaOpenSourceRequest,
    is_cancelled: impl Fn() -> bool,
) -> Result<PreparedMediaOpen, MediaPreparationFailureKind> {
    if is_cancelled() {
        return Err(MediaPreparationFailureKind::Cancelled);
    }

    match source_request {
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
                is_cancelled,
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
            if is_cancelled() {
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
        MediaOpenSourceRequest::YouTube {
            locator,
            network_config,
            youtube_config,
            demux_config,
            preferred_video_codec_order,
            system_capabilities,
        } => {
            let safe_label = SafeMediaLabel::from_service_safe_label(locator.safe_label());
            let prepared = crate::startup_media::resolve_youtube_startup_media(
                &locator,
                &network_config,
                &youtube_config,
                &demux_config,
                &preferred_video_codec_order,
                &system_capabilities,
            )
            .map_err(|error| {
                tracing::warn!(source = %safe_label, error = %error, "Подготовка YouTube media завершилась ошибкой");
                MediaPreparationFailureKind::YouTubeOpen
            })?;
            if is_cancelled() {
                return Err(MediaPreparationFailureKind::Cancelled);
            }
            let tracks = prepared.streaming_media.demuxer.tracks().to_vec();
            let duration = prepared.streaming_media.demuxer.duration();
            let metadata = prepared
                .streaming_media
                .demuxer
                .media_metadata()
                .unwrap_or_default()
                .tags;
            let prepared_media = PreparedMedia::from_external_label(
                safe_label.as_str(),
                prepared.streaming_media.demuxer,
            );
            Ok(PreparedMediaOpen {
                prepared_media,
                descriptor: PreparedMediaDescriptor::YouTube {
                    tracks,
                    duration,
                    metadata,
                    source: ActiveMediaSource::YouTubeUrl {
                        source_locator: locator,
                        selected_stream_identity: prepared.selected_stream_identity,
                    },
                    safe_label,
                },
            })
        }
    }
}
