//! Форматирование локальных locator-ов не раскрывает пути пользователя.
//!
//! `Debug` и `Display` попадают в логи, diagnostics и UI. Содержимое пути
//! (каталоги, имя пользователя, имя неизвестной платформы, сырые units)
//! в них не выводится. Показываются только вид пути, платформа и число units,
//! а у нативного пути `Display` — только имя файла.

use super::*;

/// Фрагмент, который не должен появиться ни в одном выводе.
const SECRET_PATH_PART: &str = "alice-private";

fn assert_redacted(text: &str, context: &str) {
    assert!(!text.contains(SECRET_PATH_PART), "{context}: `{text}`");
}

#[test]
fn foreign_platform_formats_known_names_and_redacts_unknown_name() {
    let cases = [
        (
            ForeignPathPlatform::Linux,
            "ForeignPathPlatform::Linux",
            "linux",
        ),
        (
            ForeignPathPlatform::MacOs,
            "ForeignPathPlatform::MacOs",
            "macos",
        ),
        (
            ForeignPathPlatform::Windows,
            "ForeignPathPlatform::Windows",
            "windows",
        ),
        (
            ForeignPathPlatform::Other(SECRET_PATH_PART.to_owned()),
            "ForeignPathPlatform::Other(<redacted>)",
            "other-platform",
        ),
    ];
    for (platform, expected_debug, expected_display) in cases {
        assert_eq!(format!("{platform:?}"), expected_debug);
        assert_eq!(platform.to_string(), expected_display);
    }
}

#[test]
fn foreign_path_encodings_show_only_kind_and_unit_count() {
    let secret_bytes = format!("/home/{SECRET_PATH_PART}/movie.mkv").into_bytes();
    let secret_wide: Vec<u16> = format!("C:\\{SECRET_PATH_PART}\\movie.mkv")
        .encode_utf16()
        .collect();
    let cases = [
        (
            ForeignPathEncoding::Utf8(format!("/home/{SECRET_PATH_PART}")),
            "ForeignPathEncoding::Utf8(<redacted>)".to_owned(),
        ),
        (
            ForeignPathEncoding::Bytes(secret_bytes.clone()),
            format!("ForeignPathEncoding::Bytes({} units)", secret_bytes.len()),
        ),
        (
            ForeignPathEncoding::Wide(secret_wide.clone()),
            format!("ForeignPathEncoding::Wide({} units)", secret_wide.len()),
        ),
        (
            ForeignPathEncoding::Opaque {
                encoding_name: SECRET_PATH_PART.to_owned(),
                raw_units: vec![1, 2, 3],
            },
            "ForeignPathEncoding::Opaque(3 units)".to_owned(),
        ),
    ];
    for (encoding, expected_debug) in cases {
        assert_eq!(format!("{encoding:?}"), expected_debug);
    }
}

#[test]
fn local_locators_never_print_parent_directories() {
    let native = LocalLocator::Native(PathBuf::from(format!(
        "/home/{SECRET_PATH_PART}/videos/movie.mkv"
    )));
    assert_eq!(
        format!("{native:?}"),
        "LocalLocator::Native(<redacted-path>)"
    );
    // Display показывает только имя файла, без родительских каталогов.
    assert_eq!(native.to_string(), "movie.mkv");
    // Путь без имени файла не раскрывается целиком.
    assert_eq!(
        LocalLocator::Native(PathBuf::from("/")).to_string(),
        "<local-path>"
    );

    let foreign = LocalLocator::Foreign(ForeignPlatformPath::new(
        ForeignPathPlatform::Windows,
        ForeignPathEncoding::Utf8(format!("C:\\{SECRET_PATH_PART}\\movie.mkv")),
    ));
    let foreign_debug = format!("{foreign:?}");
    assert_redacted(&foreign_debug, "foreign debug");
    assert!(foreign_debug.starts_with("LocalLocator::Foreign(ForeignPlatformPath"));
    assert_eq!(foreign.to_string(), "foreign windows path");
}
