use hls_playlist_core::{
    HlsKeyMethod, HlsParseErrorKind, HlsParseRequest, HlsParserLimits, HlsPlaylist,
    HlsProfileError, MediaContainerIntent, is_hls_candidate, parse_hls_playlist,
    validate_initial_profile, validate_vod_profile,
};

fn parse(text: &str) -> Result<HlsPlaylist, hls_playlist_core::HlsParseError> {
    parse_hls_playlist(HlsParseRequest::new(
        text.as_bytes(),
        Some("https://media.example.invalid/path/master.m3u8?secret=yes"),
        HlsParserLimits::default(),
    ))
}

#[test]
fn master_keeps_variants_audio_and_subtitle_descriptors() {
    let playlist = parse(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"English\",URI=\"audio.m3u8\"\n\
         #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"UA\",URI=\"subs.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000,RESOLUTION=1280x720,AUDIO=\"audio\",SUBTITLES=\"subs\"\n\
         video.m3u8\n",
    )
    .expect("valid master");
    let HlsPlaylist::Master(master) = playlist else {
        panic!("expected master");
    };
    assert_eq!(master.variants.len(), 1);
    assert_eq!(master.renditions.len(), 2);
}

#[test]
fn audio_channels_keeps_raw_descriptor_and_parses_primary_count() {
    let playlist = parse(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Atmos\",URI=\"audio.m3u8\",CHANNELS=\"2/JOC\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"audio\"\n\
         video.m3u8\n",
    )
    .expect("valid master with an RFC 8216 CHANNELS descriptor");
    let HlsPlaylist::Master(master) = playlist else {
        panic!("expected master");
    };
    let rendition = master.renditions.first().expect("one audio rendition");
    assert_eq!(rendition.channels.as_deref(), Some("2/JOC"));
    assert_eq!(
        rendition.channel_count.map(std::num::NonZeroU64::get),
        Some(2)
    );
}

#[test]
fn channels_rejects_malformed_descriptor_and_non_audio_owner() {
    assert!(matches!(
        parse(
            "#EXTM3U\n\
             #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Broken\",URI=\"audio.m3u8\",CHANNELS=\"2/\"\n\
             #EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"audio\"\n\
             video.m3u8\n"
        )
        .unwrap_err()
        .kind(),
        HlsParseErrorKind::InvalidTagSyntax { .. }
    ));
    assert!(matches!(
        parse(
            "#EXTM3U\n\
             #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"UA\",URI=\"subs.m3u8\",CHANNELS=\"2\"\n\
             #EXT-X-STREAM-INF:BANDWIDTH=1000,SUBTITLES=\"subs\"\n\
             video.m3u8\n"
        )
        .unwrap_err()
        .kind(),
        HlsParseErrorKind::InvalidRequiredStructure { .. }
    ));
}

#[test]
fn media_preserves_ranges_map_discontinuity_and_key_rotation() {
    let playlist = parse(
        "#EXTM3U\n\
         #EXT-X-TARGETDURATION:6\n\
         #EXT-X-MEDIA-SEQUENCE:42\n\
         #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n\
         #EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"100@0\"\n\
         #EXTINF:6.0,first\n\
         #EXT-X-BYTERANGE:200@100\n\
         media.mp4\n\
         #EXT-X-DISCONTINUITY\n\
         #EXT-X-KEY:METHOD=NONE\n\
         #EXTINF:6.0,second\n\
         media2.mp4\n\
         #EXT-X-ENDLIST\n",
    )
    .expect("valid media");
    let HlsPlaylist::Media(media) = &playlist else {
        panic!("expected media");
    };
    assert_eq!(media.segments[0].media_sequence, 42);
    assert!(matches!(
        media.segments[0].key.as_ref().map(|key| &key.method),
        Some(HlsKeyMethod::Aes128)
    ));
    assert!(media.segments[0].initialization_map.is_some());
    assert!(media.segments[1].discontinuity);
    assert!(media.segments[1].key.is_none());
    validate_vod_profile(&playlist, Some(MediaContainerIntent::FragmentedMp4))
        .expect("initial fMP4 VOD profile");
}

#[test]
fn profile_rejects_non_vod_unsupported_crypto_and_ll_hls() {
    let non_vod = parse("#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXTINF:5,\nsegment.ts\n").unwrap();
    assert_eq!(
        validate_vod_profile(&non_vod, None),
        Err(HlsProfileError::NonVod)
    );

    let sample_aes = parse(
        "#EXTM3U\n#EXT-X-TARGETDURATION:5\n\
         #EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"key\"\n\
         #EXTINF:5,\nsegment.ts\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    assert_eq!(
        validate_vod_profile(&sample_aes, None),
        Err(HlsProfileError::UnsupportedEncryptionMethod)
    );

    let low_latency = parse(
        "#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXT-X-PART-INF:PART-TARGET=1\n\
         #EXTINF:5,\nsegment.ts\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    assert_eq!(
        validate_vod_profile(&low_latency, None),
        Err(HlsProfileError::LowLatencySemantics)
    );
}

