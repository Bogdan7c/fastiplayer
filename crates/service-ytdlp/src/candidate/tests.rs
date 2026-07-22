use rustiplayer_config::YtDlpConfig;
use serde_json::{Value, json};
use source_core::{CancellationToken, HttpRequestTarget};
use web_media_core::{
    DynamicRange, ExtractionGeneration, ProfileExclusionReason, SourceIdentity,
    StaticCompatibilityRejection, StreamLayout, StreamLayoutKind,
};
use web_media_transport_api::{SecretRequestPurpose, SourceGeneration, TransportProviderId};

use super::model::{
    YtDlpCandidateComponentRole, YtDlpCandidateEntry, YtDlpCandidateMatchKind,
    YtDlpCandidateNormalizationRejection, YtDlpCandidateRematchError, YtDlpCandidateSelectionError,
    YtDlpNormalizedCandidate,
};
use super::normalize::normalize_candidate_document;
use super::raw::YtDlpCandidateDocument;
use super::request_material::YtDlpRequestMaterialViolation;
use crate::{YtDlpServiceError, parse_yt_dlp_media_locator};

/// Парсит synthetic JSON тем же DTO boundary, что production process output.
fn snapshot(payload: Value, generation: u64) -> super::YtDlpCandidateSnapshot {
    let document: YtDlpCandidateDocument =
        serde_json::from_value(payload).expect("synthetic candidate JSON валиден");
    normalize_candidate_document(
        document,
        SourceIdentity::new(41),
        ExtractionGeneration::new(generation),
    )
}

/// Возвращает accepted inventory row по ordinal.
fn accepted_inventory(
    snapshot: &super::YtDlpCandidateSnapshot,
    ordinal: usize,
) -> &YtDlpNormalizedCandidate {
    snapshot.inventory()[ordinal]
        .accepted()
        .expect("candidate должен быть accepted")
}

/// Возвращает rejection reason по ordinal.
fn rejected_inventory(
    snapshot: &super::YtDlpCandidateSnapshot,
    ordinal: usize,
) -> &YtDlpCandidateNormalizationRejection {
    snapshot.inventory()[ordinal]
        .rejected()
        .expect("candidate должен быть rejected")
        .reason()
}

/// Создаёт базовый progressive format без secrets.
fn progressive_format(
    format_id: &str,
    extension: &str,
    container: &str,
    video_codec: &str,
    audio_codec: &str,
) -> Value {
    json!({
        "format_id": format_id,
        "url": format!("https://media.invalid/{format_id}"),
        "protocol": "https",
        "ext": extension,
        "container": container,
        "vcodec": video_codec,
        "acodec": audio_codec,
        "dynamic_range": "SDR"
    })
}

/// MP4/WebM/M4A и audio-only rows сохраняют исходную shape без pairing-а.
#[test]
fn progressive_inventory_maps_muxed_video_only_and_audio_only_rows() {
    let snapshot = snapshot(
        json!({
            "formats": [
                progressive_format("mp4-muxed", "mp4", "mp4", "avc1.640028", "mp4a.40.2"),
                progressive_format("webm-video", "webm", "webm", "vp09.00.51.08", "none"),
                progressive_format("m4a-audio", "m4a", "m4a", "none", "mp4a.40.2"),
                progressive_format("opus-audio", "opus", "ogg", "none", "opus")
            ]
        }),
        1,
    );

    assert_eq!(snapshot.inventory().len(), 4);
    assert_eq!(
        accepted_inventory(&snapshot, 0)
            .descriptor()
            .layout()
            .kind(),
        StreamLayoutKind::Muxed
    );
    assert_eq!(
        accepted_inventory(&snapshot, 1)
            .descriptor()
            .layout()
            .kind(),
        StreamLayoutKind::VideoOnly
    );
    assert_eq!(
        accepted_inventory(&snapshot, 2)
            .descriptor()
            .layout()
            .kind(),
        StreamLayoutKind::AudioOnly
    );
    assert_eq!(
        accepted_inventory(&snapshot, 3)
            .descriptor()
            .layout()
            .kind(),
        StreamLayoutKind::AudioOnly
    );
    assert!(
        accepted_inventory(&snapshot, 0)
            .descriptor()
            .subtitles()
            .is_empty()
    );
}

