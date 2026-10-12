// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

//! Решение владельца B1: `.m3u`/`.cue` — UTF-8, а невалидный UTF-8 читается как
//! Windows-1251 с типизированным признаком; `.m3u8` остаётся строго UTF-8.

use std::fs;
use std::path::Path;

use encoding_rs::WINDOWS_1251;
use playlist_io::{
    CueDocumentSource, CueParseErrorKind, CueParseRequest, CueParserLimits, CueTextEncoding,
    GenericM3uPreview, LocalPlaylistExpansionCancellation, LocalPlaylistExpansionLimits,
    LocalPlaylistExpansionRequest, M3uDeclaredFormat, M3uDocument, M3uDocumentSource,
    M3uParseErrorKind, M3uParseRequest, M3uParserLimits, PlaylistTextEncoding, XspfParserLimits,
    expand_local_playlist, parse_cue_document, parse_m3u_document,
};

/// Кодирует текст в Windows-1251, как это сделал бы Windows-плеер.
fn cp1251(text: &str) -> Vec<u8> {
    let (bytes, _, had_unmappable) = WINDOWS_1251.encode(text);
    assert!(
        !had_unmappable,
        "The fixture must be placed entirely in cp1251."
    );
    bytes.into_owned()
}

fn parse_m3u(
    bytes: &[u8],
    format: M3uDeclaredFormat,
) -> Result<GenericM3uPreview, M3uParseErrorKind> {
    match parse_m3u_document(M3uParseRequest::new(
        bytes,
        M3uDocumentSource::local("/music/lists/list.m3u"),
        format,
        M3uParserLimits::default(),
    )) {
        Ok(M3uDocument::Generic(preview)) => Ok(preview),
        Ok(other) => panic!("Expected generic M3U, received {other:?}"),
        Err(error) => Err(error.kind()),
    }
}

fn parse_cue(bytes: &[u8]) -> Result<playlist_io::CueDocument, CueParseErrorKind> {
    parse_cue_document(CueParseRequest::new(
        bytes,
        CueDocumentSource::local("/music/disc/album.cue"),
        CueParserLimits::default(),
    ))
    .map_err(|error| error.kind().clone())
}

const CYRILLIC_M3U: &str =
    "#EXTM3U\n#EXTINF:12,Кино - Группа крови\nМузыка/Кино/Группа крови.mp3\n";

#[test]
fn cp1251_m3u_decodes_titles_and_paths_and_reports_windows_1251() {
    let preview = parse_m3u(&cp1251(CYRILLIC_M3U), M3uDeclaredFormat::M3u).expect("cp1251 m3u");

    assert_eq!(preview.text_encoding(), PlaylistTextEncoding::Windows1251);
    let entries = preview.entries().collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].extinf_hint().expect("EXTINF").display_title(),
        Some("Кино - Группа крови")
    );
    let path = entries[0]
        .import_draft()
        .reopen_locator()
        .expose_local_for_reopen()
        .expect("local")
        .expose_native_path_for_open()
        .expect("native path");
    assert_eq!(path, Path::new("/music/lists/Музыка/Кино/Группа крови.mp3"));
}

#[test]
fn utf8_m3u_with_and_without_bom_reports_no_fallback() {
    let plain = parse_m3u(CYRILLIC_M3U.as_bytes(), M3uDeclaredFormat::M3u).expect("utf8");
    assert_eq!(plain.text_encoding(), PlaylistTextEncoding::Utf8);

    let mut with_bom = vec![0xEF, 0xBB, 0xBF];
    with_bom.extend_from_slice(CYRILLIC_M3U.as_bytes());
    let bom = parse_m3u(&with_bom, M3uDeclaredFormat::M3u).expect("utf8 bom");
    assert_eq!(bom.text_encoding(), PlaylistTextEncoding::Utf8WithBom);
    assert_eq!(bom.retained_entry_count(), 1);
}

#[test]
fn m3u8_with_invalid_utf8_keeps_the_honest_error() {
    assert_eq!(
        parse_m3u(&cp1251(CYRILLIC_M3U), M3uDeclaredFormat::M3u8).err(),
        Some(M3uParseErrorKind::InvalidUtf8)
    );
}

const CYRILLIC_CUE: &str = "PERFORMER \"Земфира\"\nTITLE \"Прощай\"\nFILE \"album.flac\" FLAC\n  TRACK 01 AUDIO\n    TITLE \"Не отпускай\"\n    PERFORMER \"Земфира\"\n    INDEX 01 00:00:00\n";

#[test]
fn cp1251_cue_decodes_titles_and_reports_windows_1251() {
    let document = parse_cue(&cp1251(CYRILLIC_CUE)).expect("cp1251 cue");

    assert_eq!(document.encoding(), CueTextEncoding::Windows1251);
    assert_eq!(document.title(), Some("Прощай"));
    assert_eq!(document.performer(), Some("Земфира"));
    assert_eq!(document.tracks()[0].title(), Some("Не отпускай"));
    assert_eq!(document.tracks()[0].performer(), Some("Земфира"));
}

#[test]
fn utf8_cue_with_and_without_bom_reports_no_fallback() {
    let plain = parse_cue(CYRILLIC_CUE.as_bytes()).expect("utf8 cue");
    assert_eq!(plain.encoding(), CueTextEncoding::Utf8);
    assert_eq!(plain.title(), Some("Прощай"));

    let mut with_bom = vec![0xEF, 0xBB, 0xBF];
    with_bom.extend_from_slice(CYRILLIC_CUE.as_bytes());
    let bom = parse_cue(&with_bom).expect("utf8 bom cue");
    assert_eq!(bom.encoding(), CueTextEncoding::Utf8WithBom);
}

#[test]
fn cue_with_nul_bytes_is_still_rejected_instead_of_read_as_cp1251() {
    // UTF-16 без BOM: NUL-байты — признак бинарного/неподдерживаемого файла.
    let bomless_utf16 = CYRILLIC_CUE
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    assert_eq!(
        parse_cue(&bomless_utf16).err(),
        Some(CueParseErrorKind::UnsupportedOrInvalidEncoding)
    );
}

#[test]
fn expansion_summary_counts_documents_read_as_windows_1251() {
    let directory = std::env::temp_dir().join(format!(
        "fastiplayer-playlist-io-legacy-encoding-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).expect("temp dir");
    let legacy = directory.join("legacy.m3u");
    let modern = directory.join("modern.m3u");
    fs::write(&legacy, cp1251(CYRILLIC_M3U)).expect("write cp1251");
    fs::write(&modern, CYRILLIC_M3U).expect("write utf8");

    let summary_of = |root: &Path| {
        let cancellation = LocalPlaylistExpansionCancellation::new();
        expand_local_playlist(LocalPlaylistExpansionRequest::new(
            root,
            LocalPlaylistExpansionLimits::default(),
            M3uParserLimits::default(),
            XspfParserLimits::default(),
            &cancellation,
        ))
        .expect("expansion starts")
        .summary()
    };
    let legacy_summary = summary_of(&legacy);
    let modern_summary = summary_of(&modern);
    fs::remove_dir_all(&directory).expect("cleanup");

    assert_eq!(legacy_summary.windows_1251_documents(), 1);
    assert_eq!(legacy_summary.retained_items(), 1);
    assert_eq!(modern_summary.windows_1251_documents(), 0);
}
