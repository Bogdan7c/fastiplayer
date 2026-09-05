use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use service_ytdlp::{
    ExtractorProcessInvocation, ExtractorProcessLauncher, YtDlpExtractorAdapter, YtDlpLiveIntent,
};
use web_media_core::{
    ExtractionGeneration, ExtractorInvocationReason, SourceIdentity, WebMediaPresentationKind,
};

use super::{ExtractorCatalogProjection, presentation_from_live_intent};

/// N01 presentation mapping сохраняет VOD/live и fail-closed lifecycle states.
#[test]
fn live_intent_maps_to_exact_neutral_presentation() {
    assert_eq!(
        presentation_from_live_intent(YtDlpLiveIntent::Unspecified)
            .expect("absent live fields preserve existing VOD admission"),
        WebMediaPresentationKind::Vod
    );
    assert_eq!(
        presentation_from_live_intent(YtDlpLiveIntent::NotLive).expect("explicit VOD remains VOD"),
        WebMediaPresentationKind::Vod
    );
    assert_eq!(
        presentation_from_live_intent(YtDlpLiveIntent::Live).expect("live remains live"),
        WebMediaPresentationKind::Live
    );

    for rejected in [
        YtDlpLiveIntent::Upcoming,
        YtDlpLiveIntent::PostLive,
        YtDlpLiveIntent::Incompatible,
    ] {
        assert!(presentation_from_live_intent(rejected).is_err());
    }
}

/// Hermetic launcher сохраняет production Command/process-group и подменяет только per-command PATH.
struct ProjectionFixtureLauncher {
    executable_directory: PathBuf,
    invocations: Mutex<Vec<ExtractorProcessInvocation>>,
}

impl ExtractorProcessLauncher for ProjectionFixtureLauncher {
    fn spawn(
        &self,
        command: &mut Command,
        invocation: ExtractorProcessInvocation,
    ) -> io::Result<Child> {
        self.invocations
            .lock()
            .map_err(|_| io::Error::other("projection fixture invocation lock poisoned"))?
            .push(invocation);
        let mut command_path = OsString::from(&self.executable_directory);
        if let Some(system_path) = std::env::var_os("PATH") {
            command_path.push(":");
            command_path.push(system_path);
        }
        command.env("PATH", command_path);
        command.spawn()
    }
}

/// Public extractor snapshot доходит до existing neutral catalog/selection/presentation без потерь.
#[cfg(unix)]
#[test]
fn hermetic_snapshot_projects_formats_metadata_selection_and_presentation() {
    let fixture_directory = std::env::temp_dir().join(format!(
        "fastiplayer-n03-app-projection-{}",
        std::process::id()
    ));
    if fixture_directory.exists() {
        fs::remove_dir_all(&fixture_directory).expect("remove stale projection fixture");
    }
    fs::create_dir(&fixture_directory).expect("create projection fixture directory");
    let executable = fixture_directory.join("yt-dlp");
    fs::write(
        &executable,
        r#"#!/bin/sh
printf '%s\n' '{"title":"Projected HTML media","duration":17,"is_live":false,"formats":[{"format_id":"projected-18","url":"https://media.invalid/projected.mp4","protocol":"https","ext":"mp4","container":"mp4","vcodec":"avc1.42001E","acodec":"mp4a.40.2","dynamic_range":"SDR"}]}'
"#,
    )
    .expect("write projection fixture executable");
    let mut permissions = fs::metadata(&executable)
        .expect("read projection executable metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable, permissions).expect("make projection fixture executable");
    let launcher = Arc::new(ProjectionFixtureLauncher {
        executable_directory: fixture_directory.clone(),
        invocations: Mutex::new(Vec::new()),
    });
    let adapter = YtDlpExtractorAdapter::with_process_launcher(launcher.clone());
    let locator = service_ytdlp::parse_yt_dlp_media_locator("https://catalog.example/watch/17")
        .expect("parse projection fixture locator");

    let snapshot = adapter
        .resolve_candidate_snapshot_with_cancellation(
            &locator,
            SourceIdentity::new(3017),
            ExtractionGeneration::new(1),
            &fastiplayer_config::YtDlpConfig {
                resolve_timeout_ms: 2_000,
                ..fastiplayer_config::YtDlpConfig::default()
            },
            ExtractorInvocationReason::PageMediaResolution,
            &|| false,
        )
        .expect("resolve projection fixture snapshot");
    let active_candidate = snapshot
        .accepted_candidates()
        .next()
        .expect("projection fixture has accepted candidate");
    let active_selection = snapshot
        .selection_for(active_candidate)
        .expect("projection fixture candidate has exact selection");
    let projection = ExtractorCatalogProjection::from_snapshot(&snapshot)
        .expect("project neutral catalog")
        .with_active_selection(&active_selection)
        .expect("project neutral active selection");

    assert_eq!(projection.catalog().candidates().len(), 1);
    assert_eq!(
        projection.selection().parent().exact(),
        active_selection.exact_identity()
    );
    assert_eq!(
        projection.selection().parent().semantic(),
        active_selection.semantic_identity()
    );
    assert_eq!(projection.presentation(), WebMediaPresentationKind::Vod);
    let metadata = projection.into_playlist_metadata();
    assert_eq!(metadata.title(), Some("Projected HTML media"));
    assert_eq!(metadata.duration(), Some(Duration::from_secs(17)));
    assert_eq!(
        launcher
            .invocations
            .lock()
            .expect("projection fixture invocation lock")
            .len(),
        1
    );

    fs::remove_dir_all(fixture_directory).expect("remove projection fixture directory");
}

/// Production direct-resource classification остаётся вне extractor adapter и даёт zero spawn.
#[cfg(unix)]
#[test]
fn native_direct_fixture_cannot_reach_extractor_launcher() {
    let native_locator = crate::direct_progressive_open::classify_direct_media_url(
        "https://media.example.invalid/video.mp4",
    )
    .expect("direct MP4 fixture must remain a native source intent");

    assert!(
        native_locator
            .safe_label()
            .contains("direct media https://")
    );
    let direct_owner_source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/direct_progressive_open.rs"),
    )
    .expect("direct progressive owner source должен читаться");
    for forbidden_reference in ["service_ytdlp", "yt_dlp", "YtDlp", "Extractor"] {
        assert!(
            !direct_owner_source.contains(forbidden_reference),
            "direct progressive owner не должен ссылаться на {forbidden_reference}"
        );
    }
}

