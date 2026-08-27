//! Production adapters существующих local/direct/YtDlp preparation owners.

use std::sync::Arc;

use player_core::PreparedMedia;

use super::{
    ActiveMediaSource, MediaOpenSourceRequest, MediaPreparationFailureKind, NativeHlsOpenIntent,
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
        MediaOpenSourceRequest::NativeHls {
            source,
            intent,
            network_config,
            yt_dlp_config,
            demux_config,
            preferred_video_codec_order,
            preferred_video_height,
            system_capabilities,
            audio_capabilities,
        } => {
            let safe_label = source.safe_label().clone();
            let (expected_selection, fallback_locator) = match intent {
                NativeHlsOpenIntent::InitialWithYtDlpFallback { fallback_locator } => {
                    (None, Some(fallback_locator))
                }
                NativeHlsOpenIntent::ExactSelection(selection) => (Some(selection), None),
            };
            let mut port = crate::startup_media::native_hls::ProductionNativeHlsAdmissionPort::new(
                crate::startup_media::native_hls::NativeHlsPreparationRequest {
                    source: &source,
                    expected_selection: expected_selection.as_ref(),
                    network_config: &network_config,
                    demux_config: &demux_config,
                    preferred_video_codec_order: &preferred_video_codec_order,
                    preferred_video_height,
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
                            "Подготовка native HLS VOD завершилась ошибкой"
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
                    let active_source = ActiveMediaSource::NativeHlsUrl {
                        source,
                        selection: prepared.selection,
                    };
                    let prepared_media =
                        PreparedMedia::from_external_label(safe_label.as_str(), prepared.demuxer)
                            .with_worker_receipted_demux_seek_policy(
                            prepared.seek_port,
                            player_core::PreparedDemuxSeekLandingPolicy::AuthoritativePostTarget,
                        );
                    Ok(PreparedMediaOpen {
                        prepared_media,
                        descriptor: PreparedMediaDescriptor::NativeHls {
                            tracks,
                            duration,
                            metadata,
                            source: active_source,
                            safe_label,
                        },
                    })
                }
                crate::startup_media::native_hls::NativeHlsAttempt::RequiresYtDlpFallback(
                    reason,
                ) => {
                    let Some(locator) = fallback_locator else {
                        // Успешный native install намеренно не хранит extractor locator:
                        // exact reopen не имеет права молча сменить semantic stream.
                        tracing::warn!(
                            source = %safe_label,
                            ?reason,
                            "Exact native HLS reopen отклонён без extractor fallback"
                        );
                        return Err(MediaPreparationFailureKind::NativeHlsOpen);
                    };
                    tracing::info!(
                        source = %safe_label,
                        ?reason,
                        "Initial native HLS admission передан единственному YtDlp fallback"
                    );
                    prepare_source(
                        MediaOpenSourceRequest::YtDlp {
                            locator,
                            selection_intent:
                                crate::web_media_open::YtDlpCandidateOpenIntent::BestPlayable,
                            network_config,
                            yt_dlp_config,
                            demux_config,
                            preferred_video_codec_order,
                            system_capabilities,
                            audio_capabilities,
                        },
                        cancellation,
                    )
                }
            }
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
            let prepared_media = prepare_yt_dlp_player_media(
                safe_label.as_str(),
                prepared.demuxer,
                YtDlpPreparedMediaAttachments {
                    timeline_port: prepared.timeline_port,
                    demux_seek_port: prepared.demux_seek_port,
                    playback_window: prepared.playback_window,
                },
            )
            .map_err(|error| {
                tracing::warn!(
                    source = %safe_label,
                    error = %error,
                    "YtDlp timeline mode не прошёл PreparedMedia boundary"
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
                        composed_selection: prepared.composed_selection,
                        stream_configuration: Box::new(prepared.stream_configuration),
                        catalog_attachment: prepared.catalog_attachment,
                    },
                    safe_label,
                    vod_endpoint_recovery: prepared.vod_endpoint_recovery,
                },
            })
        }
    }
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
        MediaPreparationFailureKind::YtDlpOpen
    }
}

/// Именованный набор provider-neutral дополнений к уже открытому demuxer-у.
pub(crate) struct YtDlpPreparedMediaAttachments {
    /// Dynamic live/DVR timeline; static VOD оставляет поле пустым.
    pub(crate) timeline_port: Option<media_core::DynamicMediaTimelinePort>,

    /// Worker-receipted seek boundary для segmented static provider-а.
    pub(crate) demux_seek_port: Option<Arc<dyn player_core::PreparedDemuxSeekPort>>,

    /// Абсолютное source window для provider-а с ненулевым presentation origin.
    pub(crate) playback_window: Option<player_core::MediaPlaybackWindow>,
}

/// Собирает единый `PreparedMedia` до общего strong-install commit barrier-а.
pub(crate) fn prepare_yt_dlp_player_media(
    safe_label: &str,
    demuxer: Box<dyn media_core::Demuxer + Send>,
    attachments: YtDlpPreparedMediaAttachments,
) -> Result<PreparedMedia, player_core::PreparedMediaTimelineModeError> {
    // Разбираем именованный boundary один раз, чтобы все ingress-ы сохраняли один порядок.
    let YtDlpPreparedMediaAttachments {
        timeline_port,
        demux_seek_port,
        playback_window,
    } = attachments;
    // Базовый `PreparedMedia` получает уже открытый provider-neutral demuxer.
    let mut prepared = PreparedMedia::from_external_label(safe_label, demuxer);
    // Receipted seek прикрепляется до playback window и live-mode validation.
    if let Some(port) = demux_seek_port {
        prepared = prepared.with_worker_receipted_demux_seek(port);
    }
    // Static presentation window остаётся fallible pre-barrier подготовкой.
    if let Some(window) = playback_window {
        prepared = prepared.with_playback_window(window)?;
    }
    // Dynamic timeline устанавливается последней и fail-closed отвергает static-window конфликт.
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
    use std::sync::Arc;
    use std::time::Duration;

    use media_core::{
        DemuxSeekResult, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial,
        DynamicMediaTimelinePortGeneration, DynamicMediaTimelineState, MediaTagMetadata,
    };
    use player_core::PreparedMediaTimelineMode;

    use super::{
        YtDlpPreparedMediaAttachments, classify_yt_dlp_preparation_failure,
        merge_yt_dlp_playlist_metadata, prepare_yt_dlp_player_media, service_duration_for_timeline,
    };
    use crate::media_open::MediaPreparationFailureKind;
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
            Box::new(UnavailableFakeDemuxer),
            YtDlpPreparedMediaAttachments {
                timeline_port: Some(timeline_port),
                demux_seek_port: None,
                playback_window: None,
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
        let prepared = prepare_yt_dlp_player_media(
            "static segmented",
            Box::new(UnavailableFakeDemuxer),
            YtDlpPreparedMediaAttachments {
                timeline_port: None,
                demux_seek_port: Some(erased_seek_port),
                playback_window: Some(playback_window),
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
        let result = prepare_yt_dlp_player_media(
            "invalid mixed timeline",
            Box::new(UnavailableFakeDemuxer),
            YtDlpPreparedMediaAttachments {
                timeline_port: Some(timeline_port),
                demux_seek_port: None,
                playback_window: Some(playback_window),
            },
        );
        // Никакой mixed provider state не достигает Ready/authorize phase.
        assert!(result.is_err());
    }
}
