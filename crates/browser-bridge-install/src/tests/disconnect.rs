//! Отключение: удаляется только своё и только то, что больше никому не нужно.

use browser_bridge_protocol::extension_identity::NATIVE_HOST_NAME;
use desktop_integration::browser_session_activation_service_file;

use super::support::{
    FakeFlatpak, Fixture, RecordingReloader, connected, first_manifest_path, target, target_id,
    write_with_parents,
};
use crate::{BrowserBridgeStatus, DisconnectWarning, layout};

#[test]
fn disconnect_removes_our_files_and_keeps_unrelated_ones() {
    let fixture = Fixture::new();
    let (installer, _flatpak, reloader) = connected(&fixture, "chromium");
    let manifest = first_manifest_path(&fixture, "chromium");
    let unrelated = manifest.with_file_name("com.example.other_host.json");
    write_with_parents(&unrelated, "{\"name\":\"com.example.other_host\"}");
    let host = layout::host_install_path(&fixture.environment, target("chromium"));
    let service = layout::service_file_path(&fixture.environment);

    let outcome = installer
        .disconnect(&target_id("chromium"))
        .expect("disconnect");

    assert!(!manifest.exists() && !host.exists() && !service.exists());
    assert!(unrelated.is_file(), "files that are not ours stay");
    let mut removed = outcome.removed_files.clone();
    removed.sort();
    let mut expected = vec![manifest, host, service];
    expected.sort();
    assert_eq!(removed, expected);
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    assert_eq!(outcome.status, BrowserBridgeStatus::NotConnected);
    assert_eq!(
        reloader.calls(),
        2,
        "once on connect, once after removing the service"
    );
}

#[test]
fn shared_host_and_service_stay_while_another_system_target_is_connected() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    fixture.install_browser_executable("google-chrome-stable");
    let flatpak = FakeFlatpak::default();
    let installer = fixture.installer(&flatpak, &RecordingReloader::default());
    installer
        .connect(&target_id("chromium"))
        .expect("connect chromium");
    installer
        .connect(&target_id("google-chrome"))
        .expect("connect chrome");
    let host = layout::host_install_path(&fixture.environment, target("chromium"));
    let service = layout::service_file_path(&fixture.environment);

    installer
        .disconnect(&target_id("chromium"))
        .expect("disconnect chromium");
    assert!(host.is_file(), "shared host is still used by Chrome");
    assert!(service.is_file(), "service is still needed by Chrome");
    assert!(first_manifest_path(&fixture, "google-chrome").is_file());

    installer
        .disconnect(&target_id("google-chrome"))
        .expect("disconnect chrome");
    assert!(
        !host.exists() && !service.exists(),
        "last target removes the shared files"
    );
}

#[test]
fn service_waking_another_copy_is_kept_and_reported() {
    let fixture = Fixture::new();
    let (installer, ..) = connected(&fixture, "chromium");
    let service = layout::service_file_path(&fixture.environment);
    let other_player = std::path::PathBuf::from("/opt/installed/fastiplayer");
    let other_service = browser_session_activation_service_file(&other_player).expect("service");
    std::fs::write(&service, &other_service).expect("rewrite service");

    let outcome = installer
        .disconnect(&target_id("chromium"))
        .expect("disconnect");

    assert_eq!(
        std::fs::read_to_string(&service).expect("kept"),
        other_service
    );
    assert_eq!(
        outcome.warnings,
        vec![DisconnectWarning::ServiceFileBelongsToAnotherCopy {
            player: other_player
        }]
    );
}

#[test]
fn foreign_manifest_with_our_name_is_not_deleted() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let manifest = first_manifest_path(&fixture, "chromium");
    let foreign = format!("{{\"name\":\"{NATIVE_HOST_NAME}\",\"path\":\"/opt/other/host\"}}");
    write_with_parents(&manifest, &foreign);
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());

    let outcome = installer
        .disconnect(&target_id("chromium"))
        .expect("disconnect");

    assert_eq!(std::fs::read_to_string(&manifest).expect("kept"), foreign);
    assert_eq!(
        outcome.warnings,
        vec![DisconnectWarning::NotOurFileLeftInPlace { path: manifest }]
    );
    assert!(outcome.removed_files.is_empty());
    assert_eq!(outcome.status, BrowserBridgeStatus::ForeignFile);
}

