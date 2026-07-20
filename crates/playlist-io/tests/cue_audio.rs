use std::path::Path;

use media_core::MediaTime;
use playlist_core::{LocalLocator, PlaylistImportSourceKind};
use playlist_io::{
    CueDocument, CueDocumentSource, CueExportIneligibility, CueFileTypeKind, CueParseErrorKind,
    CueParseRequest, CueParserLimits, CueTextEncoding, parse_cue_document,
};

fn parse_utf8(document: &str) -> Result<CueDocument, playlist_io::CueParseError> {
    parse_cue_document(CueParseRequest::new(
        document.as_bytes(),
        CueDocumentSource::local("/music/disc/album.cue"),
        CueParserLimits::default(),
    ))
}

fn span(document: &CueDocument, track_index: usize) -> playlist_core::PlaylistPlaybackSpan {
    document.tracks()[track_index]
        .import_draft()
        .playback_span()
        .expect("каждый CUE AUDIO track обязан иметь span")
}

#[test]
fn single_file_builds_id_less_audio_drafts_and_document_provenance() {
    let document = parse_utf8(
        r#"
TITLE "Album Case"
PERFORMER "Album Artist"
FILE "audio.flac" FLAC
  TRACK 03 AUDIO
    TITLE "Track Case"
    INDEX 01 00:00:00
"#,
    )
    .unwrap();

    assert_eq!(document.title(), Some("Album Case"));
    assert_eq!(document.performer(), Some("Album Artist"));
    assert_eq!(document.files().len(), 1);
    assert_eq!(document.tracks().len(), 1);
    assert_eq!(document.tracks()[0].number(), 3);
    assert_eq!(document.tracks()[0].title(), Some("Track Case"));
    assert_eq!(span(&document, 0).start(), MediaTime::ZERO);
    assert_eq!(span(&document, 0).end_exclusive(), None);

    let draft = document.tracks()[0].import_draft();
    assert_eq!(draft.cached_metadata().title(), Some("Track Case"));
    assert_eq!(draft.cached_metadata().album(), Some("Album Case"));
    assert_eq!(draft.cached_metadata().artists(), &["Album Artist"]);
    assert_eq!(
        draft.provenance().source_kind(),
        PlaylistImportSourceKind::Cue
    );
    assert_eq!(
        draft.provenance().source_ordinal().expect("ordinal").get(),
        1
    );
}

#[test]
fn commands_are_case_insensitive_but_metadata_and_file_tokens_preserve_case() {
    let document = parse_utf8(
        r#"
tItLe Mixed Album
pErFoRmEr "MiXeD Artist"
fIlE "Audio.WaV" wAvE
  tRaCk 07 aUdIo
    TiTlE "MiXeD Track"
    iNdEx 01 00:00:00
"#,
    )
    .unwrap();

    assert_eq!(document.title(), Some("Mixed Album"));
    assert_eq!(document.performer(), Some("MiXeD Artist"));
    assert_eq!(document.files()[0].declared_path(), "Audio.WaV");
    assert_eq!(document.files()[0].file_type().declared_token(), "wAvE");
    assert_eq!(
        document.files()[0].file_type().kind(),
        CueFileTypeKind::Wave
    );
    assert_eq!(document.tracks()[0].title(), Some("MiXeD Track"));
}

#[test]
fn first_track_may_start_above_one_but_following_numbers_are_strictly_sequential() {
    let valid = parse_utf8(
        r#"
FILE "audio.flac" FLAC
  TRACK 07 AUDIO
    INDEX 01 00:00:00
  TRACK 08 AUDIO
    INDEX 01 01:00:00
"#,
    )
    .unwrap();
    assert_eq!(
        valid
            .tracks()
            .iter()
            .map(|track| track.number())
            .collect::<Vec<_>>(),
        vec![7, 8]
    );

    for (actual, expected) in [(7, 8), (9, 8), (6, 8)] {
        let fixture = format!(
            "FILE \"audio.flac\" FLAC\n\
             TRACK 07 AUDIO\n\
             INDEX 01 00:00:00\n\
             TRACK {actual:02} AUDIO\n\
             INDEX 01 01:00:00\n"
        );
        assert!(matches!(
            parse_utf8(&fixture).unwrap_err().kind(),
            CueParseErrorKind::NonSequentialTrackNumber {
                expected: error_expected,
                actual: error_actual,
                ..
            } if *error_expected == expected && *error_actual == actual
        ));
    }
}

