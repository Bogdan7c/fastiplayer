//! Unit-тесты нормализации transport/container/codec identity (вынесены из `normalized.rs` по лимиту 800 строк).

use super::*;

/// Unknown identity остаётся exact и не протекает в Debug.
#[test]
fn raw_identity_is_preserved_with_bounds_and_redaction() {
    let raw_text = "Future_Protocol.V2";
    let raw = RawTransportIdentity::new(raw_text).expect("raw identity valid");
    let normalized = NormalizedTransport::parse(raw);

    assert_eq!(normalized.raw().as_str(), raw_text);
    assert_eq!(normalized.family(), TransportFamily::Unknown);
    assert!(!format!("{normalized:?}").contains(raw_text));

    let error = RawCodecIdentity::new("x".repeat(MAX_RAW_IDENTITY_UTF8_BYTES + 1))
        .expect_err("overflow must be rejected");
    assert_eq!(
        error,
        RawIdentityBuildError::TooLong {
            field: RawIdentityField::Codec,
            provided_bytes: MAX_RAW_IDENTITY_UTF8_BYTES + 1,
            maximum_bytes: MAX_RAW_IDENTITY_UTF8_BYTES,
        }
    );
}

/// Protocol aliases нормализуются по S00 manifest.
#[test]
fn transport_aliases_map_to_manifest_families() {
    // HLS alias обязан остаться в concrete adaptive family.
    let hls = NormalizedTransport::parse(
        RawTransportIdentity::new("m3u8_native").expect("identity valid"),
    );
    // DASH alias обязан остаться в concrete adaptive family.
    let dash = NormalizedTransport::parse(
        RawTransportIdentity::new("dash_frag_urls").expect("identity valid"),
    );
    // Generator identity хранится отдельно от serializable DASH fragments.
    let generator = NormalizedTransport::parse(
        RawTransportIdentity::new("http_dash_segments_generator").expect("identity valid"),
    );

    // Known HLS alias не теряет family semantics.
    assert_eq!(hls.family(), TransportFamily::Hls);
    // Known DASH alias не теряет family semantics.
    assert_eq!(dash.family(), TransportFamily::Dash);
    // Generator остаётся typed exclusion без generic provider fallback.
    assert_eq!(
        generator.family(),
        TransportFamily::KnownExcluded(KnownExcludedTransport::DashGenerator)
    );

    // Каждая special identity обязана оставаться exact member-ом одной excluded family.
    for special_identity in [
        "bunnycdn",
        "soopvod",
        "niconico_live",
        "fc2_live",
        "websocket_frag",
    ] {
        // Парсим exact raw identity без alias normalization между providers.
        let special_transport = NormalizedTransport::parse(
            RawTransportIdentity::new(special_identity).expect("identity valid"),
        );
        // Serializable protocol string не является admission воспроизводимого provider-а.
        assert_eq!(
            special_transport.family(),
            TransportFamily::KnownExcluded(KnownExcludedTransport::PrivateLiveState),
            "Special identity {special_identity} unexpectedly received by a family provider"
        );
    }
}

/// Codec parser отделяет family от exact parameters.
#[test]
fn codec_family_and_parameters_are_parsed_without_raw_loss() {
    let avc =
        NormalizedCodec::parse(RawCodecIdentity::new("avc1.640028").expect("codec identity valid"));
    let aac =
        NormalizedCodec::parse(RawCodecIdentity::new("mp4a.40.2").expect("codec identity valid"));
    let future = NormalizedCodec::parse(
        RawCodecIdentity::new("futureCodec.Profile-X").expect("codec identity valid"),
    );

    assert_eq!(avc.kind(), CodecKind::Known(CodecFamily::H264));
    assert_eq!(avc.parameters().collect::<Vec<_>>(), vec!["640028"]);
    assert_eq!(aac.kind(), CodecKind::Known(CodecFamily::Aac));
    assert_eq!(aac.parameters().collect::<Vec<_>>(), vec!["40", "2"]);
    assert_eq!(future.kind(), CodecKind::Unknown);
    assert_eq!(future.raw().as_str(), "futureCodec.Profile-X");
    assert_eq!(future.parameters().collect::<Vec<_>>(), vec!["Profile-X"]);
}

