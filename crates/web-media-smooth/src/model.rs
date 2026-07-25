//! Public prepared view и private runtime seed.

use std::fmt;
use std::sync::Arc;

use smooth_streaming_fmp4::{SmoothInitializationSegment, SmoothTrackSelection};
use smooth_streaming_manifest_core::{SmoothManifest, SmoothTime};
use source_core::HttpRequestTarget;
use web_media_adaptive::{AdaptiveHttpContext, AdaptiveResourceSecretForwarding};
use web_media_core::{
    ComponentVariantCatalog, ComponentVariantExactIdentity, ComponentVariantSelection,
};
use web_media_transport_api::SourceGeneration;

/// Exact presentation evidence для root и обеих independent component clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmoothAlignedSpan {
    start: SmoothTime,
    root_end_exclusive: SmoothTime,
    video_end_exclusive: SmoothTime,
    audio_end_exclusive: SmoothTime,
    common_end_exclusive: SmoothTime,
}

impl SmoothAlignedSpan {
    /// Создаётся после exact проверки нулевых starts и root upper bound.
    pub(crate) fn new(
        start: SmoothTime,
        root_end_exclusive: SmoothTime,
        video_end_exclusive: SmoothTime,
        audio_end_exclusive: SmoothTime,
    ) -> Self {
        let common_end_exclusive = std::cmp::min(video_end_exclusive, audio_end_exclusive);
        Self {
            start,
            root_end_exclusive,
            video_end_exclusive,
            audio_end_exclusive,
            common_end_exclusive,
        }
    }

    /// Возвращает exact inclusive start.
    #[must_use]
    pub const fn start(self) -> SmoothTime {
        self.start
    }

    /// Возвращает authoritative root presentation end.
    #[must_use]
    pub const fn end_exclusive(self) -> SmoothTime {
        self.root_end_exclusive
    }

    /// Возвращает exact конец video timeline в её native clock.
    #[must_use]
    pub const fn video_end_exclusive(self) -> SmoothTime {
        self.video_end_exclusive
    }

    /// Возвращает exact конец audio timeline в её native clock.
    #[must_use]
    pub const fn audio_end_exclusive(self) -> SmoothTime {
        self.audio_end_exclusive
    }

    /// Возвращает exact более ранний component end без rescale или tolerance.
    #[must_use]
    pub const fn common_end_exclusive(self) -> SmoothTime {
        self.common_end_exclusive
    }
}

/// Полностью подготовленный neutral catalog и provider default.
pub struct SmoothPreparedCatalog {
    pub(crate) catalog: ComponentVariantCatalog,
    pub(crate) provider_default_selection: ComponentVariantSelection,
    pub(crate) source_generation: SourceGeneration,
    pub(crate) aligned_span: SmoothAlignedSpan,
    #[allow(dead_code)]
    pub(crate) runtime_seed: SmoothRuntimeSeed,
}

impl SmoothPreparedCatalog {
    /// Возвращает immutable C3 catalog.
    #[must_use]
    pub const fn catalog(&self) -> &ComponentVariantCatalog {
        &self.catalog
    }

    /// Возвращает exact provider default selection.
    #[must_use]
    pub const fn provider_default_selection(&self) -> &ComponentVariantSelection {
        &self.provider_default_selection
    }

    /// Возвращает generation исходного source session.
    #[must_use]
    pub const fn source_generation(&self) -> SourceGeneration {
        self.source_generation
    }

    /// Возвращает exact root/component presentation evidence.
    #[must_use]
    pub const fn aligned_span(&self) -> SmoothAlignedSpan {
        self.aligned_span
    }
}

impl fmt::Debug for SmoothPreparedCatalog {
    /// Не раскрывает effective URL, manifest, keys или init bytes.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothPreparedCatalog")
            .field("source_generation", &self.source_generation)
            .field("aligned_span", &self.aligned_span)
            .field("catalog_entries", &self.catalog.stored_variant_count())
            .finish_non_exhaustive()
    }
}

/// Seed будущего fragment runtime; наружу не выдаётся во избежание boundary leak.
#[allow(dead_code)]
pub(crate) struct SmoothRuntimeSeed {
    pub(crate) http: AdaptiveHttpContext,
    pub(crate) effective_manifest_target: HttpRequestTarget,
    pub(crate) fragment_secret_forwarding: AdaptiveResourceSecretForwarding,
    pub(crate) manifest: Arc<SmoothManifest>,
    pub(crate) video_rows: Box<[SmoothRuntimeRow]>,
    pub(crate) audio_rows: Box<[SmoothRuntimeRow]>,
}

/// Runtime row хранит owned init и selector, но не self-borrowing mapped track.
#[allow(dead_code)]
pub(crate) struct SmoothRuntimeRow {
    pub(crate) exact_identity: ComponentVariantExactIdentity,
    pub(crate) selection: SmoothTrackSelection,
    pub(crate) initialization: SmoothInitializationSegment,
}
