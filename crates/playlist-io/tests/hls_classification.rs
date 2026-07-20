use playlist_io::{
    HlsManifestTopology, M3uDeclaredFormat, M3uDocument, M3uDocumentSource, M3uParseErrorKind,
    M3uParseRequest, M3uParserLimits, parse_m3u_document,
};

/// Разбирает network manifest с fixed secret-safe source.
fn parse_network_hls(manifest: &str) -> Result<M3uDocument, playlist_io::M3uParseError> {
    parse_m3u_document(M3uParseRequest::new(
        manifest.as_bytes(),
        M3uDocumentSource::network("https://example.invalid/live/master.m3u8?token=secret")
            .expect("valid source"),
        M3uDeclaredFormat::M3u8,
        M3uParserLimits::default(),
    ))
}

#[test]
fn master_hls_returns_adaptive_reference_and_never_segment_rows() {
    let document = parse_network_hls(
        "#EXTM3U\n\
         #EXT-X-VERSION:7\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1280000,CODECS=\"avc1.4d401f,mp4a.40.2\"\n\
         video/main.m3u8\n",
    )
    .expect("valid master");

    match document {
        M3uDocument::AdaptiveManifestReference(reference) => {
            assert_eq!(reference.topology(), HlsManifestTopology::Master);
            assert_eq!(
                reference.manifest_source().expose_network_uri(),
                Some("https://example.invalid/live/master.m3u8?token=secret")
            );
        }
        other => panic!("master HLS не должен стать rows: {other:?}"),
    }
}

#[test]
fn media_hls_returns_adaptive_reference_and_never_segment_rows() {
    let document = parse_network_hls(
        "#EXTM3U\r\n\
         #EXT-X-TARGETDURATION:10\r\n\
         #EXTINF:9.009,\r\n\
         segment-01.ts\r\n\
         #EXTINF:9.009,\r\n\
         https://cdn.example.invalid/segment-02.ts\r\n\
         #EXT-X-ENDLIST\r\n",
    )
    .expect("valid media playlist");

    match document {
        M3uDocument::AdaptiveManifestReference(reference) => {
            assert_eq!(reference.topology(), HlsManifestTopology::Media);
        }
        other => panic!("media HLS segment URI не должен стать rows: {other:?}"),
    }
}

#[test]
fn valid_local_hls_returns_typed_unsupported_outcome() {
    let document = parse_m3u_document(M3uParseRequest::new(
        b"#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXTINF:5,\nsegment.ts\n",
        M3uDocumentSource::local("/media/local.m3u8"),
        M3uDeclaredFormat::M3u8,
        M3uParserLimits::default(),
    ))
    .expect("valid local HLS classification");

    match document {
        M3uDocument::LocalHlsManifestUnsupported(unsupported) => {
            assert_eq!(unsupported.topology(), HlsManifestTopology::Media);
            assert_eq!(
                unsupported
                    .manifest_source()
                    .expose_local_path()
                    .expect("local source"),
                &std::path::PathBuf::from("/media/local.m3u8")
            );
        }
        other => panic!("ожидался typed local-HLS outcome: {other:?}"),
    }
}

#[test]
fn bom_is_rejected_for_hls_even_though_generic_dialect_warns() {
    let error =
        parse_network_hls("\u{feff}#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXTINF:5,\nsegment.ts\n")
            .expect_err("HLS BOM запрещён");

    assert_eq!(error.kind(), M3uParseErrorKind::HlsBomNotAllowed);
}

#[test]
fn mixed_master_and_media_topology_is_rejected_before_extinf_rows() {
    let error = parse_network_hls(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000\n\
         child.m3u8\n\
         #EXT-X-TARGETDURATION:5\n\
         #EXTINF:5,\n\
         segment.ts\n",
    )
    .expect_err("mixed topology invalid");

    assert_eq!(error.kind(), M3uParseErrorKind::HlsMixedTopology);
}

#[test]
fn non_nfc_and_control_characters_are_rejected() {
    let non_nfc = "#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXTINF:5,Cafe\u{301}\nsegment.ts\n";
    let non_nfc_error = parse_network_hls(non_nfc).expect_err("NFD text invalid");
    assert_eq!(non_nfc_error.kind(), M3uParseErrorKind::HlsNotNfc);

    let control_error =
        parse_network_hls("#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXTINF:5,\u{0007}\nsegment.ts\n")
            .expect_err("control char invalid");
    assert!(matches!(
        control_error.kind(),
        M3uParseErrorKind::HlsControlCharacter { line } if line.get() == 3
    ));
}

