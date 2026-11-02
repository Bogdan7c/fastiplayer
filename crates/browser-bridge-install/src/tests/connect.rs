//! Подключение: файлы в нужных местах с нужным содержимым, повтор, предупреждения.

use browser_bridge_protocol::extension_identity::{
    CHROMIUM_DEVELOPMENT_EXTENSION_ID, FIREFOX_EXTENSION_ID, NATIVE_HOST_NAME,
};
use desktop_integration::{
    BROWSER_SESSION_BUS_NAME, SessionBusReloadError, browser_session_activation_service_file,
};
use serde_json::Value;

use super::support::{
    FakeFlatpak, Fixture, RecordingReloader, connected, first_manifest_path, mode_of, target,
    target_id, write_executable, write_with_parents,
};
use crate::{
    BrowserBridgeStatus, InstallErrorKind, InstallStep, InstallWarning, TARGETS, UnsupportedReason,
};

/// Ожидаемые места по каждой подключаемой цели: каталоги файлов подключения и посредник,
/// относительно HOME. Записаны вручную, независимо от кода `layout`.
fn expected_places(id: &str) -> (Vec<String>, String) {
    const SYSTEM_HOST: &str = ".local/share/fastiplayer/browser-host/fastiplayer-browser-host";
    let flatpak_host = |app_id: &str| {
        format!(".var/app/{app_id}/data/fastiplayer/browser-host/fastiplayer-browser-host")
    };
    let flatpak_manifests = |app_id: &str, dir: &str| {
        vec![format!(
            ".var/app/{app_id}/config/{dir}/NativeMessagingHosts"
        )]
    };
    let system_chromium = |dir: &str| vec![format!(".config/{dir}/NativeMessagingHosts")];
    match id {
        "firefox" => (
            vec![
                ".mozilla/native-messaging-hosts".into(),
                ".config/mozilla/native-messaging-hosts".into(),
            ],
            SYSTEM_HOST.into(),
        ),
        "librewolf" => (
            vec![".librewolf/native-messaging-hosts".into()],
            SYSTEM_HOST.into(),
        ),
        "waterfox" => (
            vec![".waterfox/native-messaging-hosts".into()],
            SYSTEM_HOST.into(),
        ),
        "chromium" => (system_chromium("chromium"), SYSTEM_HOST.into()),
        "google-chrome" => (system_chromium("google-chrome"), SYSTEM_HOST.into()),
        "brave" => (
            system_chromium("BraveSoftware/Brave-Browser"),
            SYSTEM_HOST.into(),
        ),
        "vivaldi" => (system_chromium("vivaldi"), SYSTEM_HOST.into()),
        "microsoft-edge" => (system_chromium("microsoft-edge"), SYSTEM_HOST.into()),
        "google-chrome-flatpak" => (
            flatpak_manifests("com.google.Chrome", "google-chrome"),
            flatpak_host("com.google.Chrome"),
        ),
        "chromium-flatpak" => (
            flatpak_manifests("org.chromium.Chromium", "chromium"),
            flatpak_host("org.chromium.Chromium"),
        ),
        "ungoogled-chromium-flatpak" => (
            flatpak_manifests(
                "io.github.ungoogled_software.ungoogled_chromium",
                "chromium",
            ),
            flatpak_host("io.github.ungoogled_software.ungoogled_chromium"),
        ),
        "brave-flatpak" => (
            flatpak_manifests("com.brave.Browser", "BraveSoftware/Brave-Browser"),
            flatpak_host("com.brave.Browser"),
        ),
        "vivaldi-flatpak" => (
            flatpak_manifests("com.vivaldi.Vivaldi", "vivaldi"),
            flatpak_host("com.vivaldi.Vivaldi"),
        ),
        "microsoft-edge-flatpak" => (
            flatpak_manifests("com.microsoft.Edge", "microsoft-edge"),
            flatpak_host("com.microsoft.Edge"),
        ),
        other => panic!("no expectations for target {other}"),
    }
}

/// Цели, которые подключать нельзя (Firefox-семейство во Flatpak).
fn is_unsupported(id: &str) -> bool {
    matches!(
        id,
        "firefox-flatpak" | "librewolf-flatpak" | "waterfox-flatpak"
    )
}