/// Disabled adapter отказывает до process spawn и не смешивает config с mapper-ом.
#[test]
fn disabled_candidate_snapshot_resolver_fails_before_process_spawn() {
    let locator = parse_yt_dlp_media_locator("https://media.invalid/watch")
        .expect("synthetic locator валиден");
    let config = YtDlpConfig {
        enabled: false,
        ..YtDlpConfig::default()
    };

    let error = super::resolve_yt_dlp_candidate_snapshot_with_config(
        &locator,
        SourceIdentity::new(1),
        ExtractionGeneration::new(1),
        &config,
    )
    .expect_err("disabled adapter не должен запускать process");
    assert!(matches!(error, YtDlpServiceError::AdapterDisabled));
}

/// Все target rows из canonical S00 format corpus проходят static normalization.
#[test]
fn canonical_s00_target_rows_are_normalized_without_silent_drops() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../compatibility/2026.07.04/fixtures/official-synthetic/format-inventory.json"
    ))
    .expect("checked-in S00 fixture валиден");
    let snapshot = snapshot(fixture["payload"].clone(), 1);

    let rejected_ordinals: Vec<_> = snapshot
        .inventory()
        .iter()
        .enumerate()
        .filter_map(|(ordinal, entry)| entry.rejected().map(|_| ordinal))
        .collect();
    assert_eq!(snapshot.inventory().len(), 13);
    assert!(
        rejected_ordinals.is_empty(),
        "S00 target rows rejected at ordinals {rejected_ordinals:?}"
    );
}

/// `requested_formats` создаёт Separate только для exact video+audio merge.
#[test]
fn selected_compound_uses_exact_components_without_cartesian_inventory() {
    let snapshot = snapshot(
        json!({
            "format_id": "video-new+audio-new",
            "requested_formats": [
                progressive_format("audio-new", "opus", "ogg", "none", "opus"),
                progressive_format("video-new", "webm", "webm", "vp9", "none")
            ],
            "formats": [
                progressive_format("video-a", "webm", "webm", "vp9", "none"),
                progressive_format("video-b", "webm", "webm", "vp9", "none"),
                progressive_format("audio-a", "opus", "ogg", "none", "opus"),
                progressive_format("audio-b", "opus", "ogg", "none", "opus")
            ]
        }),
        2,
    );

    assert_eq!(
        snapshot.inventory().len(),
        4,
        "pair combinations не создаются"
    );
    let selected = snapshot
        .selected()
        .and_then(YtDlpCandidateEntry::accepted)
        .expect("compound selected result accepted");
    assert_eq!(
        selected.descriptor().layout().kind(),
        StreamLayoutKind::Separate
    );
    assert_eq!(selected.component_count(), 2);
    assert_eq!(
        selected
            .component_request_summaries()
            .map(|summary| summary.role)
            .collect::<Vec<_>>(),
        vec![
            YtDlpCandidateComponentRole::Video,
            YtDlpCandidateComponentRole::Audio
        ]
    );
}

/// Обычный selected result не заменяется single-row `requested_formats` wrapper-ом.
#[test]
fn ordinary_selected_result_remains_one_root_component() {
    let root = progressive_format("root-selected", "mp4", "mp4", "h264", "aac");
    let requested_wrapper = progressive_format("wrapper-row", "webm", "webm", "vp9", "none");
    let mut payload = root;
    payload["requested_formats"] = json!([requested_wrapper]);
    payload["formats"] = json!([]);
    let snapshot = snapshot(payload, 2);

    let selected = snapshot
        .selected()
        .and_then(YtDlpCandidateEntry::accepted)
        .expect("root selected result accepted");
    assert_eq!(selected.component_count(), 1);
    assert_eq!(
        selected.descriptor().layout().kind(),
        StreamLayoutKind::Muxed
    );
    assert_eq!(
        selected
            .component_request_summaries()
            .next()
            .expect("single component summary")
            .role,
        YtDlpCandidateComponentRole::Muxed
    );
}

