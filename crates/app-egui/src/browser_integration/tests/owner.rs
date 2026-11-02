//! Владелец над настоящим установщиком и временным HOME.

use super::support::*;

#[test]
fn connect_then_disconnect_changes_files_and_the_view() {
    let world = World::new();
    world.install_browser("chromium");
    let mut integration = world.integration(FakeFlatpak::default(), OfferSetting::Disabled);
    wait_until_idle(&mut integration);
    let view = integration.panel_view();
    assert_eq!(row(&view, "Chromium").status_text, "Not connected");
    assert!(!world.chromium_manifest().exists());

    let target = BrowserTargetId::new("chromium");
    assert_eq!(
        integration.request_connect(&target),
        RequestOutcome::Started
    );
    assert_eq!(
        integration.panel_view().activity,
        BrowserPanelActivity::Working,
        "the buttons are disabled while the operation runs"
    );
    wait_until_idle(&mut integration);

    assert!(
        world.chromium_manifest().is_file(),
        "the connection file is written"
    );
    let view = integration.panel_view();
    let chromium = row(&view, "Chromium");
    assert_eq!(chromium.status_text, "Connected");
    assert_eq!(button_labels(chromium), ["Disconnect"]);
    assert!(view.operation_notes.is_empty());

    assert_eq!(
        integration.request_disconnect(&target),
        RequestOutcome::Started
    );
    wait_until_idle(&mut integration);

    assert!(
        !world.chromium_manifest().exists(),
        "the connection file is gone"
    );
    assert_eq!(
        row(&integration.panel_view(), "Chromium").status_text,
        "Not connected"
    );
}

#[test]
fn second_command_is_refused_while_an_operation_is_running() {
    let world = World::new();
    world.install_browser("chromium");
    let mut integration = world.integration(FakeFlatpak::default(), OfferSetting::Disabled);
    wait_until_idle(&mut integration);
    let target = BrowserTargetId::new("chromium");

    assert_eq!(
        integration.request_connect(&target),
        RequestOutcome::Started
    );
    // Результат не забран (`poll` не вызывался): операция считается идущей.
    assert_eq!(
        integration.request_disconnect(&target),
        RequestOutcome::RefusedBusy
    );
    assert_eq!(
        integration.request_allow_flatpak_bus_access(&target),
        RequestOutcome::RefusedBusy
    );
    assert_eq!(
        integration.request_survey(),
        RequestOutcome::QueuedAfterRunning
    );
    wait_until_idle(&mut integration);
    assert!(
        world.chromium_manifest().is_file(),
        "only the first command ran"
    );
}

#[test]
fn flatpak_browser_needs_permission_then_allow_connects_it() {
    let world = World::new();
    let flatpak = FakeFlatpak::default().with_app("com.google.Chrome", PERMISSIONS_WITHOUT_BUS);
    let mut integration = world.integration(flatpak, OfferSetting::Disabled);
    wait_until_idle(&mut integration);
    let target = BrowserTargetId::new("google-chrome-flatpak");

    let _ = integration.request_connect(&target);
    wait_until_idle(&mut integration);
    let view = integration.panel_view();
    let chrome = row(&view, "Google Chrome (Flatpak)");
    assert_eq!(chrome.status_text, "Needs Flatpak permission");
    assert_eq!(chrome.packaging_label, "Flatpak");

    let _ = integration.request_allow_flatpak_bus_access(&target);
    wait_until_idle(&mut integration);
    assert_eq!(
        row(&integration.panel_view(), "Google Chrome (Flatpak)").status_text,
        "Connected"
    );
}

#[test]
fn failed_command_shows_a_readable_note_and_changes_nothing() {
    let world = World::new();
    let mut integration = world.integration(FakeFlatpak::default(), OfferSetting::Disabled);
    wait_until_idle(&mut integration);

    let _ = integration.request_connect(&BrowserTargetId::new("no-such-browser"));
    wait_until_idle(&mut integration);

    let notes = integration.panel_view().operation_notes.clone();
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].starts_with("Could not connect no-such-browser"),
        "{notes:?}"
    );
}

#[test]
fn missing_installer_refuses_every_request_and_explains_why() {
    let mut integration = BrowserIntegration::start_with_backend(
        Backend::Unavailable("The HOME directory is not set".to_owned()),
        AppWakePort::disconnected(AppWakeOwner::BrowserIntegration),
        OfferSetting::Enabled,
    );
    assert_eq!(
        integration.request_survey(),
        RequestOutcome::RefusedNoInstaller
    );
    assert_eq!(
        integration.request_connect(&BrowserTargetId::new("chromium")),
        RequestOutcome::RefusedNoInstaller
    );
    let view = integration.panel_view();
    assert_eq!(
        view.global_notes,
        ["Browser integration is unavailable: The HOME directory is not set"]
    );
    assert_eq!(integration.poll(), PollOutcome::Unchanged);
}
