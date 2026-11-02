//! Владелец с НАСТОЯЩИМ установщиком экстеншена над временным HOME (фейки только у
//! `flatpak` и запуска браузера): кнопки → операции → файлы → свежий статус в модели вида.

use browser_bridge_install::Listing;

use super::support::*;

/// Правильный по форме ID Chrome Web Store (32 буквы a–p): настоящий экстеншен не нужен.
const FAKE_STORE_ID: &str = "abcdefghijklmnopabcdefghijklmnop";

fn published_listings() -> ExtensionListings {
    ExtensionListings::new(
        Listing::Published(FAKE_STORE_ID.to_owned()),
        Listing::NotPublished,
    )
    .expect("valid listings")
}

impl World {
    /// Подсказка Chromium из пакета (тихий путь).
    fn chromium_hint(&self) -> PathBuf {
        self.home
            .join(".config/chromium/External Extensions")
            .join(format!("{FAKE_STORE_ID}.json"))
    }

    fn google_chrome_hint(&self) -> PathBuf {
        self.home
            .join(".config/google-chrome/External Extensions")
            .join(format!("{FAKE_STORE_ID}.json"))
    }
}

fn extension_text(view: &BrowserPanelView, name: &str) -> String {
    row(view, name)
        .extension
        .as_ref()
        .unwrap_or_else(|| panic!("no extension line for {name}"))
        .status_text
        .clone()
}

fn extension_labels(view: &BrowserPanelView, name: &str) -> Vec<&'static str> {
    row(view, name)
        .extension
        .as_ref()
        .map(|extension| extension.buttons.iter().map(|b| b.label).collect())
        .unwrap_or_default()
}

#[test]
fn chromium_install_writes_the_hint_then_restart_status_then_cancel_cleans_up() {
    let world = World::new();
    world.install_browser("chromium");
    let launcher = RecordingLauncher::default();
    let extension = world.extension_installer(published_listings(), launcher.clone());
    let mut integration = world.integration_with_extension_installer(
        FakeFlatpak::default(),
        extension,
        OfferSetting::Disabled,
    );
    wait_until_idle(&mut integration);
    let view = integration.panel_view();
    assert_eq!(
        extension_text(&view, "Chromium"),
        "Extension: not installed"
    );
    assert_eq!(extension_labels(&view, "Chromium"), ["Install extension"]);

    let target = BrowserTargetId::new("chromium");
    assert_eq!(
        integration.request_install_extension(&target),
        RequestOutcome::Started
    );
    wait_until_idle(&mut integration);

    assert!(world.chromium_hint().is_file(), "the hint file is written");
    assert!(launcher.opened.lock().expect("opened").is_empty());
    let view = integration.panel_view();
    assert_eq!(
        view.operation_notes,
        ["Restart Chromium to finish installing the extension."]
    );
    assert_eq!(
        extension_text(&view, "Chromium"),
        "Extension: restart Chromium to finish installing"
    );
    assert_eq!(
        extension_labels(&view, "Chromium"),
        ["Install in browser", "Cancel"]
    );

    assert_eq!(
        integration.request_cancel_extension_install(&target),
        RequestOutcome::Started
    );
    wait_until_idle(&mut integration);

    assert!(!world.chromium_hint().exists(), "the hint file is gone");
    let view = integration.panel_view();
    assert_eq!(
        view.operation_notes,
        ["Cancelled. Chromium will not install the extension."]
    );
    assert_eq!(
        extension_text(&view, "Chromium"),
        "Extension: not installed"
    );
}

#[test]
fn google_chrome_install_opens_the_store_page_and_writes_nothing() {
    let world = World::new();
    world.install_browser("google-chrome-stable");
    let launcher = RecordingLauncher::default();
    let extension = world.extension_installer(published_listings(), launcher.clone());
    let mut integration = world.integration_with_extension_installer(
        FakeFlatpak::default(),
        extension,
        OfferSetting::Disabled,
    );
    wait_until_idle(&mut integration);
    let view = integration.panel_view();
    assert_eq!(
        extension_labels(&view, "Google Chrome"),
        ["Install in browser"]
    );

    let _ = integration.request_install_extension(&BrowserTargetId::new("google-chrome"));
    wait_until_idle(&mut integration);

    assert_eq!(
        *launcher.opened.lock().expect("opened"),
        [format!(
            "https://chromewebstore.google.com/detail/{FAKE_STORE_ID}"
        )]
    );
    assert!(!world.google_chrome_hint().exists());
    assert_eq!(
        integration.panel_view().operation_notes,
        ["Opened the store page in Google Chrome — press Add there."]
    );
}

#[test]
fn unpublished_listings_say_available_after_publication_and_offer_no_button() {
    let world = World::new();
    world.install_browser("chromium");
    let mut integration = world.integration(FakeFlatpak::default(), OfferSetting::Disabled);
    wait_until_idle(&mut integration);
    let view = integration.panel_view();

    assert_eq!(
        extension_text(&view, "Chromium"),
        "Extension: available after publication"
    );
    assert!(extension_labels(&view, "Chromium").is_empty());

    // Даже если команду всё же прислали, ничего не пишется, а человек видит причину.
    let _ = integration.request_install_extension(&BrowserTargetId::new("chromium"));
    wait_until_idle(&mut integration);
    assert!(!world.chromium_hint().exists());
    assert_eq!(
        integration.panel_view().operation_notes,
        [
            "Could not install the extension in Chromium: the extension is not published in the store yet"
        ]
    );
}

