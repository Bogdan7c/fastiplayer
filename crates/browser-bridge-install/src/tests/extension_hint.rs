//! Подсказка `External Extensions/<ID>.json`: запись, повтор, удаление только своего.

use serde_json::{Value, json};

use std::time::{Duration, Instant};

use super::extension_support::{
    RecordingLauncher, STORE_ID, UPDATE_URL, expected_hint_path, make_fifo, published_listings,
};
use super::support::{FakeFlatpak, Fixture, target, target_id, write_with_parents};
use crate::extension::hint::{HintState, hint_path, inspect_hint, write_hint};
use crate::{
    DirectoryCleanup, ExtensionInstallError, ExtensionInstallOutcome, ExtensionState,
    ExtensionSupport, InstallAction, RemoveHintOutcome, TARGETS,
};

/// Независимая таблица: где у каждой цели «тихого» пути лежат профили.
const EXTERNAL_HINT_TARGETS: [(&str, &str); 8] = [
    ("chromium", ".config/chromium"),
    ("brave", ".config/BraveSoftware/Brave-Browser"),
    ("vivaldi", ".config/vivaldi"),
    ("microsoft-edge", ".config/microsoft-edge"),
    (
        "chromium-flatpak",
        ".var/app/org.chromium.Chromium/config/chromium",
    ),
    (
        "brave-flatpak",
        ".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser",
    ),
    (
        "vivaldi-flatpak",
        ".var/app/com.vivaldi.Vivaldi/config/vivaldi",
    ),
    (
        "microsoft-edge-flatpak",
        ".var/app/com.microsoft.Edge/config/microsoft-edge",
    ),
];

/// Установщик с установленным браузером цели.
fn installer_with_browser(
    fixture: &Fixture,
    id: &str,
) -> (crate::ExtensionInstaller, RecordingLauncher) {
    let flatpak = fixture.install_target(target(id), FakeFlatpak::default());
    let launcher = RecordingLauncher::default();
    (
        fixture.extension_installer(published_listings(), &flatpak, &launcher),
        launcher,
    )
}

fn hint_json(path: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).expect("hint file")).expect("hint json")
}

#[test]
fn hint_is_written_at_the_exact_path_with_exactly_the_update_url_for_every_external_hint_target() {
    for (id, user_data) in EXTERNAL_HINT_TARGETS {
        let fixture = Fixture::new();
        let (installer, launcher) = installer_with_browser(&fixture, id);
        let outcome = installer.install(&target_id(id)).expect(id);
        let expected_path = expected_hint_path(&fixture, user_data);
        assert_eq!(
            outcome,
            ExtensionInstallOutcome::HintWritten {
                hint_path: expected_path.clone()
            },
            "{id}"
        );
        assert_eq!(
            hint_json(&expected_path),
            json!({"external_update_url": UPDATE_URL}),
            "{id}"
        );
        assert!(
            launcher.calls().is_empty(),
            "{id}: silent route must not launch the browser"
        );
    }
}

#[test]
fn the_independent_table_covers_every_external_hint_target_in_the_catalog() {
    let catalog_ids: Vec<&str> = TARGETS
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.extension,
                ExtensionSupport::Chromium {
                    delivery: crate::ChromiumDelivery::ExternalHint,
                    ..
                }
            )
        })
        .map(|candidate| candidate.id)
        .collect();
    let table_ids: Vec<&str> = EXTERNAL_HINT_TARGETS.iter().map(|(id, _)| *id).collect();
    assert_eq!(catalog_ids, table_ids);
}

