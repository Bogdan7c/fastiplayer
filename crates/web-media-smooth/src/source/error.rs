//! Typed и redacted construction failures selected fragment sources.

use std::fmt;

use smooth_streaming_fmp4::SmoothTrackMappingError;
use web_media_core::{ComponentKind, ComponentVariantError};

/// Ошибка consume/canonicalization приватного P2 runtime seed.
#[derive(thiserror::Error)]
pub enum SmoothFragmentSourceBuildError {
    /// Retained или caller-owned preparation token уже отменён.
    #[error("построение Smooth fragment sources отменено")]
    Cancelled,
    /// C3 selection не принадлежит retained catalog или нарушает exact scope.
    #[error("Smooth component selection не принадлежит prepared catalog")]
    Selection(#[source] ComponentVariantError),
    /// P3B требует ровно VideoAndAudio selection.
    #[error("Smooth fragment sources требуют VideoAndAudio selection")]
    SelectionLayout,
    /// Exact C3 row отсутствует в приватном runtime seed.
    #[error("selected Smooth runtime row отсутствует для оси {component:?}")]
    RuntimeRowMissing { component: ComponentKind },
    /// Exact C3 row неоднозначно повторяется в приватном runtime seed.
    #[error("selected Smooth runtime row повторяется для оси {component:?}")]
    RuntimeRowDuplicate { component: ComponentKind },
    /// Sealed manifest selection больше не remap-ится через F2.
    #[error("selected Smooth track не прошёл повторный F2 mapping")]
    Mapping(#[source] SmoothTrackMappingError),
    /// Retained init и remapped track описывают разные identities.
    #[error("retained Smooth initialization identity не совпадает с remapped track")]
    InitializationIdentityMismatch,
    /// Runtime row указывает за пределы sealed manifest.
    #[error("selected Smooth runtime stream отсутствует в sealed manifest")]
    RuntimeStreamMissing,
    /// Runtime row оказался на другой media axis.
    #[error("selected Smooth runtime row имеет неверную media axis")]
    RuntimeTrackKindMismatch,
    /// Seek replacement указал fragment за пределами validated timeline.
    #[error("Smooth fragment replacement index находится за пределами timeline")]
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