#[test]
fn malformed_text_structure_and_budgets_are_typed() {
    assert!(is_hls_candidate("#ext-x-targetduration:5"));
    let bom = parse("\u{feff}#EXTM3U\n#EXT-X-TARGETDURATION:5\n").unwrap_err();
    assert_eq!(bom.kind(), HlsParseErrorKind::BomNotAllowed);
    let mixed = parse(
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nchild.m3u8\n\
         #EXT-X-TARGETDURATION:5\n",
    )
    .unwrap_err();
    assert_eq!(mixed.kind(), HlsParseErrorKind::MixedTopology);
    let delete_control =
        parse("#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXTINF:5,\u{7f}\nsegment.ts\n").unwrap_err();
    assert!(matches!(
        delete_control.kind(),
        HlsParseErrorKind::ControlCharacter { line } if line.get() == 3
    ));
}

#[test]
fn unknown_valid_tags_and_attributes_are_ignored() {
    let playlist = parse(
        "#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXT-X-FUTURE:VALUE\n\
         #EXTINF:5,FUTURE-ATTR=\"kept opaque\"\nsegment.ts\n#EXT-X-ENDLIST\n",
    )
    .expect("unknown valid tag ignored");
    assert!(matches!(playlist, HlsPlaylist::Media(_)));

    let master = parse(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,FUTURE-ATTRIBUTE=\"opaque\"\nchild.m3u8\n",
    )
    .expect("unknown attribute on supported tag ignored");
    validate_initial_profile(&master).expect("ordinary variant remains supported");
}

#[test]
fn profile_rejects_unsupported_key_even_without_segments_and_in_master_session_key() {
    let empty_media = parse(
        "#EXTM3U\n#EXT-X-TARGETDURATION:5\n\
         #EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"key\"\n#EXT-X-ENDLIST\n",
    )
    .expect("structurally valid");
    assert_eq!(
        validate_vod_profile(&empty_media, None),
        Err(HlsProfileError::UnsupportedEncryptionMethod)
    );

    let master = parse(
        "#EXTM3U\n\
         #EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"key\",KEYFORMAT=\"drm.example\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1\nchild.m3u8\n",
    )
    .expect("structurally valid master");
    assert_eq!(
        validate_initial_profile(&master),
        Err(HlsProfileError::UnsupportedKeyFormat)
    );
}

#[test]
fn pending_segment_tags_are_single_use_and_must_reach_a_segment() {
    for malformed in [
        "#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXTINF:5,\n\
         #EXT-X-BYTERANGE:10@0\n#EXT-X-BYTERANGE:10@10\nsegment.ts\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXT-X-BYTERANGE:10@0\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXT-X-DISCONTINUITY\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXT-X-DISCONTINUITY\n\
         #EXT-X-DISCONTINUITY\n#EXTINF:5,\nsegment.ts\n",
    ] {
        assert!(matches!(
            parse(malformed).unwrap_err().kind(),
            HlsParseErrorKind::InvalidRequiredStructure { .. }
        ));
    }
}

#[test]
fn endlist_position_is_not_a_lexical_end_marker_per_rfc_8216() {
    let playlist = parse(
        "#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXT-X-ENDLIST\n\
         #EXTINF:5,\nsegment.ts\n",
    )
    .expect("RFC permits ENDLIST anywhere in the media playlist");
    validate_vod_profile(&playlist, Some(MediaContainerIntent::TransportStream))
        .expect("manifest is immutable VOD despite tag position");
}

#[test]
fn target_duration_is_positive_and_uses_exact_rfc_rounding() {
    assert!(matches!(
        parse("#EXTM3U\n#EXT-X-TARGETDURATION:0\n")
            .unwrap_err()
            .kind(),
        HlsParseErrorKind::InvalidRequiredStructure { .. }
    ));
    parse(
        "#EXTM3U\n#EXT-X-TARGETDURATION:5\n\
         #EXTINF:5.499999999999999999,\nsegment.ts\n",
    )
    .expect("rounds to target without float precision loss");
    assert!(matches!(
        parse(
            "#EXTM3U\n#EXT-X-TARGETDURATION:5\n\
             #EXTINF:5.5,\nsegment.ts\n"
        )
        .unwrap_err()
        .kind(),
        HlsParseErrorKind::InvalidRequiredStructure { .. }
    ));
    assert!(matches!(
        parse(
            "#EXTM3U\n#EXT-X-TARGETDURATION:18446744073709551615\n\
             #EXTINF:184467440737095516160.0,\nsegment.ts\n"
        )
        .unwrap_err()
        .kind(),
        HlsParseErrorKind::InvalidRequiredStructure { .. }
    ));
}