#[test]
fn every_catalog_target_gets_correct_files_in_correct_places() {
    let mut connected_count = 0;
    for catalog_target in TARGETS.iter().filter(|target| !is_unsupported(target.id)) {
        let fixture = Fixture::new();
        let (installer, _flatpak, reloader) = connected(&fixture, catalog_target.id);
        let (manifest_dirs, host_relative) = expected_places(catalog_target.id);

        // Посредник: те же байты и права 0755.
        let host = fixture.home.join(&host_relative);
        assert_eq!(
            std::fs::read(&host).expect("host copied"),
            super::support::HOST_BYTES,
            "{}",
            catalog_target.id
        );
        assert_eq!(mode_of(&host), 0o755, "{}", catalog_target.id);

        // Файл(ы) подключения: корректный JSON семейства.
        for manifest_dir in &manifest_dirs {
            let manifest_path = fixture
                .home
                .join(manifest_dir)
                .join(format!("{NATIVE_HOST_NAME}.json"));
            let parsed: Value = serde_json::from_str(
                &std::fs::read_to_string(&manifest_path)
                    .unwrap_or_else(|_| panic!("manifest missing: {}", manifest_path.display())),
            )
            .expect("manifest is valid JSON");
            assert_eq!(parsed["name"], NATIVE_HOST_NAME);
            assert_eq!(parsed["type"], "stdio");
            assert_eq!(parsed["path"], host.to_str().expect("utf8"));
            match catalog_target.family {
                browser_bridge_protocol::extension_identity::BrowserFamily::Chromium => {
                    assert_eq!(
                        parsed["allowed_origins"],
                        serde_json::json!([format!(
                            "chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/"
                        )])
                    );
                    assert!(parsed.get("allowed_extensions").is_none());
                }
                browser_bridge_protocol::extension_identity::BrowserFamily::Firefox => {
                    assert_eq!(
                        parsed["allowed_extensions"],
                        serde_json::json!([FIREFOX_EXTENSION_ID])
                    );
                    assert!(parsed.get("allowed_origins").is_none());
                }
            }
        }

        // Служба: ровно то, что генерирует desktop-integration для этого плеера.
        let service = fixture.home.join(format!(
            ".local/share/dbus-1/services/{BROWSER_SESSION_BUS_NAME}.service"
        ));
        assert_eq!(
            std::fs::read_to_string(&service).expect("service file"),
            browser_session_activation_service_file(&fixture.player).expect("service content")
        );
        assert_eq!(reloader.calls(), 1, "{}", catalog_target.id);

        // И статус после подключения — Connected.
        let survey = installer.survey().expect("survey");
        let report = survey
            .browsers
            .iter()
            .find(|report| report.target.as_str() == catalog_target.id)
            .expect("installed target is listed");
        assert_eq!(
            report.status,
            BrowserBridgeStatus::Connected,
            "{}",
            catalog_target.id
        );
        connected_count += 1;
    }
    assert_eq!(connected_count, TARGETS.len() - 3);
}

#[test]
fn reconnect_keeps_the_same_files_without_duplicates() {
    let fixture = Fixture::new();
    let (installer, _flatpak, reloader) = connected(&fixture, "firefox");
    let before = fixture.snapshot_home();

    let outcome = installer
        .connect(&target_id("firefox"))
        .expect("second connect");

    assert_eq!(fixture.snapshot_home(), before, "same paths and same bytes");
    assert_eq!(outcome.status, BrowserBridgeStatus::Connected);
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    assert_eq!(reloader.calls(), 2);
}

#[test]
fn firefox_writes_both_the_main_and_the_xdg_manifest() {
    let fixture = Fixture::new();
    connected(&fixture, "firefox");
    for relative in [
        ".mozilla/native-messaging-hosts",
        ".config/mozilla/native-messaging-hosts",
    ] {
        assert!(
            fixture
                .home
                .join(relative)
                .join(format!("{NATIVE_HOST_NAME}.json"))
                .is_file(),
            "{relative}"
        );
    }
}

#[test]
fn connect_refuses_a_browser_that_is_not_installed_and_writes_nothing() {
    let fixture = Fixture::new();
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());

    let error = installer
        .connect(&target_id("chromium"))
        .expect_err("not installed");

    assert_eq!(error.step, InstallStep::CheckTarget);
    assert_eq!(error.kind, InstallErrorKind::TargetNotInstalled);
    assert!(fixture.home_file_paths().is_empty());
}

#[test]
fn unknown_target_is_a_typed_error() {
    let fixture = Fixture::new();
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());
    let error = installer
        .connect(&target_id("netscape"))
        .expect_err("unknown");
    assert_eq!(error.kind, InstallErrorKind::UnknownTarget);
}

#[test]
fn snap_browser_is_reported_unsupported_and_connect_writes_nothing() {
    let fixture = Fixture::new();
    // Обёртка snap лежит в каталоге snapd; в PATH на неё ведёт symlink.
    write_executable(&fixture.snap_dir.join("chromium"), b"#!snap-wrapper");
    std::os::unix::fs::symlink(
        fixture.snap_dir.join("chromium"),
        fixture.bin_dir.join("chromium"),
    )
    .expect("symlink");
    let reloader = RecordingReloader::default();
    let installer = fixture.installer(&FakeFlatpak::default(), &reloader);

    let survey = installer.survey().expect("survey");
    let status = survey
        .browsers
        .iter()
        .find(|report| report.target.as_str() == "chromium")
        .map(|report| &report.status);
    assert_eq!(status, Some(&BrowserBridgeStatus::SnapUnsupported));

    let error = installer.connect(&target_id("chromium")).expect_err("snap");
    assert_eq!(
        error.kind,
        InstallErrorKind::Unsupported(UnsupportedReason::Snap)
    );
    assert!(
        fixture.home_file_paths().is_empty(),
        "nothing may be written"
    );
    assert_eq!(reloader.calls(), 0);
}

