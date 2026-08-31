use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use audio::AudioDecodeCodecFamily;
use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, CURRENT_CAPABILITY_SCHEMA_VERSION,
    SupportedVideoOutput, SystemCapabilities,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, H264Profile, SupportedVideoDecodeFormat,
    VideoCodec as DecodeVideoCodec, VideoProfile,
};
use tempfile::TempDir;
use video_frame_contract::VideoFrameContract;
use web_media_core::{
    CandidateFormatIdentity, ExactSelectionIdentity, ExtractionGeneration, SelectionRequest,
    SourceIdentity,
};

use super::*;

/// Изолирует fake `yt-dlp` PATH от параллельных tests текущего process-а.
const FALLBACK_CHILD_MARKER_ENV: &str = "RUSTIPLAYER_FALLBACK_CHILD";
/// Передаёт synthetic extractor document только isolated child process-у.
const FALLBACK_DOCUMENT_ENV: &str = "RUSTIPLAYER_FALLBACK_YTDLP_JSON";
/// Exact test name исключает повторный запуск всего app test binary.
const FALLBACK_CHILD_TEST_NAME: &str = "web_media_open::content_probe_fallback::tests::service_snapshot_ranking_keeps_successful_selection_and_exact_is_single_attempt";
/// Проверяет fallback со selected-only лучшего audio на inventory audio.
const CATALOG_AUDIO_CHILD_TEST_NAME: &str =
    "web_media_open::content_probe_fallback::tests::catalog_composition_uses_best_composable_audio";
/// Проверяет отсутствие fake composition у selected-only video.
const CATALOG_VIDEO_CHILD_TEST_NAME: &str = "web_media_open::content_probe_fallback::tests::catalog_selected_only_video_keeps_parent_choices_without_composed_target";
/// Проверяет обычную inventory video+audio composition.
const CATALOG_INVENTORY_CHILD_TEST_NAME: &str = "web_media_open::content_probe_fallback::tests::catalog_inventory_video_audio_composition_remains_available";

#[test]
fn best_playable_retries_only_typed_content_rejection() {
    let candidates = [10_u8, 20_u8];
    let mut attempts = Vec::new();
    let (selected, opened) = open_ranked_best(&candidates, &|| false, |candidate| {
        attempts.push(*candidate);
        if *candidate == 10 {
            Err(ContentProbeRejection::UnsupportedAudio.into())
        } else {
            Ok("opened")
        }
    })
    .expect("second planner-ranked candidate должен открыться");

    assert_eq!(*selected, 20);
    assert_eq!(opened, "opened");
    assert_eq!(attempts, [10, 20]);
}

#[test]
fn best_playable_uses_one_alternate_after_network_unavailable() {
    let candidates = [10_u8, 20_u8];
    let mut attempts = Vec::new();
    let (selected, opened) = open_ranked_best(&candidates, &|| false, |candidate| {
        attempts.push(*candidate);
        if *candidate == 10 {
            Err(CandidateOpenError::from(
                anyhow::Error::new(TransportOpenError::Transport(
                    TransportFailure::NetworkUnavailable,
                ))
                .context("provider добавил безопасный пользовательский контекст"),
            ))
        } else {
            Ok("opened")
        }
    })
    .expect("второй planner-ranked candidate должен открыть тот же BestPlayable intent");

    assert_eq!(*selected, 20);
    assert_eq!(opened, "opened");
    assert_eq!(attempts, [10, 20]);
}

#[test]
fn best_playable_network_fallback_is_bounded_to_one_alternate() {
    let candidates = [10_u8, 20_u8, 30_u8];
    let mut attempts = Vec::new();
    let error = open_ranked_best(&candidates, &|| false, |candidate| {
        attempts.push(*candidate);
        Err::<(), _>(CandidateOpenError::from(anyhow::Error::new(
            TransportOpenError::Transport(TransportFailure::NetworkUnavailable),
        )))
    })
    .expect_err("две недоступные identities должны исчерпать bounded fallback");

    assert_eq!(attempts, [10, 20]);
    assert!(error.to_string().contains("network fallback исчерпан"));
}

