//! Установка целиком: какой путь выбран для каждого браузера и что ушло в запуск.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use super::extension_support::{
    ADD_ONS_SLUG, DEVELOPMENT_ID, RecordingLauncher, STORE_ID, expected_hint_path,
    installed_preferences, published_listings, write_chromium_preferences,
};
use super::support::{
    FakeFlatpak, Fixture, PERMISSIONS_FULL_BUS, target, target_id, write_executable,
};
use crate::{
    BrowserLaunchCommand, ExtensionInstallError, ExtensionInstallOutcome, ExtensionListings,
    ExtensionState, ExtensionUnsupported, LaunchError, Listing, TARGETS,
};

fn store_url() -> String {
    format!("https://chromewebstore.google.com/detail/{STORE_ID}")
}

#[test]
fn google_chrome_package_opens_the_store_page_in_the_found_executable_and_writes_nothing() {
    let fixture = Fixture::new();
    // Второе имя из каталога не должно выбираться, пока первое найдено.
    fixture.install_browser_executable("google-chrome-stable");
    let launcher = RecordingLauncher::default();
    let installer =
        fixture.extension_installer(published_listings(), &FakeFlatpak::default(), &launcher);
    let outcome = installer
        .install(&target_id("google-chrome"))
        .expect("install");
    assert_eq!(
        outcome,
        ExtensionInstallOutcome::StorePageOpened { url: store_url() }
    );
    assert_eq!(
        launcher.calls(),
        vec![(
            BrowserLaunchCommand::Executable(fixture.bin_dir.join("google-chrome-stable")),
            store_url()
        )]
    );
    assert!(
        fixture.snapshot_home().is_empty(),
        "the Chrome route must not write hints"
    );
}

#[test]
fn google_chrome_flatpak_opens_the_store_page_through_its_app_id() {
    let fixture = Fixture::new();
    let flatpak = FakeFlatpak::default().with_app("com.google.Chrome", PERMISSIONS_FULL_BUS);
    let launcher = RecordingLauncher::default();
    let installer = fixture.extension_installer(published_listings(), &flatpak, &launcher);
    installer
        .install(&target_id("google-chrome-flatpak"))
        .expect("install");
    assert_eq!(
        launcher.calls(),
        vec![(
            BrowserLaunchCommand::Flatpak {
                app_id: "com.google.Chrome".to_owned()
            },
            store_url()
        )]
    );
    assert!(fixture.snapshot_home().is_empty());
}

#[test]
fn firefox_opens_the_add_ons_page_in_its_own_executable() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("firefox");
    let launcher = RecordingLauncher::default();
    let installer =
        fixture.extension_installer(published_listings(), &FakeFlatpak::default(), &launcher);
    installer.install(&target_id("firefox")).expect("install");
    assert_eq!(
        launcher.calls(),
        vec![(
            BrowserLaunchCommand::Executable(fixture.bin_dir.join("firefox")),
            format!("https://addons.mozilla.org/firefox/addon/{ADD_ONS_SLUG}/")
        )]
    );
}

#[test]
fn a_launch_failure_keeps_its_own_error_kind() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("google-chrome");
    let launcher = RecordingLauncher::failing_with(LaunchError::ProgramNotFound);
    let installer =
        fixture.extension_installer(published_listings(), &FakeFlatpak::default(), &launcher);
    assert_eq!(
        installer.install(&target_id("google-chrome")),
        Err(ExtensionInstallError::Launch(LaunchError::ProgramNotFound))
    );
}

#[test]
fn already_installed_is_a_no_op_that_neither_writes_nor_launches() {
    let fixture = Fixture::new();
    let flatpak = fixture.install_target(target("chromium"), FakeFlatpak::default());
    write_chromium_preferences(
        &fixture.home.join(".config/chromium"),
        "Default",
        "Preferences",
        &installed_preferences(STORE_ID, &json!([])),
    );
    let before = fixture.snapshot_home();
    let launcher = RecordingLauncher::default();
    let installer = fixture.extension_installer(published_listings(), &flatpak, &launcher);
    assert_eq!(
        installer.install(&target_id("chromium")),
        Ok(ExtensionInstallOutcome::AlreadyInstalled)
    );
    assert_eq!(fixture.snapshot_home(), before);
    assert!(launcher.calls().is_empty());
}

