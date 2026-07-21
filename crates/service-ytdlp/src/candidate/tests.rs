use rustiplayer_config::YtDlpConfig;
use serde_json::{Value, json};
use web_media_core::{
    DynamicRange, ExtractionGeneration, ProfileExclusionReason, SourceIdentity,
    StaticCompatibilityRejection, StreamLayout, StreamLayoutKind,
};

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
