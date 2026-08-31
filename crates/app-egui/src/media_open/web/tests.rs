use super::*;

/// Absent optional attachments не должны превращаться в скрытые adapter defaults.
#[test]
fn direct_envelope_keeps_optional_attachments_absent() {
    let locator = crate::direct_progressive_open::classify_direct_media_url(
        "https://cdn.example.test/movie.mp4?token=descriptor-secret",
    )
    .expect("direct fixture locator валиден");
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
    let locator = crate::direct_progressive_open::classify_direct_media_url(
        "https://cdn.example.test/movie.mp4?token=reopen-secret",
    )
    .expect("direct fixture locator валиден");
    let source = WebMediaSourceIntent::direct(locator.clone());

    let request = source
        .controlled_reopen_request(
            rustiplayer_config::NetworkConfig::default(),
            rustiplayer_config::PlayerDemuxConfig::default(),
            None,
        )
        .expect("direct reopen не требует adaptive capabilities");

    assert_eq!(request.safe_label().as_str(), locator.safe_label());
    let WebMediaOpenAdapterView::Direct {
        locator: reopened_locator,
        ..
    } = request.into_adapter()
    else {
        panic!("neutral request должен сохранить direct adapter intent");
    };
    assert_eq!(reopened_locator, locator);
}

/// Debug active source показывает neutral facts, но никогда не раскрывает locator material.
#[test]
fn active_web_source_debug_redacts_raw_locator_and_temporary_material() {
    let locator = crate::direct_progressive_open::classify_direct_media_url(
        "https://user:password@cdn.example.test/movie.mp4?token=debug-secret",
    )
    .expect("direct fixture locator валиден");
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
        crate::direct_progressive_open::classify_direct_media_url(
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
    let policy = web_media_hls::NativeHlsSelectionPolicy::new(
        web_media_core::PreferredHeightPolicy::NoPreference,
        vec![web_media_core::CodecFamily::H264],
    )
    .unwrap();
    let native_selection = web_media_hls::admit_native_hls_vod(
        b"#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nsegment.ts\n#EXT-X-ENDLIST\n",
        &native_target,
        hls_playlist_core::HlsParserLimits::default(),
        &policy,
        None,
    )
    .unwrap();
    let native = WebMediaSourceIntent::native_hls_vod(
        NativeHlsUrl::new(
            native_target,
            SafeMediaLabel::from_service_safe_label("media.example.test"),
        ),
        native_selection,
    );
    let native_projection = native.read_only_projection();
    assert_eq!(
        native_projection.ingress,
        WebMediaIngressKind::NativeManifest
    );
    assert!(native_projection.stream_configuration.is_none());
    assert!(native.catalog_attachment().is_none());

    let debug = format!("{direct_projection:?} {native_projection:?}");
    for secret in ["password", "direct-secret", "native-secret", "master.m3u8"] {
        assert!(!debug.contains(secret));
    }
    assert!(debug.contains("<safe-label>"));
}

#[test]
fn installed_only_action_and_unchanged_direct_settings_are_inert() {
    let source = WebMediaSourceIntent::direct(
        crate::direct_progressive_open::classify_direct_media_url(
            "https://cdn.example.test/movie.mp4?token=direct-secret",
        )
        .expect("direct fixture locator валиден"),
    );
    let app_config = rustiplayer_config::AppConfig::default();
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
        panic!("explicit direct rebuild обязан вернуть neutral reopen request");
    };
    let label = request.safe_label().to_string();
    assert!(label.contains("cdn.example.test"));
    assert!(!label.contains("direct-secret"));
}
