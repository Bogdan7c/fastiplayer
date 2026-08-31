use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
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
        "rustiplayer-n03-app-projection-{}",
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
            &rustiplayer_config::YtDlpConfig {
                resolve_timeout_ms: 2_000,
                ..rustiplayer_config::YtDlpConfig::default()
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
    let launcher = Arc::new(ProjectionFixtureLauncher {
        executable_directory: PathBuf::from("/nonexistent/n03-native-spy"),
        invocations: Mutex::new(Vec::new()),
    });
    let _extractor_adapter = YtDlpExtractorAdapter::with_process_launcher(launcher.clone());

    let native_locator =
        service_direct_media::parse_direct_media_url("https://media.example.invalid/video.mp4")
            .expect("direct MP4 fixture must remain a native source intent");

    assert!(
        native_locator
            .safe_label()
            .contains("direct media https://")
    );
    assert!(
        launcher
            .invocations
            .lock()
            .expect("native zero-spawn invocation lock")
            .is_empty()
    );
}
