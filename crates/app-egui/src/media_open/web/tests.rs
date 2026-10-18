use super::*;

#[path = "tests/native_cross_source_playlist.rs"]
mod native_cross_source_playlist;
#[path = "tests/native_dash_live_vertical.rs"]
mod native_dash_live_vertical;
#[path = "tests/native_dash_vertical.rs"]
mod native_dash_vertical;
#[path = "tests/native_hds_vertical.rs"]
mod native_hds_vertical;
#[path = "tests/native_hls_lifecycle_n14b.rs"]
mod native_hls_lifecycle_n14b;
#[path = "tests/native_hls_live_vertical.rs"]
mod native_hls_live_vertical;
#[path = "tests/native_hls_vertical.rs"]
mod native_hls_vertical;
#[path = "tests/native_smooth_vertical.rs"]
mod native_smooth_vertical;
#[path = "tests/stream_subtitles_dash_vertical.rs"]
pub(crate) mod stream_subtitles_dash_vertical;
#[path = "tests/stream_subtitles_vertical.rs"]
pub(crate) mod stream_subtitles_vertical;

/// Absent optional attachments не должны превращаться в скрытые adapter defaults.
#[test]
fn direct_envelope_keeps_optional_attachments_absent() {
    let locator = media_source_open::direct_progressive_open::classify_direct_media_url(
        "https://cdn.example.test/movie.mp4?token=descriptor-secret",
    )
    .expect("Direct fixture locator is valid");
    let source = WebMediaSourceIntent::direct(locator);
    let envelope = PreparedWebMediaEnvelope::new(
        Vec::new(),
        None,
        MediaTagMetadata::default(),
        source,
        SafeMediaLabel::from_service_safe_label("cdn.example.test"),
        None,
        None,
    );

    assert!(envelope.tracks().is_empty());
    assert_eq!(envelope.duration(), None);
    assert!(envelope.vod_endpoint_recovery().is_none());
    assert_eq!(envelope.safe_label().as_str(), "cdn.example.test");
    assert!(matches!(
        envelope.active_source(),
        crate::media_open::ActiveMediaSource::Web(_)
    ));
}

/// Controlled reopen переносит stable direct intent через neutral request variant.
#[test]
fn controlled_reopen_preserves_stable_direct_selection() {
    let locator = media_source_open::direct_progressive_open::classify_direct_media_url(
        "https://cdn.example.test/movie.mp4?token=reopen-secret",
    )
    .expect("Direct fixture locator is valid");
    let source = WebMediaSourceIntent::direct(locator.clone());

    let request = source
        .controlled_reopen_request(
            fastiplayer_config::NetworkConfig::default(),
            fastiplayer_config::PlayerDemuxConfig::default(),
            None,
        )
        .expect("Direct reopen requires adaptive capabilities.");

    assert_eq!(request.safe_label().as_str(), locator.safe_label());
    let WebMediaOpenAdapterView::Direct {
        locator: reopened_locator,
        ..
    } = request.into_adapter()
    else {
        panic!("Neutral request should be retained by direct adapter intent");
    };
    assert_eq!(reopened_locator, locator);
}

/// Native fallback обязан донести exact product reason до extractor adapter request-а.
#[test]
fn extractor_request_preserves_explicit_native_fallback_reason() {
    let locator = service_ytdlp::parse_yt_dlp_media_locator(
        "https://provider.example.test/watch?id=reason-secret",
    )
    .expect("valid extractor locator");
    let app_config = fastiplayer_config::AppConfig::default();
    let settings = WebMediaOpenSettings::from_app_config(
        &app_config,
        &capability_core::SystemCapabilities::empty(0),
        audio::AudioDecodeCapabilitySnapshot::empty(),
    );
    let request = WebMediaOpenRequest::extractor(
        locator,
        crate::web_media_open::YtDlpCandidateOpenIntent::BestPlayable,
        ExtractorInvocationReason::NativeProfileCompatibilityFallback,
        settings,
    );

    let WebMediaOpenAdapterView::Extractor {
        invocation_reason, ..
    } = request.into_adapter()
    else {
        panic!("native fallback request must remain extractor adapter");
    };
    assert_eq!(
        invocation_reason,
        ExtractorInvocationReason::NativeProfileCompatibilityFallback
    );
}