/// Missing/lying hints не угадываются и остаются typed outcomes.
#[test]
fn missing_and_lying_hints_are_not_silently_inferred() {
    let mut missing_protocol = progressive_format("missing-protocol", "mp4", "mp4", "h264", "aac");
    missing_protocol
        .as_object_mut()
        .expect("format object")
        .remove("protocol");
    let lying_codec = progressive_format("lying-codec", "mp4", "mp4", "opus", "none");
    let mut unknown_range = progressive_format("unknown-range", "webm", "webm", "vp9", "none");
    unknown_range["dynamic_range"] = json!("HDR-like marketing text");
    let lying_container = progressive_format("lying-container", "mp4", "mpegts", "h264", "aac");
    let snapshot = snapshot(
        json!({"formats": [missing_protocol, lying_codec, unknown_range, lying_container]}),
        3,
    );

    assert!(matches!(
        rejected_inventory(&snapshot, 0),
        YtDlpCandidateNormalizationRejection::Static(
            StaticCompatibilityRejection::InvalidMetadata { .. }
        )
    ));
    assert!(matches!(
        rejected_inventory(&snapshot, 1),
        YtDlpCandidateNormalizationRejection::Static(
            StaticCompatibilityRejection::InvalidMetadata { .. }
        )
    ));
    assert_eq!(
        video_dynamic_range(accepted_inventory(&snapshot, 2)),
        DynamicRange::Unknown
    );
    assert!(matches!(
        rejected_inventory(&snapshot, 3),
        YtDlpCandidateNormalizationRejection::Static(
            StaticCompatibilityRejection::ContainerHintsConflict { .. }
        )
    ));
}

/// Versioned request material сохраняет все approved S00 field shapes.
#[test]
fn hls_request_material_preserves_approved_shape_in_safe_summary() {
    let snapshot = snapshot(
        json!({
            "formats": [{
                "format_id": "hls-request-material",
                "url": "https://manifest.invalid/media.m3u8?token=secret-url",
                "manifest_url": "https://manifest.invalid/master.m3u8?token=secret-manifest",
                "protocol": "m3u8_native",
                "ext": "mp4",
                "container": "m4a_dash",
                "vcodec": "avc1.640028",
                "acodec": "mp4a.40.2",
                "fragment_base_url": "https://segments.invalid/private/",
                "fragments": [
                    {"path": "init.mp4"},
                    {"url": "https://segments.invalid/private/one.m4s", "duration": 4.0}
                ],
                "hls_media_playlist_data": "#EXTM3U\n#EXT-X-ENDLIST\n",
                "http_headers": {"Authorization": "Bearer top-secret"},
                "cookies": "session=top-secret",
                "extra_param_to_segment_url": "segment_secret=1",
                "extra_param_to_key_url": "key_secret=1",
                "hls_aes": {
                    "uri": "https://keys.invalid/private.key",
                    "key": "top-secret-key",
                    "iv": "top-secret-iv"
                }
            }]
        }),
        4,
    );

    let candidate = accepted_inventory(&snapshot, 0);
    let summary = candidate
        .component_request_summaries()
        .next()
        .expect("single component summary");
    assert_eq!(summary.material.fragment_count, 2);
    assert!(summary.material.has_url);
    assert!(summary.material.has_manifest_url);
    assert!(summary.material.has_fragment_base_url);
    assert!(summary.material.has_inline_hls);
    assert_eq!(summary.material.header_count, 1);
    assert!(summary.material.has_cookies);
    assert!(summary.material.has_segment_query);
    assert!(summary.material.has_key_query);
    assert!(summary.material.has_hls_aes);

    let diagnostic = format!("{snapshot:?}");
    for secret in [
        "secret-url",
        "secret-manifest",
        "top-secret",
        "segment_secret",
        "key_secret",
        "private.key",
    ] {
        assert!(!diagnostic.contains(secret), "Debug раскрыл secret marker");
    }
}

/// request_data и impersonation не маскируются как playable material.
#[test]
fn excluded_request_data_and_impersonation_remain_visible_rejections() {
    let mut request_data = progressive_format("request-data", "mp4", "mp4", "h264", "aac");
    request_data["request_data"] = json!("serialized-body-secret");
    let mut impersonation = progressive_format("impersonation", "mp4", "mp4", "h264", "aac");
    impersonation["impersonate"] = json!("chrome-136:windows-10");
    let snapshot = snapshot(json!({"formats": [request_data, impersonation]}), 5);

    assert_eq!(
        rejected_inventory(&snapshot, 0),
        &YtDlpCandidateNormalizationRejection::RequestMaterial(
            YtDlpRequestMaterialViolation::RequestDataRequired
        )
    );
    assert_eq!(
        rejected_inventory(&snapshot, 1),
        &YtDlpCandidateNormalizationRejection::RequestMaterial(
            YtDlpRequestMaterialViolation::ImpersonationRequired
        )
    );
    let diagnostic = format!("{snapshot:?}");
    assert!(!diagnostic.contains("serialized-body-secret"));
    assert!(!diagnostic.contains("chrome-136"));
}