#[test]
fn disabled_extension_is_reported_as_such_without_any_action() {
    let fixture = Fixture::new();
    let flatpak = fixture.install_target(target("chromium"), FakeFlatpak::default());
    write_chromium_preferences(
        &fixture.home.join(".config/chromium"),
        "Default",
        "Preferences",
        &installed_preferences(DEVELOPMENT_ID, &json!([1])),
    );
    let installer = fixture.extension_installer(
        published_listings(),
        &flatpak,
        &RecordingLauncher::default(),
    );
    assert_eq!(
        installer.install(&target_id("chromium")),
        Ok(ExtensionInstallOutcome::AlreadyInstalledButDisabled)
    );
}

#[test]
fn snap_browser_is_unsupported_and_nothing_is_written() {
    let fixture = Fixture::new();
    write_executable(&fixture.snap_dir.join("chromium"), b"#!fake-snap-chromium");
    let launcher = RecordingLauncher::default();
    let installer =
        fixture.extension_installer(published_listings(), &FakeFlatpak::default(), &launcher);
    let report = installer.survey().expect("survey").browsers.remove(0);
    assert_eq!(
        report.state,
        ExtensionState::Unsupported(ExtensionUnsupported::Snap)
    );
    assert_eq!(report.install_action, None);
    assert_eq!(
        installer.install(&target_id("chromium")),
        Err(ExtensionInstallError::Unsupported(
            ExtensionUnsupported::Snap
        ))
    );
    assert!(fixture.snapshot_home().is_empty());
    assert!(launcher.calls().is_empty());
}

#[test]
fn unsupported_catalog_targets_name_their_own_reason() {
    let fixture = Fixture::new();
    let mut flatpak = FakeFlatpak::default();
    for app_id in [
        "org.mozilla.firefox",
        "io.github.ungoogled_software.ungoogled_chromium",
    ] {
        flatpak = flatpak.with_app(app_id, PERMISSIONS_FULL_BUS);
    }
    let launcher = RecordingLauncher::default();
    let installer = fixture.extension_installer(published_listings(), &flatpak, &launcher);
    let reasons: BTreeMap<String, ExtensionState> = installer
        .survey()
        .expect("survey")
        .browsers
        .into_iter()
        .map(|report| (report.target.as_str().to_owned(), report.state))
        .collect();
    assert_eq!(
        reasons["firefox-flatpak"],
        ExtensionState::Unsupported(ExtensionUnsupported::FlatpakFirefoxFamily)
    );
    assert_eq!(
        reasons["ungoogled-chromium-flatpak"],
        ExtensionState::Unsupported(ExtensionUnsupported::NoWebStore)
    );
    assert_eq!(
        installer.install(&target_id("ungoogled-chromium-flatpak")),
        Err(ExtensionInstallError::Unsupported(
            ExtensionUnsupported::NoWebStore
        ))
    );
    assert!(fixture.snapshot_home().is_empty());
    assert!(launcher.calls().is_empty());
}

#[test]
fn unknown_and_missing_browsers_give_distinct_errors() {
    let fixture = Fixture::new();
    let installer = fixture.extension_installer(
        published_listings(),
        &FakeFlatpak::default(),
        &RecordingLauncher::default(),
    );
    assert_eq!(
        installer.install(&target_id("netscape")),
        Err(ExtensionInstallError::UnknownTarget)
    );
    assert_eq!(
        installer.install(&target_id("chromium")),
        Err(ExtensionInstallError::BrowserNotInstalled)
    );
    assert!(installer.survey().expect("survey").browsers.is_empty());
}

/// Все файлы под HOME, кроме нашей подсказки, для сравнения «до/после».
fn snapshot_without_hints(fixture: &Fixture) -> BTreeMap<PathBuf, Vec<u8>> {
    fixture
        .snapshot_home()
        .into_iter()
        .filter(|(path, _)| {
            !path
                .components()
                .any(|part| part.as_os_str() == "External Extensions")
        })
        .collect()
}

