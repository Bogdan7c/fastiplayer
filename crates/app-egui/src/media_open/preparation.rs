//! Production adapters существующих local/direct/YtDlp preparation owners.

use std::sync::Arc;

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
                classify_yt_dlp_preparation_failure(&error)
            })?;
            if cancellation.is_cancelled() {
                return Err(MediaPreparationFailureKind::Cancelled);
            }
            let tracks = prepared.demuxer.tracks().to_vec();
            let demux_duration = prepared.demuxer.duration();
            let demux_metadata = prepared.demuxer.media_metadata().unwrap_or_default().tags;
            let playlist_duration = service_duration_for_timeline(
                prepared.timeline_port.as_ref(),
                prepared.playlist_metadata.duration(),
            );
            let (duration, metadata) = merge_yt_dlp_playlist_metadata(
                demux_duration,
                demux_metadata,
                prepared.playlist_metadata.title(),
                playlist_duration,
            );
            let prepared_media = prepare_yt_dlp_player_media(
                safe_label.as_str(),
                prepared.demuxer,
                prepared.timeline_port,
                prepared.demux_seek_port,
            )
            .map_err(|error| {
                tracing::warn!(
                    source = %safe_label,
                    error = %error,
                    "HLS live timeline не прошёл PreparedMedia boundary"
                );
                MediaPreparationFailureKind::YtDlpOpen
            })?;
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

/// Сохраняет точную typed причину component finalization через anyhow context chain.
fn classify_yt_dlp_preparation_failure(error: &anyhow::Error) -> MediaPreparationFailureKind {
    if error
        .downcast_ref::<crate::web_media_open::ComponentVariantFinalizationError>()
        .is_some()
    {
        MediaPreparationFailureKind::ComponentCatalogUnavailable
    } else {
        MediaPreparationFailureKind::YtDlpOpen
    }
}

/// Устанавливает live port до возврата candidate-а к общему commit barrier.
fn prepare_yt_dlp_player_media(
    safe_label: &str,
    demuxer: Box<dyn media_core::Demuxer + Send>,
    timeline_port: Option<media_core::DynamicMediaTimelinePort>,
    demux_seek_port: Option<Arc<dyn player_core::PreparedDemuxSeekPort>>,
) -> Result<PreparedMedia, player_core::PreparedMediaTimelineModeError> {
    let mut prepared = PreparedMedia::from_external_label(safe_label, demuxer);
    if let Some(port) = demux_seek_port {
        prepared = prepared.with_worker_receipted_demux_seek(port);
    }
    match timeline_port {
        Some(port) => prepared.with_dynamic_timeline(port),
        None => Ok(prepared),
    }
}

/// Service duration не превращает dynamic live candidate в finite media.
fn service_duration_for_timeline(
    timeline_port: Option<&media_core::DynamicMediaTimelinePort>,
    service_duration: Option<std::time::Duration>,
) -> Option<std::time::Duration> {
    if timeline_port.is_some() {
        None
    } else {
        service_duration
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
    use std::num::NonZeroU64;
    use std::time::Duration;

    use media_core::{
        DemuxSeekResult, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial,
        DynamicMediaTimelinePortGeneration, DynamicMediaTimelineState, MediaTagMetadata,
    };
    use player_core::PreparedMediaTimelineMode;

    use super::{
        classify_yt_dlp_preparation_failure, merge_yt_dlp_playlist_metadata,
        prepare_yt_dlp_player_media, service_duration_for_timeline,
    };
    use crate::media_open::MediaPreparationFailureKind;
    use crate::web_media_open::ComponentVariantFinalizationError;

    #[derive(Default)]
    struct LiveFakeDemuxer;

    impl Demuxer for LiveFakeDemuxer {
        fn tracks(&self) -> &[media_core::TrackInfo] {
            &[]
        }

        fn duration(&self) -> Option<Duration> {
            None
        }

        fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
            Ok(media_core::DemuxReadEvent::TemporarilyUnavailable(
                media_core::DemuxRetryHint::new(Duration::from_millis(1)).expect("test retry hint"),
            ))
        }

        fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            panic!("fake live demux seek is outside preparation test")
        }
    }

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

    #[test]
    fn typed_component_failure_survives_anyhow_context_without_string_parsing() {
        let typed_failures = [
            ComponentVariantFinalizationError::ComponentCatalogUnavailable,
            ComponentVariantFinalizationError::SemanticRematch(
                web_media_core::ComponentVariantError::LayoutMismatch,
            ),
            ComponentVariantFinalizationError::Installation(
                crate::web_media_stream_model::component_variants::ComponentVariantInstallationError::ActiveParentMismatch,
            ),
        ];

        for typed_failure in typed_failures {
            let error =
                anyhow::Error::new(typed_failure).context("наружный YtDlp preparation context");
            assert_eq!(
                classify_yt_dlp_preparation_failure(&error),
                MediaPreparationFailureKind::ComponentCatalogUnavailable
            );
        }
    }

    #[test]
    fn unrelated_ytdlp_failure_keeps_generic_classification() {
        let error = anyhow::anyhow!("обычная provider ошибка");

        assert_eq!(
            classify_yt_dlp_preparation_failure(&error),
            MediaPreparationFailureKind::YtDlpOpen
        );
    }

    #[test]
    fn live_timeline_is_installed_before_barrier_and_service_duration_stays_unknown() {
        let (timeline_port, _publisher) =
            media_core::dynamic_media_timeline(DynamicMediaTimelineInitial {
                port_generation: DynamicMediaTimelinePortGeneration::new(
                    NonZeroU64::new(1).expect("non-zero test generation"),
                ),
                source_epoch: DynamicMediaTimelineEpoch::new(0),
                state: DynamicMediaTimelineState::without_dvr(Duration::from_secs(30).into()),
            });
        assert_eq!(
            service_duration_for_timeline(Some(&timeline_port), Some(Duration::from_secs(3_600))),
            None
        );
        let prepared = prepare_yt_dlp_player_media(
            "live",
            Box::new(LiveFakeDemuxer),
            Some(timeline_port),
            None,
        )
        .expect("live timeline attaches before barrier");
        assert_eq!(prepared.duration(), None);
        assert!(matches!(
            prepared.timeline_mode(),
            PreparedMediaTimelineMode::Live { .. }
        ));
    }
}
