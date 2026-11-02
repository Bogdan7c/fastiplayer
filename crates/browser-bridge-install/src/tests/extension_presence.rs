//! Статус экстеншена из профилей браузера: только чтение, понятные причины сбоев.

use std::path::Path;

use serde_json::json;

use super::extension_support::{
    DEVELOPMENT_ID, RecordingLauncher, STORE_ID, expected_hint_path, installed_preferences,
    published_listings, removed_by_user_preferences, write_chromium_preferences,
    write_firefox_extensions,
};
use super::support::{FakeFlatpak, Fixture, set_mode, target, target_id, write_with_parents};
use crate::{
    ExtensionInstallError, ExtensionInstallOutcome, ExtensionListings, ExtensionReport,
    ExtensionState, InstallAction, UnreadableKind,
};

/// Отчёт одной цели из обзора.
fn report_of(
    fixture: &Fixture,
    listings: ExtensionListings,
    flatpak: &FakeFlatpak,
    id: &str,
) -> ExtensionReport {
    fixture
        .extension_installer(listings, flatpak, &RecordingLauncher::default())
        .survey()
        .expect("survey")
        .browsers
        .into_iter()
        .find(|report| report.target.as_str() == id)
        .unwrap_or_else(|| panic!("{id} is not in the survey"))
}

fn chromium_world() -> (Fixture, std::path::PathBuf, FakeFlatpak) {
    let fixture = Fixture::new();
    let flatpak = fixture.install_target(target("chromium"), FakeFlatpak::default());
    let user_data = fixture.home.join(".config/chromium");
    (fixture, user_data, flatpak)
}

#[test]
fn enabled_extension_in_a_profile_is_installed_and_needs_no_action() {
    let (fixture, user_data, flatpak) = chromium_world();
    write_chromium_preferences(
        &user_data,
        "Default",
        "Preferences",
        &installed_preferences(STORE_ID, &json!([])),
    );
    let report = report_of(&fixture, published_listings(), &flatpak, "chromium");
    assert_eq!(report.state, ExtensionState::Installed);
    assert_eq!(report.install_action, None);
}

#[test]
fn disable_reasons_as_array_or_bitmask_mean_installed_but_disabled() {
    for reasons in [json!([1]), json!(4)] {
        let (fixture, user_data, flatpak) = chromium_world();
        write_chromium_preferences(
            &user_data,
            "Default",
            "Preferences",
            &installed_preferences(STORE_ID, &reasons),
        );
        let report = report_of(&fixture, published_listings(), &flatpak, "chromium");
        assert_eq!(
            report.state,
            ExtensionState::InstalledButDisabled,
            "{reasons}"
        );
        assert_eq!(report.install_action, None);
    }
    // Число 0 — «причин нет», то есть включён.
    let (fixture, user_data, flatpak) = chromium_world();
    write_chromium_preferences(
        &user_data,
        "Default",
        "Preferences",
        &installed_preferences(STORE_ID, &json!(0)),
    );
    assert_eq!(
        report_of(&fixture, published_listings(), &flatpak, "chromium").state,
        ExtensionState::Installed
    );
}

#[test]
fn removal_by_the_user_closes_the_silent_route_and_install_opens_the_store_page() {
    let (fixture, user_data, flatpak) = chromium_world();
    write_chromium_preferences(
        &user_data,
        "Default",
        "Preferences",
        &removed_by_user_preferences(STORE_ID),
    );
    let launcher = RecordingLauncher::default();
    let installer = fixture.extension_installer(published_listings(), &flatpak, &launcher);
    let report = installer.survey().expect("survey").browsers.remove(0);
    assert_eq!(report.state, ExtensionState::RemovedInBrowser);
    assert_eq!(report.install_action, Some(InstallAction::OpenStorePage));
    let outcome = installer.install(&target_id("chromium")).expect("install");
    assert_eq!(
        outcome,
        ExtensionInstallOutcome::StorePageOpened {
            url: format!("https://chromewebstore.google.com/detail/{STORE_ID}")
        }
    );
    assert!(
        !expected_hint_path(&fixture, ".config/chromium").exists(),
        "no hint on a dead silent route"
    );
    assert_eq!(launcher.calls().len(), 1);
}

#[test]
fn an_empty_settings_entry_without_the_uninstall_record_is_not_installed() {
    let (fixture, user_data, flatpak) = chromium_world();
    let preferences = json!({"extensions": {"settings": {STORE_ID: {}}}});
    write_chromium_preferences(&user_data, "Default", "Preferences", &preferences);
    let report = report_of(&fixture, published_listings(), &flatpak, "chromium");
    assert_eq!(report.state, ExtensionState::NotInstalled);
    assert_eq!(report.install_action, Some(InstallAction::InstallSilently));
}