#[test]
fn damaged_manifest_is_not_deleted_either() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let manifest = first_manifest_path(&fixture, "chromium");
    write_with_parents(&manifest, "{ broken");
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());
    let outcome = installer
        .disconnect(&target_id("chromium"))
        .expect("disconnect");
    assert_eq!(
        std::fs::read_to_string(&manifest).expect("kept"),
        "{ broken"
    );
    assert_eq!(
        outcome.warnings,
        vec![DisconnectWarning::NotOurFileLeftInPlace { path: manifest }]
    );
}

#[test]
fn disconnect_of_a_never_connected_browser_is_a_quiet_no_op() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("chromium");
    let reloader = RecordingReloader::default();
    let installer = fixture.installer(&FakeFlatpak::default(), &reloader);
    let outcome = installer
        .disconnect(&target_id("chromium"))
        .expect("disconnect");
    assert!(outcome.removed_files.is_empty() && outcome.warnings.is_empty());
    assert_eq!(outcome.status, BrowserBridgeStatus::NotConnected);
    assert_eq!(reloader.calls(), 0);
    assert!(fixture.home_file_paths().is_empty());
}

#[test]
fn flatpak_target_removes_its_own_host_copy_inside_its_app_directory() {
    let fixture = Fixture::new();
    let (installer, ..) = connected(&fixture, "google-chrome-flatpak");
    let host = layout::host_install_path(&fixture.environment, target("google-chrome-flatpak"));
    assert!(host.starts_with(fixture.home.join(".var/app/com.google.Chrome")));

    installer
        .disconnect(&target_id("google-chrome-flatpak"))
        .expect("disconnect");

    assert!(!host.exists());
    assert!(!first_manifest_path(&fixture, "google-chrome-flatpak").exists());
}

#[test]
fn disconnect_without_host_next_to_the_player_still_removes_our_files() {
    let fixture = Fixture::new();
    let (_installer, ..) = connected(&fixture, "chromium");
    let manifest = first_manifest_path(&fixture, "chromium");
    let host = layout::host_install_path(&fixture.environment, target("chromium"));
    let service = layout::service_file_path(&fixture.environment);
    std::fs::remove_file(fixture.app_dir.join(crate::source::HOST_BINARY_NAME))
        .expect("remove source host");
    let installer = fixture.installer(&FakeFlatpak::default(), &RecordingReloader::default());

    let outcome = installer
        .disconnect(&target_id("chromium"))
        .expect("disconnect");

    assert!(!manifest.exists() && !host.exists() && !service.exists());
    let mut removed = outcome.removed_files.clone();
    removed.sort();
    let mut expected = vec![manifest, host, service];
    expected.sort();
    assert_eq!(removed, expected);
    assert_eq!(outcome.status, BrowserBridgeStatus::NotConnected);
}

#[test]
fn disconnect_with_an_apostrophe_player_path_removes_ours_and_keeps_another_players_service() {
    let fixture = Fixture::new();
    let (_installer, ..) = connected(&fixture, "chromium");
    let manifest = first_manifest_path(&fixture, "chromium");
    let host = layout::host_install_path(&fixture.environment, target("chromium"));
    let service = layout::service_file_path(&fixture.environment);
    // Другая копия плеера «живёт» в каталоге с апострофом и сама Exec-ом не записывается.
    let apostrophe_dir = fixture.root.path().join("o'neil");
    std::fs::create_dir_all(&apostrophe_dir).expect("dir");
    let player = apostrophe_dir.join("fastiplayer");
    super::support::write_executable(&player, b"#!player");
    let source = crate::InstallSource::from_player_executable(player);
    assert!(source.is_err());
    let installer = crate::BrowserBridgeInstaller::new(
        fixture.environment.clone(),
        source,
        Box::new(FakeFlatpak::default()),
        Box::new(RecordingReloader::default()),
    );
    let other_service = std::fs::read_to_string(&service).expect("service of the first player");

    let outcome = installer
        .disconnect(&target_id("chromium"))
        .expect("disconnect");

    assert!(!manifest.exists() && !host.exists());
    assert_eq!(
        std::fs::read_to_string(&service).expect("kept"),
        other_service
    );
    assert!(matches!(
        outcome.warnings.as_slice(),
        [DisconnectWarning::ServiceFileBelongsToAnotherCopy { .. }]
    ));
}

