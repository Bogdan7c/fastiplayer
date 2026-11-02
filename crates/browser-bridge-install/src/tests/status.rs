//! Матрица статусов: что показывает обзор для каждого состояния диска.

use super::support::{
    FakeFlatpak, Fixture, PERMISSIONS_FULL_BUS, PERMISSIONS_WITHOUT_BUS, RecordingReloader,
    connected, first_manifest_path, set_mode, target_id, write_executable, write_with_parents,
};
use crate::source::HOST_BINARY_NAME;
use crate::{BrowserBridgeStatus, FlatpakError, SourceProblem, UnavailableReason, layout};
use desktop_integration::browser_session_activation_service_file;
use std::path::PathBuf;

/// Статус одной цели из обзора.
fn status_of(installer: &crate::BrowserBridgeInstaller, id: &str) -> BrowserBridgeStatus {
    installer
        .survey()
        .expect("survey")
        .browsers
        .into_iter()
        .find(|report| report.target.as_str() == id)
        .unwrap_or_else(|| panic!("{id} is not in the survey"))
        .status
}

#[test]
fn installed_browser_without_our_files_is_not_connected() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());
    assert_eq!(
        status_of(&installer, "chromium"),
        BrowserBridgeStatus::NotConnected
    );
}

#[test]
fn browsers_that_are_not_installed_are_absent_from_the_survey() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());
    let ids: Vec<_> = installer
        .survey()
        .expect("survey")
        .browsers
        .iter()
        .map(|report| report.target.as_str().to_owned())
        .collect();
    assert_eq!(ids, vec!["chromium".to_owned()]);
}

#[test]
fn leftover_profile_directory_alone_does_not_make_a_browser_installed() {
    let fixture = Fixture::new();
    // Папки профилей от давно удалённых браузеров.
    for leftover in [
        ".config/vivaldi",
        ".mozilla/firefox",
        ".config/BraveSoftware",
        ".var/app/com.google.Chrome/config",
    ] {
        std::fs::create_dir_all(fixture.home.join(leftover)).expect("leftover dir");
    }
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());
    assert!(installer.survey().expect("survey").browsers.is_empty());
}

#[test]
fn non_executable_file_in_path_is_not_a_browser() {
    let fixture = Fixture::new();
    write_executable(&fixture.bin_dir.join("chromium"), b"x");
    set_mode(&fixture.bin_dir.join("chromium"), 0o644);
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());
    assert!(installer.survey().expect("survey").browsers.is_empty());
}

#[test]
fn host_with_different_bytes_is_outdated() {
    let fixture = Fixture::new();
    let (installer, ..) = connected(&fixture, "chromium");
    let host = layout::host_install_path(&fixture.environment, super::support::target("chromium"));
    std::fs::write(&host, b"older version").expect("overwrite");
    set_mode(&host, 0o755);
    assert_eq!(
        status_of(&installer, "chromium"),
        BrowserBridgeStatus::Outdated
    );
}

#[test]
fn host_that_is_not_executable_is_outdated() {
    let fixture = Fixture::new();
    let (installer, ..) = connected(&fixture, "chromium");
    let host = layout::host_install_path(&fixture.environment, super::support::target("chromium"));
    set_mode(&host, 0o644);
    assert_eq!(
        status_of(&installer, "chromium"),
        BrowserBridgeStatus::Outdated
    );
}

#[test]
fn missing_service_file_is_outdated() {
    let fixture = Fixture::new();
    let (installer, ..) = connected(&fixture, "chromium");
    std::fs::remove_file(layout::service_file_path(&fixture.environment)).expect("remove service");
    assert_eq!(
        status_of(&installer, "chromium"),
        BrowserBridgeStatus::Outdated
    );
}

#[test]
fn manifest_of_ours_with_other_content_is_outdated() {
    let fixture = Fixture::new();
    let (installer, ..) = connected(&fixture, "chromium");
    let manifest = first_manifest_path(&fixture, "chromium");
    let host = layout::host_install_path(&fixture.environment, super::support::target("chromium"));
    std::fs::write(
        &manifest,
        serde_json::json!({
            "name": "io.github.bogdan7c.fastiplayer.bridge",
            "path": host,
            "type": "stdio",
            "allowed_origins": ["chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/"],
        })
        .to_string(),
    )
    .expect("rewrite manifest");
    assert_eq!(
        status_of(&installer, "chromium"),
        BrowserBridgeStatus::Outdated
    );
}

#[test]
fn firefox_with_only_one_of_two_manifests_is_outdated() {
    let fixture = Fixture::new();
    let (installer, ..) = connected(&fixture, "firefox");
    let paths =
        layout::manifest_file_paths(&fixture.environment, super::support::target("firefox"));
    std::fs::remove_file(&paths[1]).expect("remove XDG manifest");
    assert_eq!(
        status_of(&installer, "firefox"),
        BrowserBridgeStatus::Outdated
    );
}

#[test]
fn service_waking_another_existing_copy_is_connected_to_another_copy() {
    let fixture = Fixture::new();
    let (installer, ..) = connected(&fixture, "chromium");
    let other_player = fixture.root.path().join("installed/fastiplayer");
    write_executable_with_parents(&other_player);
    write_with_parents(
        &layout::service_file_path(&fixture.environment),
        &browser_session_activation_service_file(&other_player).expect("service"),
    );
    assert_eq!(
        status_of(&installer, "chromium"),
        BrowserBridgeStatus::ConnectedToAnotherCopy {
            player: other_player,
            player_exists: true
        }
    );
}

