//! Архитектурные сторожа app-egui для provider DTO yt-dlp.
//!
//! Раньше жили в тестах `web_media_extractor_adapter`, но сканируют исходники
//! именно app-egui, поэтому остались здесь после переезда адаптера в
//! `media-source-open`. Сам адаптер сторожится аналогичным тестом в своём crate.

use std::fs;
use std::path::{Path, PathBuf};

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
        "web_media_hls_open.rs",
        "web_media_hls_refresh.rs",
        "web_media_open.rs",
        "web_media_open/catalog.rs",
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