#[test]
fn track_number_range_and_overflow_are_rejected() {
    assert!(matches!(
        parse_utf8("FILE \"a.flac\" FLAC\nTRACK 00 AUDIO\nINDEX 01 00:00:00\n")
            .unwrap_err()
            .kind(),
        CueParseErrorKind::InvalidTrackNumber { .. }
    ));
    assert!(matches!(
        parse_utf8("FILE \"a.flac\" FLAC\nTRACK 100 AUDIO\nINDEX 01 00:00:00\n")
            .unwrap_err()
            .kind(),
        CueParseErrorKind::InvalidTrackNumber { .. }
    ));
}

#[test]
fn next_index01_owns_same_file_pregap_but_cross_file_boundary_stays_eof() {
    let same_file = parse_utf8(
        r#"
FILE "audio.flac" FLAC
  TRACK 01 AUDIO
    INDEX 01 00:10:00
  TRACK 02 AUDIO
    INDEX 00 00:59:00
    INDEX 01 01:00:00
"#,
    )
    .unwrap();
    assert_eq!(
        span(&same_file, 0).end_exclusive(),
        Some(MediaTime::from_secs(60))
    );
    assert_eq!(span(&same_file, 1).start(), MediaTime::from_secs(60));

    let cross_file = parse_utf8(
        r#"
FILE "disc-a.flac" FLAC
  TRACK 01 AUDIO
    INDEX 01 00:10:00
FILE "disc-b.flac" FLAC
  TRACK 02 AUDIO
    INDEX 00 00:00:00
    INDEX 01 00:03:00
"#,
    )
    .unwrap();
    assert_eq!(span(&cross_file, 0).end_exclusive(), None);
    assert_eq!(span(&cross_file, 1).start(), MediaTime::from_secs(3));
}

#[test]
fn first_track_htoa_is_retained_but_excluded_from_playback() {
    let document = parse_utf8(
        r#"
FILE "audio.flac" FLAC
  TRACK 01 AUDIO
    INDEX 00 00:00:00
    INDEX 01 00:02:00
"#,
    )
    .unwrap();

    assert_eq!(document.tracks()[0].indexes()[0].number(), 0);
    assert_eq!(span(&document, 0).start(), MediaTime::from_secs(2));
}

#[test]
fn last_track_uses_open_eof_end_and_missing_index01_is_rejected() {
    let document = parse_utf8(
        r#"
FILE "audio.flac" FLAC
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 01:00:00
"#,
    )
    .unwrap();
    assert_eq!(
        span(&document, 0).end_exclusive(),
        Some(MediaTime::from_secs(60))
    );
    assert_eq!(span(&document, 1).end_exclusive(), None);

    assert!(matches!(
        parse_utf8("FILE \"a.flac\" FLAC\nTRACK 01 AUDIO\nINDEX 00 00:00:00\n")
            .unwrap_err()
            .kind(),
        CueParseErrorKind::MissingIndex01 { track_number: 1 }
    ));
}

#[test]
fn supported_file_types_are_explicit_and_unsupported_types_fail_closed() {
    for (token, expected) in [
        ("WAVE", CueFileTypeKind::Wave),
        ("AIFF", CueFileTypeKind::Aiff),
        ("MP3", CueFileTypeKind::Mp3),
        ("FLAC", CueFileTypeKind::Flac),
    ] {
        let fixture = format!("FILE \"audio.bin\" {token}\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n");
        let document = parse_utf8(&fixture).unwrap();
        assert_eq!(document.files()[0].file_type().kind(), expected);
    }

    for token in ["BINARY", "MOTOROLA", "UNKNOWN"] {
        let fixture = format!("FILE \"audio.bin\" {token}\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n");
        assert!(matches!(
            parse_utf8(&fixture).unwrap_err().kind(),
            CueParseErrorKind::UnsupportedFileType { .. }
        ));
    }
}

