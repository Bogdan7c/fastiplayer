//! Тесты определения вызывающего по аргументам.

use std::ffi::OsString;

use browser_bridge_protocol::extension_identity::{
    BrowserFamily, CHROMIUM_DEVELOPMENT_EXTENSION_ID, FIREFOX_EXTENSION_ID,
};

use super::*;

fn arguments(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn chromium_origin(extension_id: &str) -> String {
    format!("chrome-extension://{extension_id}/")
}

#[test]
fn our_chromium_extension_is_accepted() {
    let caller = identify_caller(&arguments(&[&chromium_origin(
        CHROMIUM_DEVELOPMENT_EXTENSION_ID,
    )]))
    .unwrap();
    assert_eq!(caller.browser(), BrowserFamily::Chromium);
}

#[test]
fn chromium_on_windows_may_add_the_parent_window_handle() {
    let caller = identify_caller(&arguments(&[
        &chromium_origin(CHROMIUM_DEVELOPMENT_EXTENSION_ID),
        "--parent-window=0",
    ]))
    .unwrap();
    assert_eq!(caller.browser(), BrowserFamily::Chromium);
}

#[test]
fn our_firefox_extension_is_accepted() {
    let caller = identify_caller(&arguments(&[
        "/home/user/.mozilla/native-messaging-hosts/io.github.bogdan7c.fastiplayer.bridge.json",
        FIREFOX_EXTENSION_ID,
    ]))
    .unwrap();
    assert_eq!(caller.browser(), BrowserFamily::Firefox);
}

#[test]
fn a_foreign_chromium_extension_is_rejected() {
    assert_eq!(
        identify_caller(&arguments(&[&chromium_origin(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        )])),
        Err(CallerRejection::ForeignExtension {
            browser: BrowserFamily::Chromium
        })
    );
}

#[test]
fn a_foreign_firefox_extension_is_rejected() {
    assert_eq!(
        identify_caller(&arguments(&["/tmp/manifest.json", "evil@example.org"])),
        Err(CallerRejection::ForeignExtension {
            browser: BrowserFamily::Firefox
        })
    );
}

#[test]
fn our_firefox_id_inside_a_chromium_origin_is_foreign() {
    // ID проверяется в своём семействе: подстановка ID Firefox в origin Chrome — чужой.
    assert_eq!(
        identify_caller(&arguments(&[&chromium_origin(FIREFOX_EXTENSION_ID)])),
        Err(CallerRejection::ForeignExtension {
            browser: BrowserFamily::Chromium
        })
    );
}

#[test]
fn no_arguments_means_not_started_by_a_browser() {
    assert_eq!(identify_caller(&[]), Err(CallerRejection::NoCallerArgument));
}

#[test]
fn malformed_chromium_origins_are_rejected() {
    let with_path = format!("chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/page.html");
    let without_slash = format!("chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}");
    for origin in [
        with_path.as_str(),
        without_slash.as_str(),
        "chrome-extension:///",
    ] {
        assert_eq!(
            identify_caller(&arguments(&[origin])),
            Err(CallerRejection::MalformedChromiumOrigin),
            "{origin}"
        );
    }
}

#[test]
fn unexpected_extra_chromium_arguments_are_rejected() {
    assert_eq!(
        identify_caller(&arguments(&[
            &chromium_origin(CHROMIUM_DEVELOPMENT_EXTENSION_ID),
            "--enable-something"
        ])),
        Err(CallerRejection::UnrecognizedArguments)
    );
}

#[test]
fn firefox_form_needs_exactly_two_arguments() {
    for values in [
        vec!["/tmp/manifest.json"],
        vec!["/tmp/manifest.json", FIREFOX_EXTENSION_ID, "extra"],
        vec!["/tmp/manifest.json", ""],
    ] {
        assert_eq!(
            identify_caller(&arguments(&values)),
            Err(CallerRejection::UnrecognizedArguments),
            "{values:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_arguments_are_rejected() {
    use std::os::unix::ffi::OsStringExt;
    let broken = OsString::from_vec(vec![0x63, 0xff, 0xfe]);
    assert_eq!(
        identify_caller(&[broken]),
        Err(CallerRejection::UnrecognizedArguments)
    );
    let broken_id = OsString::from_vec(vec![0xff]);
    assert_eq!(
        identify_caller(&[OsString::from("/tmp/manifest.json"), broken_id]),
        Err(CallerRejection::UnrecognizedArguments)
    );
}
