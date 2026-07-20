use std::path::{Path, PathBuf};

use playlist_core::DurableReopenLocator;
use playlist_io::{
    GenericM3uPreview, M3uDeclaredFormat, M3uDocument, M3uDocumentSource, M3uDurationHint,
    M3uImportIssueKind, M3uParseRequest, M3uParserLimits, parse_m3u_document,
};

/// Возвращает generic preview либо делает test failure понятным.
fn generic_preview(document: M3uDocument) -> GenericM3uPreview {
    match document {
        M3uDocument::Generic(preview) => preview,
        other => panic!("ожидался generic M3U preview, получено {other:?}"),
    }
}

/// Разбирает local document с default budgets.
fn parse_local(document_text: &str) -> GenericM3uPreview {
    generic_preview(
        parse_m3u_document(M3uParseRequest::new(
            document_text.as_bytes(),
            M3uDocumentSource::local("/music/lists/list.m3u"),
            M3uDeclaredFormat::M3u,
            M3uParserLimits::default(),
        ))
        .expect("generic M3U должен разбираться"),
    )
}

/// Возвращает exact local path из entry draft.
fn local_path(entry: &playlist_io::GenericM3uEntryDraft) -> &Path {
    entry
        .import_draft()
        .reopen_locator()
        .expose_local_for_reopen()
        .expect("ожидался local locator")
        .expose_native_path_for_open()
        .expect("ожидался native path")
}

/// Возвращает exact URL из entry draft.
fn network_uri(entry: &playlist_io::GenericM3uEntryDraft) -> &str {
    entry
        .import_draft()
        .reopen_locator()
        .expose_url_for_reopen()
        .expect("ожидался URL locator")
        .expose_secret_for_open()
}

#[test]
fn generic_bom_is_warning_and_does_not_remove_first_entry() {
    let preview = parse_local("\u{feff}song.mp3\n");

    assert_eq!(preview.retained_entry_count(), 1);
    let issues = preview.issues().iter().collect::<Vec<_>>();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].kind(), M3uImportIssueKind::Utf8BomIgnored);
    assert_eq!(issues[0].line().get(), 1);
}

#[test]
fn lf_crlf_comments_optional_header_and_extinf_association_are_supported() {
    let preview = parse_local(
        "#EXTM3U\r\n\
         # обычный комментарий\r\n\
         #EXTINF:12.5,Первая запись\r\n\
         first.mp3\r\n\
         #EXTINF:-1,Неизвестная длительность\r\n\
         # комментарий между hint и locator\r\n\
         second.mp3\r\n\
         third.mp3\r\n",
    );
    let entries = preview.entries().collect::<Vec<_>>();

    assert_eq!(entries.len(), 3);
    let first_hint = entries[0].extinf_hint().expect("первый EXTINF");
    assert_eq!(first_hint.display_title(), Some("Первая запись"));
    assert!(matches!(first_hint.duration(), M3uDurationHint::Known(_)));
    assert_eq!(
        entries[0].import_draft().cached_metadata().title(),
        Some("Первая запись")
    );
    assert_eq!(
        entries[0]
            .import_draft()
            .cached_metadata()
            .duration()
            .expect("positive duration")
            .as_secs_f64(),
        12.5
    );

    let second_hint = entries[1].extinf_hint().expect("второй EXTINF");
    assert_eq!(
        second_hint.duration(),
        M3uDurationHint::Unknown,
        "negative duration остаётся unknown metadata hint"
    );
    assert_eq!(entries[2].extinf_hint(), None);
    assert_eq!(preview.issues().retained_issue_count(), 0);
}

#[test]
fn relative_absolute_file_and_network_locators_become_ordered_drafts() {
    let preview = parse_local(
        "relative/song.mp3\n\
         /absolute/movie.mkv\n\
         file:///tmp/encoded%20name.flac\n\
         file://localhost/tmp/local.mp3\n\
         https://user:secret@example.invalid/private/video.mp4?token=secret\n",
    );
    let entries = preview.entries().collect::<Vec<_>>();

    assert_eq!(entries.len(), 5);
    assert_eq!(
        local_path(entries[0]),
        Path::new("/music/lists/relative/song.mp3")
    );
    assert_eq!(local_path(entries[1]), Path::new("/absolute/movie.mkv"));
    assert_eq!(local_path(entries[2]), Path::new("/tmp/encoded name.flac"));
    assert_eq!(local_path(entries[3]), Path::new("/tmp/local.mp3"));
    assert_eq!(
        network_uri(entries[4]),
        "https://user:secret@example.invalid/private/video.mp4?token=secret"
    );
}

#[test]
fn network_document_resolves_relative_uris_and_keeps_hierarchical_scheme_drafts() {
    let source = M3uDocumentSource::network(
        "https://example.invalid/private/lists/list.m3u?manifest_token=secret",
    )
    .expect("valid source");
    let preview = generic_preview(
        parse_m3u_document(M3uParseRequest::new(
            b"../media/song.mp3\nftp://cdn.example.invalid/archive.flac\n",
            source,
            M3uDeclaredFormat::M3u,
            M3uParserLimits::default(),
        ))
        .expect("generic network M3U"),
    );
    let entries = preview.entries().collect::<Vec<_>>();

    assert_eq!(entries.len(), 2);
    assert_eq!(
        network_uri(entries[0]),
        "https://example.invalid/private/media/song.mp3"
    );
    assert_eq!(
        network_uri(entries[1]),
        "ftp://cdn.example.invalid/archive.flac"
    );
}