#[test]
fn survey_and_install_modify_nothing_in_browser_profiles_except_our_hint() {
    let fixture = Fixture::new();
    let flatpak = fixture.install_target(target("chromium"), FakeFlatpak::default());
    fixture.install_browser_executable("firefox");
    let user_data = fixture.home.join(".config/chromium");
    write_chromium_preferences(
        &user_data,
        "Default",
        "Preferences",
        &json!({"extensions": {}}),
    );
    write_chromium_preferences(
        &user_data,
        "Default",
        "Secure Preferences",
        &json!({"x": 1}),
    );
    super::support::write_with_parents(
        &fixture.home.join(".mozilla/firefox/profiles.ini"),
        "[Profile0]\nIsRelative=1\nPath=p\n",
    );
    super::support::write_with_parents(
        &fixture.home.join(".mozilla/firefox/p/extensions.json"),
        r#"{"addons":[]}"#,
    );
    let before = fixture.snapshot_home();
    let installer = fixture.extension_installer(
        published_listings(),
        &flatpak,
        &RecordingLauncher::default(),
    );
    installer.survey().expect("survey");
    assert_eq!(fixture.snapshot_home(), before, "survey must be read-only");
    installer.install(&target_id("chromium")).expect("install");
    installer
        .install(&target_id("firefox"))
        .expect("firefox install");
    let after = fixture.snapshot_home();
    assert_eq!(snapshot_without_hints(&fixture), before);
    let added: Vec<_> = after
        .keys()
        .filter(|path| !before.contains_key(*path))
        .collect();
    assert_eq!(
        added,
        vec![&expected_hint_path(&fixture, ".config/chromium")]
    );
}

#[test]
fn survey_reports_only_found_browsers_in_catalog_order() {
    let fixture = Fixture::new();
    fixture.install_browser_executable("vivaldi");
    fixture.install_browser_executable("firefox");
    let flatpak = FakeFlatpak::default().with_app("com.brave.Browser", PERMISSIONS_FULL_BUS);
    let installer = fixture.extension_installer(
        published_listings(),
        &flatpak,
        &RecordingLauncher::default(),
    );
    let ids: Vec<String> = installer
        .survey()
        .expect("survey")
        .browsers
        .into_iter()
        .map(|report| report.target.as_str().to_owned())
        .collect();
    assert_eq!(ids, ["firefox", "vivaldi", "brave-flatpak"]);
}

#[test]
fn catalog_extension_roots_agree_with_the_native_messaging_roots() {
    for candidate in TARGETS {
        let crate::ExtensionSupport::Chromium { user_data, .. } = candidate.extension else {
            continue;
        };
        let fixture = Fixture::new();
        let resolved = user_data
            .resolve(&fixture.environment, candidate.flatpak_app_id())
            .expect("root resolves");
        assert_eq!(
            candidate.manifest_directories(&fixture.environment),
            vec![resolved.join("NativeMessagingHosts")],
            "{}",
            candidate.id
        );
    }
}

#[test]
fn listings_reject_ids_that_could_escape_the_hint_folder() {
    for bad in ["../evil", "short", &"Z".repeat(32)] {
        assert_eq!(
            ExtensionListings::new(Listing::Published(bad.to_owned()), Listing::NotPublished),
            Err(crate::ListingError::InvalidChromeWebStoreId),
            "{bad}"
        );
    }
    assert_eq!(
        ExtensionListings::new(Listing::NotPublished, Listing::Published("a/b".to_owned())),
        Err(crate::ListingError::InvalidFirefoxSlug)
    );
}

#[test]
fn protocol_listings_are_valid_and_unpublished_until_session_24() {
    // `published()` не паникует и не подменяет некорректные значения молча:
    // неверная константа протокола дала бы здесь «не опубликовано», а этот тест
    // сверяет, что значение совпадает с константой.
    use browser_bridge_protocol::extension_identity::{CHROME_WEB_STORE_LISTING, StoreListing};
    let listings = ExtensionListings::published();
    assert_eq!(
        listings.chrome_web_store_id(),
        match CHROME_WEB_STORE_LISTING {
            StoreListing::Published(id) => Some(id),
            StoreListing::NotPublished => None,
        }
    );
}