/// Debug active source показывает neutral facts, но никогда не раскрывает locator material.
#[test]
fn active_web_source_debug_redacts_raw_locator_and_temporary_material() {
    let locator = media_source_open::direct_progressive_open::classify_direct_media_url(
        "https://user:password@cdn.example.test/movie.mp4?token=debug-secret",
    )
    .expect("Direct fixture locator is valid");
    let source = WebMediaSourceIntent::direct(locator);

    let debug = format!("{:?}", crate::media_open::ActiveMediaSource::Web(source));

    assert!(debug.contains("DirectResource"));
    assert!(!debug.contains("password"));
    assert!(!debug.contains("debug-secret"));
    assert!(!debug.contains("movie.mp4"));
}

#[test]
fn direct_and_native_read_only_projections_are_neutral_and_secret_safe() {
    let direct = WebMediaSourceIntent::direct(
        media_source_open::direct_progressive_open::classify_direct_media_url(
            "https://user:password@cdn.example.test/movie.mp4?token=direct-secret",
        )
        .unwrap(),
    );
    let direct_projection = direct.read_only_projection();
    assert_eq!(
        direct_projection.ingress,
        WebMediaIngressKind::DirectResource
    );
    assert!(direct_projection.stream_configuration.is_none());
    assert!(direct.catalog_attachment().is_none());
    assert!(!direct_projection.source_label.contains("password"));
    assert!(!direct_projection.source_label.contains("direct-secret"));

    let native_target = source_core::HttpRequestTarget::parse_exact(
        "https://media.example.test/master.m3u8?token=native-secret",
    )
    .unwrap();
    let native_source = NativeHlsUrl::new(
        native_target,
        SafeMediaLabel::from_service_safe_label("media.example.test"),
    );
    let native_source_identity = native_source.source_identity();
    let native_parent = web_media_core::ExactSelectionIdentity::new(
        web_media_core::CandidateIdentity::new(
            native_source_identity,
            web_media_core::ExtractionGeneration::new(1),
            web_media_core::CandidateFormatIdentity::new("native-hls-vod").unwrap(),
        ),
        web_media_core::SemanticIdentity::new(native_source_identity, "native-hls-vod").unwrap(),
    )
    .unwrap();
    let native_source_state = NativeHlsSourceState::new(
        web_media_core::WebMediaSelection::candidate(native_parent),
        None,
        crate::web_media_stream_model::WebMediaSelectionPreference::from_global_config(
            &fastiplayer_config::WebMediaConfig::default(),
        ),
    )
    .unwrap();
    let native = WebMediaSourceIntent::native_hls(
        native_source,
        WebMediaPresentationKind::Vod,
        native_source_state,
    );
    let native_projection = native.read_only_projection();
    assert_eq!(
        native_projection.ingress,
        WebMediaIngressKind::NativeManifest
    );
    assert!(native_projection.stream_configuration.is_some());
    assert!(native.catalog_attachment().is_some());

    let debug = format!("{direct_projection:?} {native_projection:?}");
    for secret in ["password", "direct-secret", "native-secret", "master.m3u8"] {
        assert!(!debug.contains(secret));
    }
    assert!(debug.contains("<safe-label>"));
}

#[test]
fn installed_only_action_and_unchanged_direct_settings_are_inert() {
    let source = WebMediaSourceIntent::direct(
        media_source_open::direct_progressive_open::classify_direct_media_url(
            "https://cdn.example.test/movie.mp4?token=direct-secret",
        )
        .expect("Direct fixture locator is valid"),
    );
    let app_config = fastiplayer_config::AppConfig::default();
    let settings = WebMediaOpenSettings::from_app_config(
        &app_config,
        &capability_core::SystemCapabilities::empty(0),
        audio::AudioDecodeCapabilitySnapshot::empty(),
    );

    assert!(matches!(
        source.selection_switch_request(
            WebMediaSelectionSwitchIntent::CatalogTarget(
                crate::web_media_catalog::WebMediaSelectionTarget::InstalledOnly,
            ),
            settings.clone(),
        ),
        WebMediaSelectionSwitchResolution::NoChange
    ));

    let inert_policy = WebMediaSettingsReconfigurePolicy {
        direct_resource: DirectResourceSettingsAction::KeepInstalled,
        selection: WebMediaSettingsSelectionPolicy::PreserveInstalled,
    };
    assert!(!source.requires_settings_reconfigure(inert_policy));
    assert!(matches!(
        source.settings_reconfigure_request(
            inert_policy,
            app_config.network.clone(),
            app_config.player.demux,
            settings.clone(),
        ),
        WebMediaSettingsReconfigureDecision::NoChange
    ));

    let WebMediaSettingsReconfigureDecision::Reopen(request) = source.settings_reconfigure_request(
        WebMediaSettingsReconfigurePolicy {
            direct_resource: DirectResourceSettingsAction::Rebuild,
            selection: WebMediaSettingsSelectionPolicy::PreserveInstalled,
        },
        app_config.network,
        app_config.player.demux,
        settings,
    ) else {
        panic!("Explicitly direct rebuild must return the neutral reopen request.");
    };
    let label = request.safe_label().to_string();
    assert!(label.contains("cdn.example.test"));
    assert!(!label.contains("direct-secret"));
}