#[test]
fn best_playable_timeout_remains_terminal_without_alternate_attempt() {
    let candidates = [10_u8, 20_u8];
    let mut attempts = 0_usize;
    let error = open_ranked_best(&candidates, &|| false, |_| {
        attempts = attempts.saturating_add(1);
        Err::<(), _>(CandidateOpenError::from(anyhow::Error::new(
            TransportOpenError::Transport(TransportFailure::Timeout),
        )))
    })
    .expect_err("timeout не должен умножаться на размер candidate inventory");

    assert_eq!(attempts, 1);
    assert!(matches!(
        error.downcast_ref::<TransportOpenError>(),
        Some(TransportOpenError::Transport(TransportFailure::Timeout))
    ));
}

#[test]
fn best_playable_exhaustion_preserves_most_recent_retryable_error() {
    let candidates = [10_u8, 20_u8];
    let error = open_ranked_best(&candidates, &|| false, |candidate| {
        if *candidate == 10 {
            Err::<(), _>(ContentProbeRejection::UnsupportedVideo.into())
        } else {
            Err::<(), _>(CandidateOpenError::from(anyhow::Error::new(
                TransportOpenError::Transport(TransportFailure::NetworkUnavailable),
            )))
        }
    })
    .expect_err("exhaustion должен сохранить последнюю фактическую причину");

    assert!(matches!(
        error.downcast_ref::<TransportOpenError>(),
        Some(TransportOpenError::Transport(
            TransportFailure::NetworkUnavailable
        ))
    ));
    assert!(error.to_string().contains("content_rejections=1"));
    assert!(error.to_string().contains("unavailable_candidates=1"));
}

#[test]
fn exact_content_rejection_does_not_try_another_candidate() {
    let mut attempts = 0_usize;
    let error = open_single(&10_u8, |_| {
        attempts += 1;
        Err::<(), _>(ContentProbeRejection::UnsupportedVideo.into())
    })
    .expect_err("Exact content rejection должен остаться terminal");

    assert_eq!(attempts, 1);
    assert_eq!(
        error.downcast_ref::<ContentProbeRejection>(),
        Some(&ContentProbeRejection::UnsupportedVideo)
    );
}

#[test]
fn fatal_best_playable_failure_is_not_masked_by_neighbor() {
    let candidates = [10_u8, 20_u8];
    let mut attempts = 0_usize;
    let error = open_ranked_best(&candidates, &|| false, |_| {
        attempts += 1;
        Err::<(), _>(CandidateOpenError::Fatal(anyhow::anyhow!(
            "terminal provider failure"
        )))
    })
    .expect_err("terminal failure не должен запускать fallback");

    assert_eq!(attempts, 1);
    assert!(error.to_string().contains("terminal provider failure"));
}

/// Реальный service snapshot доказывает mapping planner rank → exact active selection.
#[test]
fn service_snapshot_ranking_keeps_successful_selection_and_exact_is_single_attempt() {
    if env::var_os(FALLBACK_CHILD_MARKER_ENV).is_some() {
        assert_child_service_snapshot_fallback();
        return;
    }

    run_isolated_service_snapshot_child(FALLBACK_CHILD_TEST_NAME, fallback_candidate_document());
}

/// Selected-only лучший audio пропускается ради следующего composable audio.
#[test]
fn catalog_composition_uses_best_composable_audio() {
    if env::var_os(FALLBACK_CHILD_MARKER_ENV).is_some() {
        assert_child_catalog_choice_count(4);
        return;
    }
    run_isolated_service_snapshot_child(
        CATALOG_AUDIO_CHILD_TEST_NAME,
        selected_audio_catalog_document(),
    );
}

