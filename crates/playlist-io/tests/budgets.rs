use playlist_io::{
    M3uDeclaredFormat, M3uDocument, M3uDocumentSource, M3uImportIssueKind, M3uParseErrorKind,
    M3uParseRequest, M3uParserLimits, M3uParserLimitsError, parse_m3u_document,
};

/// Возвращает generic preview для budget assertions.
fn generic_preview(document: M3uDocument) -> playlist_io::GenericM3uPreview {
    match document {
        M3uDocument::Generic(preview) => preview,
        other => panic!("ожидался generic preview: {other:?}"),
    }
}

#[test]
fn invalid_zero_and_over_domain_limits_are_rejected_before_parse() {
    assert_eq!(
        M3uParserLimits::new(0, 1, 1, 1),
        Err(M3uParserLimitsError::ZeroDocumentBytes)
    );
    assert_eq!(
        M3uParserLimits::new(1, 0, 1, 1),
        Err(M3uParserLimitsError::ZeroLineBytes)
    );
    assert_eq!(
        M3uParserLimits::new(1, 1, 0, 1),
        Err(M3uParserLimitsError::ZeroItems)
    );
    assert_eq!(
        M3uParserLimits::new(1, 1, 1, 0),
        Err(M3uParserLimitsError::ZeroIssues)
    );
    assert!(matches!(
        M3uParserLimits::new(1, 1, playlist_core::MAX_PLAYLIST_ITEMS + 1, 1),
        Err(M3uParserLimitsError::ItemLimitExceedsDomainCapacity { .. })
    ));
}

#[test]
fn oversized_document_is_fatal_before_utf8_or_classification() {
    let limits = M3uParserLimits::new(4, 32, 4, 4).expect("valid limits");
    let error = parse_m3u_document(M3uParseRequest::new(
        b"12345",
        M3uDocumentSource::local("/tmp/list.m3u"),
        M3uDeclaredFormat::M3u,
        limits,
    ))
    .expect_err("document cap");

    assert_eq!(error.kind(), M3uParseErrorKind::DocumentLimitExceeded);
}

#[test]
fn oversized_generic_line_is_issue_and_partial_preview_continues() {
    let limits = M3uParserLimits::new(128, 9, 4, 4).expect("valid limits");
    let preview = generic_preview(
        parse_m3u_document(M3uParseRequest::new(
            b"first.mp3\nthis-line-is-too-long\nlast.mp3\n",
            M3uDocumentSource::local("/tmp/list.m3u"),
            M3uDeclaredFormat::M3u,
            limits,
        ))
        .expect("generic partial preview"),
    );

    assert_eq!(preview.retained_entry_count(), 2);
    assert_eq!(
        preview.issues().iter().next().expect("line issue").kind(),
        M3uImportIssueKind::LineLimitExceeded
    );
}

#[test]
fn oversized_hls_line_is_fatal_and_never_degrades_to_generic_rows() {
    let limits = M3uParserLimits::new(256, 24, 4, 4).expect("valid limits");
    let error = parse_m3u_document(M3uParseRequest::new(
        b"#EXTM3U\n#EXT-X-TARGETDURATION:123456789\n#EXTINF:5,\nsegment.ts\n",
        M3uDocumentSource::network("https://example.invalid/live.m3u8").expect("valid source"),
        M3uDeclaredFormat::M3u8,
        limits,
    ))
    .expect_err("strict HLS line cap");

    assert!(matches!(
        error.kind(),
        M3uParseErrorKind::HlsLineLimitExceeded { line } if line.get() == 2
    ));
}

#[test]
fn item_limit_stops_materialization_without_allocating_extra_draft() {
    let limits = M3uParserLimits::new(256, 64, 2, 8).expect("valid limits");
    let preview = generic_preview(
        parse_m3u_document(M3uParseRequest::new(
            b"one.mp3\ntwo.mp3\nthree.mp3\nfour.mp3\n",
            M3uDocumentSource::local("/tmp/list.m3u"),
            M3uDeclaredFormat::M3u,
            limits,
        ))
        .expect("bounded preview"),
    );

    assert_eq!(preview.retained_entry_count(), 2);
    assert!(preview.truncated_by_item_limit());
    assert_eq!(
        preview.issues().iter().next().expect("item issue").kind(),
        M3uImportIssueKind::ItemLimitExceeded
    );
}

#[test]
fn issue_storage_is_bounded_with_exact_omitted_accounting() {
    let limits = M3uParserLimits::new(256, 64, 8, 2).expect("valid limits");
    let preview = generic_preview(
        parse_m3u_document(M3uParseRequest::new(
            b"#EXTVLCOPT:a=1\n#EXTVLCOPT:b=2\n#EXTVLCOPT:c=3\n#EXTVLCOPT:d=4\n",
            M3uDocumentSource::local("/tmp/list.m3u"),
            M3uDeclaredFormat::M3u,
            limits,
        ))
        .expect("bounded issues"),
    );

    assert_eq!(preview.issues().retained_issue_count(), 2);
    assert_eq!(preview.issues().omitted_issue_count(), 2);
}

#[test]
fn invalid_utf8_is_fatal_without_lossy_replacement() {
    let error = parse_m3u_document(M3uParseRequest::new(
        &[0xff, b'\n'],
        M3uDocumentSource::local("/tmp/list.m3u"),
        M3uDeclaredFormat::M3u,
        M3uParserLimits::default(),
    ))
    .expect_err("invalid UTF-8");

    assert_eq!(error.kind(), M3uParseErrorKind::InvalidUtf8);
}