#[test]
fn disconnect_removes_our_empty_directories_but_never_browser_ones() {
    let fixture = Fixture::new();
    let (installer, ..) = connected(&fixture, "chromium");
    let host_dir = fixture.home.join(".local/share/fastiplayer/browser-host");
    let app_dir = fixture.home.join(".local/share/fastiplayer");

    let outcome = installer
        .disconnect(&target_id("chromium"))
        .expect("disconnect");

    assert!(!host_dir.exists() && !app_dir.exists());
    assert_eq!(outcome.removed_directories, vec![host_dir, app_dir]);
    assert!(
        fixture
            .home
            .join(".config/chromium/NativeMessagingHosts")
            .is_dir()
    );
    assert!(fixture.home.join(".local/share/dbus-1/services").is_dir());
}

#[test]
fn foreign_file_in_our_directory_keeps_it() {
    let fixture = Fixture::new();
    let (installer, ..) = connected(&fixture, "chromium");
    let app_dir = fixture.home.join(".local/share/fastiplayer");
    let foreign = app_dir.join("settings.keep");
    write_with_parents(&foreign, "mine");

    let outcome = installer
        .disconnect(&target_id("chromium"))
        .expect("disconnect");

    assert!(foreign.is_file() && app_dir.is_dir());
    assert!(!app_dir.join("browser-host").exists());
    assert_eq!(
        outcome.removed_directories,
        vec![app_dir.join("browser-host")]
    );
}

#[test]
fn flatpak_disconnect_removes_its_empty_app_directories_but_not_the_app_config() {
    let fixture = Fixture::new();
    let (installer, ..) = connected(&fixture, "google-chrome-flatpak");
    let data = fixture.home.join(".var/app/com.google.Chrome/data");

    installer
        .disconnect(&target_id("google-chrome-flatpak"))
        .expect("disconnect");

    assert!(!data.join("fastiplayer").exists());
    assert!(data.is_dir(), "the app's own data directory is not ours");
    assert!(
        fixture
            .home
            .join(".var/app/com.google.Chrome/config/google-chrome/NativeMessagingHosts")
            .is_dir()
    );
}

#[test]
fn symlinked_application_directory_does_not_break_disconnect() {
    let fixture = Fixture::new();
    // Каталог данных приложения — симлинк (stow, dotfiles).
    let real_dir = fixture.root.path().join("dotfiles/fastiplayer");
    std::fs::create_dir_all(&real_dir).expect("real dir");
    let data = fixture.home.join(".local/share");
    std::fs::create_dir_all(&data).expect("data");
    std::os::unix::fs::symlink(&real_dir, data.join("fastiplayer")).expect("symlink");
    let (installer, ..) = connected(&fixture, "chromium");
    let service = layout::service_file_path(&fixture.environment);
    assert!(service.is_file());

    let outcome = installer
        .disconnect(&target_id("chromium"))
        .expect("disconnect must succeed");

    assert!(!service.exists(), "service removal must still happen");
    assert!(
        std::fs::symlink_metadata(data.join("fastiplayer"))
            .expect("link")
            .is_symlink()
    );
    assert!(real_dir.is_dir(), "the link target is intact");
    assert!(
        !outcome
            .removed_directories
            .contains(&data.join("fastiplayer"))
    );
    assert_eq!(outcome.status, BrowserBridgeStatus::NotConnected);
}

#[test]
fn unknown_player_path_keeps_the_service_with_an_owner_unknown_warning() {
    let fixture = Fixture::new();
    let (_installer, ..) = connected(&fixture, "chromium");
    let service = layout::service_file_path(&fixture.environment);
    let kept = std::fs::read_to_string(&service).expect("service");
    // Путь плеера неизвестен: источник сообщает `PlayerExecutableUnknown`.
    let installer = crate::BrowserBridgeInstaller::new(
        fixture.environment.clone(),
        Err(crate::SourceProblem::PlayerExecutableUnknown {
            kind: std::io::ErrorKind::NotFound,
        }),
        Box::new(FakeFlatpak::default()),
        Box::new(RecordingReloader::default()),
    );

    let outcome = installer
        .disconnect(&target_id("chromium"))
        .expect("disconnect");

    assert_eq!(std::fs::read_to_string(&service).expect("kept"), kept);
    assert_eq!(
        outcome.warnings,
        vec![DisconnectWarning::ServiceFileOwnerUnknown {
            exec_player: fixture.player.clone()
        }]
    );
}