/// Текущая YouTube shape с `http_chunk_size = 10 MiB` остаётся playable.
#[test]
fn youtube_http_chunk_size_becomes_neutral_range_request_limit() {
    let mut youtube_format =
        progressive_format("youtube-2026-07-04", "webm", "webm", "vp9", "opus");
    youtube_format["downloader_options"] = json!({"http_chunk_size": 10 * 1024 * 1024});
    let snapshot = snapshot(json!({"formats": [youtube_format]}), 13);

    let candidate = accepted_inventory(&snapshot, 0);
    let material_summary = candidate
        .component_request_summaries()
        .next()
        .expect("single YouTube component")
        .material;
    assert_eq!(
        material_summary.http_range_request_limit_bytes,
        Some(10 * 1024 * 1024)
    );

    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("progressive-http").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );
    let request = candidate
        .transport_components(&context)
        .expect("safe YouTube downloader hint должен пройти transport boundary")
        .into_iter()
        .next()
        .expect("single YouTube component")
        .into_request();
    assert_eq!(
        request
            .http_range_request_limit()
            .expect("typed range request limit")
            .maximum_bytes(),
        10 * 1024 * 1024
    );
}

/// Неизвестный downloader state и невалидные chunk limits остаются fail-closed.
#[test]
fn downloader_options_reject_unknown_state_and_invalid_http_chunk_sizes() {
    let mut unknown_option = progressive_format("unknown-option", "webm", "webm", "vp9", "opus");
    unknown_option["downloader_options"] = json!({"future_private_state": true});
    let mut mixed_options = progressive_format("mixed-options", "webm", "webm", "vp9", "opus");
    mixed_options["downloader_options"] =
        json!({"http_chunk_size": 10 * 1024 * 1024, "ws": "private-state"});
    let mut zero_chunk = progressive_format("zero-chunk", "webm", "webm", "vp9", "opus");
    zero_chunk["downloader_options"] = json!({"http_chunk_size": 0});
    let mut fractional_chunk =
        progressive_format("fractional-chunk", "webm", "webm", "vp9", "opus");
    fractional_chunk["downloader_options"] = json!({"http_chunk_size": 1.5});
    let mut oversized_chunk = progressive_format("oversized-chunk", "webm", "webm", "vp9", "opus");
    oversized_chunk["downloader_options"] = json!({"http_chunk_size": 65 * 1024 * 1024});
    let snapshot = snapshot(
        json!({
            "formats": [
                unknown_option,
                mixed_options,
                zero_chunk,
                fractional_chunk,
                oversized_chunk
            ]
        }),
        14,
    );

    for index in 0..=1 {
        assert_eq!(
            rejected_inventory(&snapshot, index),
            &YtDlpCandidateNormalizationRejection::RequestMaterial(
                YtDlpRequestMaterialViolation::DownloaderStateRequired
            )
        );
    }
    for index in 2..=4 {
        assert_eq!(
            rejected_inventory(&snapshot, index),
            &YtDlpCandidateNormalizationRejection::RequestMaterial(
                YtDlpRequestMaterialViolation::InvalidHttpChunkSize
            )
        );
    }
    assert!(!format!("{snapshot:?}").contains("private-state"));
}

/// Duplicate format ID не удаляется и не становится вторым selectable candidate-ом.
#[test]
fn duplicate_format_identity_is_a_visible_rejection() {
    let snapshot = snapshot(
        json!({
            "formats": [
                progressive_format("duplicate", "webm", "webm", "vp9", "none"),
                progressive_format("duplicate", "webm", "webm", "vp9", "none")
            ]
        }),
        6,
    );

    assert!(snapshot.inventory()[0].accepted().is_some());
    assert_eq!(
        rejected_inventory(&snapshot, 1),
        &YtDlpCandidateNormalizationRejection::DuplicateFormatIdentity
    );
}