#[test]
fn browser_found_only_in_the_snap_directory_is_snap() {
    let fixture = Fixture::new();
    write_executable(&fixture.snap_dir.join("firefox"), b"#!snap-wrapper");
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());
    let survey = installer.survey().expect("survey");
    assert_eq!(survey.browsers.len(), 1);
    assert_eq!(
        survey.browsers[0].status,
        BrowserBridgeStatus::SnapUnsupported
    );
}

#[test]
fn flatpak_firefox_is_unsupported_and_connect_writes_nothing() {
    let fixture = Fixture::new();
    let flatpak = FakeFlatpak::default().with_app("org.mozilla.firefox", "");
    let installer = fixture.installer(&flatpak, &RecordingReloader::default());

    let survey = installer.survey().expect("survey");
    assert_eq!(survey.browsers.len(), 1);
    assert_eq!(
        survey.browsers[0].status,
        BrowserBridgeStatus::FlatpakUnsupported
    );

    let error = installer
        .connect(&target_id("firefox-flatpak"))
        .expect_err("unsupported");
    assert_eq!(
        error.kind,
        InstallErrorKind::Unsupported(UnsupportedReason::FlatpakFirefoxFamily)
    );
    assert!(fixture.home_file_paths().is_empty());
}

#[test]
fn foreign_valid_manifest_is_reported_and_overwritten_with_a_warning() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let manifest = first_manifest_path(&fixture, "chromium");
    write_with_parents(
        &manifest,
        &format!("{{\"name\":\"{NATIVE_HOST_NAME}\",\"path\":\"/opt/other/host\"}}"),
    );
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());
    assert_eq!(
        installer.survey().expect("survey").browsers[0].status,
        BrowserBridgeStatus::ForeignFile
    );

    let outcome = installer.connect(&target_id("chromium")).expect("connect");

    assert_eq!(
        outcome.warnings,
        vec![InstallWarning::OverwroteForeignFile { path: manifest }]
    );
    assert_eq!(outcome.status, BrowserBridgeStatus::Connected);
}

#[test]
fn damaged_manifest_is_reported_and_replaced_with_a_warning() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let manifest = first_manifest_path(&fixture, "chromium");
    write_with_parents(&manifest, "{\"name\": \"io.github.bogdan7c.fa");
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());
    assert_eq!(
        installer.survey().expect("survey").browsers[0].status,
        BrowserBridgeStatus::ForeignFile
    );

    let outcome = installer.connect(&target_id("chromium")).expect("connect");

    assert_eq!(
        outcome.warnings,
        vec![InstallWarning::ReplacedDamagedFile {
            path: manifest.clone()
        }]
    );
    serde_json::from_str::<Value>(&std::fs::read_to_string(&manifest).expect("read"))
        .expect("valid JSON now");
}

#[test]
fn failed_bus_reload_is_a_warning_and_the_connection_still_completes() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let reloader = RecordingReloader::failing_with(SessionBusReloadError::NotResponding);
    let installer = fixture.installer(&FakeFlatpak::default(), &reloader);

    let outcome = installer.connect(&target_id("chromium")).expect("connect");

    assert_eq!(
        outcome.warnings,
        vec![InstallWarning::BusReloadFailed(
            SessionBusReloadError::NotResponding
        )]
    );
    assert_eq!(outcome.status, BrowserBridgeStatus::Connected);
}

#[test]
fn connect_takes_over_the_service_from_another_copy_with_a_warning() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let service_path = crate::layout::service_file_path(&fixture.environment);
    let other_player = std::path::PathBuf::from("/opt/installed/fastiplayer");
    write_with_parents(
        &service_path,
        &browser_session_activation_service_file(&other_player).expect("service"),
    );
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());

    let outcome = installer.connect(&target_id("chromium")).expect("connect");

    assert_eq!(
        outcome.warnings,
        vec![InstallWarning::TookOverFromAnotherCopy {
            previous_player: other_player
        }]
    );
    assert_eq!(
        std::fs::read_to_string(&service_path).expect("service"),
        browser_session_activation_service_file(&fixture.player).expect("expected")
    );
}

#[test]
fn flatpak_chrome_without_bus_access_is_connected_but_needs_permission() {
    let fixture = Fixture::new();
    let flatpak = FakeFlatpak::default()
        .with_app("com.google.Chrome", super::support::PERMISSIONS_WITHOUT_BUS);
    let installer = fixture.installer(&flatpak, &RecordingReloader::default());

    let outcome = installer
        .connect(&target_id("google-chrome-flatpak"))
        .expect("connect");

    assert_eq!(outcome.status, BrowserBridgeStatus::NeedsFlatpakPermission);
    assert!(first_manifest_path(&fixture, "google-chrome-flatpak").is_file());
}

#[test]
fn connect_never_leaves_temporary_files_next_to_the_results() {
    let fixture = Fixture::new();
    connected(&fixture, "chromium");
    let leftovers: Vec<_> = fixture
        .home_file_paths()
        .into_iter()
        .filter(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            !(name == "fastiplayer-browser-host"
                || name.ends_with(".json")
                || name.ends_with(".service"))
        })
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert!(target("chromium").manifest_directories(&fixture.environment)[0].is_dir());
}
