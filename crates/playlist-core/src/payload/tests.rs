#[cfg(unix)]
use std::path::PathBuf;

use media_core::{MediaDuration, MediaTime};

use super::*;
use crate::{PlaylistPlaybackSpan, PlaylistPlaybackSpanError};

#[test]
fn playback_span_uses_checked_strict_boundaries() {
    let start = MediaTime::from_secs(10);
    let span = PlaylistPlaybackSpan::from_start_and_duration(start, MediaDuration::from_secs(5))
        .expect("bounded span");

    assert_eq!(span.start(), start);
    assert_eq!(span.end_exclusive(), Some(MediaTime::from_secs(15)));
    assert_eq!(span.duration(), Some(MediaDuration::from_secs(5)));
    assert_eq!(
        PlaylistPlaybackSpan::new(start, Some(start)),
        Err(PlaylistPlaybackSpanError::EndNotAfterStart)
    );
    assert_eq!(
        PlaylistPlaybackSpan::from_start_and_duration(start, MediaDuration::ZERO),
        Err(PlaylistPlaybackSpanError::ZeroDuration)
    );
    assert_eq!(
        PlaylistPlaybackSpan::from_start_and_duration(MediaTime::MAX, MediaDuration::from_nanos(1),),
        Err(PlaylistPlaybackSpanError::EndOverflow)
    );
}

#[test]
fn service_payload_is_versioned_bounded_exact_and_redacted() {
    let secret = b"https://child.invalid/watch?id=secret-token".to_vec();
    let locator = service_locator(
        ServiceReopenMaterialKind::StableWebpageIdentity,
        secret.clone(),
    )
    .expect("stable child identity");
    let same = service_locator(
        ServiceReopenMaterialKind::StableWebpageIdentity,
        secret.clone(),
    )
    .expect("same stable child identity");
    let different = service_locator(
        ServiceReopenMaterialKind::StableWebpageIdentity,
        b"https://child.invalid/watch?id=other".to_vec(),
    )
    .expect("different stable child identity");

    assert_eq!(locator, same);
    assert_ne!(locator, different);
    let exposed = locator
        .expose_service_payload_for_reopen()
        .expect("service payload");
    assert_eq!(exposed.expose_payload_for_reopen(), secret);
    let debug = format!("{locator:?}");
    assert!(!debug.contains("secret-token"));
    assert!(debug.contains("<redacted-service-payload>"));
    assert!(matches!(
        DurableReopenLocator::from_service_payload(
            "yt-dlp",
            CURRENT_DURABLE_REOPEN_PAYLOAD_VERSION + 1,
            ServiceReopenMaterialKind::StableWebpageIdentity,
            b"stable".to_vec(),
        ),
        Err(DurableReopenLocatorBuildError::UnknownPayloadVersion { .. })
    ));
    assert!(matches!(
        service_locator(
            ServiceReopenMaterialKind::StableWebpageIdentity,
            vec![0; MAX_DURABLE_REOPEN_SERVICE_PAYLOAD_BYTES + 1],
        ),
        Err(DurableReopenLocatorBuildError::ServicePayloadLimitExceeded { .. })
    ));
}

#[test]
fn service_owner_grammar_and_bounds_are_explicit() {
    assert!(matches!(
        DurableReopenLocator::from_service_payload(
            "Yt Dlp",
            CURRENT_DURABLE_REOPEN_PAYLOAD_VERSION,
            ServiceReopenMaterialKind::StableExtractorIdentity,
            b"stable".to_vec(),
        ),
        Err(DurableReopenLocatorBuildError::InvalidServiceOwner)
    ));
    assert!(matches!(
        DurableReopenLocator::from_service_payload(
            "x".repeat(MAX_DURABLE_REOPEN_SERVICE_OWNER_BYTES + 1),
            CURRENT_DURABLE_REOPEN_PAYLOAD_VERSION,
            ServiceReopenMaterialKind::StableExtractorIdentity,
            b"stable".to_vec(),
        ),
        Err(DurableReopenLocatorBuildError::ServiceOwnerLimitExceeded { .. })
    ));
}