/// Собирает Rust sources рекурсивно без нового test-only dependency.
fn collect_rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("app source directory должна читаться")
    {
        let path = entry.expect("app source entry должна читаться").path();
        if path.is_dir() {
            collect_rust_sources(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
}

/// Test sources вправе строить provider fixtures, но production lifecycle — только adapters.
fn is_test_source(relative_path: &str) -> bool {
    relative_path.contains("/tests/")
        || relative_path.ends_with("/tests.rs")
        || relative_path.ends_with("_tests.rs")
}

/// Exact allowlist не даёт provider DTO утечь в queue/session/UI/persistence owners.
#[test]
fn provider_dtos_stay_inside_exact_extractor_adapter_allowlist() {
    const PROVIDER_DTO_MARKERS: &[&str] = &[
        "YtDlpCandidateSelection",
        "YtDlpCandidateSnapshot",
        "YtDlpComposedSelection",
        "YtDlpDashFragment",
        "YtDlpDashInputKind",
        "YtDlpDashRequestMaterial",
        "YtDlpDashTransportComponent",
        "YtDlpDurableReopen",
        "YtDlpHlsManifestInputKind",
        "YtDlpLiveIntent",
        "YtDlpNormalizedCandidate",
        "YtDlpPlaylistMetadata",
        "YtDlpProgressiveTransportRequestContext",
        "YtDlpTopology",
        "YtDlpTransportRequestContext",
    ];
    const ALLOWED_PRODUCTION_SOURCES: &[&str] = &[
        "playlist_runtime/url_import.rs",
        "startup_media/yt_dlp.rs",
        "url_topology_drafts.rs",
        "url_topology_drafts/mapper.rs",
        "url_topology_drafts/model.rs",
        "url_topology_drafts/service_adapter.rs",
        "web_media_dash_open.rs",
        "web_media_dash_refresh.rs",
        "web_media_extractor_adapter.rs",
        "web_media_hls_open.rs",
        "web_media_hls_refresh.rs",
        "web_media_open.rs",
        "web_media_open/catalog.rs",
        "web_media_open/component_variants.rs",
        "web_media_open/content_probe_fallback.rs",
        "web_media_open/hds.rs",
        "web_media_open/preparation.rs",
        "web_media_open/runtime.rs",
        "web_media_open/smooth.rs",
        "web_media_open/source_state.rs",
    ];
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut rust_sources = Vec::new();
    collect_rust_sources(&source_root, &mut rust_sources);
    let mut actual_sources = rust_sources
        .into_iter()
        .filter_map(|source_path| {
            let relative_path = source_path
                .strip_prefix(&source_root)
                .expect("collected source обязан быть внутри app src")
                .to_string_lossy()
                .replace('\\', "/");
            if is_test_source(&relative_path) {
                return None;
            }
            let source = fs::read_to_string(&source_path).expect("app Rust source должен читаться");
            PROVIDER_DTO_MARKERS
                .iter()
                .any(|marker| source.contains(marker))
                .then_some(relative_path)
        })
        .collect::<Vec<_>>();
    actual_sources.sort();

    assert_eq!(actual_sources, ALLOWED_PRODUCTION_SOURCES);
}

/// Durable active source может хранить selection identity, но не transport material.
#[test]
fn active_source_shape_excludes_ephemeral_endpoint_header_and_cookie_types() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let extractor_state = fs::read_to_string(source_root.join("web_media_open/source_state.rs"))
        .expect("extractor source-state owner должен читаться");
    let neutral_source = fs::read_to_string(source_root.join("media_open/web.rs"))
        .expect("neutral web source owner должен читаться");
    let durable_source_shape = format!("{extractor_state}\n{neutral_source}");

    assert!(durable_source_shape.contains("YtDlpCandidateSelection"));
    for forbidden_type in [
        "YtDlpNormalizedCandidate",
        "YtDlpTransportRequestContext",
        "YtDlpProgressiveTransportRequestContext",
        "YtDlpDashFragment",
        "TransportOpenRequest",
        "SecretHttpUrl",
        "HttpHeader",
        "Cookie",
    ] {
        assert!(
            !durable_source_shape.contains(forbidden_type),
            "active source не должен хранить ephemeral type {forbidden_type}"
        );
    }
}
