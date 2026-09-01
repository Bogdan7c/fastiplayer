//! Secret-safe typed admission categories для direct HDS ingress.

use hds_manifest_core::F4mManifestError;
use web_media_adaptive::AdaptiveTransportError;

use crate::catalog::HdsNoPlayableRendition;

/// Stable failure category без URL, XML body или provider-private payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdsPrepareFailureKind {
    /// Caller отменил admission/preparation.
    Cancelled,
    /// HTTP/session/provider infrastructure не смогла получить resource.
    Network,
    /// Well-formed fetched XML не является F4M manifest.
    InvalidRoot,
    /// Manifest/bootstrap подтверждает live profile вне N12.
    LiveProfile,
    /// Manifest явно требует DRM/protected-video semantics.
    DrmProtected,
    /// Manifest содержит private/namespaced extension.
    PrivateExtension,
    /// F4M profile валиден, но rendition/runtime profile не поддержан.
    UnsupportedProfile,
    /// Документ похож на F4M, но нарушает XML/schema/value invariants.
    MalformedManifest,
    /// Catalog, exact selection, demux или runtime preparation завершились ошибкой.
    RuntimePreparation,
}

/// Typed live exclusion сохраняет owner и не требует разбора текста ошибки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HdsLiveProfileError {
    /// Root/child F4M объявил live streamType.
    Manifest,
    /// Adobe bootstrap объявил live timeline.
    Bootstrap,
}

impl std::fmt::Display for HdsLiveProfileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manifest => formatter.write_str("HDS F4M manifest declares a live profile"),
            Self::Bootstrap => formatter.write_str("HDS bootstrap declares a live profile"),
        }
    }
}

impl std::error::Error for HdsLiveProfileError {}

/// Проверяет всю anyhow source-chain и возвращает public admission category.
#[must_use]
pub fn classify_hds_prepare_error(error: &anyhow::Error) -> HdsPrepareFailureKind {
    for cause in error.chain() {
        if let Some(transport) = cause.downcast_ref::<AdaptiveTransportError>() {
            return if matches!(transport, AdaptiveTransportError::Cancelled) {
                HdsPrepareFailureKind::Cancelled
            } else {
                HdsPrepareFailureKind::Network
            };
        }
        if cause.downcast_ref::<HdsLiveProfileError>().is_some() {
            return HdsPrepareFailureKind::LiveProfile;
        }
        if cause.downcast_ref::<HdsNoPlayableRendition>().is_some() {
            return HdsPrepareFailureKind::UnsupportedProfile;
        }
        if let Some(manifest) = cause.downcast_ref::<F4mManifestError>() {
            return match manifest {
                F4mManifestError::InvalidRoot => HdsPrepareFailureKind::InvalidRoot,
                F4mManifestError::DrmProtected(_) => HdsPrepareFailureKind::DrmProtected,
                F4mManifestError::PrivateExtension(_) => HdsPrepareFailureKind::PrivateExtension,
                F4mManifestError::UnsupportedFeature(_) => {
                    HdsPrepareFailureKind::UnsupportedProfile
                }
                F4mManifestError::Xml(_)
                | F4mManifestError::InvalidValue { .. }
                | F4mManifestError::StringTooLong
                | F4mManifestError::CountExceeded { .. }
                | F4mManifestError::BootstrapTooLarge
                | F4mManifestError::MissingMedia => HdsPrepareFailureKind::MalformedManifest,
            };
        }
    }
    HdsPrepareFailureKind::RuntimePreparation
}
