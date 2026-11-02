//! Деградация по профилям: битый файл одного профиля не отравляет браузер;
//! разбор `profiles.ini`; не-обычные файлы; сбой `flatpak` как отдельная ошибка.

use std::time::{Duration, Instant};

use serde_json::json;

use super::extension_support::{
    RecordingLauncher, STORE_ID, installed_preferences, make_fifo, published_listings,
    removed_by_user_preferences, write_chromium_preferences, write_firefox_extensions,
};
use super::support::{FakeFlatpak, Fixture, target, target_id, write_with_parents};
use crate::FlatpakError;
use crate::{
    ExtensionInstallError, ExtensionInstallOutcome, ExtensionReport, ExtensionState, InstallAction,
    UnreadableKind,
};

fn survey_report(fixture: &Fixture, flatpak: &FakeFlatpak, id: &str) -> ExtensionReport {
    fixture
        .extension_installer(published_listings(), flatpak, &RecordingLauncher::default())
        .survey()
        .expect("survey")
        .browsers
        .into_iter()
        .find(|report| report.target.as_str() == id)
        .expect("browser is surveyed")
}

fn chromium_world() -> (Fixture, std::path::PathBuf, FakeFlatpak) {
    let fixture = Fixture::new();
    let flatpak = fixture.install_target(target("chromium"), FakeFlatpak::default());
    let user_data = fixture.home.join(".config/chromium");
    (fixture, user_data, flatpak)
}

#[test]
fn one_malformed_chromium_profile_does_not_hide_an_installed_extension_in_another() {
    let (fixture, user_data, flatpak) = chromium_world();
    write_with_parents(&user_data.join("Default/Preferences"), "{ broken");
    write_chromium_preferences(
        &user_data,
        "Profile 1",
        "Preferences",
        &installed_preferences(STORE_ID, &json!([])),
    );
    let report = survey_report(&fixture, &flatpak, "chromium");
    assert_eq!(report.state, ExtensionState::Installed);
    // Install не отказывает из-за одного нечитаемого профиля.
    let installer = fixture.extension_installer(
        published_listings(),
        &flatpak,
        &RecordingLauncher::default(),
    );
    assert_eq!(
        installer.install(&target_id("chromium")),
        Ok(ExtensionInstallOutcome::AlreadyInstalled)
    );
}

#[test]
fn a_bad_profile_next_to_a_clean_one_still_allows_the_silent_install() {
    let (fixture, user_data, flatpak) = chromium_world();
    write_with_parents(&user_data.join("Default/Preferences"), "{ broken");
    write_chromium_preferences(&user_data, "Profile 1", "Preferences", &json!({}));
    let installer = fixture.extension_installer(
        published_listings(),
        &flatpak,
        &RecordingLauncher::default(),
    );
    let report = installer.survey().expect("survey").browsers.remove(0);
    assert_eq!(report.state, ExtensionState::NotInstalled);
    assert_eq!(report.install_action, Some(InstallAction::InstallSilently));
    assert!(matches!(
        installer.install(&target_id("chromium")),
        Ok(ExtensionInstallOutcome::HintWritten { .. })
    ));
}

#[test]
fn removal_in_one_readable_profile_wins_over_a_broken_neighbour_but_not_over_an_installed_one() {
    let (fixture, user_data, flatpak) = chromium_world();
    write_with_parents(&user_data.join("Default/Preferences"), "{ broken");
    write_chromium_preferences(
        &user_data,
        "Profile 1",
        "Preferences",
        &removed_by_user_preferences(STORE_ID),
    );
    assert_eq!(
        survey_report(&fixture, &flatpak, "chromium").state,
        ExtensionState::RemovedInBrowser
    );
    write_chromium_preferences(
        &user_data,
        "Profile 2",
        "Secure Preferences",
        &installed_preferences(STORE_ID, &json!([1])),
    );
    assert_eq!(
        survey_report(&fixture, &flatpak, "chromium").state,
        ExtensionState::InstalledButDisabled
    );
}

#[test]
fn the_first_failing_path_is_reported_only_when_every_existing_file_failed() {
    let (fixture, user_data, flatpak) = chromium_world();
    let first = user_data.join("Default/Preferences");
    write_with_parents(&first, "{ broken");
    write_with_parents(
        &user_data.join("Profile 1/Secure Preferences"),
        "also broken",
    );
    assert_eq!(
        survey_report(&fixture, &flatpak, "chromium").state,
        ExtensionState::Unreadable {
            path: first,
            kind: UnreadableKind::MalformedJson
        }
    );
}

#[test]
fn an_empty_profile_file_means_missing_and_profile_names_must_end_in_digits() {
    let (fixture, user_data, flatpak) = chromium_world();
    write_with_parents(&user_data.join("Default/Preferences"), "");
    // Не профиль: «Profile x» и «Profile » не читаются, их битое содержимое неважно.
    write_with_parents(&user_data.join("Profile x/Preferences"), "{ broken");
    assert_eq!(
        survey_report(&fixture, &flatpak, "chromium").state,
        ExtensionState::NotInstalled
    );
}