#[test]
fn subindexes_are_retained_and_make_exact_export_ineligible() {
    let document = parse_utf8(
        r#"
FILE "audio.flac" FLAC
  TRACK 01 AUDIO
    INDEX 01 00:00:00
    INDEX 02 00:10:00
    INDEX 03 00:20:00
"#,
    )
    .unwrap();

    assert_eq!(
        document.tracks()[0]
            .indexes()
            .iter()
            .map(|index| index.number())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(!document.is_export_eligible());
    assert_eq!(
        document.export_ineligibilities(),
        &[
            CueExportIneligibility::RetainedSubIndex {
                track_number: 1,
                index_number: 2,
            },
            CueExportIneligibility::RetainedSubIndex {
                track_number: 1,
                index_number: 3,
            },
        ]
    );
}

#[test]
fn index_grammar_rejects_duplicate_gap_reverse_and_index00_after_start() {
    for indexes in [
        "INDEX 00 00:00:00\nINDEX 00 00:00:00",
        "INDEX 00 00:00:00\nINDEX 02 00:01:00",
        "INDEX 01 00:00:00\nINDEX 01 00:01:00",
        "INDEX 01 00:00:00\nINDEX 03 00:01:00",
        "INDEX 01 00:00:00\nINDEX 02 00:01:00\nINDEX 00 00:02:00",
    ] {
        let fixture = format!("FILE \"a.flac\" FLAC\nTRACK 01 AUDIO\n{indexes}\n");
        assert!(matches!(
            parse_utf8(&fixture).unwrap_err().kind(),
            CueParseErrorKind::InvalidIndexSequence { .. }
        ));
    }
}

#[test]
fn index01_may_be_first_but_index02_may_not() {
    parse_utf8("FILE \"a.flac\" FLAC\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n").unwrap();
    assert!(matches!(
        parse_utf8("FILE \"a.flac\" FLAC\nTRACK 01 AUDIO\nINDEX 02 00:00:00\n")
            .unwrap_err()
            .kind(),
        CueParseErrorKind::InvalidIndexSequence { .. }
    ));
    assert!(matches!(
        parse_utf8("FILE \"a.flac\" FLAC\nTRACK 01 AUDIO\nINDEX 100 00:00:00\n")
            .unwrap_err()
            .kind(),
        CueParseErrorKind::InvalidIndexNumber { .. }
    ));
}

#[test]
fn timestamps_must_not_decrease_inside_file_even_across_tracks() {
    let fixture = r#"
FILE "a.flac" FLAC
  TRACK 01 AUDIO
    INDEX 01 01:00:00
  TRACK 02 AUDIO
    INDEX 00 00:59:00
    INDEX 01 01:01:00
"#;
    assert!(matches!(
        parse_utf8(fixture).unwrap_err().kind(),
        CueParseErrorKind::TimestampMovedBackwards { .. }
    ));

    let equal_index00_and_index01 = r#"
FILE "a.flac" FLAC
  TRACK 01 AUDIO
    INDEX 00 00:01:00
    INDEX 01 00:01:00
"#;
    parse_utf8(equal_index00_and_index01).unwrap();
}

#[test]
fn equal_neighbor_track_starts_are_rejected_as_empty_domain_span() {
    let fixture = r#"
FILE "a.flac" FLAC
  TRACK 01 AUDIO
    INDEX 01 00:01:00
  TRACK 02 AUDIO
    INDEX 01 00:01:00
"#;
    assert_eq!(
        parse_utf8(fixture).unwrap_err().kind(),
        &CueParseErrorKind::EmptyPlaybackSpan { track_number: 1 }
    );
}

#[test]
fn data_tracks_are_rejected_and_never_become_drafts() {
    let error =
        parse_utf8("FILE \"disc.bin\" WAVE\nTRACK 01 MODE1/2352\nINDEX 01 00:00:00\n").unwrap_err();
    assert!(matches!(
        error.kind(),
        CueParseErrorKind::DataTrackUnsupported { declared_mode, .. }
            if declared_mode == "MODE1/2352"
    ));
}

#[test]
fn utf8_bom_and_bom_marked_utf16_are_supported_but_guessing_is_forbidden() {
    let cue = "FILE \"a.flac\" FLAC\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n";

    let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
    utf8_bom.extend_from_slice(cue.as_bytes());
    let utf8_document = parse_cue_document(CueParseRequest::new(
        &utf8_bom,
        CueDocumentSource::local("/music/a.cue"),
        CueParserLimits::default(),
    ))
    .unwrap();
    assert_eq!(utf8_document.encoding(), CueTextEncoding::Utf8WithBom);

    for (bom, little_endian, expected) in [
        (
            [0xFF, 0xFE],
            true,
            CueTextEncoding::Utf16LittleEndianWithBom,
        ),
        ([0xFE, 0xFF], false, CueTextEncoding::Utf16BigEndianWithBom),
    ] {
        let mut encoded = bom.to_vec();
        for unit in cue.encode_utf16() {
            let encoded_unit = if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            };
            encoded.extend_from_slice(&encoded_unit);
        }
        let document = parse_cue_document(CueParseRequest::new(
            &encoded,
            CueDocumentSource::local("/music/a.cue"),
            CueParserLimits::default(),
        ))
        .unwrap();
        assert_eq!(document.encoding(), expected);
    }

    let bomless_utf16 = cue
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    assert!(matches!(
        parse_cue_document(CueParseRequest::new(
            &bomless_utf16,
            CueDocumentSource::local("/music/a.cue"),
            CueParserLimits::default(),
        ))
        .unwrap_err()
        .kind(),
        CueParseErrorKind::UnsupportedOrInvalidEncoding
    ));
}

