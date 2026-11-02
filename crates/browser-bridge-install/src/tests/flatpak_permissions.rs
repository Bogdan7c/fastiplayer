//! Разбор прав Flatpak: живой вывод и граничные случаи покрытия имени.

use crate::{FlatpakBusAccess, parse_bus_access};

/// Живой вывод `flatpak info --show-permissions com.google.Chrome` с пользовательским
/// override «вся шина» (снят на машине разработчика).
const LIVE_CHROME_FULL_BUS: &str = "[Context]
shared=ipc;network;
sockets=cups;pcsc;pulseaudio;session-bus;system-bus;wayland;x11;
devices=all;dri;input;kvm;shm;usb;
filesystems=host-etc;xdg-download;

[Session Bus Policy]
org.freedesktop.Notifications=talk
org.mpris.MediaPlayer2.chromium.*=own
org.kde.plasma.browser.integration=talk

[System Bus Policy]
org.bluez=talk
";

/// Права без `session-bus`, с произвольной строкой политики шины.
fn with_policy(policy_line: &str) -> String {
    format!(
        "[Context]\nsockets=wayland;x11;\n\n[Session Bus Policy]\norg.freedesktop.Notifications=talk\n{policy_line}\n"
    )
}

#[test]
fn live_output_with_session_bus_socket_is_full_access() {
    assert_eq!(
        parse_bus_access(LIVE_CHROME_FULL_BUS),
        FlatpakBusAccess::FullSessionBus
    );
}

#[test]
fn negated_session_bus_socket_is_not_access() {
    let text = "[Context]\nsockets=wayland;!session-bus;x11;\n\n[Session Bus Policy]\norg.freedesktop.Notifications=talk\n";
    assert_eq!(parse_bus_access(text), FlatpakBusAccess::Missing);
}

#[test]
fn later_negation_wins_over_earlier_session_bus() {
    assert_eq!(
        parse_bus_access("[Context]\nsockets=session-bus;!session-bus;\n"),
        FlatpakBusAccess::Missing
    );
}

#[test]
fn exact_talk_name_of_the_browser_session_grants_access() {
    let text = with_policy("io.github.Bogdan7c.Fastiplayer.BrowserSession=talk");
    assert_eq!(parse_bus_access(&text), FlatpakBusAccess::TalkNameGranted);
}

#[test]
fn own_level_also_grants_access() {
    let text = with_policy("io.github.Bogdan7c.Fastiplayer.BrowserSession=own");
    assert_eq!(parse_bus_access(&text), FlatpakBusAccess::TalkNameGranted);
}

#[test]
fn application_mask_covers_the_browser_session_name() {
    let text = with_policy("io.github.Bogdan7c.Fastiplayer.*=talk");
    assert_eq!(parse_bus_access(&text), FlatpakBusAccess::TalkNameGranted);
}

#[test]
fn wider_mask_covers_the_browser_session_name() {
    let text = with_policy("io.github.Bogdan7c.*=talk");
    assert_eq!(parse_bus_access(&text), FlatpakBusAccess::TalkNameGranted);
}

#[test]
fn exact_application_name_does_not_cover_the_browser_session_name() {
    let text = with_policy("io.github.Bogdan7c.Fastiplayer=talk");
    assert_eq!(parse_bus_access(&text), FlatpakBusAccess::Missing);
}

#[test]
fn see_level_does_not_count() {
    let text = with_policy("io.github.Bogdan7c.Fastiplayer.*=see");
    assert_eq!(parse_bus_access(&text), FlatpakBusAccess::Missing);
}

#[test]
fn unrelated_mask_with_a_similar_prefix_does_not_count() {
    let text = with_policy(
        "io.github.Bogdan7c.Fastiplayer.Browser.*=talk\nio.github.Bogdan7c.Fast.*=talk",
    );
    assert_eq!(parse_bus_access(&text), FlatpakBusAccess::Missing);
}

#[test]
fn talk_policy_in_the_system_bus_section_does_not_count() {
    let text = "[Context]\nsockets=wayland;\n\n[System Bus Policy]\nio.github.Bogdan7c.Fastiplayer.*=talk\n";
    assert_eq!(parse_bus_access(text), FlatpakBusAccess::Missing);
}

#[test]
fn empty_text_means_missing() {
    assert_eq!(parse_bus_access(""), FlatpakBusAccess::Missing);
}

#[test]
fn access_kinds_report_whether_the_bridge_is_allowed() {
    assert!(FlatpakBusAccess::FullSessionBus.allows_bridge());
    assert!(FlatpakBusAccess::TalkNameGranted.allows_bridge());
    assert!(!FlatpakBusAccess::Missing.allows_bridge());
}