/// Транспорт субтитров с одной дорожкой: проверяет только доставку источника через intent.
struct OneTrackSubtitleTransport;

impl media_source_open::stream_subtitles::StreamSubtitleTransport for OneTrackSubtitleTransport {
    fn tracks(&self) -> Vec<media_source_open::stream_subtitles::StreamSubtitleTrackInfo> {
        vec![
            media_source_open::stream_subtitles::StreamSubtitleTrackInfo {
                id: media_source_open::stream_subtitles::StreamSubtitleTrackId::new(0),
                language: Some("en".into()),
                name: "English".into(),
                forced: false,
                default: true,
                autoselect: true,
            },
        ]
    }

    fn load_index(
        &self,
        _track: media_source_open::stream_subtitles::StreamSubtitleTrackId,
        _cancellation: &source_core::CancellationToken,
    ) -> Result<
        media_source_open::stream_subtitles::StreamSegmentIndex,
        media_source_open::stream_subtitles::StreamSubtitleError,
    > {
        Err(media_source_open::stream_subtitles::StreamSubtitleError::Stale)
    }

    fn fetch_and_decode(
        &self,
        _segment: &media_source_open::stream_subtitles::StreamSegment,
        _limits: media_source_open::stream_subtitles::StreamSegmentLimits,
        _cancellation: &source_core::CancellationToken,
    ) -> Result<
        media_source_open::stream_subtitles::SegmentDecode,
        media_source_open::stream_subtitles::StreamSubtitleError,
    > {
        Err(media_source_open::stream_subtitles::StreamSubtitleError::Stale)
    }
}

#[test]
fn native_hls_intent_hands_out_the_subtitle_source_of_its_own_open() {
    let target = source_core::HttpRequestTarget::parse_exact(
        "https://media.example.test/master.m3u8?token=native-secret",
    )
    .unwrap();
    let source = NativeHlsUrl::new(
        target,
        SafeMediaLabel::from_service_safe_label("media.example.test"),
    );
    let identity = source.source_identity();
    let parent = web_media_core::ExactSelectionIdentity::new(
        web_media_core::CandidateIdentity::new(
            identity,
            web_media_core::ExtractionGeneration::new(1),
            web_media_core::CandidateFormatIdentity::new("native-hls-vod").unwrap(),
        ),
        web_media_core::SemanticIdentity::new(identity, "native-hls-vod").unwrap(),
    )
    .unwrap();
    let build_state = || {
        NativeHlsSourceState::new(
            web_media_core::WebMediaSelection::candidate(parent.clone()),
            None,
            crate::web_media_stream_model::WebMediaSelectionPreference::from_global_config(
                &fastiplayer_config::WebMediaConfig::default(),
            ),
        )
        .unwrap()
    };
    let subtitles = media_source_open::stream_subtitles::StreamSubtitleSource::new(
        std::sync::Arc::new(OneTrackSubtitleTransport),
    )
    .expect("one track");

    let with_subtitles = WebMediaSourceIntent::native_hls(
        source.clone(),
        WebMediaPresentationKind::Vod,
        build_state().with_stream_subtitle_source(Some(subtitles.clone())),
    );
    assert_eq!(with_subtitles.stream_subtitle_source(), Some(subtitles));
    let without_subtitles =
        WebMediaSourceIntent::native_hls(source, WebMediaPresentationKind::Vod, build_state());
    assert_eq!(without_subtitles.stream_subtitle_source(), None);

    let direct = WebMediaSourceIntent::direct(
        media_source_open::direct_progressive_open::classify_direct_media_url(
            "https://cdn.example.test/movie.mp4",
        )
        .expect("Direct fixture locator is valid"),
    );
    assert_eq!(direct.stream_subtitle_source(), None);
}