/// Runtime container ids нормализуются тем же owner-ом, что extractor codec names.
#[test]
fn container_codec_ids_map_to_public_codec_families_without_raw_loss() {
    let cases = [
        ("V_VP9", CodecFamily::Vp9),
        ("V_MPEG4/ISO/AVC", CodecFamily::H264),
        ("A_OPUS", CodecFamily::Opus),
        ("A_VORBIS", CodecFamily::Vorbis),
        ("A_AAC", CodecFamily::Aac),
        ("A_AAC/MPEG4/LC", CodecFamily::Aac),
        ("AACL", CodecFamily::Aac),
        ("A_FLAC", CodecFamily::Flac),
        ("A_PCM_S16LE", CodecFamily::Pcm),
    ];

    for (raw_codec, expected_family) in cases {
        let codec = NormalizedCodec::parse(
            RawCodecIdentity::new(raw_codec).expect("container codec identity valid"),
        );
        assert_eq!(codec.kind(), CodecKind::Known(expected_family));
        assert_eq!(codec.raw().as_str(), raw_codec);
    }
}

/// Конфликт известных ext/container hints не разрешается угадыванием.
#[test]
fn container_hint_conflict_is_typed() {
    let identity = ContainerIdentity::parse(
        Some(RawExtensionIdentity::new("webm").expect("ext is valid")),
        Some(RawContainerIdentity::new("mp4").expect("container valid")),
    );

    assert_eq!(
        identity.consistent_family(),
        Err(ContainerHintConflict {
            extension: ContainerFamily::WebM,
            container: ContainerFamily::IsoBmff,
        })
    );
}

/// Более точный container hint не конфликтует с совместимым file extension.
#[test]
fn container_refinement_pairs_resolve_to_the_more_specific_family() {
    let cases = [
        ("mp4", "mp4_dash", ContainerFamily::FragmentedIsoBmff),
        ("mkv", "webm", ContainerFamily::WebM),
        ("flv", "f4f", ContainerFamily::F4f),
    ];

    for (extension, container, expected) in cases {
        let identity = ContainerIdentity::parse(
            Some(RawExtensionIdentity::new(extension).expect("ext is valid")),
            Some(RawContainerIdentity::new(container).expect("container valid")),
        );

        assert_eq!(identity.consistent_family(), Ok(Some(expected)));
    }
}

/// F4V — ISO-BMFF identity, тогда как F4F остаётся отдельным Adobe fragment path.
#[test]
fn f4v_routes_to_iso_bmff_without_collapsing_f4f_into_flv() {
    let f4v = ContainerIdentity::parse(
        Some(RawExtensionIdentity::new("f4v").expect("ext is valid")),
        None,
    );
    let f4f = ContainerIdentity::parse(
        Some(RawExtensionIdentity::new("f4f").expect("ext is valid")),
        None,
    );

    assert_eq!(f4v.extension_family(), Some(ContainerFamily::IsoBmff));
    assert_eq!(f4v.consistent_family(), Ok(Some(ContainerFamily::IsoBmff)));
    assert_eq!(f4f.extension_family(), Some(ContainerFamily::F4f));
    assert_eq!(f4f.consistent_family(), Ok(Some(ContainerFamily::F4f)));
}

/// yt-dlp Smooth Streaming extensions обозначают fragmented ISO-BMFF tracks.
#[test]
fn isma_and_ismv_route_to_fragmented_iso_bmff() {
    for extension in ["isma", "ismv"] {
        let identity = ContainerIdentity::parse(
            Some(RawExtensionIdentity::new(extension).expect("ext is valid")),
            None,
        );
        assert_eq!(
            identity.consistent_family(),
            Ok(Some(ContainerFamily::FragmentedIsoBmff))
        );
    }
}