#[test]
fn tag_case_and_forbidden_whitespace_are_rejected() {
    let case_error =
        parse_network_hls("#EXTM3U\n#ext-x-targetduration:5\n#EXTINF:5,\nsegment.ts\n")
            .expect_err("tag names case-sensitive");
    assert!(matches!(
        case_error.kind(),
        M3uParseErrorKind::HlsInvalidTagCase { line } if line.get() == 2
    ));

    let whitespace_error = parse_network_hls(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH =1000\n\
         child.m3u8\n",
    )
    .expect_err("attribute whitespace invalid");
    assert!(matches!(
        whitespace_error.kind(),
        M3uParseErrorKind::HlsWhitespaceNotAllowed { line } if line.get() == 2
    ));

    let uri_whitespace_error =
        parse_network_hls("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\n child.m3u8\n")
            .expect_err("URI whitespace invalid");
    assert!(matches!(
        uri_whitespace_error.kind(),
        M3uParseErrorKind::HlsWhitespaceNotAllowed { line } if line.get() == 3
    ));
}

#[test]
fn duplicate_attribute_names_are_rejected() {
    let error = parse_network_hls(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000,BANDWIDTH=2000\n\
         child.m3u8\n",
    )
    .expect_err("duplicate attribute invalid");

    assert!(matches!(
        error.kind(),
        M3uParseErrorKind::HlsDuplicateAttribute { line } if line.get() == 2
    ));
}

#[test]
fn duplicate_singleton_tag_is_rejected() {
    let error = parse_network_hls(
        "#EXTM3U\n\
         #EXT-X-VERSION:7\n\
         #EXT-X-VERSION:7\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000\n\
         child.m3u8\n",
    )
    .expect_err("singleton tag duplicate invalid");

    assert!(matches!(
        error.kind(),
        M3uParseErrorKind::HlsDuplicateTag { line } if line.get() == 3
    ));
}

#[test]
fn duplicate_header_and_malformed_uri_attribute_are_rejected() {
    let duplicate_header_error = parse_network_hls(
        "#EXTM3U\n\
         #EXTM3U\n\
         #EXT-X-TARGETDURATION:5\n",
    )
    .expect_err("EXTM3U singleton");
    assert!(matches!(
        duplicate_header_error.kind(),
        M3uParseErrorKind::HlsDuplicateTag { line } if line.get() == 2
    ));

    let uri_attribute_error = parse_network_hls(
        "#EXTM3U\n\
         #EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=1000,URI=\"https://[invalid\"\n",
    )
    .expect_err("malformed attribute URI");
    assert!(matches!(
        uri_attribute_error.kind(),
        M3uParseErrorKind::HlsInvalidUri { line } if line.get() == 2
    ));
}

#[test]
fn empty_media_playlist_with_required_target_duration_is_classified() {
    let document = parse_network_hls(
        "#EXTM3U\n\
         #EXT-X-TARGETDURATION:5\n\
         #EXT-X-ENDLIST\n",
    )
    .expect("empty Media Playlist remains valid");

    match document {
        M3uDocument::AdaptiveManifestReference(reference) => {
            assert_eq!(reference.topology(), HlsManifestTopology::Media);
        }
        other => panic!("empty media HLS не должен стать generic rows: {other:?}"),
    }
}

#[test]
fn header_must_be_exact_physical_first_line() {
    let error = parse_network_hls("\n#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXTINF:5,\nsegment.ts\n")
        .expect_err("leading blank before header invalid");

    assert_eq!(error.kind(), M3uParseErrorKind::HlsMissingHeader);
}

#[test]
fn malformed_required_association_and_uri_are_rejected() {
    let missing_uri_error = parse_network_hls("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\n")
        .expect_err("STREAM-INF needs URI");
    assert!(matches!(
        missing_uri_error.kind(),
        M3uParseErrorKind::HlsInvalidRequiredStructure { line } if line.get() == 2
    ));

    let malformed_uri_error = parse_network_hls(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000\n\
         https://[invalid\n",
    )
    .expect_err("malformed URI invalid");
    assert!(matches!(
        malformed_uri_error.kind(),
        M3uParseErrorKind::HlsInvalidUri { line } if line.get() == 3
    ));
}
