//! Focused-тесты границы `YtDlpCandidateOpenIntent` после переезда в crate.
//!
//! Намерение создаётся только именованными конструкторами, а `app-egui` читает
//! его через getter-ы. Тесты закрепляют, что каждый конструктор сохраняет exact
//! selection, quality preference и component intent без подмены.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Arc;

use service_ytdlp::{
    ExtractorProcessInvocation, ExtractorProcessLauncher, YtDlpCandidateSelection,
    YtDlpCandidateSnapshot, YtDlpExtractorAdapter,
};
use web_media_core::{
    ExtractionGeneration, ExtractorInvocationReason, SourceIdentity, StreamLayout,
};

use super::YtDlpCandidateOpenIntent;
use super::component_variants::YtDlpComponentSelectionOpenIntent;
use super::component_variants_tests::{configuration_for, parent, video_catalog, video_selection};
use crate::web_media_stream_model::WebMediaSelectionPreference;

/// Подставной `yt-dlp`: один video-only и один audio-only формат одного источника.
const SEPARATE_AV_YT_DLP_SCRIPT: &str = r#"#!/bin/sh
printf '%s\n' '{"title":"Intent fixture","duration":17,"is_live":false,"formats":[{"format_id":"video-1080","url":"https://media.invalid/video.webm","protocol":"https","ext":"webm","container":"webm","vcodec":"vp09.00.40.08","acodec":"none","width":1920,"height":1080,"dynamic_range":"SDR"},{"format_id":"audio-opus","url":"https://media.invalid/audio.webm","protocol":"https","ext":"webm","container":"webm","vcodec":"none","acodec":"opus","abr":128}]}'
"#;

/// Запускает production `Command`, подменяя только PATH на каталог с подставным скриптом.
struct ScriptedYtDlpLauncher {
    executable_directory: PathBuf,
}

impl ExtractorProcessLauncher for ScriptedYtDlpLauncher {
    fn spawn(
        &self,
        command: &mut Command,
        _invocation: ExtractorProcessInvocation,
    ) -> io::Result<Child> {
        let mut command_path = OsString::from(&self.executable_directory);
        if let Some(system_path) = std::env::var_os("PATH") {
            command_path.push(":");
            command_path.push(system_path);
        }
        command.env("PATH", command_path);
        command.spawn()
    }
}

