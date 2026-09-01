//! Production adapters существующих local/direct/YtDlp preparation owners.

use super::web::WebMediaOpenAdapterView;
use super::{
    MediaOpenSourceRequest, MediaPreparationFailureKind, NativeHlsOpenIntent,
    PreparedMediaDescriptor, PreparedMediaOpen, PreparedWebMediaAttachments,
    PreparedWebMediaEnvelope, PreparedWebMediaSeekAttachment, SafeMediaLabel, WebMediaOpenRequest,
    WebMediaOpenSettings, WebMediaSourceIntent, compose_prepared_web_media,
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
        MediaOpenSourceRequest::Web(request) => match request.into_adapter() {
            WebMediaOpenAdapterView::Direct {
                locator,
                network_config,
                demux_config,
            } => {
                let safe_label = SafeMediaLabel::from_service_safe_label(locator.safe_label());
                let opened = crate::startup_media::resolve_direct_media_startup_media(
                &locator,
                &network_config,
                &demux_config,
                cancellation.source_token(),
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
                let (demuxer, endpoint_recovery) = opened.into_runtime_parts();
                let prepared_media = compose_prepared_web_media(
                    safe_label.as_str(),
                    demuxer,
                    PreparedWebMediaAttachments::default(),
                )
                .expect("direct VOD has no conflicting timeline attachments");
                let source = WebMediaSourceIntent::direct(locator);
                Ok(PreparedMediaOpen {
                    prepared_media,
                    descriptor: PreparedMediaDescriptor::Web(PreparedWebMediaEnvelope::new(
                        tracks,
                        duration,
                        metadata,
                        source,
                        safe_label,
                        None,
                        Some(endpoint_recovery),
                    )),
                })
            }
            WebMediaOpenAdapterView::NativeHls {
                source,
                intent,
                settings,
            } => {
                let WebMediaOpenSettings {
                    network_config,
                    web_media_config,
                    yt_dlp_config,
                    demux_config,
                    preferred_video_codec_order,
                    system_capabilities,
                    audio_capabilities,
                } = settings;
                let safe_label = source.safe_label().clone();
                let (expected_selection, mut fallback_owner) = match intent {
                    NativeHlsOpenIntent::InitialWithYtDlpFallback { fallback_locator } => (
                        None,
                        super::native_fallback::NativeWebFallbackOwner::before_installed(
                            fallback_locator,
                        ),
                    ),
                    NativeHlsOpenIntent::SemanticSelection(selection) => (
                        Some(selection),
                        super::native_fallback::NativeWebFallbackOwner::installed(),
                    ),
                };
                let mut port =
                    crate::startup_media::native_hls::ProductionNativeHlsAdmissionPort::new(
                        crate::startup_media::native_hls::NativeHlsPreparationRequest {
                            source: &source,
                            expected_selection: expected_selection.as_ref(),
                            network_config: &network_config,
                            web_media_config: &web_media_config,
                            demux_config: &demux_config,
                            preferred_video_codec_order: &preferred_video_codec_order,
                            system_capabilities: &system_capabilities,
                            audio_capabilities,
                            start: web_media_hls::HlsVodStartIntent::Beginning,
                            cancellation: cancellation.source_token(),
                        },
                    );
                let attempt =
                    crate::startup_media::native_hls::NativeHlsAdmissionPort::prepare(&mut port)
                        .map_err(|error| {
                            tracing::warn!(
                                source = %safe_label,
                                error = %error,
                                "Подготовка native HLS завершилась ошибкой"
                            );
                            if cancellation.is_cancelled() {
                                MediaPreparationFailureKind::Cancelled
                            } else {
                                MediaPreparationFailureKind::NativeHlsOpen
                            }
                        })?;
                match attempt {
                    crate::startup_media::native_hls::NativeHlsAttempt::Prepared(prepared) => {
                        if cancellation.is_cancelled() {
                            return Err(MediaPreparationFailureKind::Cancelled);
                        }
                        let tracks = prepared.tracks().to_vec();
                        let duration = prepared.duration();
                        let metadata = prepared.demuxer.media_metadata().unwrap_or_default().tags;
                        let runtime_attachments =
                            prepared.lifecycle.into_web_attachments(prepared.seek_port);
                        let active_source = WebMediaSourceIntent::native_hls(
                            source,
                            runtime_attachments.presentation,
                            prepared.source_state,
                        );
                        let prepared_media = compose_prepared_web_media(
                            safe_label.as_str(),
                            prepared.demuxer,
                            runtime_attachments.prepared,
                        )
                        .expect("native HLS lifecycle produced compatible timeline attachments");
                        Ok(PreparedMediaOpen {
                            prepared_media,
                            descriptor: PreparedMediaDescriptor::Web(
                                PreparedWebMediaEnvelope::new(
                                    tracks,
                                    duration,
                                    metadata,
                                    active_source,
                                    safe_label,
                                    None,
                                    runtime_attachments.vod_endpoint_recovery,
                                ),
                            ),
                        })
                    }
                    crate::startup_media::native_hls::NativeHlsAttempt::RequiresExtractorFallback(
                        trigger,
                    ) => {
                        let fallback = fallback_owner.claim(trigger).map_err(|rejection| {
                            tracing::warn!(
                                source = %safe_label,
                                ?trigger,
                                ?rejection,
                                "Native HLS fallback отклонён единым lifecycle gate-ом"
                            );
                            MediaPreparationFailureKind::NativeHlsOpen
                        })?;
                        let (locator, invocation_reason) = fallback.into_parts();
                        if !yt_dlp_config.enabled {
                            tracing::warn!(
                                source = %safe_label,
                                ?invocation_reason,
                                "Native HLS fallback запрещён отключённым extractor-ом"
                            );
                            return Err(MediaPreparationFailureKind::NativeHlsOpen);
                        }
                        tracing::info!(
                            source = %safe_label,
                            ?invocation_reason,
                            "Initial native HLS admission передан единственному YtDlp fallback"
                        );
                        prepare_source(
                            MediaOpenSourceRequest::Web(WebMediaOpenRequest::extractor(
                                locator,
                                crate::web_media_open::YtDlpCandidateOpenIntent::BestPlayable,
                                invocation_reason,
                                WebMediaOpenSettings {
                                    network_config,
                                    web_media_config,
                                    yt_dlp_config,
                                    demux_config,
                                    preferred_video_codec_order,
                                    system_capabilities,
                                    audio_capabilities,
                                },
                            )),
                            cancellation,
                        )
                    }
                }
            }
            WebMediaOpenAdapterView::NativeDash {
                source,
                intent,
                settings,
            } => {
                let WebMediaOpenSettings {
                    network_config,
                    web_media_config,
                    yt_dlp_config,
                    demux_config,
                    preferred_video_codec_order,
                    system_capabilities,
                    audio_capabilities,
                } = settings;
                let safe_label = source.safe_label().clone();
                let (expected_selection, mut fallback_owner) = match intent {
                    super::NativeDashOpenIntent::InitialWithYtDlpFallback { fallback_locator } => (
                        None,
                        super::native_fallback::NativeWebFallbackOwner::before_installed(
                            fallback_locator,
                        ),
                    ),
                    super::NativeDashOpenIntent::SemanticSelection(selection) => (
                        Some(selection),
                        super::native_fallback::NativeWebFallbackOwner::installed(),
                    ),
                };
                let attempt = crate::startup_media::native_dash::prepare_native_dash_attempt(
                    crate::startup_media::native_dash::NativeDashPreparationRequest {
                        source: &source,
                        expected_selection: expected_selection.as_ref(),
                        network_config: &network_config,
                        web_media_config: &web_media_config,
                        demux_config: &demux_config,
                        system_capabilities: &system_capabilities,
                        audio_capabilities,
                        cancellation: cancellation.source_token(),
                    },
                )
                .map_err(|error| {
                    tracing::warn!(
                        source = %safe_label,
                        error = %error,
                        "Подготовка native DASH завершилась ошибкой"
                    );
                    if cancellation.is_cancelled() {
                        MediaPreparationFailureKind::Cancelled
                    } else {
                        MediaPreparationFailureKind::NativeDashOpen
                    }
                })?;
                match attempt {
                    crate::startup_media::native_dash::NativeDashAttempt::Prepared(prepared) => {
                        if cancellation.is_cancelled() {
                            return Err(MediaPreparationFailureKind::Cancelled);
                        }
                        let crate::startup_media::native_dash::PreparedNativeDashMedia {
                            demuxer,
                            seek_port,
                            source_state,
                            lifecycle,
                        } = prepared;
                        let tracks = demuxer.tracks().to_vec();
                        let duration = demuxer.duration();
                        let metadata = demuxer.media_metadata().unwrap_or_default().tags;
                        let runtime_attachments = lifecycle.into_web_attachments(seek_port);
                        let active_source = WebMediaSourceIntent::native_dash(
                            source,
                            runtime_attachments.presentation,
                            source_state,
                        );
                        let prepared_media = compose_prepared_web_media(
                            safe_label.as_str(),
                            demuxer,
                            runtime_attachments.prepared,
                        )
                        .map_err(|error| {
                            tracing::warn!(
                                source = %safe_label,
                                error = %error,
                                "Native DASH composition нарушила prepared attachment contract"
                            );
                            MediaPreparationFailureKind::NativeDashOpen
                        })?;
                        Ok(PreparedMediaOpen {
                            prepared_media,
                            descriptor: PreparedMediaDescriptor::Web(
                                PreparedWebMediaEnvelope::new(
                                    tracks,
                                    duration,
                                    metadata,
                                    active_source,
                                    safe_label,
                                    None,
                                    runtime_attachments.vod_endpoint_recovery,
                                ),
                            ),
                        })
                    }
                    crate::startup_media::native_dash::NativeDashAttempt::RequiresExtractorFallback(
                        trigger,
                    ) => {
                        let fallback = fallback_owner.claim(trigger).map_err(|rejection| {
                            tracing::warn!(
                                source = %safe_label,
                                ?trigger,
                                ?rejection,
                                "Native DASH fallback отклонён единым lifecycle gate-ом"
                            );
                            MediaPreparationFailureKind::NativeDashOpen
                        })?;
                        let (locator, invocation_reason) = fallback.into_parts();
                        if !yt_dlp_config.enabled {
                            tracing::warn!(
                                source = %safe_label,
                                ?invocation_reason,
                                "Initial native DASH fallback запрещён отключённым extractor-ом"
                            );
                            return Err(MediaPreparationFailureKind::NativeDashOpen);
                        }
                        tracing::info!(
                            source = %safe_label,
                            ?invocation_reason,
                            "Initial native DASH admission передан единственному YtDlp fallback"
                        );
                        prepare_source(
                            MediaOpenSourceRequest::Web(WebMediaOpenRequest::extractor(
                                locator,
                                crate::web_media_open::YtDlpCandidateOpenIntent::BestPlayable,
                                invocation_reason,
                                WebMediaOpenSettings {
                                    network_config,
                                    web_media_config,
                                    yt_dlp_config,
                                    demux_config,
                                    preferred_video_codec_order,
                                    system_capabilities,
                                    audio_capabilities,
                                },
                            )),
                            cancellation,
                        )
                    }
                }
            }
            WebMediaOpenAdapterView::NativeHds {
                source,
                intent,
                settings,
            } => super::native_hds_preparation::prepare_native_hds_source(
                source,
                intent,
                settings,
                cancellation,
            ),
            WebMediaOpenAdapterView::NativeSmooth {
                source,
                intent,
                settings,
            } => {
                let WebMediaOpenSettings {
                    network_config,
                    web_media_config,
                    yt_dlp_config,
                    demux_config,
                    preferred_video_codec_order,
                    system_capabilities,
                    audio_capabilities,
                } = settings;
                let safe_label = source.safe_label().clone();
                let (expected_selection, mut fallback_owner) = match intent {
                    super::NativeSmoothOpenIntent::InitialWithYtDlpFallback {
                        fallback_locator,
                    } => (
                        None,
                        super::native_fallback::NativeWebFallbackOwner::before_installed(
                            fallback_locator,
                        ),
                    ),
                    super::NativeSmoothOpenIntent::SemanticSelection(selection) => (
                        Some(selection),
                        super::native_fallback::NativeWebFallbackOwner::installed(),
                    ),
                };
                let attempt = crate::startup_media::native_smooth::prepare_native_smooth_attempt(
                    crate::startup_media::native_smooth::NativeSmoothPreparationRequest {
                        source: &source,
                        expected_selection: expected_selection.as_ref(),
                        network_config: &network_config,
                        web_media_config: &web_media_config,
                        demux_config: &demux_config,
                        system_capabilities: &system_capabilities,
                        audio_capabilities,
                        cancellation: cancellation.source_token(),
                    },
                )
                .map_err(|error| {
                    tracing::warn!(
                        source = %safe_label,
                        error = %error,
                        "Подготовка native Smooth завершилась ошибкой"
                    );
                    if cancellation.is_cancelled() {
                        MediaPreparationFailureKind::Cancelled
                    } else {
                        MediaPreparationFailureKind::NativeSmoothOpen
                    }
                })?;
                match attempt {
                    crate::startup_media::native_smooth::NativeSmoothAttempt::Prepared(
                        prepared,
                    ) => {
                        if cancellation.is_cancelled() {
                            return Err(MediaPreparationFailureKind::Cancelled);
                        }
                        let crate::startup_media::native_smooth::PreparedNativeSmoothMedia {
                            demuxer,
                            seek_port,
                            source_state,
                            endpoint_recovery,
                        } = prepared;
                        let tracks = demuxer.tracks().to_vec();
                        let duration = demuxer.duration();
                        let metadata = demuxer.media_metadata().unwrap_or_default().tags;
                        let active_source =
                            WebMediaSourceIntent::native_smooth(source, source_state);
                        let prepared_media = compose_prepared_web_media(
                            safe_label.as_str(),
                            demuxer,
                            PreparedWebMediaAttachments {
                                demux_seek: Some(
                                    PreparedWebMediaSeekAttachment::WorkerReceipted(seek_port),
                                ),
                                ..PreparedWebMediaAttachments::default()
                            },
                        )
                        .map_err(|error| {
                            tracing::warn!(
                                source = %safe_label,
                                error = %error,
                                "Native Smooth composition нарушила prepared attachment contract"
                            );
                            MediaPreparationFailureKind::NativeSmoothOpen
                        })?;
                        Ok(PreparedMediaOpen {
                            prepared_media,
                            descriptor: PreparedMediaDescriptor::Web(
                                PreparedWebMediaEnvelope::new(
                                    tracks,
                                    duration,
                                    metadata,
                                    active_source,
                                    safe_label,
                                    None,
                                    Some(endpoint_recovery),
                                ),
                            ),
                        })
                    }
                    crate::startup_media::native_smooth::NativeSmoothAttempt::RequiresExtractorFallback(
                        trigger,
                    ) => {
                        let fallback = fallback_owner.claim(trigger).map_err(|rejection| {
                            tracing::warn!(
                                source = %safe_label,
                                ?trigger,
                                ?rejection,
                                "Native Smooth fallback отклонён единым lifecycle gate-ом"
                            );
                            MediaPreparationFailureKind::NativeSmoothOpen
                        })?;
                        let (locator, invocation_reason) = fallback.into_parts();
                        if !yt_dlp_config.enabled {
                            tracing::warn!(
                                source = %safe_label,
                                ?invocation_reason,
                                "Initial native Smooth fallback запрещён отключённым extractor-ом"
                            );
                            return Err(MediaPreparationFailureKind::NativeSmoothOpen);
                        }
                        tracing::info!(
                            source = %safe_label,
                            ?invocation_reason,
                            "Initial native Smooth admission передан единственному YtDlp fallback"
                        );
                        prepare_source(
                            MediaOpenSourceRequest::Web(WebMediaOpenRequest::extractor(
                                locator,
                                crate::web_media_open::YtDlpCandidateOpenIntent::BestPlayable,
                                invocation_reason,
                                WebMediaOpenSettings {
                                    network_config,
                                    web_media_config,
                                    yt_dlp_config,
                                    demux_config,
                                    preferred_video_codec_order,
                                    system_capabilities,
                                    audio_capabilities,
                                },
                            )),
                            cancellation,
                        )
                    }
                }
            }
            WebMediaOpenAdapterView::Extractor {
                locator,
                selection_intent,
                invocation_reason,
                settings,
            } => {
                let WebMediaOpenSettings {
                    network_config,
                    web_media_config,
                    yt_dlp_config,
                    demux_config,
                    preferred_video_codec_order,
                    system_capabilities,
                    audio_capabilities,
                } = settings;
                let safe_label = SafeMediaLabel::from_service_safe_label(locator.safe_label());
                let prepared = crate::web_media_open::prepare_yt_dlp_web_media(
                &locator,
                &network_config,
                &web_media_config,
                &yt_dlp_config,
                &demux_config,
                &preferred_video_codec_order,
                &system_capabilities,
                audio_capabilities,
                selection_intent,
                invocation_reason,
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
                let demux_duration = prepared.playback_window.and_then(|window| {
                    window
                        .end_exclusive()
                        .and_then(|end| end.as_duration().checked_sub(window.start().as_duration()))
                });
                let demux_duration = demux_duration.or_else(|| prepared.demuxer.duration());
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
                let prepared_media = compose_prepared_web_media(
                    safe_label.as_str(),
                    prepared.demuxer,
                    PreparedWebMediaAttachments {
                        timeline_port: prepared.timeline_port,
                        demux_seek: prepared
                            .demux_seek_port
                            .map(PreparedWebMediaSeekAttachment::WorkerReceipted),
                        playback_window: prepared.playback_window,
                        initial_position: None,
                    },
                )
                .map_err(|error| {
                    tracing::warn!(
                        source = %safe_label,
                        error = %error,
                        "YtDlp timeline mode не прошёл PreparedMedia boundary"
                    );
                    MediaPreparationFailureKind::ExtractorOpen
                })?;
                let source = WebMediaSourceIntent::extractor(
                    locator,
                    prepared.presentation,
                    prepared.source_state,
                    prepared.extractor_reason,
                );
                Ok(PreparedMediaOpen {
                    prepared_media,
                    descriptor: PreparedMediaDescriptor::Web(PreparedWebMediaEnvelope::new(
                        tracks,
                        duration,
                        metadata,
                        source,
                        safe_label,
                        prepared.playback_window,
                        prepared.vod_endpoint_recovery,
                    )),
                })
            }
        },
    }
}

/// Выполняет тот же source preparation path синхронно для settings transaction.
///
/// Settings уже владеет app-side runtime fence, поэтому отдельный background
/// coordinator здесь не создаётся; physical adapter dispatch остаётся единым.
pub(crate) fn prepare_source_synchronously(
    source_request: MediaOpenSourceRequest,
) -> Result<PreparedMediaOpen, MediaPreparationFailureKind> {
    let cancellation = super::executor::PreparationCancellation::new();
    prepare_source(source_request, &cancellation)
}

/// Сохраняет typed component/DASH причину через anyhow context chain.
fn classify_yt_dlp_preparation_failure(error: &anyhow::Error) -> MediaPreparationFailureKind {
    if error
        .downcast_ref::<crate::web_media_open::ComponentVariantFinalizationError>()
        .is_some()
    {
        MediaPreparationFailureKind::ComponentCatalogUnavailable
    } else if let Some(dash_error) = error.downcast_ref::<dash_mpd_core::DashDynamicMpdError>() {
        match dash_error {
            dash_mpd_core::DashDynamicMpdError::ProfileExcluded(_) => {
                MediaPreparationFailureKind::DashLiveProfileExcluded
            }
            dash_mpd_core::DashDynamicMpdError::Schema(_) => {
                MediaPreparationFailureKind::DashLiveSchemaRejected
            }
        }
    } else {
        MediaPreparationFailureKind::ExtractorOpen
    }
}

/// Service duration не превращает dynamic live candidate в finite media.
pub(crate) fn service_duration_for_timeline(
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
pub(crate) fn merge_yt_dlp_playlist_metadata(
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
    use std::sync::Arc;
    use std::time::Duration;

    use media_core::{
        DemuxSeekResult, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial,
        DynamicMediaTimelinePortGeneration, DynamicMediaTimelineState, MediaTagMetadata,
    };
    use player_core::PreparedMediaTimelineMode;

    use super::{
        classify_yt_dlp_preparation_failure, merge_yt_dlp_playlist_metadata,
        service_duration_for_timeline,
    };
    use crate::media_open::{
        MediaPreparationFailureKind, PreparedWebMediaAttachments, PreparedWebMediaSeekAttachment,
        compose_prepared_web_media,
    };
    use crate::web_media_open::ComponentVariantFinalizationError;

    /// Fake demuxer моделирует provider readiness без привязки к VOD/live режиму.
    #[derive(Default)]
    struct UnavailableFakeDemuxer;

    impl Demuxer for UnavailableFakeDemuxer {
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

    /// Fake port нужен только для проверки ownership внутри общего S41 boundary.
    struct FakePreparedDemuxSeekPort;

    impl player_core::PreparedDemuxSeekPort for FakePreparedDemuxSeekPort {
        /// Integration helper не выполняет реальный seek во время preparation.
        fn enqueue_seek(
            &self,
            _request_id: player_core::PreparedDemuxSeekRequestId,
            _request: media_core::DemuxSeekRequest,
        ) -> Result<(), player_core::PreparedDemuxSeekEnqueueError> {
            // Вызов означал бы, что preparation незаконно исполняет post-install lifecycle.
            panic!("prepare boundary не должен выполнять demux seek")
        }

        /// До player install fake receipt отсутствует.
        fn poll_seek_receipt(&self) -> Option<player_core::PreparedDemuxSeekReceipt> {
            // `None` сохраняет nonblocking semantics production port-а.
            None
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
    fn typed_dash_profile_and_schema_failures_survive_anyhow_context() {
        let profile_error =
            anyhow::Error::new(dash_mpd_core::DashDynamicMpdError::ProfileExcluded(
                dash_mpd_core::DashDynamicProfileExclusion::UnsupportedDeclaredProfile,
            ))
            .context("наружный DASH live preparation context");
        assert_eq!(
            classify_yt_dlp_preparation_failure(&profile_error),
            MediaPreparationFailureKind::DashLiveProfileExcluded
        );

        let schema_error =
            dash_mpd_core::parse_dynamic_dash_mpd(dash_mpd_core::DashMpdParseRequest {
                document_bytes: b"<NotMpd/>",
                xml_budgets: bounded_xml_reader::XmlBudgets::builder()
                    .maximum_document_bytes(1_024)
                    .maximum_depth(8)
                    .maximum_tokens(32)
                    .maximum_attributes_per_element(8)
                    .maximum_attribute_count(16)
                    .maximum_attribute_bytes(512)
                    .maximum_namespace_declarations_per_element(4)
                    .maximum_namespace_declaration_count(8)
                    .maximum_namespace_bytes(256)
                    .maximum_text_bytes(512)
                    .build()
                    .expect("test XML budgets"),
                limits: dash_mpd_core::DashMpdLimits {
                    maximum_periods: 1,
                    maximum_adaptation_sets_per_period: 1,
                    maximum_representations_per_adaptation_set: 1,
                    maximum_segments_per_list: 1,
                    maximum_timeline_entries: 1,
                    maximum_schema_string_bytes: 256,
                },
            })
            .expect_err("invalid root должен дать schema error");
        let schema_error =
            anyhow::Error::new(schema_error).context("наружный DASH live preparation context");
        assert_eq!(
            classify_yt_dlp_preparation_failure(&schema_error),
            MediaPreparationFailureKind::DashLiveSchemaRejected
        );
    }

    #[test]
    fn unrelated_ytdlp_failure_keeps_generic_classification() {
        let error = anyhow::anyhow!("обычная provider ошибка");

        assert_eq!(
            classify_yt_dlp_preparation_failure(&error),
            MediaPreparationFailureKind::ExtractorOpen
        );
    }

    /// Cancelled neutral request обязан завершиться до adapter I/O и не менять error semantics.
    #[test]
    fn cancelled_web_request_stops_before_adapter_dispatch() {
        let cancellation = super::super::executor::PreparationCancellation::new();
        cancellation.cancel(player_core::MediaInstallCancellationCause::UserCancelled);
        let locator = crate::direct_progressive_open::classify_direct_media_url(
            "https://unreachable.example.test/cancelled-before-io.mp4",
        )
        .expect("direct fixture locator валиден");
        let request = crate::media_open::MediaOpenSourceRequest::Web(
            crate::media_open::WebMediaOpenRequest::direct(
                locator,
                rustiplayer_config::NetworkConfig::default(),
                rustiplayer_config::PlayerDemuxConfig::default(),
            ),
        );

        let result = super::prepare_source(request, &cancellation);

        assert!(matches!(
            result,
            Err(MediaPreparationFailureKind::Cancelled)
        ));
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
        let prepared = compose_prepared_web_media(
            "live",
            Box::new(UnavailableFakeDemuxer),
            PreparedWebMediaAttachments {
                timeline_port: Some(timeline_port),
                ..PreparedWebMediaAttachments::default()
            },
        )
        .expect("live timeline attaches before barrier");
        assert_eq!(prepared.duration(), None);
        assert!(matches!(
            prepared.timeline_mode(),
            PreparedMediaTimelineMode::Live { .. }
        ));
    }

    /// Static providers получают seek/window attachments через тот же intent-boundary.
    #[test]
    fn static_seek_and_playback_window_share_one_pre_barrier_prepared_media_path() {
        // Fake port остаётся снаружи, чтобы проверить передачу ownership без вызова I/O.
        let seek_port = Arc::new(FakePreparedDemuxSeekPort);
        // Trait-object clone имитирует concrete DASH/Smooth/HDS port.
        let erased_seek_port: Arc<dyn player_core::PreparedDemuxSeekPort> = seek_port.clone();
        // Ненулевой absolute origin моделирует HDS presentation window.
        let playback_window = player_core::MediaPlaybackWindow::new(
            Duration::from_secs(5).into(),
            Some(Duration::from_secs(12).into()),
        )
        .expect("static test window валидно");
        // Общий helper прикрепляет все static intents до strong-install barrier-а.
        let prepared = compose_prepared_web_media(
            "static segmented",
            Box::new(UnavailableFakeDemuxer),
            PreparedWebMediaAttachments {
                demux_seek: Some(PreparedWebMediaSeekAttachment::WorkerReceipted(
                    erased_seek_port,
                )),
                playback_window: Some(playback_window),
                ..PreparedWebMediaAttachments::default()
            },
        )
        .expect("static attachments совместимы");
        // Static source не должен случайно получить live timeline mode.
        assert!(matches!(
            prepared.timeline_mode(),
            PreparedMediaTimelineMode::Static { .. }
        ));
        // Window проходит в player без provider-specific timestamp rewriting.
        assert_eq!(prepared.playback_window(), Some(playback_window));
        // Один Arc остаётся у test owner-а, второй — внутри PreparedMedia.
        assert_eq!(Arc::strong_count(&seek_port), 2);
    }

    /// Static playback window и dynamic live mode остаются взаимно исключающимися.
    #[test]
    fn live_timeline_and_static_window_conflict_fails_before_strong_install_barrier() {
        // Dynamic port моделирует единственные Implemented live rows HLS/DASH.
        let (timeline_port, _publisher) =
            media_core::dynamic_media_timeline(DynamicMediaTimelineInitial {
                port_generation: DynamicMediaTimelinePortGeneration::new(
                    NonZeroU64::new(2).expect("non-zero test generation"),
                ),
                source_epoch: DynamicMediaTimelineEpoch::new(0),
                state: DynamicMediaTimelineState::without_dvr(Duration::from_secs(30).into()),
            });
        // Static window моделирует VOD-only HDS semantics.
        let playback_window = player_core::MediaPlaybackWindow::new(
            Duration::from_secs(5).into(),
            Some(Duration::from_secs(12).into()),
        )
        .expect("static test window валидно");
        // Конфликт обязан terminal-resolve как recoverable preparation error.
        let result = compose_prepared_web_media(
            "invalid mixed timeline",
            Box::new(UnavailableFakeDemuxer),
            PreparedWebMediaAttachments {
                timeline_port: Some(timeline_port),
                playback_window: Some(playback_window),
                ..PreparedWebMediaAttachments::default()
            },
        );
        // Никакой mixed provider state не достигает Ready/authorize phase.
        assert!(result.is_err());
    }
}
