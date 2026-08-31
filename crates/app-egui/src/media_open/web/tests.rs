use super::*;

/// Absent optional attachments не должны превращаться в скрытые adapter defaults.
#[test]
fn direct_envelope_keeps_optional_attachments_absent() {
    let locator = service_direct_media::parse_direct_media_url(
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
    let locator = service_direct_media::parse_direct_media_url(
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
    let locator = service_direct_media::parse_direct_media_url(
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