#[test]
fn the_external_extensions_folder_holds_exactly_the_hint_file_after_install() {
    let fixture = Fixture::new();
    let (installer, _launcher) = installer_with_browser(&fixture, "chromium");
    installer.install(&target_id("chromium")).expect("install");
    let directory = fixture.home.join(".config/chromium/External Extensions");
    let names: Vec<String> = std::fs::read_dir(&directory)
        .expect("directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names, vec![format!("{STORE_ID}.json")], "no temp leftovers");
}

#[test]
fn repeat_install_while_waiting_for_restart_opens_the_store_page_and_keeps_the_hint() {
    let fixture = Fixture::new();
    let (installer, launcher) = installer_with_browser(&fixture, "chromium");
    installer.install(&target_id("chromium")).expect("first");
    let after_first = fixture.snapshot_home();
    let second = installer.install(&target_id("chromium")).expect("second");
    assert_eq!(
        second,
        ExtensionInstallOutcome::StorePageOpened {
            url: format!("https://chromewebstore.google.com/detail/{STORE_ID}")
        }
    );
    assert_eq!(fixture.snapshot_home(), after_first, "hint is unchanged");
    assert_eq!(after_first.len(), 1);
    assert_eq!(launcher.calls().len(), 1);
    let report = installer.survey().expect("survey").browsers.remove(0);
    assert_eq!(report.state, ExtensionState::WaitingForBrowserRestart);
    assert_eq!(report.install_action, Some(InstallAction::OpenStorePage));
}

#[test]
fn a_foreign_hint_is_never_overwritten_install_falls_back_to_the_store_page() {
    let fixture = Fixture::new();
    let (installer, launcher) = installer_with_browser(&fixture, "chromium");
    let path = expected_hint_path(&fixture, ".config/chromium");
    let foreign = br#"{"external_crx": "/x.crx", "external_version": "1"}"#;
    write_with_parents(&path, std::str::from_utf8(foreign).expect("utf8"));
    let report = installer.survey().expect("survey").browsers.remove(0);
    assert_eq!(report.hint, HintState::Foreign);
    assert_eq!(report.install_action, Some(InstallAction::OpenStorePage));
    let outcome = installer.install(&target_id("chromium")).expect("install");
    assert_eq!(
        outcome,
        ExtensionInstallOutcome::StorePageOpened {
            url: format!("https://chromewebstore.google.com/detail/{STORE_ID}")
        }
    );
    assert_eq!(std::fs::read(&path).expect("kept"), foreign);
    assert_eq!(launcher.calls().len(), 1);
}

#[test]
fn write_hint_itself_refuses_to_overwrite_a_foreign_file() {
    let directory = tempfile::tempdir().expect("dir");
    let path = hint_path(directory.path(), STORE_ID);
    write_with_parents(&path, "{\"external_crx\": \"/x.crx\"}");
    assert_eq!(
        write_hint(directory.path(), STORE_ID),
        Err(ExtensionInstallError::ForeignHintInTheWay { path: path.clone() })
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("kept"),
        "{\"external_crx\": \"/x.crx\"}"
    );
}

#[test]
fn a_fifo_in_place_of_the_hint_is_foreign_never_opened_and_never_touched() {
    let fixture = Fixture::new();
    let (installer, _launcher) = installer_with_browser(&fixture, "chromium");
    let path = expected_hint_path(&fixture, ".config/chromium");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    make_fifo(&path);
    let started = Instant::now();
    assert_eq!(inspect_hint(&path), Ok(HintState::Foreign));
    let report = installer.survey().expect("survey").browsers.remove(0);
    assert_eq!(report.hint, HintState::Foreign);
    assert_eq!(
        installer.cancel_pending_install(&target_id("chromium")),
        Ok(RemoveHintOutcome::ForeignHintKept)
    );
    assert!(matches!(
        installer.install(&target_id("chromium")),
        Ok(ExtensionInstallOutcome::StorePageOpened { .. })
    ));
    assert!(started.elapsed() < Duration::from_secs(5), "must not block");
    let file_type = std::fs::symlink_metadata(&path)
        .expect("still there")
        .file_type();
    assert!(std::os::unix::fs::FileTypeExt::is_fifo(&file_type));
}

#[test]
fn cancel_deletes_our_hint_and_the_empty_directory_but_keeps_the_profile_folder() {
    let fixture = Fixture::new();
    let (installer, _launcher) = installer_with_browser(&fixture, "chromium");
    installer.install(&target_id("chromium")).expect("install");
    let outcome = installer
        .cancel_pending_install(&target_id("chromium"))
        .expect("remove");
    assert_eq!(
        outcome,
        RemoveHintOutcome::Removed {
            directory: DirectoryCleanup::Removed
        }
    );
    let user_data = fixture.home.join(".config/chromium");
    assert!(user_data.is_dir());
    assert!(!user_data.join("External Extensions").exists());
}

#[test]
fn cancel_leaves_a_foreign_hint_byte_identical_and_reports_it() {
    let fixture = Fixture::new();
    let (installer, _launcher) = installer_with_browser(&fixture, "chromium");
    let path = expected_hint_path(&fixture, ".config/chromium");
    let foreign = br#"{ "external_update_url" : "https://example.org/updates", "x": 1 }"#;
    write_with_parents(&path, std::str::from_utf8(foreign).expect("utf8"));
    let outcome = installer
        .cancel_pending_install(&target_id("chromium"))
        .expect("remove");
    assert_eq!(outcome, RemoveHintOutcome::ForeignHintKept);
    assert_eq!(std::fs::read(&path).expect("kept"), foreign);
}

#[test]
fn cancel_keeps_a_directory_that_holds_other_extensions_hints() {
    let fixture = Fixture::new();
    let (installer, _launcher) = installer_with_browser(&fixture, "chromium");
    installer.install(&target_id("chromium")).expect("install");
    let neighbour = fixture
        .home
        .join(".config/chromium/External Extensions/someoneelse.json");
    write_with_parents(&neighbour, "{}");
    let outcome = installer
        .cancel_pending_install(&target_id("chromium"))
        .expect("remove");
    assert_eq!(
        outcome,
        RemoveHintOutcome::Removed {
            directory: DirectoryCleanup::Kept
        }
    );
    assert!(neighbour.is_file());
    assert!(!expected_hint_path(&fixture, ".config/chromium").exists());
}

#[test]
fn cancel_never_deletes_a_symlinked_directory() {
    let fixture = Fixture::new();
    let (installer, _launcher) = installer_with_browser(&fixture, "chromium");
    let real = fixture.root.path().join("real-external");
    std::fs::create_dir_all(&real).expect("real dir");
    let user_data = fixture.home.join(".config/chromium");
    std::fs::create_dir_all(&user_data).expect("user data");
    let link = user_data.join("External Extensions");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    installer.install(&target_id("chromium")).expect("install");
    assert!(real.join(format!("{STORE_ID}.json")).is_file());
    let outcome = installer
        .cancel_pending_install(&target_id("chromium"))
        .expect("remove");
    assert_eq!(
        outcome,
        RemoveHintOutcome::Removed {
            directory: DirectoryCleanup::Kept
        }
    );
    assert!(std::fs::symlink_metadata(&link).expect("link").is_symlink());
    assert!(real.is_dir());
    assert!(!real.join(format!("{STORE_ID}.json")).exists());
}

#[test]
fn cancel_without_a_hint_is_a_reported_no_op() {
    let fixture = Fixture::new();
    let (installer, _launcher) = installer_with_browser(&fixture, "chromium");
    assert_eq!(
        installer
            .cancel_pending_install(&target_id("chromium"))
            .expect("remove"),
        RemoveHintOutcome::NothingToRemove
    );
    assert!(fixture.snapshot_home().is_empty());
}

#[test]
fn cancel_for_browsers_without_hint_routes_and_before_publication_gives_distinct_errors() {
    let fixture = Fixture::new();
    let (installer, _launcher) = installer_with_browser(&fixture, "firefox");
    assert_eq!(
        installer.cancel_pending_install(&target_id("firefox")),
        Err(ExtensionInstallError::NoHintForThisBrowser)
    );
    assert_eq!(
        installer.cancel_pending_install(&target_id("google-chrome")),
        Err(ExtensionInstallError::NoHintForThisBrowser)
    );
    assert_eq!(
        installer.cancel_pending_install(&target_id("nope")),
        Err(ExtensionInstallError::UnknownTarget)
    );
    let unpublished = fixture.extension_installer(
        crate::ExtensionListings::unpublished(),
        &FakeFlatpak::default(),
        &RecordingLauncher::default(),
    );
    assert_eq!(
        unpublished.cancel_pending_install(&target_id("chromium")),
        Err(ExtensionInstallError::NotPublished)
    );
}

#[test]
fn hint_content_is_compared_by_meaning_not_by_bytes() {
    let directory = tempfile::tempdir().expect("dir");
    let path = directory.path().join("hint.json");
    let cases = [
        (
            format!("{{\"external_update_url\":\"{UPDATE_URL}\"}}"),
            HintState::Ours,
        ),
        (
            format!("\n  {{ \"external_update_url\" :   \"{UPDATE_URL}\" }}\n\n"),
            HintState::Ours,
        ),
        (
            format!("{{\"external_update_url\":\"{UPDATE_URL}\",\"extra\":true}}"),
            HintState::Foreign,
        ),
        (
            "{\"external_update_url\":\"https://other.example/\"}".to_owned(),
            HintState::Foreign,
        ),
        ("not json".to_owned(), HintState::Foreign),
        ("[]".to_owned(), HintState::Foreign),
    ];
    for (text, expected) in cases {
        std::fs::write(&path, &text).expect("write");
        assert_eq!(inspect_hint(&path), Ok(expected), "{text}");
    }
    std::fs::remove_file(&path).expect("remove");
    assert_eq!(inspect_hint(&path), Ok(HintState::Absent));
}

#[test]
fn an_unwritable_profile_folder_gives_a_typed_error_and_leaves_nothing() {
    let fixture = Fixture::new();
    let (installer, _launcher) = installer_with_browser(&fixture, "chromium");
    let user_data = fixture.home.join(".config/chromium");
    std::fs::create_dir_all(&user_data).expect("user data");
    super::support::set_mode(&user_data, 0o500);
    let writable_anyway = std::fs::create_dir(user_data.join("probe")).is_ok();
    let result = installer.install(&target_id("chromium"));
    super::support::set_mode(&user_data, 0o755);
    if writable_anyway {
        return; // root: права не действуют, проверять нечего
    }
    assert!(
        matches!(result, Err(ExtensionInstallError::HintIo { .. })),
        "{result:?}"
    );
    assert!(!user_data.join("External Extensions").exists());
}