#[test]
fn service_waking_a_deleted_copy_says_the_player_no_longer_exists() {
    let fixture = Fixture::new();
    let (installer, ..) = connected(&fixture, "chromium");
    let gone_player = PathBuf::from("/nonexistent/opt/fastiplayer");
    write_with_parents(
        &layout::service_file_path(&fixture.environment),
        &browser_session_activation_service_file(&gone_player).expect("service"),
    );
    assert_eq!(
        status_of(&installer, "chromium"),
        BrowserBridgeStatus::ConnectedToAnotherCopy {
            player: gone_player,
            player_exists: false
        }
    );
}

#[test]
fn flatpak_without_bus_access_needs_permission_and_with_talk_mask_is_connected() {
    let fixture = Fixture::new();
    let flatpak = FakeFlatpak::default().with_app("com.google.Chrome", PERMISSIONS_WITHOUT_BUS);
    let installer = fixture.installer(&flatpak, &RecordingReloader::default());
    installer
        .connect(&target_id("google-chrome-flatpak"))
        .expect("connect");
    assert_eq!(
        status_of(&installer, "google-chrome-flatpak"),
        BrowserBridgeStatus::NeedsFlatpakPermission
    );

    flatpak
        .state
        .permissions
        .lock()
        .expect("permissions")
        .insert(
            "com.google.Chrome".into(),
            super::support::PERMISSIONS_WITH_TALK_MASK.into(),
        );
    assert_eq!(
        status_of(&installer, "google-chrome-flatpak"),
        BrowserBridgeStatus::Connected
    );
}

#[test]
fn flatpak_not_yet_connected_does_not_query_permissions() {
    let fixture = Fixture::new();
    let flatpak = FakeFlatpak::default().with_app("com.google.Chrome", PERMISSIONS_FULL_BUS);
    *flatpak.state.permissions_error.lock().expect("error") = Some(FlatpakError::TimedOut);
    let installer = fixture.installer(&flatpak, &RecordingReloader::default());
    assert_eq!(
        status_of(&installer, "google-chrome-flatpak"),
        BrowserBridgeStatus::NotConnected
    );
}

#[test]
fn unreadable_flatpak_permissions_make_the_status_unavailable() {
    let fixture = Fixture::new();
    let flatpak = FakeFlatpak::default().with_app("com.google.Chrome", PERMISSIONS_FULL_BUS);
    let installer = fixture.installer(&flatpak, &RecordingReloader::default());
    installer
        .connect(&target_id("google-chrome-flatpak"))
        .expect("connect");
    *flatpak.state.permissions_error.lock().expect("error") = Some(FlatpakError::TimedOut);
    assert_eq!(
        status_of(&installer, "google-chrome-flatpak"),
        BrowserBridgeStatus::Unavailable(UnavailableReason::FlatpakPermissions(
            FlatpakError::TimedOut
        ))
    );
}

#[test]
fn flatpak_tool_failure_is_reported_in_the_survey_and_hides_only_flatpak_targets() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let flatpak = FakeFlatpak::default();
    *flatpak.state.list_error.lock().expect("error") = Some(FlatpakError::TimedOut);
    let installer = fixture.installer(&flatpak, &RecordingReloader::default());
    let survey = installer.survey().expect("survey");
    assert_eq!(survey.flatpak_problem, Some(FlatpakError::TimedOut));
    assert_eq!(survey.browsers.len(), 1);
}

#[test]
fn missing_host_next_to_the_player_blocks_connect_but_not_seeing_the_real_state() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let (installer, ..) = connected(&fixture, "chromium");
    std::fs::remove_file(fixture.app_dir.join(HOST_BINARY_NAME)).expect("remove host");
    let installer_without_host =
        fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());
    drop(installer);

    let survey = installer_without_host.survey().expect("survey");

    let expected_problem = SourceProblem::HostBinaryMissing {
        expected: fixture.app_dir.join(HOST_BINARY_NAME),
        player: fixture.player.clone(),
    };
    assert_eq!(survey.source, Err(expected_problem.clone()));
    // Файлы наши и на месте: статус настоящий, а не «недоступно».
    assert_eq!(survey.browsers[0].status, BrowserBridgeStatus::Connected);
    let error = installer_without_host
        .connect(&target_id("chromium"))
        .expect_err("source problem");
    assert_eq!(
        error.kind,
        crate::InstallErrorKind::Source(expected_problem)
    );
}

#[test]
fn survey_without_a_healthy_source_still_reports_not_connected_and_outdated() {
    let fixture = Fixture::new();
    let (_installer, ..) = connected(&fixture, "chromium");
    let host = layout::host_install_path(&fixture.environment, super::support::target("chromium"));
    set_mode(&host, 0o644);
    std::fs::remove_file(fixture.app_dir.join(HOST_BINARY_NAME)).expect("remove host");
    fixture.install_browser_executable("google-chrome-stable");
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());

    assert_eq!(
        status_of(&installer, "chromium"),
        BrowserBridgeStatus::Outdated
    );
    assert_eq!(
        status_of(&installer, "google-chrome"),
        BrowserBridgeStatus::NotConnected
    );
}

fn write_executable_with_parents(path: &std::path::Path) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("parents");
    write_executable(path, b"#!other-player");
}