/// AC-3 и E-AC-3 — две разные exact family во всех словарях (manifest, yt-dlp, demux).
#[test]
fn dolby_digital_spellings_map_to_distinct_exact_families() {
    let cases = [
        ("ac-3", CodecKind::Known(CodecFamily::Ac3)),
        ("AC-3", CodecKind::Known(CodecFamily::Ac3)),
        ("ac3", CodecKind::Known(CodecFamily::Ac3)),
        ("A_AC3", CodecKind::Known(CodecFamily::Ac3)),
        ("ec-3", CodecKind::Known(CodecFamily::Eac3)),
        ("ec+3", CodecKind::Known(CodecFamily::Eac3)),
        ("eac3", CodecKind::Known(CodecFamily::Eac3)),
        ("A_EAC3", CodecKind::Known(CodecFamily::Eac3)),
        // Соседние, но другие кодеки не подменяются wildcard-ом.
        ("ac-4", CodecKind::Unknown),
        ("A_AC3/BSID9", CodecKind::Unknown),
        ("A_TRUEHD", CodecKind::Unknown),
        ("dtsc", CodecKind::Unknown),
    ];

    for (raw_codec, expected_kind) in cases {
        let codec =
            NormalizedCodec::parse(RawCodecIdentity::new(raw_codec).expect("codec identity valid"));
        assert_eq!(codec.kind(), expected_kind, "codec {raw_codec}");
        assert_eq!(codec.raw().as_str(), raw_codec);
    }
    assert_eq!(CodecFamily::Ac3.media_kind(), CodecMediaKind::Audio);
    assert_eq!(CodecFamily::Eac3.media_kind(), CodecMediaKind::Audio);
}

/// Сырые Dolby-расширения — отдельная exact family, а не MPEG audio и не ISO-BMFF;
/// контейнер ISO-BMFF рядом с `ac3`-расширением остаётся typed-конфликтом.
#[test]
fn raw_dolby_extensions_route_to_ac3_elementary_family() {
    for extension in ["ac3", "eac3", "ec3", "EC3"] {
        let identity = ContainerIdentity::parse(
            Some(RawExtensionIdentity::new(extension).expect("ext is valid")),
            None,
        );
        assert_eq!(
            identity.consistent_family(),
            Ok(Some(ContainerFamily::Ac3Elementary)),
            "{extension}"
        );
    }
    let conflicting = ContainerIdentity::parse(
        Some(RawExtensionIdentity::new("ac3").expect("ext is valid")),
        Some(RawContainerIdentity::new("mp4").expect("container valid")),
    );
    assert_eq!(
        conflicting.consistent_family(),
        Err(ContainerHintConflict {
            extension: ContainerFamily::Ac3Elementary,
            container: ContainerFamily::IsoBmff,
        })
    );
}

/// Separate-пара открывается только одним runtime-маршрутом: смешанная HLS+HTTPS
/// (реальный YouTube `616+251`) маршрута не имеет, а progressive HTTP+FTP — имеет.
#[test]
fn component_composition_route_rejects_mixed_adaptive_and_progressive_pairs() {
    let https = TransportFamily::ProgressiveHttp(HttpScheme::Https);
    let ftp = TransportFamily::ProgressiveFtp(FtpScheme::Ftp);

    assert!(https.shares_component_composition_route(https));
    assert!(https.shares_component_composition_route(ftp));
    assert!(TransportFamily::Hls.shares_component_composition_route(TransportFamily::Hls));
    assert!(TransportFamily::Dash.shares_component_composition_route(TransportFamily::Dash));

    assert!(!TransportFamily::Hls.shares_component_composition_route(https));
    assert!(!https.shares_component_composition_route(TransportFamily::Hls));
    assert!(!TransportFamily::Dash.shares_component_composition_route(ftp));
    assert!(!TransportFamily::Hls.shares_component_composition_route(TransportFamily::Dash));
    assert!(
        !TransportFamily::Rtmp(RtmpVariant::Rtmp)
            .shares_component_composition_route(TransportFamily::Rtmp(RtmpVariant::Rtmpe))
    );
    assert!(!TransportFamily::Unknown.shares_component_composition_route(TransportFamily::Unknown));
}
