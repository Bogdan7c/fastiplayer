//! Кнопка «Allow»: фейковый `flatpak` записывает выданное, статус пересчитывается.

use super::support::{FakeFlatpak, Fixture, PERMISSIONS_WITHOUT_BUS, RecordingReloader, target_id};
use crate::{AllowError, BrowserBridgeStatus, FlatpakError};

#[test]
fn allow_grants_the_exact_app_id_and_recomputes_the_status() {
    let fixture = Fixture::new();
    let flatpak = FakeFlatpak::default().with_app("com.google.Chrome", PERMISSIONS_WITHOUT_BUS);
    let installer = fixture.installer(&flatpak, &RecordingReloader::default());
    let connect = installer
        .connect(&target_id("google-chrome-flatpak"))
        .expect("connect");
    assert_eq!(connect.status, BrowserBridgeStatus::NeedsFlatpakPermission);

    let outcome = installer
        .allow_flatpak_bus_access(&target_id("google-chrome-flatpak"))
        .expect("allow");

    assert_eq!(flatpak.grants(), vec!["com.google.Chrome".to_owned()]);
    assert_eq!(outcome.status, BrowserBridgeStatus::Connected);
}

#[test]
fn allow_on_a_package_browser_is_refused_without_touching_flatpak() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let flatpak = FakeFlatpak::default();
    let installer = fixture.installer(&flatpak, &RecordingReloader::default());
    let error = installer
        .allow_flatpak_bus_access(&target_id("chromium"))
        .expect_err("not flatpak");
    assert_eq!(error, AllowError::NotAFlatpakTarget);
    assert!(flatpak.grants().is_empty());
}

#[test]
fn allow_on_unknown_target_or_flatpak_firefox_or_missing_app_is_refused() {
    let fixture = Fixture::new();
    let flatpak = FakeFlatpak::default().with_app("org.mozilla.firefox", "");
    let installer = fixture.installer(&flatpak, &RecordingReloader::default());
    assert_eq!(
        installer
            .allow_flatpak_bus_access(&target_id("netscape"))
            .expect_err("unknown"),
        AllowError::UnknownTarget
    );
    assert_eq!(
        installer
            .allow_flatpak_bus_access(&target_id("firefox-flatpak"))
            .expect_err("firefox"),
        AllowError::FlatpakFirefoxFamilyUnsupported
    );
    assert_eq!(
        installer
            .allow_flatpak_bus_access(&target_id("google-chrome-flatpak"))
            .expect_err("not installed"),
        AllowError::NotInstalled
    );
    assert!(flatpak.grants().is_empty());
}

#[test]
fn tool_failure_is_returned_and_permissions_stay_unchanged() {
    let fixture = Fixture::new();
    let flatpak = FakeFlatpak::default().with_app("com.google.Chrome", PERMISSIONS_WITHOUT_BUS);
    let failure = FlatpakError::Failed {
        exit_code: Some(1),
        stderr: "boom".into(),
    };
    *flatpak.state.grant_error.lock().expect("error") = Some(failure.clone());
    let installer = fixture.installer(&flatpak, &RecordingReloader::default());
    installer
        .connect(&target_id("google-chrome-flatpak"))
        .expect("connect");

    let error = installer
        .allow_flatpak_bus_access(&target_id("google-chrome-flatpak"))
        .expect_err("failure");

    assert_eq!(error, AllowError::Tool(failure));
    let survey = installer.survey().expect("survey");
    assert_eq!(
        survey.browsers[0].status,
        BrowserBridgeStatus::NeedsFlatpakPermission
    );
}