#[test]
fn malformed_and_opaque_uri_lines_are_issues_while_valid_preview_survives() {
    let preview = parse_local(
        "first.mp3\n\
         https://[invalid\n\
         data:text/plain,not-media\n\
         file://remote-host/private.mp3\n\
         second.mp3\n",
    );
    let entries = preview.entries().collect::<Vec<_>>();
    let issue_kinds = preview
        .issues()
        .iter()
        .map(playlist_io::M3uImportIssue::kind)
        .collect::<Vec<_>>();

    assert_eq!(entries.len(), 2);
    assert_eq!(local_path(entries[0]), Path::new("/music/lists/first.mp3"));
    assert_eq!(local_path(entries[1]), Path::new("/music/lists/second.mp3"));
    assert_eq!(
        issue_kinds,
        vec![
            M3uImportIssueKind::MalformedLocator,
            M3uImportIssueKind::UnsupportedLocatorScheme,
            M3uImportIssueKind::UnsupportedLocatorScheme,
        ]
    );
}

#[test]
fn duplicates_are_preserved_as_independent_id_less_drafts() {
    let preview = parse_local("duplicate.mp3\nduplicate.mp3\n");
    let entries = preview.entries().collect::<Vec<_>>();

    assert_eq!(entries.len(), 2);
    assert_eq!(local_path(entries[0]), local_path(entries[1]));
    assert_eq!(
        entries[0].import_draft().reopen_locator(),
        entries[1].import_draft().reopen_locator()
    );
}

#[test]
fn unsupported_directive_and_malformed_extinf_do_not_hide_later_rows() {
    let preview = parse_local(
        "#EXTVLCOPT:http-referrer=https://secret.invalid/\n\
         #EXTINF:not-a-number,title\n\
         first.mp3\n\
         #EXTINF:2,orphan\n",
    );
    let issue_kinds = preview
        .issues()
        .iter()
        .map(playlist_io::M3uImportIssue::kind)
        .collect::<Vec<_>>();

    assert_eq!(preview.retained_entry_count(), 1);
    assert_eq!(
        issue_kinds,
        vec![
            M3uImportIssueKind::UnsupportedDirective,
            M3uImportIssueKind::MalformedExtInf,
            M3uImportIssueKind::OrphanedExtInf,
        ]
    );
}

#[test]
fn source_and_draft_debug_are_secret_safe() {
    let raw_secret = "https://alice:password@example.invalid/private/list.m3u?token=secret";
    let source = M3uDocumentSource::network(raw_secret).expect("valid network source");
    let source_debug = format!("{source:?}");
    let preview = generic_preview(
        parse_m3u_document(M3uParseRequest::new(
            b"https://bob:password@cdn.invalid/private.mp3?token=secret\n",
            source,
            M3uDeclaredFormat::M3u,
            M3uParserLimits::default(),
        ))
        .expect("generic preview"),
    );
    let draft_debug = format!("{:?}", preview.entries().next().expect("one preview entry"));

    for secret_fragment in ["alice", "password", "/private", "token=secret"] {
        assert!(!source_debug.contains(secret_fragment));
    }
    for secret_fragment in ["bob", "password", "/private.mp3", "token=secret"] {
        assert!(!draft_debug.contains(secret_fragment));
    }
}

#[test]
fn local_source_keeps_exact_path_identity_in_root_provenance() {
    let source_path = PathBuf::from("/private/playlists/source.m3u");
    let preview = generic_preview(
        parse_m3u_document(M3uParseRequest::new(
            b"song.mp3\n",
            M3uDocumentSource::local(source_path.clone()),
            M3uDeclaredFormat::M3u,
            M3uParserLimits::default(),
        ))
        .expect("generic preview"),
    );
    let entry = preview.entries().next().expect("one entry");
    let root_locator = entry.import_draft().provenance().root_locator();

    assert!(matches!(root_locator, DurableReopenLocator::Local(_)));
    assert_eq!(
        root_locator
            .expose_local_for_reopen()
            .expect("local root")
            .expose_native_path_for_open(),
        Some(source_path.as_path())
    );
}

#[test]
fn absolute_network_uri_preserves_exact_reopen_identity_after_validation() {
    let exact_uri = "https://EXAMPLE.invalid:443/media/%7eclip.mp4?token=%2f";
    let preview = parse_local(&format!("{exact_uri}\n"));
    let entry = preview.entries().next().expect("one URL entry");

    assert_eq!(network_uri(entry), exact_uri);
}

#[test]
fn generic_m3u8_provenance_is_distinct_and_bom_is_rejected() {
    let source = M3uDocumentSource::local("/music/lists/list.m3u8");
    let preview = generic_preview(
        parse_m3u_document(M3uParseRequest::new(
            b"song.mp3\n",
            source.clone(),
            M3uDeclaredFormat::M3u8,
            M3uParserLimits::default(),
        ))
        .expect("generic M3U8 preview"),
    );
    let entry = preview.entries().next().expect("one entry");

    assert_eq!(
        entry.import_draft().provenance().source_kind(),
        playlist_core::PlaylistImportSourceKind::M3u8
    );

    let error = parse_m3u_document(M3uParseRequest::new(
        "\u{feff}song.mp3\n".as_bytes(),
        source,
        M3uDeclaredFormat::M3u8,
        M3uParserLimits::default(),
    ))
    .expect_err("generic M3U8 BOM запрещён");
    assert_eq!(
        error.kind(),
        playlist_io::M3uParseErrorKind::GenericM3u8BomNotAllowed
    );
}