/// HDR policy читает только typed field и не угадывает range по codec/label.
#[test]
fn hdr_hint_policy_preserves_sdr_hdr_and_unknown_buckets() {
    let sdr = progressive_format("sdr", "webm", "webm", "vp9", "none");
    let mut hdr = progressive_format("hdr", "webm", "webm", "vp09.02.10.10", "none");
    hdr["dynamic_range"] = json!("HDR10");
    let mut missing = progressive_format("missing-range", "webm", "webm", "vp9", "none");
    missing
        .as_object_mut()
        .expect("format object")
        .remove("dynamic_range");
    let snapshot = snapshot(json!({"formats": [sdr, hdr, missing]}), 7);

    assert_eq!(
        video_dynamic_range(accepted_inventory(&snapshot, 0)),
        DynamicRange::Sdr
    );
    assert_eq!(
        video_dynamic_range(accepted_inventory(&snapshot, 1)),
        DynamicRange::Hdr
    );
    assert_eq!(
        video_dynamic_range(accepted_inventory(&snapshot, 2)),
        DynamicRange::Unknown
    );
}

/// Exact ID stale в старом generation, но semantic attributes rematch новый ID.
#[test]
fn exact_selection_stales_locally_and_semantically_rematches_after_reextraction() {
    let original = snapshot(
        json!({"formats": [progressive_format("old-id", "webm", "webm", "vp9", "none")]}),
        8,
    );
    let selection = original
        .selection_for(accepted_inventory(&original, 0))
        .expect("candidate принадлежит original inventory");

    let stale_same_generation = snapshot(
        json!({"formats": [progressive_format("other-id", "webm", "webm", "vp9", "none")]}),
        8,
    );
    assert_eq!(
        stale_same_generation
            .rematch_exact(&selection)
            .expect_err("same-generation missing ID должен быть stale"),
        YtDlpCandidateRematchError::StaleExactIdentity
    );

    let refreshed = snapshot(
        json!({"formats": [progressive_format("new-id", "webm", "webm", "vp9", "none")]}),
        9,
    );
    let matched = refreshed
        .rematch_exact(&selection)
        .expect("semantic attributes совпадают");
    assert_eq!(matched.kind(), YtDlpCandidateMatchKind::SemanticRematch);
    assert_eq!(
        original
            .selection_for(accepted_inventory(&refreshed, 0))
            .expect_err("foreign generation нельзя превратить в local selection"),
        YtDlpCandidateSelectionError::ForeignGeneration
    );

    let mut changed_height = progressive_format("new-id-2", "webm", "webm", "vp9", "none");
    changed_height["height"] = json!(720);
    let changed = snapshot(json!({"formats": [changed_height]}), 10);
    assert_eq!(
        changed
            .rematch_exact(&selection)
            .expect_err("изменённые semantic attributes не rematch-ятся"),
        YtDlpCandidateRematchError::StaleExactIdentity
    );

    let ambiguous = snapshot(
        json!({
            "formats": [
                progressive_format("semantic-a", "webm", "webm", "vp9", "none"),
                progressive_format("semantic-b", "webm", "webm", "vp9", "none")
            ]
        }),
        11,
    );
    assert_eq!(
        ambiguous
            .rematch_exact(&selection)
            .expect_err("semantic duplicate не должен выбираться по source order"),
        YtDlpCandidateRematchError::AmbiguousSemanticIdentity
    );
}

/// Unknown transport и explicit DRM сохраняются как profile-visible rows.
#[test]
fn unknown_and_profile_excluded_candidates_remain_visible() {
    let mut unknown = progressive_format("unknown", "mp4", "mp4", "h264", "aac");
    unknown["protocol"] = json!("future_transport_v2");
    let mut drm = progressive_format("drm", "mp4", "mp4", "h264", "aac");
    drm["has_drm"] = json!(true);
    let snapshot = snapshot(json!({"formats": [unknown, drm]}), 11);

    assert!(matches!(
        rejected_inventory(&snapshot, 0),
        YtDlpCandidateNormalizationRejection::Static(
            StaticCompatibilityRejection::UnknownTransport { .. }
        )
    ));
    assert_eq!(
        rejected_inventory(&snapshot, 1),
        &YtDlpCandidateNormalizationRejection::Static(
            StaticCompatibilityRejection::ProfileExcluded {
                reason: ProfileExclusionReason::Drm
            }
        )
    );
}

