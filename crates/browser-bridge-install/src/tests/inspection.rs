//! Мелкие чистые части: окружение, разбор службы, классификация манифеста.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use browser_bridge_protocol::extension_identity::{BrowserFamily, NATIVE_HOST_NAME};
use desktop_integration::browser_session_activation_service_file;

use crate::inspection::parse_service_player;
use crate::manifest::{ManifestError, ManifestOwnership, build_manifest, classify_manifest};
use crate::{EnvironmentError, InstallEnvironment};

#[test]
fn environment_uses_xdg_defaults_and_ignores_relative_xdg_values() {
    let environment = InstallEnvironment::from_variables(
        Some("/home/ann".into()),
        Some("relative/config".into()),
        None,
        Some(OsString::from("/usr/bin:/opt/bin")),
    )
    .expect("environment");
    assert_eq!(
        environment.xdg_config_home,
        PathBuf::from("/home/ann/.config")
    );
    assert_eq!(
        environment.xdg_data_home,
        PathBuf::from("/home/ann/.local/share")
    );
    assert_eq!(
        environment.executable_search_dirs,
        vec![PathBuf::from("/usr/bin"), PathBuf::from("/opt/bin")]
    );
}

#[test]
fn environment_honours_absolute_xdg_values() {
    let environment = InstallEnvironment::from_variables(
        Some("/home/ann".into()),
        Some("/cfg".into()),
        Some("/data".into()),
        None,
    )
    .expect("environment");
    assert_eq!(environment.xdg_config_home, PathBuf::from("/cfg"));
    assert_eq!(environment.xdg_data_home, PathBuf::from("/data"));
    assert!(environment.executable_search_dirs.is_empty());
}

#[test]
fn missing_or_relative_home_is_a_typed_error() {
    assert_eq!(
        InstallEnvironment::from_variables(None, None, None, None),
        Err(EnvironmentError::HomeUnavailable)
    );
    assert_eq!(
        InstallEnvironment::from_variables(Some("home".into()), None, None, None),
        Err(EnvironmentError::HomeUnavailable)
    );
}

#[test]
fn player_path_is_read_back_from_the_generated_service_file() {
    let player = Path::new("/home/ann/my player/fastiplayer");
    let service = browser_session_activation_service_file(player).expect("service");
    assert_eq!(parse_service_player(&service), Some(player.to_path_buf()));
}

#[test]
fn foreign_or_malformed_service_files_have_no_player() {
    assert_eq!(
        parse_service_player(
            "[D-BUS Service]\nName=org.other.Service\nExec='/bin/x' --browser-session\n"
        ),
        None
    );
    assert_eq!(
        parse_service_player(
            "[D-BUS Service]\nName=io.github.Bogdan7c.Fastiplayer.BrowserSession\nExec=/bin/x --browser-session\n"
        ),
        None
    );
    assert_eq!(
        parse_service_player(
            "[D-BUS Service]\nName=io.github.Bogdan7c.Fastiplayer.BrowserSession\nExec='/bin/x' --other\n"
        ),
        None
    );
}

#[test]
fn manifest_classification_separates_identical_ours_foreign_and_damaged() {
    let host =
        Path::new("/home/ann/.local/share/fastiplayer/browser-host/fastiplayer-browser-host");
    let expected = build_manifest(BrowserFamily::Chromium, host).expect("manifest");
    assert_eq!(
        classify_manifest(&expected, &expected, host),
        ManifestOwnership::Identical
    );

    let ours_older = format!(
        "{{\"name\":\"{NATIVE_HOST_NAME}\",\"path\":\"{}\"}}",
        host.display()
    );
    assert_eq!(
        classify_manifest(&ours_older, &expected, host),
        ManifestOwnership::OursButDiffers
    );

    let foreign = format!("{{\"name\":\"{NATIVE_HOST_NAME}\",\"path\":\"/somewhere/else\"}}");
    assert_eq!(
        classify_manifest(&foreign, &expected, host),
        ManifestOwnership::Foreign
    );
    assert_eq!(
        classify_manifest("[1,2,3]", &expected, host),
        ManifestOwnership::Foreign
    );
    assert_eq!(
        classify_manifest("{ broken", &expected, host),
        ManifestOwnership::Damaged
    );
}

#[test]
fn manifest_for_a_non_utf8_host_path_is_a_typed_error() {
    use std::os::unix::ffi::OsStrExt;
    let host = Path::new(std::ffi::OsStr::from_bytes(b"/home/\xff/host"));
    assert_eq!(
        build_manifest(BrowserFamily::Chromium, host),
        Err(ManifestError::HostPathNotUtf8)
    );
}