#[test]
fn the_development_id_counts_as_installed_even_before_publication() {
    let (fixture, user_data, flatpak) = chromium_world();
    write_chromium_preferences(
        &user_data,
        "Profile 2",
        "Secure Preferences",
        &installed_preferences(DEVELOPMENT_ID, &json!([])),
    );
    let report = report_of(
        &fixture,
        ExtensionListings::unpublished(),
        &flatpak,
        "chromium",
    );
    assert_eq!(report.state, ExtensionState::Installed);
}

#[test]
fn the_best_state_across_profiles_and_files_wins() {
    let (fixture, user_data, flatpak) = chromium_world();
    write_chromium_preferences(
        &user_data,
        "Default",
        "Preferences",
        &installed_preferences(STORE_ID, &json!([1])),
    );
    write_chromium_preferences(
        &user_data,
        "Profile 1",
        "Secure Preferences",
        &installed_preferences(STORE_ID, &json!([])),
    );
    // Не профиль: каталог с другим именем не читается.
    write_chromium_preferences(
        &user_data,
        "System Profile",
        "Preferences",
        &removed_by_user_preferences(STORE_ID),
    );
    let report = report_of(&fixture, published_listings(), &flatpak, "chromium");
    assert_eq!(report.state, ExtensionState::Installed);
}

#[test]
fn hint_without_the_extension_in_any_profile_waits_for_the_browser_restart() {
    let (fixture, user_data, flatpak) = chromium_world();
    write_chromium_preferences(
        &user_data,
        "Default",
        "Preferences",
        &json!({"extensions": {"settings": {}}}),
    );
    let installer = fixture.extension_installer(
        published_listings(),
        &flatpak,
        &RecordingLauncher::default(),
    );
    installer.install(&target_id("chromium")).expect("install");
    let report = installer.survey().expect("survey").browsers.remove(0);
    assert_eq!(report.state, ExtensionState::WaitingForBrowserRestart);
    assert_eq!(report.hint, crate::HintState::Ours);
}

#[test]
fn awaiting_publication_blocks_install_without_touching_disk_or_the_browser() {
    let (fixture, _user_data, flatpak) = chromium_world();
    let launcher = RecordingLauncher::default();
    let installer =
        fixture.extension_installer(ExtensionListings::unpublished(), &flatpak, &launcher);
    let report = installer.survey().expect("survey").browsers.remove(0);
    assert_eq!(report.state, ExtensionState::AwaitingPublication);
    assert_eq!(report.install_action, None);
    assert_eq!(
        installer.install(&target_id("chromium")),
        Err(ExtensionInstallError::NotPublished)
    );
    assert!(fixture.snapshot_home().is_empty());
    assert!(launcher.calls().is_empty());
}

/// `profiles.ini` с относительным профилем, абсолютным профилем и посторонними секциями.
fn firefox_profiles_ini(absolute_profile: &Path) -> String {
    format!(
        "[General]\nStartWithLastProfile=1\n\n[Install4F96D1932A9F858E]\nDefault=Profiles/abc.default\n\n\
         [Profile1]\nName=work\nIsRelative=0\nPath={}\n\n[Profile0]\nName=default\nIsRelative=1\nPath=Profiles/abc.default\n",
        absolute_profile.display()
    )
}

fn firefox_world() -> (Fixture, FakeFlatpak) {
    let fixture = Fixture::new();
    let flatpak = fixture.install_target(target("firefox"), FakeFlatpak::default());
    (fixture, flatpak)
}

#[test]
fn firefox_profiles_relative_and_absolute_and_both_roots_are_scanned() {
    let (fixture, flatpak) = firefox_world();
    let absolute_profile = fixture.root.path().join("elsewhere/work-profile");
    let mozilla = fixture.home.join(".mozilla/firefox");
    write_with_parents(
        &mozilla.join("profiles.ini"),
        &firefox_profiles_ini(&absolute_profile),
    );
    // В относительном профиле экстеншена нет, в абсолютном он включён.
    write_with_parents(
        &mozilla.join("Profiles/abc.default/extensions.json"),
        r#"{"addons": []}"#,
    );
    write_firefox_extensions(&absolute_profile, &json!({}));
    assert_eq!(
        report_of(&fixture, published_listings(), &flatpak, "firefox").state,
        ExtensionState::Installed
    );
}