#[test]
fn foreign_hint_is_never_overwritten_and_install_falls_back_to_the_store_page() {
    let world = World::new();
    world.install_browser("chromium");
    let hint = world.chromium_hint();
    std::fs::create_dir_all(hint.parent().expect("parent")).expect("dir");
    std::fs::write(
        &hint,
        br#"{"external_update_url":"https://example.invalid/"}"#,
    )
    .expect("write");
    let foreign_bytes = std::fs::read(&hint).expect("read");
    let launcher = RecordingLauncher::default();
    let extension = world.extension_installer(published_listings(), launcher.clone());
    let mut integration = world.integration_with_extension_installer(
        FakeFlatpak::default(),
        extension,
        OfferSetting::Disabled,
    );
    wait_until_idle(&mut integration);
    let view = integration.panel_view();
    assert_eq!(extension_labels(&view, "Chromium"), ["Install in browser"]);
    let notes = &row(&view, "Chromium")
        .extension
        .as_ref()
        .expect("line")
        .notes;
    assert_eq!(
        notes,
        &["Another program left an extension hint for this browser"]
    );

    let target = BrowserTargetId::new("chromium");
    let _ = integration.request_cancel_extension_install(&target);
    wait_until_idle(&mut integration);
    assert!(hint.is_file(), "a foreign file is left in place");
    assert_eq!(
        integration.panel_view().operation_notes,
        ["The hint file belongs to another program and was left in place."]
    );

    let _ = integration.request_install_extension(&target);
    wait_until_idle(&mut integration);
    assert_eq!(
        *launcher.opened.lock().expect("opened"),
        [format!(
            "https://chromewebstore.google.com/detail/{FAKE_STORE_ID}"
        )]
    );
    assert_eq!(
        integration.panel_view().operation_notes,
        ["Opened the store page in Chromium — press Add there."]
    );
    assert_eq!(
        std::fs::read(&hint).expect("read"),
        foreign_bytes,
        "the foreign file is byte-identical"
    );
}

#[test]
fn extension_commands_are_refused_while_an_operation_is_running() {
    let world = World::new();
    world.install_browser("chromium");
    let extension = world.extension_installer(published_listings(), RecordingLauncher::default());
    let mut integration = world.integration_with_extension_installer(
        FakeFlatpak::default(),
        extension,
        OfferSetting::Disabled,
    );
    wait_until_idle(&mut integration);
    let target = BrowserTargetId::new("chromium");

    assert_eq!(
        integration.request_connect(&target),
        RequestOutcome::Started
    );
    assert_eq!(
        integration.request_install_extension(&target),
        RequestOutcome::RefusedBusy
    );
    assert_eq!(
        integration.request_cancel_extension_install(&target),
        RequestOutcome::RefusedBusy
    );
    wait_until_idle(&mut integration);
    assert!(
        !world.chromium_hint().exists(),
        "the refused command did nothing"
    );
}

#[test]
fn routed_extension_commands_start_the_right_operations() {
    let world = World::new();
    world.install_browser("chromium");
    let extension = world.extension_installer(published_listings(), RecordingLauncher::default());
    let mut integration = world.integration_with_extension_installer(
        FakeFlatpak::default(),
        extension,
        OfferSetting::Disabled,
    );
    wait_until_idle(&mut integration);
    let click = |command| SettingsUiAction::BrowserIntegration {
        target_id: "chromium".to_owned(),
        command,
    };

    let mut actions = vec![
        click(BrowserCommand::InstallExtension),
        SettingsUiAction::Apply,
    ];
    let effect = route_settings_actions(&mut integration, &mut actions);
    assert_eq!(effect, RoutingEffect::VisibleChange);
    assert_eq!(
        actions,
        [SettingsUiAction::Apply],
        "the click is intercepted"
    );
    wait_until_idle(&mut integration);
    assert!(world.chromium_hint().is_file(), "Install ran");

    let mut actions = vec![click(BrowserCommand::CancelExtensionInstall)];
    route_settings_actions(&mut integration, &mut actions);
    assert!(actions.is_empty());
    wait_until_idle(&mut integration);
    assert!(!world.chromium_hint().exists(), "Cancel ran");
}

#[test]
fn cancel_without_a_pending_install_says_there_is_nothing_to_cancel() {
    let world = World::new();
    world.install_browser("chromium");
    let extension = world.extension_installer(published_listings(), RecordingLauncher::default());
    let mut integration = world.integration_with_extension_installer(
        FakeFlatpak::default(),
        extension,
        OfferSetting::Disabled,
    );
    wait_until_idle(&mut integration);
    let _ = integration.request_cancel_extension_install(&BrowserTargetId::new("chromium"));
    wait_until_idle(&mut integration);
    assert_eq!(
        integration.panel_view().operation_notes,
        ["Nothing to cancel."]
    );
}

#[test]
fn flatpak_tool_failure_has_its_own_reason_text() {
    use browser_bridge_install::{ExtensionInstallError, FlatpakError};
    let text = super::super::extension_operations::error_text(
        &ExtensionInstallError::FlatpakUnavailable(FlatpakError::TimedOut),
    );
    assert_eq!(
        text,
        "the `flatpak` tool did not answer, so Flatpak browsers cannot be checked"
    );
}
