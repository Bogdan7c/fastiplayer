use rustiplayer_config::YtDlpConfig;
use serde_json::{Value, json};
use source_core::{CancellationToken, HttpRequestTarget};
use web_media_core::{
    ContainerFamily, ContentProbedTrackEvidence, DynamicRange, ExtractionGeneration,
    ProfileExclusionReason, SourceIdentity, StaticCompatibilityRejection, StaticDescriptorField,
    StaticMetadataViolation, StreamLayout, StreamLayoutKind,
};
use web_media_playback_plan::{
    CandidateQualityScore, CandidateRuntimeRequirements, PlanningCandidate,
    PlanningCandidateSnapshot,
};
use web_media_transport_api::{
    MediaComponentRole, MediaPresentation, RedirectHopCount, SecretRequestPurpose,
    SourceGeneration, TransportOpenRequest, TransportProviderId,
};

use super::model::{
    YtDlpCandidateComponentRole, YtDlpCandidateEntry, YtDlpCandidateMatchKind,
    YtDlpCandidateNormalizationRejection, YtDlpCandidateRematchError, YtDlpCandidateSelectionError,
    YtDlpLiveIntent, YtDlpNormalizedCandidate, YtDlpVideoColorEvidence,
};
use super::normalize::normalize_candidate_document;
use super::raw::YtDlpCandidateDocument;
use super::request_material::YtDlpRequestMaterialViolation;
use crate::{YtDlpServiceError, parse_yt_dlp_media_locator};

/// Возвращает HTTP target из transport projection для HTTP-only assertions.
fn http_transport_target(request: &TransportOpenRequest) -> &HttpRequestTarget {
    request
        .target()
        .as_http()
        .expect("yt-dlp transport projection must be HTTP(S)")
}

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

#[test]
fn official_live_fields_form_explicit_fail_closed_intent() {
    let live = snapshot(
        json!({
            "is_live": true,
            "live_status": "is_live",
            "formats": [progressive_format(
                "live",
                "mp4",
                "mp4",
                "avc1.640028",
                "mp4a.40.2"
            )]
        }),
        1,
    );
    assert_eq!(live.live_intent(), YtDlpLiveIntent::Live);

    let absent = snapshot(
        json!({
            "formats": [progressive_format(
                "unspecified",
                "mp4",
                "mp4",
                "avc1.640028",
                "mp4a.40.2"
            )]
        }),
        1,
    );
    assert_eq!(absent.live_intent(), YtDlpLiveIntent::Unspecified);

    let conflict = snapshot(
        json!({
            "is_live": true,
            "live_status": "not_live",
            "formats": [progressive_format(
                "conflict",
                "mp4",
                "mp4",
                "avc1.640028",
                "mp4a.40.2"
            )]
        }),
        1,
    );
    assert_eq!(conflict.live_intent(), YtDlpLiveIntent::Incompatible);
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

/// S37: progressive FTP material доказывает single-URL subset и отвергает HTTP auth.
#[test]
fn progressive_ftp_request_material_rejects_http_authorization_and_adaptive_extras() {
    let mut protected = progressive_ftp_format("protected-ftp", "webm", "webm", "vp9", "opus");
    protected["http_headers"] = json!({"Authorization": "Bearer ftp-secret"});
    let protected_snapshot = snapshot(json!({"formats": [protected]}), 15);
    let candidate = accepted_inventory(&protected_snapshot, 0);
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("progressive-ftp").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );
    let error = candidate
        .ftp_transport_components(&context)
        .expect_err("HTTP auth не должен проходить progressive FTP boundary");
    assert!(matches!(
        error,
        super::YtDlpTransportRequestError::FtpRequestMaterial(
            YtDlpRequestMaterialViolation::HttpOnlyMaterialForFtp
        )
    ));
    assert!(!format!("{error:?} {error}").contains("ftp-secret"));

    let mut fragmented = progressive_ftp_format("fragmented-ftp", "webm", "webm", "vp9", "opus");
    fragmented["fragments"] = json!([{"url": "ftp://media.invalid/part1"}]);
    let fragmented_snapshot = snapshot(json!({"formats": [fragmented]}), 16);
    let candidate = accepted_inventory(&fragmented_snapshot, 0);
    let error = candidate
        .ftp_transport_components(&context)
        .expect_err("fragmented FTP row не должен проходить progressive subset");
    assert!(matches!(
        error,
        super::YtDlpTransportRequestError::FtpRequestMaterial(
            YtDlpRequestMaterialViolation::NonProgressiveMaterial
        )
    ));
}

/// S37: FTP transport projection строит empty-secret request и redacts diagnostics.
#[test]
fn ftp_transport_components_project_empty_secret_progressive_request() {
    let mut ftp_format = progressive_ftp_format("muxed-ftp-webm", "webm", "webm", "vp9", "opus");
    ftp_format["url"] = json!("ftp://ftp-user:ftp-secret@media.invalid/private/video.webm");
    let snapshot = snapshot(json!({"formats": [ftp_format]}), 17);
    let candidate = accepted_inventory(&snapshot, 0);
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("progressive-ftp").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );
    let components = candidate
        .ftp_transport_components(&context)
        .expect("public FTP component должен стать neutral request");
    assert_eq!(components.len(), 1);
    let request = components
        .into_iter()
        .next()
        .expect("single component")
        .into_request();
    assert!(request.secrets().is_empty());
    assert!(request.target().as_ftp().is_some());
    let diagnostic = format!("{snapshot:?} {request:?}");
    assert!(!diagnostic.contains("ftp-secret"));
    assert!(!diagnostic.contains("ftp-user"));
}

/// FTP Ogg сохраняет различие explicit отсутствующего video и неизвестного audio codec.
#[test]
fn ftp_ogg_partial_codec_metadata_uses_content_probe_without_fake_codec() {
    let snapshot = snapshot(
        json!({
            "formats": [{
                "format_id": "ftp-ogg-probed",
                "url": "ftp://media.invalid/song.ogg",
                "protocol": "ftp",
                "ext": "ogg",
                "vcodec": "none",
                "acodec": null
            }]
        }),
        18,
    );
    let candidate = accepted_inventory(&snapshot, 0);
    let StreamLayout::ContentProbed(descriptor) = candidate.descriptor().layout() else {
        panic!("partial FTP codec metadata должна строить content-probed layout");
    };
    assert!(matches!(
        descriptor.video(),
        ContentProbedTrackEvidence::Absent
    ));
    assert!(matches!(
        descriptor.audio(),
        ContentProbedTrackEvidence::Unknown
    ));
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("progressive-ftp").unwrap(),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );
    let [component] = candidate
        .ftp_transport_components(&context)
        .expect("content-probed FTP Ogg должен проецироваться")
        .try_into()
        .expect("single FTP candidate содержит один component");
    assert_eq!(component.role(), MediaComponentRole::ContentProbed);
    assert_eq!(component.container(), ContainerFamily::Ogg);
}

/// S37: progressive HTTP transport отвергает FTP primary target.
#[test]
fn progressive_http_transport_rejects_ftp_primary_target() {
    let snapshot = snapshot(
        json!({
            "formats": [progressive_ftp_format(
                "ftp-not-http",
                "webm",
                "webm",
                "vp9",
                "opus"
            )]
        }),
        18,
    );
    let candidate = accepted_inventory(&snapshot, 0);
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("progressive-http").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );
    let error = candidate
        .transport_components(&context)
        .expect_err("FTP target не должен проходить progressive HTTP boundary");
    assert!(matches!(
        error,
        super::YtDlpTransportRequestError::RequestMaterial(
            YtDlpRequestMaterialViolation::NonHttpProgressiveMaterial
        )
    ));
}