/// Достаёт video dynamic range из любой video-bearing shape.
fn video_dynamic_range(candidate: &YtDlpNormalizedCandidate) -> DynamicRange {
    match candidate.descriptor().layout() {
        StreamLayout::Muxed(component) => component.video().dynamic_range(),
        StreamLayout::Separate { video, .. } | StreamLayout::VideoOnly(video) => {
            video.video().dynamic_range()
        }
        StreamLayout::AudioOnly(_) => panic!("audio-only candidate не имеет dynamic range"),
    }
}

/// S23 mapping сохраняет exact candidate identity и строит один neutral public request.
#[test]
fn planning_and_transport_share_the_same_exact_candidate() {
    let snapshot = snapshot(
        json!({
            "formats": [progressive_format(
                "muxed-webm",
                "webm",
                "webm",
                "vp9",
                "opus"
            )]
        }),
        7,
    );
    let candidate = accepted_inventory(&snapshot, 0);
    let planning = snapshot
        .planning_snapshot()
        .expect("public progressive candidate должен планироваться");
    assert_eq!(planning.candidates().len(), 1);
    assert_eq!(
        planning.candidates()[0].descriptor().identity(),
        candidate.descriptor().identity()
    );

    let provider = TransportProviderId::new("progressive-http").expect("provider ID");
    let context = super::YtDlpTransportRequestContext::new(
        provider,
        SourceGeneration::new(1),
        CancellationToken::new(),
    );
    let components = candidate
        .transport_components(&context)
        .expect("public component должен стать neutral request");
    assert_eq!(components.len(), 1);
    assert_eq!(
        components[0].role(),
        web_media_transport_api::MediaComponentRole::Muxed
    );
    assert_eq!(
        components[0].container(),
        web_media_core::ContainerFamily::WebM
    );
    let request = components
        .into_iter()
        .next()
        .expect("single component")
        .into_request();
    assert!(request.secrets().is_empty());
}

/// Queue metadata и exact candidate должны происходить из одного extraction snapshot-а.
#[test]
fn candidate_snapshot_keeps_playlist_metadata_with_its_generation() {
    let snapshot = snapshot(
        json!({
            "title": "  Название из extractor-а  ",
            "duration": 42.25,
            "formats": [progressive_format(
                "metadata-webm",
                "webm",
                "webm",
                "vp9",
                "opus"
            )]
        }),
        12,
    );

    assert_eq!(
        snapshot.playlist_metadata().title(),
        Some("Название из extractor-а")
    );
    assert_eq!(
        snapshot.playlist_metadata().duration(),
        Some(std::time::Duration::from_millis(42_250))
    );
    assert_eq!(snapshot.generation(), ExtractionGeneration::new(12));
}

/// S26 маппит effective Authorization/Cookie state в один scoped secret context.
#[test]
fn transport_maps_authorized_material_with_origin_path_and_secure_scope() {
    let mut protected = progressive_format("protected", "webm", "webm", "vp9", "opus");
    protected["http_headers"] = json!({
        "Authorization": "Bearer secret",
        "Cookie": "session=cookie-secret"
    });
    let snapshot = snapshot(json!({"formats": [protected]}), 1);
    let candidate = accepted_inventory(&snapshot, 0);
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("progressive-http").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );

    let request = candidate
        .transport_components(&context)
        .expect("serialized auth должен стать scoped request")
        .into_iter()
        .next()
        .expect("single protected component")
        .into_request();
    let material = request
        .secrets()
        .material_for(request.target(), SecretRequestPurpose::PrimaryResource)
        .expect("initial target находится в собственном scope");
    assert_eq!(material.headers_for_request().len(), 1);
    assert_eq!(material.headers_for_request()[0].name, "Authorization");
    assert_eq!(material.headers_for_request()[0].value, "Bearer secret");
    assert_eq!(
        material.cookies_for_request(),
        Some(b"session=cookie-secret".as_slice())
    );

    let same_path_child = HttpRequestTarget::parse_exact("https://media.invalid/protected/segment")
        .expect("same-path target");
    let sibling_path = HttpRequestTarget::parse_exact("https://media.invalid/private")
        .expect("sibling-path target");
    let cross_origin = HttpRequestTarget::parse_exact("https://cdn.invalid/protected")
        .expect("cross-origin target");
    let downgrade =
        HttpRequestTarget::parse_exact("http://media.invalid/protected").expect("downgrade target");
    assert!(
        request
            .secrets()
            .material_for(&same_path_child, SecretRequestPurpose::PrimaryResource)
            .is_some()
    );
    assert!(
        request
            .secrets()
            .material_for(&sibling_path, SecretRequestPurpose::PrimaryResource)
            .is_none()
    );
    assert!(
        request
            .secrets()
            .material_for(&cross_origin, SecretRequestPurpose::PrimaryResource)
            .is_none()
    );
    assert!(
        request
            .secrets()
            .material_for(&downgrade, SecretRequestPurpose::PrimaryResource)
            .is_none()
    );

    let diagnostic = format!("{snapshot:?} {request:?}");
    assert!(!diagnostic.contains("Bearer secret"));
    assert!(!diagnostic.contains("cookie-secret"));
}