#[test]
fn firefox_xdg_root_is_scanned_and_a_user_disabled_addon_is_installed_but_disabled() {
    let (fixture, flatpak) = firefox_world();
    let xdg = fixture.home.join(".config/mozilla/firefox");
    write_with_parents(
        &xdg.join("profiles.ini"),
        "[Profile0]\nName=x\nIsRelative=1\nPath=p1\n",
    );
    write_firefox_extensions(&xdg.join("p1"), &json!({"userDisabled": true}));
    let report = report_of(&fixture, published_listings(), &flatpak, "firefox");
    assert_eq!(report.state, ExtensionState::InstalledButDisabled);
    assert_eq!(report.install_action, None);
}

#[test]
fn firefox_without_the_addon_offers_the_store_page_and_without_publication_waits() {
    let (fixture, flatpak) = firefox_world();
    let mozilla = fixture.home.join(".mozilla/firefox");
    write_with_parents(
        &mozilla.join("profiles.ini"),
        "[Profile0]\nIsRelative=1\nPath=p\n",
    );
    write_with_parents(
        &mozilla.join("p/extensions.json"),
        r#"{"addons": [{"id": "x@y", "active": true}]}"#,
    );
    let report = report_of(&fixture, published_listings(), &flatpak, "firefox");
    assert_eq!(report.state, ExtensionState::NotInstalled);
    assert_eq!(report.install_action, Some(InstallAction::OpenStorePage));
    let report = report_of(
        &fixture,
        ExtensionListings::unpublished(),
        &flatpak,
        "firefox",
    );
    assert_eq!(report.state, ExtensionState::AwaitingPublication);
}

#[test]
fn leftover_empty_profile_roots_mean_not_installed_not_an_error() {
    let (fixture, flatpak) = firefox_world();
    std::fs::create_dir_all(fixture.home.join(".mozilla/firefox")).expect("empty root");
    assert_eq!(
        report_of(&fixture, published_listings(), &flatpak, "firefox").state,
        ExtensionState::NotInstalled
    );
}

#[test]
fn an_unreadable_preferences_file_is_reported_and_other_browsers_are_still_surveyed() {
    let (fixture, user_data, flatpak) = chromium_world();
    fixture.install_browser_executable("vivaldi");
    let preferences = write_chromium_preferences(&user_data, "Default", "Preferences", &json!({}));
    set_mode(&preferences, 0o000);
    let readable_anyway = std::fs::read(&preferences).is_ok();
    let installer = fixture.extension_installer(
        published_listings(),
        &flatpak,
        &RecordingLauncher::default(),
    );
    let survey = installer.survey().expect("survey");
    let install = installer.install(&target_id("chromium"));
    set_mode(&preferences, 0o644);
    if readable_anyway {
        return; // root: права не действуют
    }
    let states: Vec<_> = survey
        .browsers
        .iter()
        .map(|report| (report.target.as_str().to_owned(), report.state.clone()))
        .collect();
    assert_eq!(
        states,
        vec![
            (
                "chromium".to_owned(),
                ExtensionState::Unreadable {
                    path: preferences.clone(),
                    kind: UnreadableKind::Io(std::io::ErrorKind::PermissionDenied)
                }
            ),
            ("vivaldi".to_owned(), ExtensionState::NotInstalled),
        ]
    );
    assert!(matches!(
        install,
        Err(ExtensionInstallError::ProfileUnreadable { .. })
    ));
    assert!(!expected_hint_path(&fixture, ".config/chromium").exists());
}

#[test]
fn broken_json_is_unreadable_not_silently_not_installed() {
    let (fixture, user_data, flatpak) = chromium_world();
    let path = user_data.join("Default/Preferences");
    write_with_parents(&path, "{ this is not json");
    let report = report_of(&fixture, published_listings(), &flatpak, "chromium");
    assert_eq!(
        report.state,
        ExtensionState::Unreadable {
            path,
            kind: UnreadableKind::MalformedJson
        }
    );
    assert_eq!(report.install_action, None);
}

#[test]
fn an_oversized_profile_file_is_refused_not_read() {
    let (fixture, user_data, flatpak) = chromium_world();
    let path = user_data.join("Default/Preferences");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    let file = std::fs::File::create(&path).expect("create");
    file.set_len(64 * 1024 * 1024 + 1).expect("sparse file");
    let report = report_of(&fixture, published_listings(), &flatpak, "chromium");
    assert_eq!(
        report.state,
        ExtensionState::Unreadable {
            path,
            kind: UnreadableKind::TooLarge
        }
    );
}
