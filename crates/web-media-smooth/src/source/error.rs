//! Typed и redacted construction failures selected fragment sources.

use std::fmt;

use smooth_streaming_fmp4::SmoothTrackMappingError;
use web_media_core::{ComponentKind, ComponentVariantError};

/// Ошибка consume/canonicalization приватного provider runtime seed.
#[derive(thiserror::Error)]
pub enum SmoothFragmentSourceBuildError {
    /// Retained или caller-owned preparation token уже отменён.
    #[error("Construction of Smooth fragment sources cancelled")]
    Cancelled,
    /// C3 selection не принадлежит retained catalog или нарушает exact scope.
    #[error("Smooth component selection is not prepared catalog")]
    Selection(#[source] ComponentVariantError),
    /// P3B требует ровно VideoAndAudio selection.
    #[error("Smooth fragment sources require VideoAndAudio selection")]
    SelectionLayout,
    /// Exact C3 row отсутствует в приватном runtime seed.
    #[error("selected Smooth runtime row is missing for axis {component:?}")]
    RuntimeRowMissing { component: ComponentKind },
    /// Exact C3 row неоднозначно повторяется в приватном runtime seed.
    #[error("selected Smooth runtime row is duplicated for axis {component:?}")]
    RuntimeRowDuplicate { component: ComponentKind },
    /// Sealed manifest selection больше не remap-ится через F2.
    #[error("selected Smooth track failed repeated F2 mapping")]
    Mapping(#[source] SmoothTrackMappingError),
    /// Retained init и remapped track описывают разные identities.
    #[error("retained Smooth initialization identity does not match the remapped track")]
    InitializationIdentityMismatch,
    /// Runtime row указывает за пределы sealed manifest.
    #[error("selected Smooth runtime stream is missing from the sealed manifest")]
    RuntimeStreamMissing,
    /// Runtime row оказался на другой media axis.
    #[error("selected Smooth runtime row has the wrong media axis")]
    RuntimeTrackKindMismatch,
    /// Seek replacement указал fragment за пределами validated timeline.
    #[error("The Smooth fragment replacement index is located outside the timeline.")]
    FragmentIndexOutOfRange,
}

impl fmt::Debug for SmoothFragmentSourceBuildError {
    /// Debug не включает identities, manifest, target или codec state.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothFragmentSourceBuildError")
            .field("kind", &self.kind_name())
            .finish()
    }
}

impl SmoothFragmentSourceBuildError {
    /// Возвращает fixed diagnostics classification.
    const fn kind_name(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Selection(_) => "selection",
            Self::SelectionLayout => "selection-layout",
            Self::RuntimeRowMissing { .. } => "runtime-row-missing",
            Self::RuntimeRowDuplicate { .. } => "runtime-row-duplicate",
            Self::Mapping(_) => "mapping",
            Self::InitializationIdentityMismatch => "initialization-identity",
            Self::RuntimeStreamMissing => "runtime-stream-missing",
            Self::RuntimeTrackKindMismatch => "runtime-track-kind",
            Self::FragmentIndexOutOfRange => "fragment-index-out-of-range",
        }
    }
}