/// Fresh extraction строит новый auth context и не наследует старый cookie jar.
#[test]
fn refresh_reextraction_replaces_serialized_authorization_state() {
    let protected_format = |generation_cookie: &str| {
        let mut protected = progressive_format("protected", "webm", "webm", "vp9", "opus");
        protected["cookies"] = json!(format!("session={generation_cookie}"));
        protected
    };
    let first = snapshot(json!({"formats": [protected_format("first-secret")]}), 1);
    let refreshed = snapshot(json!({"formats": [protected_format("second-secret")]}), 2);
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("progressive-http").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );

    let serialized_cookie = |candidate: &YtDlpNormalizedCandidate| {
        let request = candidate
            .transport_components(&context)
            .expect("protected component maps")
            .into_iter()
            .next()
            .expect("single component")
            .into_request();
        request
            .secrets()
            .material_for(request.target(), SecretRequestPurpose::PrimaryResource)
            .and_then(|material| material.cookies_for_request().map(ToOwned::to_owned))
            .expect("serialized cookie exists")
    };

    assert_eq!(
        serialized_cookie(accepted_inventory(&first, 0)),
        b"session=first-secret"
    );
    assert_eq!(
        serialized_cookie(accepted_inventory(&refreshed, 0)),
        b"session=second-secret"
    );
}

/// Две competing Cookie serializations не получают неявный приоритет.
#[test]
fn conflicting_cookie_serializations_are_typed_incompatible() {
    let mut protected = progressive_format("protected", "webm", "webm", "vp9", "opus");
    protected["http_headers"] = json!({"Cookie": "header=secret"});
    protected["cookies"] = json!("field=secret");
    let snapshot = snapshot(json!({"formats": [protected]}), 1);
    let candidate = accepted_inventory(&snapshot, 0);
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("progressive-http").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );

    let error = candidate
        .transport_components(&context)
        .expect_err("conflicting cookies должны fail closed");
    assert!(matches!(
        error,
        super::YtDlpTransportRequestError::RequestMaterial(
            YtDlpRequestMaterialViolation::ConflictingCookieMaterial
        )
    ));
    assert!(!format!("{error:?} {error}").contains("secret"));
}

/// S23 запрещает вернуть второй service-owned WebM/HTTP/demux playback stack.
#[test]
fn public_surface_and_manifest_have_no_legacy_webm_opener() {
    let public_surface = include_str!("../lib.rs");
    let service_manifest = include_str!("../../Cargo.toml");
    let forbidden_public_symbols = [
        concat!("open_streaming_", "media_from"),
        concat!("open_seekable_", "vod_from"),
        concat!("YtDlpStreaming", "Media"),
        concat!("YtDlpSelectedStream", "Identity"),
    ];

    for forbidden_symbol in forbidden_public_symbols {
        assert!(
            !public_surface.contains(forbidden_symbol),
            "legacy playback symbol снова появился в public surface"
        );
    }
    for forbidden_dependency in ["reqwest", "symphonia-demux", "web-media-http"] {
        assert!(
            !service_manifest.contains(forbidden_dependency),
            "service-ytdlp снова владеет transport/demux dependency"
        );
    }
}