/// Selected-only video остаётся parent choice, но не создаёт fake A/V target.
#[test]
fn catalog_selected_only_video_keeps_parent_choices_without_composed_target() {
    if env::var_os(FALLBACK_CHILD_MARKER_ENV).is_some() {
        assert_child_catalog_choice_count(2);
        return;
    }
    run_isolated_service_snapshot_child(
        CATALOG_VIDEO_CHILD_TEST_NAME,
        selected_video_catalog_document(),
    );
}

/// Обычная inventory video+audio пара по-прежнему создаёт composed choice.
#[test]
fn catalog_inventory_video_audio_composition_remains_available() {
    if env::var_os(FALLBACK_CHILD_MARKER_ENV).is_some() {
        assert_child_catalog_choice_count(3);
        return;
    }
    run_isolated_service_snapshot_child(
        CATALOG_INVENTORY_CHILD_TEST_NAME,
        inventory_av_catalog_document(),
    );
}

/// Запускает один production-normalization assertion в isolated child process-е.
fn run_isolated_service_snapshot_child(test_name: &str, document: &str) {
    let fake_tools = TempDir::new().expect("create fallback fake-tools directory");
    install_fake_yt_dlp(fake_tools.path());
    let output = Command::new(env::current_exe().expect("current app-egui test binary"))
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(FALLBACK_CHILD_MARKER_ENV, "1")
        .env(FALLBACK_DOCUMENT_ENV, document)
        .env("PATH", path_with_fake_tools_first(fake_tools.path()))
        .output()
        .expect("spawn isolated service snapshot test child");

    assert!(
        output.status.success(),
        "service snapshot child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Child строит реальный service snapshot и считает full catalog projection.
fn assert_child_catalog_choice_count(expected_choice_count: usize) {
    let locator = service_ytdlp::parse_yt_dlp_media_locator("https://page.example.test/catalog")
        .expect("parse synthetic catalog locator");
    let yt_dlp_config = YtDlpConfig::default();
    let snapshot = service_ytdlp::resolve_yt_dlp_candidate_snapshot_with_config(
        &locator,
        SourceIdentity::new(92),
        ExtractionGeneration::new(1),
        &yt_dlp_config,
    )
    .expect("resolve real catalog service snapshot");
    let planning = snapshot
        .planning_snapshot()
        .expect("map catalog snapshot to planner");
    let runtime = super::super::WebOpenRuntime::new(
        &NetworkConfig::default(),
        &rustiplayer_config::PlayerDemuxConfig::default(),
    )
    .expect("create catalog runtime capability registries");
    let system_capabilities = h264_test_system_capabilities();
    let audio_capabilities = audio::AudioDecodeCapabilitySnapshot::empty()
        .with_available_family(AudioDecodeCodecFamily::Opus);
    let capabilities = PlaybackCapabilitySnapshot::new(
        &runtime.transport_capabilities,
        &runtime.demux_capabilities,
        &system_capabilities,
        audio_capabilities,
    );
    let policy = super::super::selection_policy(
        &rustiplayer_config::WebMediaConfig::default(),
        &[rustiplayer_config::VideoCodec::H264],
    )
    .expect("create catalog playback policy");
    let active_candidate = snapshot
        .accepted_candidates()
        .next()
        .expect("catalog fixture has an accepted candidate");
    let active_selection = snapshot
        .selection_for(active_candidate)
        .expect("catalog active selection");

    let choice_count = super::super::catalog::projected_parent_choice_count(
        super::super::catalog::CatalogAttachmentRequest {
            candidate_snapshot: &snapshot,
            planning_snapshot: &planning,
            capabilities,
            policy: &policy,
            active_selection: &active_selection,
            active_composed: None,
        },
    )
    .expect("project real service catalog choices");
    assert_eq!(choice_count, expected_choice_count);
}

/// Capability report содержит один software-compatible H.264 output.
fn h264_test_system_capabilities() -> SystemCapabilities {
    let backend_id = DecodeBackendId::new("catalog_h").expect("valid catalog fixture backend ID");
    let output = SupportedVideoOutput {
        backend: backend_id.clone(),
        decode_format: SupportedVideoDecodeFormat {
            codec: DecodeVideoCodec::H264,
            profile: VideoProfile::H264(H264Profile::Baseline),
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(3840),
            max_height: Some(2160),
            max_fps: Some(60.0),
            hdr_input: false,
        },
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
    };
    SystemCapabilities {
        schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id,
            display_name: "Catalog fixture H.264 backend".to_owned(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            raw_supported_outputs: vec![output.clone()],
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            diagnostics: Vec::new(),
        }],
        render_backends: Vec::new(),
        playable_video_outputs: vec![output],
    }
}

/// Child использует production extractor normalization и planner snapshot.
fn assert_child_service_snapshot_fallback() {
    let locator =
        service_ytdlp::parse_yt_dlp_media_locator("https://page.example.test/runtime-fallback")
            .expect("parse synthetic page locator");
    let yt_dlp_config = YtDlpConfig::default();
    let snapshot = service_ytdlp::resolve_yt_dlp_candidate_snapshot_with_config(
        &locator,
        SourceIdentity::new(91),
        ExtractionGeneration::new(1),
        &yt_dlp_config,
    )
    .expect("resolve real service candidate snapshot");
    let planning = snapshot
        .planning_snapshot()
        .expect("map real service snapshot to planner");
    let runtime = super::super::WebOpenRuntime::new(
        &NetworkConfig::default(),
        &rustiplayer_config::PlayerDemuxConfig::default(),
    )
    .expect("create app runtime capability registries");
    let system_capabilities = capability_core::SystemCapabilities::empty(1);
    let audio_capabilities = audio::AudioDecodeCapabilitySnapshot::empty();
    let capabilities = PlaybackCapabilitySnapshot::new(
        &runtime.transport_capabilities,
        &runtime.demux_capabilities,
        &system_capabilities,
        audio_capabilities,
    );
    let policy = super::super::selection_policy(
        &rustiplayer_config::WebMediaConfig::default(),
        &[rustiplayer_config::VideoCodec::Vp9],
    )
    .expect("create app playback policy");
    let ranked = ranked_best_playable_candidates(&snapshot, &planning, capabilities, &policy)
        .expect("rank real service candidates");
    assert_eq!(
        ranked.len(),
        2,
        "fixture должна дать две playable alternatives"
    );
    let first_descriptor = ranked[0].descriptor();
    let duplicate_rank = [
        (
            first_descriptor.identity(),
            first_descriptor.semantic_identity(),
        ),
        (
            first_descriptor.identity(),
            first_descriptor.semantic_identity(),
        ),
    ];
    assert!(
        map_ranked_service_candidates(&snapshot, duplicate_rank.into_iter()).is_err(),
        "duplicate planner identity должна fail-closed"
    );
    let missing_exact_identity = CandidateIdentity::new(
        snapshot.source(),
        snapshot.generation(),
        CandidateFormatIdentity::new("missing-planner-candidate")
            .expect("bounded missing test identity"),
    );
    let missing_rank = [(
        &missing_exact_identity,
        first_descriptor.semantic_identity(),
    )];
    assert!(
        map_ranked_service_candidates(&snapshot, missing_rank.into_iter()).is_err(),
        "missing planner identity должна fail-closed"
    );
    let first_selection = snapshot
        .selection_for(ranked[0])
        .expect("first ranked candidate selection");
    let expected_second_selection = snapshot
        .selection_for(ranked[1])
        .expect("second ranked candidate selection");
    let incomplete_planning = PlanningCandidateSnapshot::new(
        planning.source(),
        planning.generation(),
        vec![planning.candidates()[0].clone()],
    )
    .expect("bounded incomplete planning fixture");
    assert!(
        super::super::catalog::projected_parent_choice_count(
            super::super::catalog::CatalogAttachmentRequest {
                candidate_snapshot: &snapshot,
                planning_snapshot: &incomplete_planning,
                capabilities,
                policy: &policy,
                active_selection: &first_selection,
                active_composed: None,
            },
        )
        .is_err(),
        "catalog должен отвергнуть mispaired planning snapshot"
    );

    let neutral_projection =
        crate::web_media_extractor_adapter::ExtractorCatalogProjection::from_snapshot(&snapshot)
            .and_then(|projection| projection.with_active_selection(&first_selection))
            .expect("extractor adapter должен построить neutral active selection");
    let stream_configuration =
        crate::web_media_stream_model::WebMediaStreamConfiguration::from_neutral_catalog(
            &planning,
            capabilities,
            &policy,
            neutral_projection.selection(),
            crate::web_media_stream_model::WebMediaSelectionPreference::GlobalBestPlayable,
        )
        .expect("canonical real snapshot должен построить URL stream model");
    assert_eq!(
        stream_configuration.candidates().len(),
        2,
        "selected + formats duplicate не должен создавать третью URL option"
    );

    let catalog_choice_count = super::super::catalog::projected_parent_choice_count(
        super::super::catalog::CatalogAttachmentRequest {
            candidate_snapshot: &snapshot,
            planning_snapshot: &planning,
            capabilities,
            policy: &policy,
            active_selection: &first_selection,
            active_composed: None,
        },
    )
    .expect("canonical real snapshot должен построить URL catalog");
    assert_eq!(
        catalog_choice_count, 2,
        "selected + formats duplicate не должен создавать третью catalog choice"
    );

    let mut best_attempts = 0_usize;
    let (_, active_selection) = open_ranked_best(ranked.iter().copied(), &|| false, |candidate| {
        best_attempts = best_attempts.saturating_add(1);
        let selection = snapshot
            .selection_for(candidate)
            .expect("ranked candidate selection");
        if best_attempts == 1 {
            Err(ContentProbeRejection::UnsupportedAudio.into())
        } else {
            Ok(selection)
        }
    })
    .expect("second real planner-ranked candidate should succeed");
    assert_eq!(best_attempts, 2);
    assert_eq!(active_selection, expected_second_selection);

    let exact_identity = ExactSelectionIdentity::new(
        first_selection.exact_identity().clone(),
        first_selection.semantic_identity().clone(),
    )
    .expect("same-snapshot exact identity");
    let exact_plan = web_media_playback_plan::plan_playback(
        &planning,
        capabilities,
        &SelectionRequest::Exact(exact_identity),
        &policy,
    )
    .expect("plan same-snapshot exact candidate");
    let exact_candidate = snapshot
        .accepted_candidates()
        .find(|candidate| {
            candidate.descriptor().identity() == exact_plan.selected().exact_identity()
        })
        .expect("map exact plan to real service candidate");
    let mut exact_attempts = 0_usize;
    let exact_error = open_single(exact_candidate, |_| {
        exact_attempts = exact_attempts.saturating_add(1);
        Err::<(), _>(ContentProbeRejection::UnsupportedAudio.into())
    })
    .expect_err("Exact content rejection must remain terminal");
    assert_eq!(exact_attempts, 1);
    assert_eq!(
        exact_error.downcast_ref::<ContentProbeRejection>(),
        Some(&ContentProbeRejection::UnsupportedAudio)
    );
}

/// Устанавливает process-compatible fake `yt-dlp` только в child PATH.
fn install_fake_yt_dlp(fake_tools_directory: &Path) {
    let executable_path = fake_tools_directory.join("yt-dlp");
    let script = concat!(
        "#!/bin/sh\n",
        "set -eu\n",
        "printf '%s\\n' \"${RUSTIPLAYER_FALLBACK_YTDLP_JSON:?missing fixture JSON}\"\n",
    );
    fs::write(&executable_path, script).expect("write fallback fake yt-dlp");
    let mut permissions = fs::metadata(&executable_path)
        .expect("read fallback fake yt-dlp metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable_path, permissions)
        .expect("mark fallback fake yt-dlp executable");
}

/// Строит child-only PATH без глобального mutation параллельного test process-а.
fn path_with_fake_tools_first(fake_tools_directory: &Path) -> OsString {
    let inherited_path = env::var_os("PATH").unwrap_or_default();
    env::join_paths(
        std::iter::once(fake_tools_directory.to_path_buf())
            .chain(env::split_paths(&inherited_path)),
    )
    .expect("join fallback child PATH")
}

/// Реальная форма повторяет selected candidate внутри `formats[]` inventory.
fn fallback_candidate_document() -> &'static str {
    r#"{"id":"runtime-fallback","title":"Runtime fallback","format_id":"higher-quality","url":"https://media.example.test/higher.ogg","protocol":"https","ext":"ogg","container":"ogg","vcodec":null,"acodec":null,"quality":10,"formats":[{"format_id":"higher-quality","url":"https://media.example.test/higher.ogg","protocol":"https","ext":"ogg","container":"ogg","vcodec":null,"acodec":null,"quality":10},{"format_id":"lower-quality","url":"https://media.example.test/lower.ogg","protocol":"https","ext":"ogg","container":"ogg","vcodec":null,"acodec":null,"quality":1}]}"#
}

/// Selected-only audio выше inventory audio, но composition обязана выбрать inventory.
fn selected_audio_catalog_document() -> &'static str {
    r#"{"id":"selected-audio-catalog","title":"Selected audio catalog","format_id":"selected-audio","url":"https://media.example.test/selected.opus","protocol":"https","ext":"opus","container":"ogg","vcodec":"none","acodec":"opus","quality":100,"abr":192,"formats":[{"format_id":"inventory-video","url":"https://media.example.test/video.mp4","protocol":"https","ext":"mp4","container":"mp4","vcodec":"avc1.42001E","acodec":"none","width":1280,"height":720,"fps":30,"dynamic_range":"SDR","quality":5},{"format_id":"inventory-audio","url":"https://media.example.test/audio.opus","protocol":"https","ext":"opus","container":"ogg","vcodec":"none","acodec":"opus","quality":1,"abr":96}]}"#
}