/// Создаёт базовый progressive FTP format без secrets.
fn progressive_ftp_format(
    format_id: &str,
    extension: &str,
    container: &str,
    video_codec: &str,
    audio_codec: &str,
) -> Value {
    json!({
        "format_id": format_id,
        "url": format!("ftp://media.invalid/{format_id}.webm"),
        "protocol": "ftp",
        "ext": extension,
        "container": container,
        "vcodec": video_codec,
        "acodec": audio_codec,
        "dynamic_range": "SDR"
    })
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

/// Возвращает неизменённую checked-in S36A `target-ism-fmp4` format row.
fn target_ism_format() -> Value {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../compatibility/2026.07.04/fixtures/official-synthetic/format-inventory.json"
    ))
    .expect("checked-in S00 fixture валиден");
    fixture["payload"]["formats"]
        .as_array()
        .expect("fixture formats array")
        .iter()
        .find(|format| format["fixture_id"] == "target-ism-fmp4")
        .expect("checked-in target-ism-fmp4 row")
        .clone()
}

/// Моделирует exact selected-result shape реального Unified Streaming URL в yt-dlp 2026.07.04.
fn real_selected_ism_payload() -> Value {
    let manifest = "https://manifest.invalid/channel.ism/Manifest";
    let mut video = target_ism_format();
    video["format_id"] = json!("video_eng-2200");
    video["vcodec"] = json!("AVC1");
    video["acodec"] = json!("none");
    video["ext"] = json!("ismv");
    video["fragments"] = json!([{
        "url": "https://manifest.invalid/channel.ism/QualityLevels(2200000)/Fragments(video_eng=0)",
        "duration": 4.0
    }]);

    let mut audio = target_ism_format();
    audio["format_id"] = json!("audio_eng-128");
    audio["vcodec"] = json!("none");
    audio["acodec"] = json!("AACL");
    audio["ext"] = json!("isma");
    audio["fragments"] = json!([{
        "url": "https://manifest.invalid/channel.ism/QualityLevels(128002)/Fragments(audio_eng=0)",
        "duration": 3.968
    }]);

    json!({
        "format_id": "video_eng-2200+audio_eng-128",
        "protocol": "ism+ism",
        "ext": "mp4",
        "vcodec": "AVC1",
        "acodec": "AACL",
        "requested_formats": [video, audio],
        "webpage_url": manifest
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

#[test]
fn inventory_video_and_audio_compose_and_semantically_rematch_without_format_id_or_url_leakage() {
    let mut old_video = progressive_format("old-video", "webm", "webm", "vp9.2", "none");
    old_video["dynamic_range"] = json!("HDR10");
    let old = snapshot(
        json!({
            "formats": [
                old_video,
                progressive_format("old-audio", "webm", "webm", "none", "opus")
            ]
        }),
        1,
    );
    let old_video = accepted_inventory(&old, 0);
    let old_audio = accepted_inventory(&old, 1);
    let selection = old
        .compose_inventory_av(
            &old.selection_for(old_video).unwrap(),
            &old.selection_for(old_audio).unwrap(),
        )
        .expect("fresh inventory components должны compose-иться");
    assert_eq!(
        selection.descriptor().layout().kind(),
        StreamLayoutKind::Separate
    );
    let debug = format!("{selection:?}");
    assert!(!debug.contains("old-video"));
    assert!(!debug.contains("old-audio"));
    assert!(!debug.contains("media.invalid"));

    let mut fresh_video = progressive_format("fresh-video", "webm", "webm", "vp9.2", "none");
    fresh_video["dynamic_range"] = json!("HDR10");
    let fresh = snapshot(
        json!({
            "formats": [
                progressive_format("fresh-audio", "webm", "webm", "none", "opus"),
                fresh_video
            ]
        }),
        2,
    );
    let (kind, fresh_selection, candidate) = fresh
        .rematch_composed(&selection)
        .expect("обе semantic components должны независимо rematch-иться");
    assert_eq!(kind, super::YtDlpCompositionMatchKind::SemanticRematch);
    assert_eq!(
        fresh_selection.descriptor().identity().generation(),
        fresh.generation()
    );
    assert_eq!(
        fresh_selection
            .video_parent_selection()
            .exact_identity()
            .generation(),
        fresh.generation()
    );
    assert_eq!(fresh_selection.descriptor(), candidate.descriptor());
    assert_eq!(
        candidate.descriptor().layout().kind(),
        StreamLayoutKind::Separate
    );
    assert_eq!(candidate.component_count(), 2);
    assert_eq!(
        candidate.video_color_evidence(),
        Some(YtDlpVideoColorEvidence::Bt2020PqLimited)
    );
    let roles = candidate
        .component_request_summaries()
        .map(|summary| summary.role)
        .collect::<Vec<_>>();
    assert_eq!(
        roles,
        vec![
            YtDlpCandidateComponentRole::Video,
            YtDlpCandidateComponentRole::Audio
        ]
    );
}

/// Duplicate selected component использует canonical request material, сохраняя inventory gate.
#[test]
fn selected_inventory_duplicate_composes_with_selected_request_material() {
    let inventory_video = progressive_format("selected-video", "webm", "webm", "vp9", "none");
    let inventory_audio = progressive_format("inventory-audio", "webm", "webm", "none", "opus");
    let mut selected_result = inventory_video.clone();
    selected_result["http_headers"] = json!({"Referer": "https://page.invalid/selected"});
    selected_result["formats"] = json!([inventory_video, inventory_audio]);
    let snapshot = snapshot(selected_result, 28);
    let inventory_video = accepted_inventory(&snapshot, 0);
    let inventory_audio = accepted_inventory(&snapshot, 1);

    let composed = snapshot
        .compose_inventory_av(
            &snapshot.selection_for(inventory_video).unwrap(),
            &snapshot.selection_for(inventory_audio).unwrap(),
        )
        .expect("equivalent selected+inventory video должен быть composable");
    let (kind, _, candidate) = snapshot
        .rematch_composed(&composed)
        .expect("same-generation composition должна восстановить candidate");
    assert_eq!(kind, super::YtDlpCompositionMatchKind::Exact);
    let request_summaries = candidate.component_request_summaries().collect::<Vec<_>>();
    assert_eq!(
        request_summaries[0].role,
        YtDlpCandidateComponentRole::Video
    );
    assert_eq!(
        request_summaries[0].material.header_count, 1,
        "video component обязан сохранить richer selected request material"
    );
}

/// Rejected selected shadow нельзя обойти semantic rematch-ем через inventory twin.
#[test]
fn composed_rematch_rejects_inventory_twin_shadowed_by_rejected_selected() {
    let old = snapshot(
        json!({
            "formats": [
                progressive_format("old-video", "webm", "webm", "vp9", "none"),
                progressive_format("old-audio", "webm", "webm", "none", "opus")
            ]
        }),
        29,
    );
    let old_composed = old
        .compose_inventory_av(
            &old.selection_for(accepted_inventory(&old, 0)).unwrap(),
            &old.selection_for(accepted_inventory(&old, 1)).unwrap(),
        )
        .expect("old inventory pair должна compose-иться");

    let fresh_video = progressive_format("fresh-video", "webm", "webm", "vp9", "none");
    let fresh_audio = progressive_format("fresh-audio", "webm", "webm", "none", "opus");
    let mut rejected_selected = fresh_video.clone();
    rejected_selected["request_data"] = json!("provider-private-request-body");
    rejected_selected["formats"] = json!([fresh_video, fresh_audio]);
    let fresh = snapshot(rejected_selected, 30);

    assert!(
        fresh
            .selected()
            .and_then(YtDlpCandidateEntry::rejected)
            .is_some()
    );
    assert_eq!(
        fresh.rematch_composed(&old_composed).unwrap_err(),
        super::YtDlpCompositionError::MissingVideoComponent
    );
}

#[test]
fn composed_rematch_reports_missing_component_without_default_fallback() {
    let old = snapshot(
        json!({
            "formats": [
                progressive_format("video", "webm", "webm", "vp9", "none"),
                progressive_format("audio", "webm", "webm", "none", "opus")
            ]
        }),
        1,
    );
    let selection = old
        .compose_inventory_av(
            &old.selection_for(accepted_inventory(&old, 0)).unwrap(),
            &old.selection_for(accepted_inventory(&old, 1)).unwrap(),
        )
        .unwrap();
    let fresh = snapshot(
        json!({
            "formats": [progressive_format(
                "fresh-video",
                "webm",
                "webm",
                "vp9",
                "none"
            )]
        }),
        2,
    );
    assert_eq!(
        fresh.rematch_composed(&selection),
        Err(super::YtDlpCompositionError::MissingAudioComponent)
    );
}

#[test]
fn audio_fallback_rank_uses_standard_hints_and_not_inventory_order() {
    let mut lower = progressive_format("lower", "webm", "webm", "none", "opus");
    lower["preference"] = json!(1);
    lower["language_preference"] = json!(5);
    lower["quality"] = json!(2);
    lower["abr"] = json!(64);
    lower["audio_channels"] = json!(2);
    lower["asr"] = json!(48_000);
    let mut higher = lower.clone();
    higher["format_id"] = json!("higher");
    higher["url"] = json!("https://media.invalid/higher");
    higher["abr"] = json!(192);
    let snapshot = snapshot(json!({"formats": [higher, lower]}), 1);
    let first = accepted_inventory(&snapshot, 0);
    let second = accepted_inventory(&snapshot, 1);
    assert!(first.audio_fallback_rank().unwrap() > second.audio_fallback_rank().unwrap());
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

/// Mixed progressive Separate маршрутизирует каждый physical resource своим provider-ом.
#[test]
fn mixed_http_ftp_compound_projects_component_wise_transport_requests() {
    let snapshot = snapshot(
        json!({
            "format_id": "ftp-video+http-audio",
            "requested_formats": [
                progressive_format("http-audio", "opus", "ogg", "none", "opus"),
                progressive_ftp_format("ftp-video", "webm", "webm", "vp9", "none")
            ]
        }),
        19,
    );
    let selected = snapshot
        .selected()
        .and_then(YtDlpCandidateEntry::accepted)
        .expect("mixed progressive candidate");
    let http_provider = TransportProviderId::new("progressive-http").expect("HTTP provider ID");
    let ftp_provider = TransportProviderId::new("progressive-ftp").expect("FTP provider ID");
    let context = super::YtDlpProgressiveTransportRequestContext::new(
        http_provider.clone(),
        ftp_provider.clone(),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );

    let components = selected
        .progressive_transport_components(&context)
        .expect("component-wise projection");
    assert_eq!(components.len(), 2);
    for component in components {
        let role = component.role();
        let request = component.into_request();
        match role {
            MediaComponentRole::Video => {
                assert_eq!(request.provider(), &ftp_provider);
                assert!(request.target().as_ftp().is_some());
            }
            MediaComponentRole::Audio => {
                assert_eq!(request.provider(), &http_provider);
                assert!(request.target().as_http().is_some());
            }
            MediaComponentRole::Muxed
            | MediaComponentRole::ContentProbed
            | MediaComponentRole::Subtitle
            | MediaComponentRole::PresentationManifest => {
                panic!("Separate candidate содержит недопустимую component role")
            }
        }
    }
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

/// HDS S00 row проецируется в один VOD manifest request с directory-scoped secrets.
#[test]
fn hds_transport_projection_preserves_manifest_scope_and_lifecycle() {
    let snapshot = snapshot(
        json!({
            "formats": [{
                "format_id": "hds-f4f",
                "url": "https://manifest.invalid/hds/stream.f4m?token=url-secret",
                "manifest_url": "https://manifest.invalid/hds/stream.f4m?token=manifest-secret",
                "protocol": "f4m",
                "ext": "flv",
                "container": "f4f",
                "vcodec": "avc1.4d401f",
                "acodec": "mp4a.40.2",
                "http_headers": {"Authorization": "Bearer hds-header-secret"},
                "cookies": "session=hds-cookie-secret"
            }]
        }),
        9,
    );
    let candidate = accepted_inventory(&snapshot, 0);
    let cancellation = CancellationToken::new();
    let provider = TransportProviderId::new("hds-manifest-http").expect("provider ID");
    let context = super::YtDlpTransportRequestContext::new(
        provider.clone(),
        SourceGeneration::new(13),
        cancellation.clone(),
    );

    let request = candidate
        .hds_transport_request(&context)
        .expect("approved HDS row должен проецироваться");

    assert_eq!(request.provider(), &provider);
    assert_eq!(request.presentation(), MediaPresentation::Vod);
    assert_eq!(request.source_generation(), SourceGeneration::new(13));
    assert_eq!(
        request.component().role(),
        MediaComponentRole::ContentProbed
    );
    assert_eq!(
        http_transport_target(&request).expose_secret_for_request(),
        "https://manifest.invalid/hds/stream.f4m?token=manifest-secret"
    );

    let initial_material = request
        .secrets()
        .material_for(
            http_transport_target(&request),
            SecretRequestPurpose::PrimaryResource,
        )
        .expect("manifest target должен находиться в HDS scope");
    assert_eq!(
        initial_material.headers_for_request()[0].value,
        "Bearer hds-header-secret"
    );
    assert_eq!(
        initial_material.cookies_for_request(),
        Some(b"session=hds-cookie-secret".as_slice())
    );

    let child_manifest = HttpRequestTarget::parse_exact("https://manifest.invalid/hds/child.f4m")
        .expect("same-directory child target");
    let f4f_fragment =
        HttpRequestTarget::parse_exact("https://manifest.invalid/hds/streamSeg1-Frag1")
            .expect("same-directory F4F target");
    let cross_origin = HttpRequestTarget::parse_exact("https://cdn.invalid/hds/streamSeg1-Frag1")
        .expect("cross-origin target");
    assert!(
        request
            .secrets()
            .material_for(&child_manifest, SecretRequestPurpose::PrimaryResource)
            .is_some()
    );
    assert!(
        request
            .secrets()
            .material_for(&f4f_fragment, SecretRequestPurpose::PrimaryResource)
            .is_some()
    );
    assert!(
        request
            .secrets()
            .material_for(&cross_origin, SecretRequestPurpose::PrimaryResource)
            .is_none()
    );

    let diagnostic = format!("{snapshot:?} {request:?}");
    assert!(!diagnostic.contains("hds-header-secret"));
    assert!(!diagnostic.contains("hds-cookie-secret"));

    cancellation.cancel();
    assert!(request.cancellation().is_cancelled());
}

/// yt-dlp HDS output hint `flv` не подменяет provider-owned F4F demux contract.
#[test]
fn hds_null_codecs_and_flv_output_hint_use_content_probed_f4f_contract() {
    let snapshot = snapshot(
        json!({
            "formats": [{
                "format_id": "hds-probed",
                "url": "https://manifest.invalid/hds/stream.f4m",
                "manifest_url": "https://manifest.invalid/hds/stream.f4m",
                "protocol": "f4m",
                "ext": "flv",
                "vcodec": null,
                "acodec": null
            }]
        }),
        10,
    );
    let candidate = accepted_inventory(&snapshot, 0);
    let StreamLayout::ContentProbed(descriptor) = candidate.descriptor().layout() else {
        panic!("HDS null codecs должны сохранить content-probed descriptor");
    };
    assert_eq!(descriptor.probe_container(), ContainerFamily::F4f);
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("hds-manifest-http").unwrap(),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );
    let request = candidate
        .hds_transport_request(&context)
        .expect("content-probed HDS candidate должен открыть F4M hierarchy");
    assert_eq!(
        request.component().role(),
        MediaComponentRole::ContentProbed
    );
}

/// HDS transport не имеет права превращать произвольный container hint в F4F.
#[test]
fn hds_rejects_non_flv_non_f4f_container_hints_before_provider_open() {
    let snapshot = snapshot(
        json!({
            "formats": [
                {
                    "format_id": "hds-ogg",
                    "url": "https://manifest.invalid/hds/stream.f4m",
                    "manifest_url": "https://manifest.invalid/hds/stream.f4m",
                    "protocol": "f4m",
                    "ext": "ogg",
                    "vcodec": null,
                    "acodec": null
                },
                {
                    "format_id": "hds-mp4",
                    "url": "https://manifest.invalid/hds/stream.f4m",
                    "manifest_url": "https://manifest.invalid/hds/stream.f4m",
                    "protocol": "f4m",
                    "ext": "mp4",
                    "vcodec": "avc1.4d401f",
                    "acodec": "mp4a.40.2"
                }
            ]
        }),
        11,
    );

    for ordinal in 0..2 {
        assert!(matches!(
            rejected_inventory(&snapshot, ordinal),
            YtDlpCandidateNormalizationRejection::Static(
                StaticCompatibilityRejection::InvalidMetadata {
                    field: StaticDescriptorField::Container,
                    violation: StaticMetadataViolation::ContradictoryHints,
                }
            )
        ));
    }
}

#[test]
fn hls_transport_projection_accepts_hls_fields_without_progressive_profile_rejection() {
    let snapshot = snapshot(
        json!({
            "formats": [{
                "format_id": "hls-runtime",
                "url": "https://media.invalid/private/media.m3u8",
                "protocol": "m3u8_native",
                "ext": "ts",
                "container": "mpegts",
                "vcodec": "avc1.640028",
                "acodec": "mp4a.40.2",
                "width": 1280,
                "height": 720,
                "hls_media_playlist_data":
                    "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\none.ts\n#EXT-X-ENDLIST\n",
                "http_headers": {"Authorization": "Bearer hls-secret"},
                "cookies": "session=hls-cookie",
                "extra_param_to_segment_url": "segment_token=secret",
                "extra_param_to_key_url": "key_token=secret"
            }]
        }),
        1,
    );
    let candidate = accepted_inventory(&snapshot, 0);
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("progressive-http").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );

    let request = candidate
        .hls_transport_request(&context)
        .expect("HLS projection must not apply progressive exclusions");
    assert_eq!(
        http_transport_target(&request).origin().scheme(),
        source_core::HttpScheme::Https
    );
    let sibling_segment =
        HttpRequestTarget::parse_exact("https://media.invalid/private/segment1.ts")
            .expect("same-directory sibling");
    let cross_origin = HttpRequestTarget::parse_exact("https://cdn.invalid/private/segment1.ts")
        .expect("cross-origin CDN sibling");
    assert!(
        request
            .secrets()
            .material_for(&sibling_segment, SecretRequestPurpose::MediaSegment)
            .is_some(),
        "playlist directory scope must forward secrets to same-origin siblings"
    );
    assert!(
        request
            .secrets()
            .material_for(&cross_origin, SecretRequestPurpose::MediaSegment)
            .is_none(),
        "cross-origin CDN must stay outside secret scope"
    );
    let diagnostic = format!("{snapshot:?} {request:?}");
    for secret in ["hls-secret", "hls-cookie", "segment_token", "key_token"] {
        assert!(!diagnostic.contains(secret));
    }
}

#[test]
fn dash_transport_projection_preserves_serialized_roles_and_scoped_request_context() {
    let snapshot = snapshot(
        json!({
            "formats": [{
                "format_id": "dash-runtime",
                "url": "https://media.invalid/private/manifest.mpd",
                "protocol": "http_dash_segments",
                "ext": "webm",
                "container": "webm",
                "vcodec": "none",
                "acodec": "opus",
                "fragment_base_url": "https://media.invalid/private/",
                "fragments": [
                    {"path": "init.webm"},
                    {"path": "one.webm", "duration": 1.0}
                ],
                "http_headers": {"Authorization": "Bearer dash-secret"},
                "cookies": "session=dash-cookie",
                "extra_param_to_segment_url": "segment_token=secret"
            }]
        }),
        1,
    );
    let candidate = accepted_inventory(&snapshot, 0);
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("progressive-http").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );

    let mut projected = candidate
        .dash_transport_components(&context)
        .expect("serialized DASH projection");
    assert_eq!(projected.len(), 1);
    let (_, _, material, request) = projected.remove(0).into_parts();
    let roles = material
        .input()
        .fragments()
        .map(|fragment| fragment.role())
        .collect::<Vec<_>>();
    assert_eq!(
        roles,
        vec![
            super::YtDlpDashFragmentRole::Initialization,
            super::YtDlpDashFragmentRole::Media
        ]
    );
    assert_eq!(
        http_transport_target(&request).expose_secret_for_request(),
        "https://media.invalid/private/init.webm"
    );
    let media_target = HttpRequestTarget::parse_exact("https://media.invalid/private/one.webm")
        .expect("media target");
    let scoped = request
        .secrets()
        .material_for(&media_target, SecretRequestPurpose::MediaSegment)
        .expect("same scoped media target");
    assert_eq!(
        scoped
            .query_override_for_request()
            .map(|query| query.expose_secret_for_request()),
        Some("segment_token=secret")
    );
    let diagnostic = format!("{snapshot:?} {material:?} {request:?}");
    for secret in ["dash-secret", "dash-cookie", "segment_token"] {
        assert!(!diagnostic.contains(secret));
    }
}

/// Невоспроизводимое request material не маскируется как playable descriptor.
#[test]
fn non_reconstructible_request_material_remains_visible_and_redacted() {
    // Serialized request body остаётся отдельным typed exclusion.
    let mut request_data = progressive_format("request-data", "mp4", "mp4", "h264", "aac");
    // Synthetic marker проверяет redaction и не содержит реального секрета.
    request_data["request_data"] = json!("serialized-body-secret");
    // Browser impersonation требует недоказанного fingerprint provider-а.
    let mut impersonation = progressive_format("impersonation", "mp4", "mp4", "h264", "aac");
    // Synthetic browser identity не должна попасть в diagnostics.
    impersonation["impersonate"] = json!("chrome-136:windows-10");
    // BunnyCDN private ping state не является public provider descriptor-ом.
    let mut private_ping = progressive_format("private-ping", "mp4", "mp4", "h264", "aac");
    // Synthetic private state проверяет fail-closed mapping без network I/O.
    private_ping["_bunnycdn_ping_data"] = json!({"secret": "private-ping-secret"});
    // Mutable cookie refresh state также принадлежит живому extractor runtime.
    let mut private_cookie_refresh =
        progressive_format("private-cookie", "mp4", "mp4", "h264", "aac");
    // Synthetic refresh identity не должна пережить normalization или попасть в diagnostics.
    private_cookie_refresh["_cookie_refresh_params"] = json!({"video_id": "private-refresh-video"});
    // Один snapshot позволяет проверить видимость каждой independent rejection row.
    let snapshot = snapshot(
        json!({
            "formats": [
                request_data,
                impersonation,
                private_ping,
                private_cookie_refresh
            ]
        }),
        5,
    );

    // Request body возвращает точную owner-specific причину.
    assert_eq!(
        rejected_inventory(&snapshot, 0),
        &YtDlpCandidateNormalizationRejection::RequestMaterial(
            YtDlpRequestMaterialViolation::RequestDataRequired
        )
    );
    // Impersonation не схлопывается с private extractor state.
    assert_eq!(
        rejected_inventory(&snapshot, 1),
        &YtDlpCandidateNormalizationRejection::RequestMaterial(
            YtDlpRequestMaterialViolation::ImpersonationRequired
        )
    );
    // BunnyCDN private state требует живого extractor owner-а.
    assert_eq!(
        rejected_inventory(&snapshot, 2),
        &YtDlpCandidateNormalizationRejection::RequestMaterial(
            YtDlpRequestMaterialViolation::PrivateExtractorStateRequired
        )
    );
    // Mutable cookie refresh state возвращает ту же точную boundary-причину.
    assert_eq!(
        rejected_inventory(&snapshot, 3),
        &YtDlpCandidateNormalizationRejection::RequestMaterial(
            YtDlpRequestMaterialViolation::PrivateExtractorStateRequired
        )
    );
    // Debug projection обязан оставаться bounded и secret-safe.
    let diagnostic = format!("{snapshot:?}");
    // Request body marker не отражается в Debug.
    assert!(!diagnostic.contains("serialized-body-secret"));
    // Browser fingerprint identity не отражается в Debug.
    assert!(!diagnostic.contains("chrome-136"));
    // Private ping secret не отражается в Debug.
    assert!(!diagnostic.contains("private-ping-secret"));
    // Private refresh identity не отражается в Debug.
    assert!(!diagnostic.contains("private-refresh-video"));
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

/// Реальная форма `selected + formats[]` имеет одного канонического candidate-а.
#[test]
fn selected_inventory_duplicate_is_canonical_but_inventory_membership_remains_valid() {
    let inventory_duplicate =
        progressive_format("selected-duplicate", "webm", "webm", "vp9", "opus");
    let mut selected_result = inventory_duplicate.clone();
    selected_result["http_headers"] = json!({"Referer": "https://page.invalid/selected"});
    selected_result["formats"] = json!([inventory_duplicate]);
    let snapshot = snapshot(selected_result, 20);

    let selected_candidate = snapshot
        .selected()
        .and_then(YtDlpCandidateEntry::accepted)
        .expect("selected result должен быть accepted");
    let inventory_candidate = accepted_inventory(&snapshot, 0);
    let canonical = snapshot.accepted_candidates().collect::<Vec<_>>();

    assert_eq!(canonical.len(), 1);
    assert!(std::ptr::eq(canonical[0], selected_candidate));
    let selected_token = snapshot
        .selection_for(selected_candidate)
        .expect("canonical selected token должен строиться");
    let inventory_token = snapshot
        .selection_for(inventory_candidate)
        .expect("эквивалентный shadowed inventory token должен canonicalize-иться");
    assert_eq!(inventory_token, selected_token);
    assert_eq!(
        snapshot
            .planning_snapshot()
            .expect("canonical snapshot должен планироваться")
            .candidates()
            .len(),
        1
    );
}

/// Shadowed duplicate обязан совпадать с canonical selection по color evidence.
#[test]
fn shadowed_inventory_color_drift_is_not_a_selectable_canonical_candidate() {
    let mut inventory_duplicate =
        progressive_format("color-drift", "webm", "webm", "vp09.02.10.10", "none");
    inventory_duplicate["dynamic_range"] = json!("HLG");
    let mut selected_result = inventory_duplicate.clone();
    selected_result["dynamic_range"] = json!("HDR10");
    selected_result["formats"] = json!([
        inventory_duplicate,
        progressive_format("audio", "opus", "ogg", "none", "opus")
    ]);
    let snapshot = snapshot(selected_result, 23);
    let selected_candidate = snapshot
        .selected()
        .and_then(YtDlpCandidateEntry::accepted)
        .expect("selected HDR video accepted");
    let inventory_candidate = accepted_inventory(&snapshot, 0);

    assert_ne!(
        selected_candidate.descriptor(),
        inventory_candidate.descriptor()
    );
    assert_ne!(
        selected_candidate.video_color_evidence(),
        inventory_candidate.video_color_evidence()
    );

    assert_eq!(
        snapshot
            .selection_for(inventory_candidate)
            .expect_err("shadowed color drift не должен выдавать non-canonical token"),
        YtDlpCandidateSelectionError::CandidateNotInInventory
    );

    let selected_video = snapshot
        .selection_for(selected_candidate)
        .expect("canonical selected video token");
    let audio = snapshot
        .selection_for(accepted_inventory(&snapshot, 1))
        .expect("canonical inventory audio token");
    assert!(matches!(
        snapshot.compose_inventory_av(&selected_video, &audio),
        Err(super::YtDlpCompositionError::ForeignGenerationOrInventory)
    ));
}

/// Rejected selected origin shadow-ит более бедную accepted inventory строку.
#[test]
fn rejected_selected_identity_cannot_fall_open_to_inventory_twin() {
    let inventory_twin = progressive_format("rejected-selected", "webm", "webm", "vp9", "opus");
    let mut selected_result = inventory_twin.clone();
    selected_result["request_data"] = json!("provider-private-request-body");
    selected_result["formats"] = json!([inventory_twin]);
    let snapshot = snapshot(selected_result, 24);

    assert!(
        snapshot
            .selected()
            .and_then(YtDlpCandidateEntry::rejected)
            .is_some()
    );
    assert!(snapshot.inventory()[0].accepted().is_some());
    assert_eq!(snapshot.accepted_candidates().count(), 0);
    assert_eq!(
        snapshot
            .planning_snapshot()
            .expect("shadowed rejection должен дать пустой, но валидный planner snapshot")
            .candidates()
            .len(),
        0
    );
}

/// Shared alignment boundary отвергает missing и unexpected planner rows.
#[test]
fn planning_snapshot_alignment_requires_the_full_canonical_identity_set() {
    let first = progressive_format("aligned-first", "webm", "webm", "vp9", "opus");
    let second = progressive_format("aligned-second", "webm", "webm", "vp9", "opus");
    let full_service = snapshot(json!({"formats": [first.clone(), second]}), 25);
    let partial_service = snapshot(json!({"formats": [first]}), 25);
    let full_planning = full_service
        .planning_snapshot()
        .expect("full planning snapshot");
    let partial_planning = partial_service
        .planning_snapshot()
        .expect("partial planning snapshot");

    assert_eq!(
        full_service.validate_planning_snapshot_alignment(&full_planning),
        Ok(())
    );
    assert_eq!(
        full_service.validate_planning_snapshot_alignment(&partial_planning),
        Err(super::YtDlpPlanningSnapshotAlignmentError::CandidateIdentityMismatch)
    );
    assert_eq!(
        partial_service.validate_planning_snapshot_alignment(&full_planning),
        Err(super::YtDlpPlanningSnapshotAlignmentError::CandidateIdentityMismatch)
    );
    let reversed_planning = PlanningCandidateSnapshot::new(
        full_planning.source(),
        full_planning.generation(),
        full_planning.candidates().iter().rev().cloned().collect(),
    )
    .expect("reordered planning snapshot");
    assert_eq!(
        full_service.validate_planning_snapshot_alignment(&reversed_planning),
        Ok(())
    );

    let first_planning_candidate = &full_planning.candidates()[0];
    assert!(
        full_service
            .canonical_candidate_for_planning_identity(
                first_planning_candidate.descriptor().identity(),
                first_planning_candidate.descriptor().semantic_identity(),
            )
            .is_some()
    );
}

/// Identity не разрешает caller-у подменить service-owned quality ranking.
#[test]
fn planning_snapshot_alignment_rejects_altered_quality_with_same_identity() {
    let service = snapshot(
        json!({
            "formats": [progressive_format(
                "altered-quality",
                "webm",
                "webm",
                "vp9",
                "opus"
            )]
        }),
        26,
    );
    let canonical = service
        .planning_snapshot()
        .expect("canonical planning snapshot");
    let original = &canonical.candidates()[0];
    let altered_candidate = PlanningCandidate::new(
        original.descriptor().clone(),
        original.runtime_requirements().clone(),
        CandidateQualityScore::new(original.quality_score().value() + 1),
    )
    .expect("altered quality remains a structurally valid planner row");
    let altered = PlanningCandidateSnapshot::new(
        canonical.source(),
        canonical.generation(),
        vec![altered_candidate],
    )
    .expect("same-identity altered quality snapshot");

    assert_eq!(
        service.validate_planning_snapshot_alignment(&altered),
        Err(super::YtDlpPlanningSnapshotAlignmentError::CandidateProjectionMismatch)
    );
}

/// Identity не разрешает caller-у подменить service-owned decode requirements.
#[test]
fn planning_snapshot_alignment_rejects_altered_runtime_with_same_identity() {
    let service = snapshot(
        json!({
            "formats": [progressive_format(
                "altered-runtime",
                "webm",
                "webm",
                "vp9",
                "opus"
            )]
        }),
        27,
    );
    let canonical = service
        .planning_snapshot()
        .expect("canonical planning snapshot");
    let original = &canonical.candidates()[0];
    let mut altered_runtime = original.runtime_requirements().clone();
    let CandidateRuntimeRequirements::Muxed { video, .. } = &mut altered_runtime else {
        panic!("fixture должна строить muxed runtime requirements");
    };
    video.fps = Some(240.0);
    let altered_candidate = PlanningCandidate::new(
        original.descriptor().clone(),
        altered_runtime,
        original.quality_score(),
    )
    .expect("altered FPS remains a structurally valid planner row");
    let altered = PlanningCandidateSnapshot::new(
        canonical.source(),
        canonical.generation(),
        vec![altered_candidate],
    )
    .expect("same-identity altered runtime snapshot");

    assert_eq!(
        service.validate_planning_snapshot_alignment(&altered),
        Err(super::YtDlpPlanningSnapshotAlignmentError::CandidateProjectionMismatch)
    );
}

/// Duplicate selected/inventory не создаёт ложную ambiguity при fresh rematch.
#[test]
fn fresh_rematch_treats_selected_inventory_duplicate_as_one_candidate() {
    let original = snapshot(
        json!({
            "formats": [progressive_format(
                "old-format",
                "webm",
                "webm",
                "vp9",
                "opus"
            )]
        }),
        21,
    );
    let original_selection = original
        .selection_for(accepted_inventory(&original, 0))
        .expect("original candidate принадлежит snapshot-у");

    let fresh_inventory = progressive_format("fresh-format", "webm", "webm", "vp9", "opus");
    let mut fresh_selected = fresh_inventory.clone();
    fresh_selected["formats"] = json!([fresh_inventory]);
    let fresh = snapshot(fresh_selected, 22);

    let matched = fresh
        .rematch_exact(&original_selection)
        .expect("одна физическая alternative должна semantic-rematch-иться");
    assert_eq!(matched.kind(), YtDlpCandidateMatchKind::SemanticRematch);
    assert!(std::ptr::eq(
        matched.candidate(),
        fresh
            .selected()
            .and_then(YtDlpCandidateEntry::accepted)
            .expect("fresh selected candidate accepted")
    ));
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

/// Unknown transport и explicit profile exclusions сохраняются как видимые rows.
#[test]
fn unknown_and_profile_excluded_candidates_remain_visible() {
    // Future unknown identity остаётся bounded и не получает generic fallback.
    let mut unknown = progressive_format("unknown", "mp4", "mp4", "h264", "aac");
    // Exact raw identity сохраняется внутри typed rejection.
    unknown["protocol"] = json!("future_transport_v2");
    // DRM остаётся отдельной profile-exclusion причиной.
    let mut drm = progressive_format("drm", "mp4", "mp4", "h264", "aac");
    // Explicit flag запрещает candidate admission до transport selection.
    drm["has_drm"] = json!(true);
    // Каждая special provider identity получает собственную inventory row.
    let special_rows = [
        "bunnycdn",
        "soopvod",
        "niconico_live",
        "fc2_live",
        "websocket_frag",
    ]
    .into_iter()
    .map(|special_protocol| {
        // Descriptor остаётся обычным synthetic progressive shape только для проверки transport gate.
        let mut special_row = progressive_format(special_protocol, "mp4", "mp4", "h264", "aac");
        // Exact protocol identity не нормализуется между special providers.
        special_row["protocol"] = json!(special_protocol);
        // Возвращаем independent inventory row.
        special_row
    })
    .collect::<Vec<_>>();
    // Собираем один ordered inventory без cloning provider descriptors.
    let mut candidate_rows = vec![unknown, drm];
    // Special rows продолжают source order после общих rejection cases.
    candidate_rows.extend(special_rows);
    // Snapshot сохраняет unknown, DRM и все exact special rows в source order.
    let snapshot = snapshot(json!({"formats": candidate_rows}), 11);

    // Future unknown transport остаётся видимым typed rejection.
    assert!(matches!(
        rejected_inventory(&snapshot, 0),
        YtDlpCandidateNormalizationRejection::Static(
            StaticCompatibilityRejection::UnknownTransport { .. }
        )
    ));
    // DRM получает точную profile owner-причину.
    assert_eq!(
        rejected_inventory(&snapshot, 1),
        &YtDlpCandidateNormalizationRejection::Static(
            StaticCompatibilityRejection::ProfileExcluded {
                reason: ProfileExclusionReason::Drm
            }
        )
    );
    // Каждая special identity остаётся отдельной rejected inventory row.
    for rejected_index in 2..7 {
        // Serializable protocol string не отменяет потребность в live extractor state.
        assert_eq!(
            rejected_inventory(&snapshot, rejected_index),
            &YtDlpCandidateNormalizationRejection::Static(
                StaticCompatibilityRejection::ProfileExcluded {
                    reason: ProfileExclusionReason::RequiresLiveExtractorState
                }
            )
        );
    }
}

/// Достаёт video dynamic range из любой video-bearing shape.
fn video_dynamic_range(candidate: &YtDlpNormalizedCandidate) -> DynamicRange {
    match candidate.descriptor().layout() {
        StreamLayout::Muxed(component) => component.video().dynamic_range(),
        StreamLayout::Separate { video, .. } | StreamLayout::VideoOnly(video) => {
            video.video().dynamic_range()
        }
        StreamLayout::HlsMuxedCodecDeferred(component) => component.dynamic_range(),
        StreamLayout::ContentProbed(component) if !component.video().is_absent() => {
            component.video_hints().dynamic_range()
        }
        StreamLayout::ContentProbed(_) => {
            panic!("audio-only content-probed candidate не имеет dynamic range")
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
        .material_for(
            http_transport_target(&request),
            SecretRequestPurpose::PrimaryResource,
        )
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
            .material_for(
                http_transport_target(&request),
                SecretRequestPurpose::PrimaryResource,
            )
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

/// Checked-in ISM target создаёт ровно один VOD request с прежними lifecycle fences.
#[test]
fn checked_in_ism_target_projects_one_scoped_vod_manifest_request() {
    let mut ism_format = target_ism_format();
    ism_format["http_headers"] = json!({"Authorization": "Bearer smooth-header-secret"});
    ism_format["cookies"] = json!("session=smooth-cookie-secret");
    let snapshot = snapshot(json!({"formats": [ism_format]}), 7);
    let candidate = accepted_inventory(&snapshot, 0);
    let cancellation = CancellationToken::new();
    let provider = TransportProviderId::new("smooth-manifest-http").expect("provider ID");
    let context = super::YtDlpTransportRequestContext::new(
        provider.clone(),
        SourceGeneration::new(11),
        cancellation.clone(),
    );

    let request = candidate
        .smooth_manifest_transport_request(&context)
        .expect("approved ISM target должен проецироваться");

    assert_eq!(request.provider(), &provider);
    assert_eq!(request.presentation(), MediaPresentation::Vod);
    assert_eq!(request.source_generation(), SourceGeneration::new(11));
    assert_eq!(
        request.component().role(),
        MediaComponentRole::PresentationManifest
    );
    assert_eq!(
        request.component().exact(),
        candidate.descriptor().identity()
    );
    assert_eq!(
        request.component().semantic(),
        candidate.descriptor().semantic_identity()
    );
    assert_eq!(
        http_transport_target(&request).expose_secret_for_request(),
        "https://manifest.invalid/channel.ism/Manifest"
    );
    assert_eq!(request.http_range_request_limit(), None);

    let initial_material = request
        .secrets()
        .material_for(
            http_transport_target(&request),
            SecretRequestPurpose::PrimaryResource,
        )
        .expect("initial manifest target должен находиться в собственном scope");
    assert_eq!(initial_material.headers_for_request().len(), 1);
    assert_eq!(
        initial_material.headers_for_request()[0].name,
        "Authorization"
    );
    assert_eq!(
        initial_material.headers_for_request()[0].value,
        "Bearer smooth-header-secret"
    );
    assert_eq!(
        initial_material.cookies_for_request(),
        Some(b"session=smooth-cookie-secret".as_slice())
    );

    let same_path_child = HttpRequestTarget::parse_exact(
        "https://manifest.invalid/channel.ism/QualityLevels(2200000)/Fragments(video=0)",
    )
    .expect("same-presentation target");
    let cross_origin = HttpRequestTarget::parse_exact("https://cdn.invalid/channel.ism/Manifest")
        .expect("cross-origin target");
    assert!(
        request
            .secrets()
            .material_for(&same_path_child, SecretRequestPurpose::PrimaryResource)
            .is_some()
    );
    assert!(
        request
            .secrets()
            .material_for(&cross_origin, SecretRequestPurpose::PrimaryResource)
            .is_none()
    );
    let redirect_authorization = request
        .redirects()
        .authorize_redirect(
            http_transport_target(&request),
            &cross_origin,
            RedirectHopCount::none(),
        )
        .expect("cross-origin CDN redirect разрешён без secrets");
    assert!(!redirect_authorization.permits_secret_scope_check());

    let diagnostic = format!("{snapshot:?} {request:?}");
    assert!(!diagnostic.contains("smooth-header-secret"));
    assert!(!diagnostic.contains("smooth-cookie-secret"));

    cancellation.cancel();
    assert!(request.cancellation().is_cancelled());
}

/// ISM projection не угадывает transport/layout/component shape.
#[test]
fn smooth_transport_rejects_non_ism_and_non_av_layout_candidates() {
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("smooth-manifest-http").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );

    let progressive = snapshot(
        json!({
            "formats": [progressive_format(
                "progressive",
                "mp4",
                "mp4",
                "avc1.640028",
                "mp4a.40.2"
            )]
        }),
        1,
    );
    assert!(matches!(
        accepted_inventory(&progressive, 0).smooth_manifest_transport_request(&context),
        Err(super::YtDlpTransportRequestError::SmoothTransport)
    ));

    let mut video_only_ism = target_ism_format();
    video_only_ism["acodec"] = json!("none");
    let video_only = snapshot(json!({"formats": [video_only_ism]}), 1);
    assert!(matches!(
        accepted_inventory(&video_only, 0).smooth_manifest_transport_request(&context),
        Err(super::YtDlpTransportRequestError::SmoothLayout)
    ));

    let compound = snapshot(
        json!({
            "format_id": "video+audio",
            "requested_formats": [
                progressive_format("video", "mp4", "mp4", "avc1.640028", "none"),
                progressive_format("audio", "m4a", "m4a", "none", "mp4a.40.2")
            ]
        }),
        1,
    );
    let compound = compound
        .selected()
        .and_then(YtDlpCandidateEntry::accepted)
        .expect("exact compound candidate accepted");
    assert!(matches!(
        compound.smooth_manifest_transport_request(&context),
        Err(super::YtDlpTransportRequestError::SmoothTransport)
    ));
}

/// Exact approved ISM row не расширяется на другой container или codec family.
#[test]
fn smooth_transport_rejects_non_fmp4_non_h264_and_non_aac_profiles() {
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("smooth-manifest-http").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );

    let mut wrong_container = target_ism_format();
    wrong_container["ext"] = json!("webm");
    wrong_container["container"] = json!("webm");
    let wrong_container = snapshot(json!({"formats": [wrong_container]}), 1);
    assert!(matches!(
        accepted_inventory(&wrong_container, 0).smooth_manifest_transport_request(&context),
        Err(super::YtDlpTransportRequestError::SmoothContainer)
    ));

    let mut wrong_video = target_ism_format();
    wrong_video["vcodec"] = json!("vp9");
    let wrong_video = snapshot(json!({"formats": [wrong_video]}), 1);
    assert!(matches!(
        accepted_inventory(&wrong_video, 0).smooth_manifest_transport_request(&context),
        Err(super::YtDlpTransportRequestError::SmoothVideoCodec)
    ));

    let mut wrong_audio = target_ism_format();
    wrong_audio["acodec"] = json!("opus");
    let wrong_audio = snapshot(json!({"formats": [wrong_audio]}), 1);
    assert!(matches!(
        accepted_inventory(&wrong_audio, 0).smooth_manifest_transport_request(&context),
        Err(super::YtDlpTransportRequestError::SmoothAudioCodec)
    ));
}

/// Реальные yt-dlp Smooth fourcc/ext aliases сохраняют H.264/AAC fMP4 contract.
#[test]
fn smooth_transport_accepts_avc1_aacl_and_ismv_aliases() {
    let mut format = target_ism_format();
    format["vcodec"] = json!("AVC1");
    format["acodec"] = json!("AACL");
    format["ext"] = json!("ismv");
    format["container"] = json!("m4a_dash");
    let snapshot = snapshot(json!({"formats": [format]}), 1);
    let candidate = accepted_inventory(&snapshot, 0);
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("smooth-manifest-http").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );

    candidate
        .smooth_manifest_transport_request(&context)
        .expect("реальные Smooth aliases должны сохранить FMP4 H.264/AAC request");
}

/// Реальный selected `ism+ism` shape проходит planner и становится одним presentation request-ом.
#[test]
fn real_selected_ism_separate_components_reach_presentation_manifest_boundary() {
    let snapshot = snapshot(real_selected_ism_payload(), 12);
    let candidate = snapshot
        .selected()
        .and_then(YtDlpCandidateEntry::accepted)
        .expect("реальный selected Smooth candidate должен нормализоваться");
    let planning = snapshot
        .planning_snapshot()
        .expect("bare AVC1 должен оставить профиль container preflight-у");
    let [planned] = planning.candidates() else {
        panic!("реальный Smooth selected result должен дать один planning candidate");
    };
    match planned.runtime_requirements() {
        CandidateRuntimeRequirements::Separate { video, audio } => {
            assert_eq!(video.codec, codec_core::VideoCodec::H264);
            assert_eq!(video.profile, None);
            assert_eq!(video.bit_depth, None);
            assert_eq!(video.chroma, None);
            assert_eq!(*audio, audio_core::AudioDecodeCodecFamily::Aac);
        }
        unexpected => panic!("ожидался separate Smooth runtime, получен {unexpected:?}"),
    }

    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("smooth-manifest-http").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );
    let request = candidate
        .smooth_manifest_transport_request(&context)
        .expect("video+audio fragments того же Manifest должны проецироваться");

    assert_eq!(
        request.component().role(),
        MediaComponentRole::PresentationManifest
    );
    assert_eq!(request.presentation(), MediaPresentation::Vod);
    assert_eq!(
        http_transport_target(&request).expose_secret_for_request(),
        "https://manifest.invalid/channel.ism/Manifest"
    );
}

#[test]
fn separate_smooth_components_reject_different_target_and_authorization_context() {
    let context = super::YtDlpTransportRequestContext::new(
        TransportProviderId::new("smooth-manifest-http").expect("provider ID"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );

    let mut different_target = real_selected_ism_payload();
    different_target["requested_formats"][1]["url"] =
        json!("https://manifest.invalid/other.ism/Manifest");
    different_target["requested_formats"][1]["manifest_url"] =
        json!("https://manifest.invalid/other.ism/Manifest");
    different_target["requested_formats"][1]["fragments"] = json!([]);
    let different_target = snapshot(different_target, 13);
    let different_target = different_target
        .selected()
        .and_then(YtDlpCandidateEntry::accepted)
        .expect("different-target candidate всё ещё имеет честный separate descriptor");
    assert!(matches!(
        different_target.smooth_manifest_transport_request(&context),
        Err(super::YtDlpTransportRequestError::SmoothPresentationTargetMismatch)
    ));

    let mut different_headers = real_selected_ism_payload();
    different_headers["requested_formats"][0]["http_headers"] =
        json!({"Authorization": "Bearer video-secret"});
    different_headers["requested_formats"][1]["http_headers"] =
        json!({"Authorization": "Bearer audio-secret"});
    let different_headers = snapshot(different_headers, 14);
    let different_headers = different_headers
        .selected()
        .and_then(YtDlpCandidateEntry::accepted)
        .expect("different-context candidate должен дойти до request projection");
    assert!(matches!(
        different_headers.smooth_manifest_transport_request(&context),
        Err(super::YtDlpTransportRequestError::SmoothPresentationRequestContextMismatch)
    ));
}

/// S23 запрещает вернуть второй service-owned WebM/HTTP/demux playback stack.
#[test]
fn hls_null_codecs_with_height_becomes_deferred_layout() {
    let snapshot = snapshot(
        json!({
            "formats": [{
                "format_id": "hls-deferred",
                "url": "https://media.invalid/master.m3u8",
                "protocol": "m3u8_native",
                "ext": "mp4",
                "height": 720,
                "vcodec": null,
                "acodec": null,
            }]
        }),
        1,
    );
    assert_eq!(
        accepted_inventory(&snapshot, 0)
            .descriptor()
            .layout()
            .kind(),
        StreamLayoutKind::HlsMuxedCodecDeferred
    );
}

/// Null codec metadata остаётся unknown evidence и допускает authoritative demux content probe.
#[test]
fn progressive_ogg_null_codecs_builds_content_probed_candidate() {
    let snapshot = snapshot(
        json!({
            "formats": [{
                "format_id": "progressive-ogg-null-codecs",
                "url": "https://media.invalid/file.ogg",
                "protocol": "https",
                "ext": "ogg",
                "vcodec": null,
                "acodec": null,
            }]
        }),
        1,
    );
    let candidate = accepted_inventory(&snapshot, 0);
    let StreamLayout::ContentProbed(descriptor) = candidate.descriptor().layout() else {
        panic!("null codec metadata должна строить content-probed layout");
    };
    assert_eq!(descriptor.probe_container(), ContainerFamily::Ogg);
    assert!(matches!(
        descriptor.video(),
        ContentProbedTrackEvidence::Unknown
    ));
    assert!(matches!(
        descriptor.audio(),
        ContentProbedTrackEvidence::Unknown
    ));

    let context = super::YtDlpProgressiveTransportRequestContext::new(
        TransportProviderId::new("progressive-http").unwrap(),
        TransportProviderId::new("progressive-ftp").unwrap(),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );
    let [component] = candidate
        .progressive_transport_components(&context)
        .expect("content-probed Ogg должен проецироваться в progressive transport")
        .try_into()
        .expect("single candidate содержит один physical resource");
    assert_eq!(component.role(), MediaComponentRole::ContentProbed);
    assert_eq!(component.container(), ContainerFamily::Ogg);
}

/// HLS без height не принимает deferred layout при null codecs.
#[test]
fn hls_null_codecs_without_height_rejects_missing_codec() {
    let snapshot = snapshot(
        json!({
            "formats": [{
                "format_id": "hls-no-height",
                "url": "https://media.invalid/master.m3u8",
                "protocol": "m3u8_native",
                "ext": "mp4",
                "vcodec": null,
                "acodec": null,
            }]
        }),
        1,
    );
    assert!(matches!(
        rejected_inventory(&snapshot, 0),
        YtDlpCandidateNormalizationRejection::Static(
            StaticCompatibilityRejection::InvalidMetadata { .. }
        )
    ));
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