#[test]
fn a_fifo_profile_file_is_not_a_regular_file_and_the_survey_returns_promptly() {
    let (fixture, user_data, flatpak) = chromium_world();
    let fifo = user_data.join("Default/Preferences");
    std::fs::create_dir_all(fifo.parent().expect("parent")).expect("dir");
    make_fifo(&fifo);
    let started = Instant::now();
    let report = survey_report(&fixture, &flatpak, "chromium");
    assert!(started.elapsed() < Duration::from_secs(5), "survey blocked");
    assert_eq!(
        report.state,
        ExtensionState::Unreadable {
            path: fifo.clone(),
            kind: UnreadableKind::NotARegularFile
        }
    );
    // С чистым соседним профилем FIFO деградирует до «не мешает».
    write_chromium_preferences(&user_data, "Profile 1", "Preferences", &json!({}));
    assert_eq!(
        survey_report(&fixture, &flatpak, "chromium").state,
        ExtensionState::NotInstalled
    );
}

fn firefox_world() -> (Fixture, FakeFlatpak, std::path::PathBuf) {
    let fixture = Fixture::new();
    let flatpak = fixture.install_target(target("firefox"), FakeFlatpak::default());
    let mozilla = fixture.home.join(".mozilla/firefox");
    (fixture, flatpak, mozilla)
}

#[test]
fn one_malformed_firefox_profile_does_not_hide_the_addon_in_another() {
    let (fixture, flatpak, mozilla) = firefox_world();
    write_with_parents(
        &mozilla.join("profiles.ini"),
        "[Profile0]\nIsRelative=1\nPath=bad\n[Profile1]\nIsRelative=1\nPath=good\n",
    );
    write_with_parents(&mozilla.join("bad/extensions.json"), "{ broken");
    write_firefox_extensions(&mozilla.join("good"), &json!({}));
    assert_eq!(
        survey_report(&fixture, &flatpak, "firefox").state,
        ExtensionState::Installed
    );
}

#[test]
fn an_unreadable_profiles_ini_of_one_root_does_not_poison_the_other_root() {
    let (fixture, flatpak, mozilla) = firefox_world();
    write_with_parents(&mozilla.join("profiles.ini"), "[Profile0]\nPath=x\n");
    std::fs::remove_file(mozilla.join("profiles.ini")).expect("remove");
    make_fifo(&mozilla.join("profiles.ini"));
    let xdg = fixture.home.join(".config/mozilla/firefox");
    write_with_parents(
        &xdg.join("profiles.ini"),
        "[Profile0]\nIsRelative=1\nPath=p\n",
    );
    write_firefox_extensions(&xdg.join("p"), &json!({}));
    assert_eq!(
        survey_report(&fixture, &flatpak, "firefox").state,
        ExtensionState::Installed
    );
}

#[test]
fn when_every_firefox_file_fails_the_first_failing_path_is_reported() {
    let (fixture, flatpak, mozilla) = firefox_world();
    write_with_parents(
        &mozilla.join("profiles.ini"),
        "[Profile0]\nIsRelative=1\nPath=p\n",
    );
    let broken = mozilla.join("p/extensions.json");
    write_with_parents(&broken, "{ broken");
    assert_eq!(
        survey_report(&fixture, &flatpak, "firefox").state,
        ExtensionState::Unreadable {
            path: broken,
            kind: UnreadableKind::MalformedJson
        }
    );
}

#[test]
fn profiles_ini_values_are_trimmed_missing_is_relative_means_absolute_and_other_sections_are_ignored()
 {
    let (fixture, flatpak, mozilla) = firefox_world();
    let absolute = fixture.root.path().join("abs profile");
    let ini = format!(
        "[ProfileGroups]\nIsRelative = 1\nPath = ignored\n\n\
         [Profile0]\n  Path = {}  \n\n\
         [Profile1]\nIsRelative = 1\nPath = relative one \n",
        absolute.display()
    );
    write_with_parents(&mozilla.join("profiles.ini"), &ini);
    // Профиль без IsRelative — абсолютный (с пробелами в имени), в нём аддон отключён.
    write_firefox_extensions(&absolute, &json!({"userDisabled": true}));
    // Секция ProfileGroups не профиль: её «ignored» с битым JSON не должен читаться.
    write_with_parents(&mozilla.join("ignored/extensions.json"), "{ broken");
    // Относительный профиль с пробелом в конце значения: аддон включён.
    write_firefox_extensions(&mozilla.join("relative one"), &json!({}));
    assert_eq!(
        survey_report(&fixture, &flatpak, "firefox").state,
        ExtensionState::Installed
    );
    std::fs::remove_dir_all(mozilla.join("relative one")).expect("remove relative profile");
    assert_eq!(
        survey_report(&fixture, &flatpak, "firefox").state,
        ExtensionState::InstalledButDisabled,
        "the absolute profile was found without IsRelative"
    );
}

#[test]
fn a_flatpak_tool_failure_is_its_own_error_and_packages_do_not_need_flatpak() {
    let fixture = Fixture::new();
    let flatpak = fixture.install_target(target("chromium-flatpak"), FakeFlatpak::default());
    *flatpak.state.list_error.lock().expect("error") = Some(FlatpakError::TimedOut);
    fixture.install_browser_executable("vivaldi");
    let launcher = RecordingLauncher::default();
    let installer = fixture.extension_installer(published_listings(), &flatpak, &launcher);
    assert_eq!(
        installer.install(&target_id("chromium-flatpak")),
        Err(ExtensionInstallError::FlatpakUnavailable(
            FlatpakError::TimedOut
        ))
    );
    // Пакетный браузер при сломанном flatpak ставится как обычно.
    assert!(matches!(
        installer.install(&target_id("vivaldi")),
        Ok(ExtensionInstallOutcome::HintWritten { .. })
    ));
    assert!(launcher.calls().is_empty());
}