#[cfg(unix)]
#[test]
fn relative_file_resolution_preserves_non_utf_native_parent() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    let parent = PathBuf::from(OsString::from_vec(vec![
        b'/', b'm', b'u', b's', b'i', b'c', b'/', 0xFF,
    ]));
    let source = CueDocumentSource::local(parent.join("album.cue"));
    let document = parse_cue_document(CueParseRequest::new(
        b"FILE \"audio.flac\" FLAC\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n",
        source,
        CueParserLimits::default(),
    ))
    .unwrap();

    assert_eq!(
        document.files()[0]
            .resolved_locator()
            .expose_local_for_reopen(),
        Some(&LocalLocator::Native(parent.join("audio.flac")))
    );
}

#[test]
fn frame_arithmetic_keeps_75_fps_identity_and_rejects_overflow() {
    let document = parse_utf8("FILE \"a.flac\" FLAC\nTRACK 01 AUDIO\nINDEX 01 00:00:74\n").unwrap();
    let timestamp = document.tracks()[0].indexes()[0].timestamp();
    assert_eq!(timestamp.total_frames(), 74);
    assert_eq!(
        timestamp.media_time().as_duration().subsec_nanos(),
        986_666_666
    );

    let overflow = format!(
        "FILE \"a.flac\" FLAC\nTRACK 01 AUDIO\nINDEX 01 {}:00:00\n",
        u64::MAX
    );
    assert!(matches!(
        parse_utf8(&overflow).unwrap_err().kind(),
        CueParseErrorKind::TimestampOverflow { .. }
    ));
}

#[test]
fn unknown_command_is_retained_case_preserving_and_blocks_export() {
    let document = parse_utf8(
        r#"
REM Keep This Case
FILE "a.flac" FLAC
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
    )
    .unwrap();

    assert_eq!(document.unknown_commands().len(), 1);
    assert_eq!(document.unknown_commands()[0].command(), "REM");
    assert_eq!(document.unknown_commands()[0].arguments(), "Keep This Case");
    assert!(!document.is_export_eligible());
    assert!(matches!(
        document.export_ineligibilities(),
        [CueExportIneligibility::UnknownCommand { .. }]
    ));
}

#[test]
fn explicit_budgets_reject_document_line_file_unknown_and_retained_text_overruns() {
    let source = || CueDocumentSource::local("/music/a.cue");
    let parse_with =
        |bytes: &[u8], limits| parse_cue_document(CueParseRequest::new(bytes, source(), limits));

    let tiny_document = CueParserLimits::new(1, 64, 1, 1, 64).unwrap();
    assert!(matches!(
        parse_with(b"FILE", tiny_document).unwrap_err().kind(),
        CueParseErrorKind::DocumentLimitExceeded
    ));

    let tiny_line = CueParserLimits::new(1024, 4, 1, 1, 64).unwrap();
    assert!(matches!(
        parse_with(b"FILE \"a\" FLAC", tiny_line)
            .unwrap_err()
            .kind(),
        CueParseErrorKind::LineLimitExceeded { .. }
    ));

    let one_file = CueParserLimits::new(1024, 128, 1, 4, 128).unwrap();
    assert!(matches!(
        parse_with(
            b"FILE \"a\" FLAC\nFILE \"b\" FLAC\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n",
            one_file
        )
        .unwrap_err()
        .kind(),
        CueParseErrorKind::FileLimitExceeded { .. }
    ));

    let one_unknown = CueParserLimits::new(1024, 128, 1, 1, 128).unwrap();
    assert!(matches!(
        parse_with(
            b"REM one\nREM two\nFILE \"a\" FLAC\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n",
            one_unknown
        )
        .unwrap_err()
        .kind(),
        CueParseErrorKind::UnknownCommandLimitExceeded { .. }
    ));

    let tiny_text = CueParserLimits::new(1024, 128, 1, 4, 3).unwrap();
    assert!(matches!(
        parse_with(
            b"TITLE long\nFILE \"a\" FLAC\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n",
            tiny_text
        )
        .unwrap_err()
        .kind(),
        CueParseErrorKind::RetainedTextLimitExceeded { .. }
    ));
}

#[test]
fn relative_and_absolute_file_paths_resolve_without_io() {
    let relative =
        parse_utf8("FILE \"../audio.flac\" FLAC\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n").unwrap();
    assert_eq!(
        relative.files()[0]
            .resolved_locator()
            .expose_local_for_reopen(),
        Some(&LocalLocator::Native(
            Path::new("/music/disc").join("../audio.flac")
        ))
    );

    let absolute =
        parse_utf8("FILE \"/archive/audio.flac\" FLAC\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n")
            .unwrap();
    assert_eq!(
        absolute.files()[0]
            .resolved_locator()
            .expose_local_for_reopen(),
        Some(&LocalLocator::Native(
            Path::new("/archive/audio.flac").to_path_buf()
        ))
    );
}
