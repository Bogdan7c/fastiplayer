//! Неудобная реальность: странные имена, нет профиля, нельзя писать, битые ссылки.

use serde_json::Value;

use super::support::{
    FakeFlatpak, Fixture, RecordingReloader, first_manifest_path, set_mode, target_id,
};
use crate::source::HOST_BINARY_NAME;
use crate::{
    BrowserBridgeInstaller, BrowserBridgeStatus, InstallErrorKind, InstallSource, InstallStep,
    SourceProblem, UnavailableReason, layout,
};
use desktop_integration::ActivationServiceFileError;

#[test]
fn home_with_spaces_cyrillic_quotes_and_apostrophe_gives_valid_json_with_the_exact_path() {
    let fixture = Fixture::with_home_name("Дом \"кавычки\" и it's здесь");
    fixture.install_browser_executable("chromium");
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());

    let outcome = installer.connect(&target_id("chromium")).expect("connect");

    let manifest_text =
        std::fs::read_to_string(first_manifest_path(&fixture, "chromium")).expect("manifest");
    let parsed: Value =
        serde_json::from_str(&manifest_text).expect("manifest must stay valid JSON");
    let expected_host = fixture
        .home
        .join(".local/share/fastiplayer/browser-host")
        .join(HOST_BINARY_NAME);
    assert_eq!(parsed["path"].as_str(), expected_host.to_str());
    assert!(parsed["path"].as_str().expect("path").contains("it's"));
    assert!(expected_host.is_file());
    assert_eq!(outcome.status, BrowserBridgeStatus::Connected);
}

#[test]
fn absent_profile_directory_is_created_by_connect() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    assert!(!fixture.home.join(".config").exists());
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());

    installer.connect(&target_id("chromium")).expect("connect");

    assert!(
        fixture
            .home
            .join(".config/chromium/NativeMessagingHosts")
            .is_dir()
    );
}

/// Может ли текущий пользователь писать в каталог с правами 0555 (root может).
fn directory_is_writable_despite_mode(directory: &std::path::Path) -> bool {
    let probe = directory.join("probe");
    let writable = std::fs::write(&probe, b"x").is_ok();
    let _ = std::fs::remove_file(probe);
    writable
}

#[test]
fn read_only_config_directory_gives_a_typed_error_and_leaves_nothing_behind() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let config = fixture.home.join(".config");
    std::fs::create_dir_all(&config).expect("config");
    set_mode(&config, 0o555);
    if directory_is_writable_despite_mode(&config) {
        set_mode(&config, 0o755);
        return; // root игнорирует права: проверять нечего
    }
    let reloader = RecordingReloader::default();
    let installer = fixture.installer(&FakeFlatpak::default(), &reloader);

    let result = installer.connect(&target_id("chromium"));

    set_mode(&config, 0o755); // чтобы временный каталог смог удалиться
    let error = result.expect_err("cannot write into a read-only config");
    assert_eq!(error.step, InstallStep::PrepareDirectories);
    assert!(
        matches!(
            error.kind,
            InstallErrorKind::Io {
                kind: std::io::ErrorKind::PermissionDenied,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(
        fixture.home_file_paths().is_empty(),
        "no helper or service left behind"
    );
    assert_eq!(reloader.calls(), 0);
}

#[test]
fn dangling_symlink_as_config_directory_is_unavailable_and_connect_fails_without_panic() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    std::fs::create_dir_all(fixture.home.join(".config")).expect("config");
    let dangling = fixture.home.join(".config/chromium");
    std::os::unix::fs::symlink(fixture.root.path().join("does-not-exist"), &dangling)
        .expect("symlink");
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());

    let survey = installer.survey().expect("survey");
    assert!(
        matches!(
            &survey.browsers[0].status,
            BrowserBridgeStatus::Unavailable(UnavailableReason::Filesystem { path, .. }) if *path == dangling
        ),
        "{:?}",
        survey.browsers[0].status
    );

    let error = installer
        .connect(&target_id("chromium"))
        .expect_err("dangling link");
    assert_eq!(error.step, InstallStep::PrepareDirectories);
    assert!(
        matches!(error.kind, InstallErrorKind::Io { .. }),
        "{error:?}"
    );
    // Кроме самой висячей ссылки, на диске не появилось ничего.
    assert_eq!(
        fixture.home_file_paths(),
        std::collections::BTreeSet::from([dangling])
    );
}

#[test]
fn file_instead_of_config_directory_is_unavailable() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    std::fs::write(fixture.home.join(".config"), b"not a directory").expect("file");
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());
    let status = installer
        .survey()
        .expect("survey")
        .browsers
        .remove(0)
        .status;
    assert!(
        matches!(
            status,
            BrowserBridgeStatus::Unavailable(UnavailableReason::Filesystem {
                kind: std::io::ErrorKind::NotADirectory,
                ..
            })
        ),
        "{status:?}"
    );
}

#[test]
fn missing_host_binary_next_to_the_player_is_a_source_problem() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.app_dir.join(HOST_BINARY_NAME)).expect("remove");
    assert_eq!(
        fixture.source(),
        Err(SourceProblem::HostBinaryMissing {
            expected: fixture.app_dir.join(HOST_BINARY_NAME),
            player: fixture.player.clone(),
        })
    );
}

#[test]
fn player_path_with_an_apostrophe_is_rejected_as_unsupported() {
    let fixture = Fixture::new();
    let apostrophe_dir = fixture.root.path().join("o'neil");
    std::fs::create_dir_all(&apostrophe_dir).expect("dir");
    let player = apostrophe_dir.join("fastiplayer");
    super::support::write_executable(&player, b"#!player");
    super::support::write_executable(&apostrophe_dir.join(HOST_BINARY_NAME), b"#!host");

    let source = InstallSource::from_player_executable(player.clone());

    assert_eq!(
        source,
        Err(SourceProblem::PlayerPathUnsupported {
            player,
            reason: ActivationServiceFileError::UnsupportedCharacter { character: '\'' }
        })
    );
    fixture.install_browser_executable("chromium");
    let installer = BrowserBridgeInstaller::new(
        fixture.environment.clone(),
        source,
        Box::new(FakeFlatpak::default()),
        Box::new(RecordingReloader::default()),
    );
    let error = installer
        .connect(&target_id("chromium"))
        .expect_err("unsupported player path");
    assert!(matches!(
        error.kind,
        InstallErrorKind::Source(SourceProblem::PlayerPathUnsupported { .. })
    ));
    assert!(fixture.home_file_paths().is_empty());
}

#[test]
fn connect_replaces_a_stale_host_binary_atomically_with_the_new_bytes() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let host = layout::host_install_path(&fixture.environment, super::support::target("chromium"));
    super::support::write_with_parents(&host, "old binary from another version");
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());

    installer.connect(&target_id("chromium")).expect("connect");

    assert_eq!(
        std::fs::read(&host).expect("host"),
        super::support::HOST_BYTES
    );
}