/// Selected-only video остаётся самостоятельным parent-ом без inventory composition.
fn selected_video_catalog_document() -> &'static str {
    r#"{"id":"selected-video-catalog","title":"Selected video catalog","format_id":"selected-video","url":"https://media.example.test/selected.mp4","protocol":"https","ext":"mp4","container":"mp4","vcodec":"avc1.42001E","acodec":"none","width":1280,"height":720,"fps":30,"dynamic_range":"SDR","quality":100,"formats":[{"format_id":"inventory-audio","url":"https://media.example.test/audio.opus","protocol":"https","ext":"opus","container":"ogg","vcodec":"none","acodec":"opus","quality":1,"abr":96}]}"#
}

/// Обычные inventory video и audio образуют один дополнительный composed target.
fn inventory_av_catalog_document() -> &'static str {
    r#"{"id":"inventory-av-catalog","title":"Inventory A/V catalog","formats":[{"format_id":"inventory-video","url":"https://media.example.test/video.mp4","protocol":"https","ext":"mp4","container":"mp4","vcodec":"avc1.42001E","acodec":"none","width":1280,"height":720,"fps":30,"dynamic_range":"SDR","quality":5},{"format_id":"inventory-audio","url":"https://media.example.test/audio.opus","protocol":"https","ext":"opus","container":"ogg","vcodec":"none","acodec":"opus","quality":1,"abr":96}]}"#
}