#[test]
fn every_ephemeral_transport_category_is_rejected_without_payload_echo() {
    let ephemeral_kinds = [
        ServiceReopenMaterialKind::FormatUrl,
        ServiceReopenMaterialKind::ManifestUrl,
        ServiceReopenMaterialKind::FragmentUrl,
        ServiceReopenMaterialKind::KeyUrl,
        ServiceReopenMaterialKind::SignedEndpoint,
        ServiceReopenMaterialKind::Headers,
        ServiceReopenMaterialKind::Cookies,
        ServiceReopenMaterialKind::AuthorizationOrSession,
    ];

    for material_kind in ephemeral_kinds {
        let error = service_locator(
            material_kind,
            b"Authorization: secret-cookie=signed-token".to_vec(),
        )
        .expect_err("ephemeral material must never become durable");
        let diagnostic = format!("{error:?}");
        assert!(matches!(
            error,
            DurableReopenLocatorBuildError::EphemeralTransportMaterial { .. }
        ));
        assert!(!diagnostic.contains("secret-cookie"));
        assert!(!diagnostic.contains("signed-token"));
    }
}

#[test]
fn url_ancillary_and_provenance_debug_are_secret_safe() {
    let raw_url = "https://user:password@example.invalid/private/path?token=secret#fragment";
    let root = DurableReopenLocator::url(
        SecretUrlLocator::from_reopenable_url(raw_url).expect("exact secret URL"),
    );
    let provenance = PlaylistImportProvenance::new(
        root.clone(),
        PlaylistImportSourceKind::Service,
        NonZeroU32::new(7),
    );
    let hint = PlaylistAncillaryTrackHint::new(
        "subtitle-private-id",
        Some("uk".to_owned()),
        Some("Українські".to_owned()),
        PlaylistAncillaryTrackSelectionKind::Manual,
        PlaylistAncillaryTrackOrigin::External(root),
        Some("service-format-private-id".to_owned()),
    )
    .expect("bounded hint");

    let diagnostics = format!("{provenance:?} {hint:?}");
    for secret in [
        "password",
        "/private/path",
        "token=secret",
        "subtitle-private-id",
        "service-format-private-id",
    ] {
        assert!(!diagnostics.contains(secret));
    }
}

#[test]
fn text_bounds_reject_empty_and_oversized_values() {
    assert!(matches!(
        PlaylistAncillaryTrackHint::new(
            "",
            None,
            None,
            PlaylistAncillaryTrackSelectionKind::Manual,
            PlaylistAncillaryTrackOrigin::Embedded,
            None,
        ),
        Err(PlaylistPayloadBuildError::EmptyText {
            field: PlaylistPayloadTextField::AncillarySemanticIdentity
        })
    ));
}

#[cfg(unix)]
#[test]
fn durable_native_path_keeps_exact_non_utf_identity() {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let exact_bytes = vec![
        b'/', b'm', b'e', b'd', b'i', b'a', b'/', 0xff, b'.', b'm', b'k', b'v',
    ];
    let locator = DurableReopenLocator::local(LocalLocator::Native(PathBuf::from(
        OsString::from_vec(exact_bytes.clone()),
    )));
    let reopened = locator
        .expose_local_for_reopen()
        .and_then(LocalLocator::expose_native_path_for_open)
        .expect("native path");

    assert_eq!(reopened.as_os_str().as_bytes(), exact_bytes);
    assert!(!format!("{locator:?}").contains("media"));
}

fn service_locator(
    material_kind: ServiceReopenMaterialKind,
    payload: Vec<u8>,
) -> Result<DurableReopenLocator, DurableReopenLocatorBuildError> {
    DurableReopenLocator::from_service_payload(
        "yt-dlp",
        CURRENT_DURABLE_REOPEN_PAYLOAD_VERSION,
        material_kind,
        payload,
    )
}