#[test]
fn rendition_required_invariants_and_group_references_are_structural() {
    for malformed in [
        "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"x\",FORCED=NO\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"a\"\nchild.m3u8\n",
        "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"x\",DEFAULT=YES,AUTOSELECT=NO\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"a\"\nchild.m3u8\n",
        "#EXTM3U\n#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"s\",NAME=\"x\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,SUBTITLES=\"s\"\nchild.m3u8\n",
        "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"x\"\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"x\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"a\"\nchild.m3u8\n",
        "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"x\",DEFAULT=YES\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"y\",DEFAULT=YES\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"a\"\nchild.m3u8\n",
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"missing\"\nchild.m3u8\n",
        "#EXTM3U\n#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"shared\",NAME=\"x\",URI=\"s.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"shared\"\nchild.m3u8\n",
        "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a1\",NAME=\"English\",LANGUAGE=\"en\"\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a2\",NAME=\"Deutsch\",LANGUAGE=\"de\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"a1\"\nchild1.m3u8\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"a2\"\nchild2.m3u8\n",
    ] {
        assert!(matches!(
            parse(malformed).unwrap_err().kind(),
            HlsParseErrorKind::InvalidRequiredStructure { .. }
        ));
    }
    assert!(matches!(
        parse(
            "#EXTM3U\n#EXT-X-MEDIA:TYPE=FUTURE,GROUP-ID=\"a\",NAME=\"x\"\n\
             #EXT-X-STREAM-INF:BANDWIDTH=1\nchild.m3u8\n"
        )
        .unwrap_err()
        .kind(),
        HlsParseErrorKind::InvalidTagSyntax { .. }
    ));
}

#[test]
fn known_unsupported_master_semantics_are_profile_rejections_only() {
    let cases = [
        (
            "#EXTM3U\n#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=1,URI=\"iframe.m3u8\"\n",
            HlsProfileError::IFrameVariant,
        ),
        (
            "#EXTM3U\n#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"key\"\n\
             #EXT-X-STREAM-INF:BANDWIDTH=1\nchild.m3u8\n",
            HlsProfileError::SessionKey,
        ),
        (
            "#EXTM3U\n#EXT-X-MEDIA:TYPE=VIDEO,GROUP-ID=\"v\",NAME=\"camera\",URI=\"v.m3u8\"\n\
             #EXT-X-STREAM-INF:BANDWIDTH=1,VIDEO=\"v\"\nchild.m3u8\n",
            HlsProfileError::VideoRendition,
        ),
        (
            "#EXTM3U\n#EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID=\"cc\",NAME=\"CC\",INSTREAM-ID=\"CC1\"\n\
             #EXT-X-STREAM-INF:BANDWIDTH=1,CLOSED-CAPTIONS=\"cc\"\nchild.m3u8\n",
            HlsProfileError::ClosedCaptions,
        ),
        (
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,HDCP-LEVEL=TYPE-0\nchild.m3u8\n",
            HlsProfileError::OutputProtection,
        ),
        (
            "#EXTM3U\n#EXT-X-VERSION:8\n\
             #EXT-X-STREAM-INF:BANDWIDTH=1\nchild.m3u8\n",
            HlsProfileError::UnsupportedProtocolVersion,
        ),
    ];
    for (manifest, expected) in cases {
        let playlist = parse(manifest).expect("valid RFC structure");
        assert_eq!(validate_initial_profile(&playlist), Err(expected));
    }
}

#[test]
fn safe_session_metadata_is_validated_but_not_a_profile_feature() {
    let playlist = parse(
        "#EXTM3U\n\
         #EXT-X-SESSION-DATA:DATA-ID=\"example.title\",VALUE=\"Title\",FUTURE=\"opaque\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1\nchild.m3u8\n",
    )
    .expect("valid session metadata");
    validate_initial_profile(&playlist).expect("session data does not affect playback");

    for malformed in [
        "#EXTM3U\n#EXT-X-SESSION-DATA:VALUE=\"Title\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1\nchild.m3u8\n",
        "#EXTM3U\n#EXT-X-SESSION-DATA:DATA-ID=\"id\",VALUE=\"x\",URI=\"meta.json\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1\nchild.m3u8\n",
        "#EXTM3U\n#EXT-X-SESSION-DATA:DATA-ID=\"id\",VALUE=\"x\"\n\
         #EXT-X-SESSION-DATA:DATA-ID=\"id\",VALUE=\"y\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1\nchild.m3u8\n",
    ] {
        assert!(matches!(
            parse(malformed).unwrap_err().kind(),
            HlsParseErrorKind::InvalidRequiredStructure { .. }
        ));
    }
}

#[test]
fn start_and_event_are_precise_vod_profile_rejections() {
    let start = parse(
        "#EXTM3U\n#EXT-X-START:TIME-OFFSET=1.5\n#EXT-X-TARGETDURATION:5\n\
         #EXTINF:5,\nsegment.ts\n#EXT-X-ENDLIST\n",
    )
    .expect("valid start structure");
    assert_eq!(
        validate_vod_profile(&start, None),
        Err(HlsProfileError::StartOffset)
    );

    let event = parse(
        "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXT-X-TARGETDURATION:5\n\
         #EXTINF:5,\nsegment.ts\n#EXT-X-ENDLIST\n",
    )
    .expect("valid event structure");
    assert_eq!(
        validate_vod_profile(&event, None),
        Err(HlsProfileError::EventPlaylist)
    );
}
