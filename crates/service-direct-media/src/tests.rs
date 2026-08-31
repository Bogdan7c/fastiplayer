use demux_api::DemuxRegistry;
use symphonia_demux::{DemuxerOptions, SymphoniaDemuxFactory};
use web_media_transport_api::TransportRequestTarget;

use super::{DirectMediaOpenError, DirectMediaUrlUnsupportedReason, parse_direct_media_url};

/// Собирает реальные Symphonia registrations без network side effects.
fn symphonia_registry() -> DemuxRegistry {
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("Symphonia descriptor"),
        ))
        .expect("Symphonia registry");
    registry
}

/// HTTP Ogg/WebM и FTP Ogg классифицируются по одному registry capability source.
#[test]
fn production_demux_extensions_classify_http_and_ftp_resources_directly() {
    let registry = symphonia_registry();
    for locator in [
        "https://media.example.test/audio.ogg",
        "https://media.example.test/video.webm",
        "ftp://media.example.test/audio.ogg",
        "ftps://media.example.test/audio.OPUS",
    ] {
        let direct = parse_direct_media_url(locator, &registry)
            .unwrap_or_else(|error| panic!("{locator} должен быть direct: {error}"));
        assert!(matches!(
            direct.request_target_for_open(),
            TransportRequestTarget::Http(_) | TransportRequestTarget::Ftp(_)
        ));
    }
}

/// Пустой registry отклоняет даже исторически разрешённый MP4 extension.
#[test]
fn extension_support_follows_registry_instead_of_service_allowlist() {
    let empty_registry = DemuxRegistry::new();
    let error = parse_direct_media_url("https://media.example.test/video.mp4", &empty_registry)
        .expect_err("пустой registry не должен наследовать старый MP4 allowlist");
    assert!(matches!(
        error,
        DirectMediaOpenError::UnsupportedUrl {
            reason: DirectMediaUrlUnsupportedReason::UnsupportedExtension,
        }
    ));
}

/// FTP credentials/query сохраняются exact для reopen, но не попадают в diagnostics.
#[test]
fn ftp_credentials_query_and_path_are_redacted_from_all_safe_projections() {
    let registry = symphonia_registry();
    let secret = "ftp://user:password@media.example.test/private/audio.ogg?token=secret";
    let direct = parse_direct_media_url(secret, &registry).expect("FTP Ogg direct locator");

    assert_eq!(direct.expose_secret_for_open(), secret);
    assert!(direct.requires_sensitive_persistence_acknowledgement());
    assert!(matches!(
        direct.request_target_for_open(),
        TransportRequestTarget::Ftp(_)
    ));
    let safe = format!("{direct:?} {direct}");
    for forbidden in ["user", "password", "private", "token", "secret"] {
        assert!(
            !safe.contains(forbidden),
            "safe projection раскрыл {forbidden}"
        );
    }
}

/// Query/fragment никогда не участвуют в extension classification.
#[test]
fn extension_is_read_only_from_url_path() {
    let registry = symphonia_registry();
    let direct = parse_direct_media_url(
        "https://media.example.test/audio.OGG?format=mp4#video.webm",
        &registry,
    )
    .expect("path Ogg должен победить query/fragment");
    assert_eq!(direct.extension().as_extension_hint(), "ogg");
}

/// Manifest и unsupported protocol остаются typed pre-I/O rejections.
#[test]
fn manifest_and_unrelated_protocol_keep_distinct_rejections() {
    let registry = symphonia_registry();
    let manifest = parse_direct_media_url("https://media.example.test/live.m3u8", &registry)
        .expect_err("manifest не является progressive resource");
    assert!(matches!(
        manifest,
        DirectMediaOpenError::UnsupportedUrl {
            reason: DirectMediaUrlUnsupportedReason::ManifestUnsupported,
        }
    ));

    let unrelated = parse_direct_media_url("rtsp://media.example.test/video.webm", &registry)
        .expect_err("RTSP не принадлежит direct progressive ingress");
    assert!(matches!(
        unrelated,
        DirectMediaOpenError::UnsupportedUrl {
            reason: DirectMediaUrlUnsupportedReason::UnsupportedProtocol,
        }
    ));
}

/// Invalid FTP command text отклоняется до network I/O без secret reflection.
#[test]
fn invalid_ftp_target_error_is_secret_safe() {
    let registry = symphonia_registry();
    let error = parse_direct_media_url(
        "ftp://user:password@media.example.test/private%0Acommand.ogg?token=secret",
        &registry,
    )
    .expect_err("control character должен быть отклонён");
    let report = format!("{error:#}");
    for forbidden in ["user", "password", "private", "token", "secret"] {
        assert!(
            !report.contains(forbidden),
            "error report раскрыл {forbidden}"
        );
    }
}