/// Получает настоящий provider snapshot через public extractor adapter без сети.
fn separate_av_snapshot(fixture_name: &str) -> YtDlpCandidateSnapshot {
    let fixture_directory = std::env::temp_dir().join(format!(
        "fastiplayer-open-intent-{fixture_name}-{}",
        std::process::id()
    ));
    if fixture_directory.exists() {
        fs::remove_dir_all(&fixture_directory).expect("remove stale intent fixture");
    }
    fs::create_dir(&fixture_directory).expect("create intent fixture directory");
    let executable = fixture_directory.join("yt-dlp");
    fs::write(&executable, SEPARATE_AV_YT_DLP_SCRIPT).expect("write intent fixture executable");
    let mut permissions = fs::metadata(&executable)
        .expect("read intent fixture metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable, permissions).expect("make intent fixture executable");
    let adapter = YtDlpExtractorAdapter::with_process_launcher(Arc::new(ScriptedYtDlpLauncher {
        executable_directory: fixture_directory.clone(),
    }));
    let locator = service_ytdlp::parse_yt_dlp_media_locator("https://catalog.example/watch/17")
        .expect("parse intent fixture locator");
    let snapshot = adapter
        .resolve_candidate_snapshot_with_cancellation(
            &locator,
            SourceIdentity::new(4017),
            ExtractionGeneration::new(1),
            &fastiplayer_config::YtDlpConfig {
                resolve_timeout_ms: 2_000,
                ..fastiplayer_config::YtDlpConfig::default()
            },
            ExtractorInvocationReason::PageMediaResolution,
            &|| false,
        )
        .expect("resolve intent fixture snapshot");
    // Snapshot уже в памяти, каталог со скриптом больше не нужен.
    fs::remove_dir_all(&fixture_directory).expect("remove intent fixture directory");
    snapshot
}

/// Exact selection первого accepted candidate-а с заданной формой layout.
fn selection_with_layout(
    snapshot: &YtDlpCandidateSnapshot,
    matches_layout: impl Fn(&StreamLayout) -> bool,
) -> YtDlpCandidateSelection {
    let candidate = snapshot
        .accepted_candidates()
        .find(|candidate| matches_layout(candidate.descriptor().layout()))
        .expect("fixture содержит candidate нужной формы");
    snapshot
        .selection_for(candidate)
        .expect("fixture candidate имеет exact selection")
}

fn video_only_selection(snapshot: &YtDlpCandidateSnapshot) -> YtDlpCandidateSelection {
    selection_with_layout(snapshot, |layout| {
        matches!(layout, StreamLayout::VideoOnly(_))
    })
}

fn audio_only_selection(snapshot: &YtDlpCandidateSnapshot) -> YtDlpCandidateSelection {
    selection_with_layout(snapshot, |layout| {
        matches!(layout, StreamLayout::AudioOnly(_))
    })
}

#[test]
fn best_playable_requests_provider_default_components() {
    assert_eq!(
        YtDlpCandidateOpenIntent::BestPlayable.component_selection_intent(),
        YtDlpComponentSelectionOpenIntent::ProviderDefault
    );
}

#[test]
fn exact_parent_provider_default_keeps_selection_and_preference_but_resets_components() {
    let snapshot = separate_av_snapshot("exact-default");
    let selection = video_only_selection(&snapshot);
    let preference = WebMediaSelectionPreference::GlobalPreferredHeight(720);

    let intent = YtDlpCandidateOpenIntent::exact_parent_provider_default(
        Box::new(selection.clone()),
        preference,
    );

    let YtDlpCandidateOpenIntent::Exact(exact) = &intent else {
        panic!("ожидался exact intent");
    };
    assert_eq!(exact.selection(), &selection);
    assert_eq!(exact.preference(), preference);
    assert_eq!(
        intent.component_selection_intent(),
        YtDlpComponentSelectionOpenIntent::ProviderDefault
    );
}

#[test]
fn exact_preserving_configuration_inherits_installed_preference_and_component_intent() {
    let snapshot = separate_av_snapshot("exact-preserving");
    let selection = video_only_selection(&snapshot);
    // Fixture-конфигурация установлена с GlobalPreferredHeight(1080) и без component catalog-а.
    let installed_configuration = configuration_for(parent(1, 2, "parent", "stable-parent"));

    let intent = YtDlpCandidateOpenIntent::exact_preserving_installed_stream_configuration(
        Box::new(selection.clone()),
        &installed_configuration,
    );

    let YtDlpCandidateOpenIntent::Exact(exact) = &intent else {
        panic!("ожидался exact intent");
    };
    assert_eq!(exact.selection(), &selection);
    assert_eq!(exact.preference(), installed_configuration.preference());
    assert_eq!(
        intent.component_selection_intent(),
        YtDlpComponentSelectionOpenIntent::ProviderDefault
    );
}

#[test]
fn exact_with_semantic_selection_keeps_installed_preference_and_carries_request() {
    let snapshot = separate_av_snapshot("exact-semantic");
    let selection = video_only_selection(&snapshot);
    let installed_parent = parent(1, 2, "parent", "stable-parent");
    let installed_configuration = configuration_for(installed_parent.clone());
    let catalog = video_catalog(installed_parent, 7, "stable-720", "stable-1080");
    let semantic_request = video_selection(&catalog, 0).semantic_rematch_request();

    let intent = YtDlpCandidateOpenIntent::exact_with_component_semantic_selection(
        Box::new(selection.clone()),
        &installed_configuration,
        semantic_request.clone(),
    );

    let YtDlpCandidateOpenIntent::Exact(exact) = &intent else {
        panic!("ожидался exact intent");
    };
    assert_eq!(exact.selection(), &selection);
    assert_eq!(exact.preference(), installed_configuration.preference());
    assert_eq!(
        intent.component_selection_intent(),
        YtDlpComponentSelectionOpenIntent::Semantic(semantic_request)
    );
}

#[test]
fn composed_exposes_composition_parent_and_preference_with_provider_default_components() {
    let snapshot = separate_av_snapshot("composed");
    let video = video_only_selection(&snapshot);
    let audio = audio_only_selection(&snapshot);
    let composed_selection = snapshot
        .compose_inventory_av(&video, &audio)
        .expect("fixture video-only + audio-only составляются");
    let preference = WebMediaSelectionPreference::GlobalPreferredHeight(1080);

    let intent = YtDlpCandidateOpenIntent::composed(
        Box::new(composed_selection.clone()),
        Box::new(video.clone()),
        preference,
    );

    let YtDlpCandidateOpenIntent::Composed(composed) = &intent else {
        panic!("ожидался composed intent");
    };
    assert_eq!(composed.selection(), &composed_selection);
    assert_eq!(composed.parent_preference(), &video);
    assert_eq!(composed.preference(), preference);
    assert_eq!(
        intent.component_selection_intent(),
        YtDlpComponentSelectionOpenIntent::ProviderDefault
    );
}
