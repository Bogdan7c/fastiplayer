//! Provider-owned построение immutable HLS master catalog.
//!
//! Child references остаются private. Public proof port видит только opaque
//! child identity и возвращает descriptors лишь после transport, content probe,
//! demux track-shape и capability checks.

mod build;
mod discovery;
mod identity;
mod reopen;
mod rows;

#[cfg(test)]
mod tests;

use std::num::NonZeroUsize;

use hls_playlist_core::{HlsPlaylist, MasterPlaylist};
use web_media_core::{
    AudioTrackDescriptor, ComponentVariantCatalog, ComponentVariantCatalogIdentity,
    ComponentVariantCatalogLimit, ComponentVariantEdgeLimit, ComponentVariantError,
    ComponentVariantSelection, ComponentVariantSemanticSelectionRequest, VideoTrackDescriptor,
};

use crate::{HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenError};

pub use build::build_hls_catalog;
pub use discovery::discover_hls_catalog;
pub(crate) use reopen::HlsCatalogMatchMode;
pub use reopen::{HlsCatalogReopenError, HlsCatalogReopenSelection};

/// Provider seed, явно отличающий media playlist от master inventory.
#[derive(Clone, Copy, Debug)]
pub enum HlsCatalogTopologySeed<'playlist> {
    /// Media playlist содержит segments, а не selectable sibling variants.
    Unavailable,
    /// Validated master inventory, который можно доказать в catalog.
    Master(&'playlist MasterPlaylist),
}

/// Классифицирует parser-owned topology без выдуманных rows для media playlist.
pub const fn seed_hls_catalog_topology(playlist: &HlsPlaylist) -> HlsCatalogTopologySeed<'_> {
    match playlist {
        HlsPlaylist::Master(master) => HlsCatalogTopologySeed::Master(master),
        HlsPlaylist::Media(_) => HlsCatalogTopologySeed::Unavailable,
    }
}

/// Snapshot-local opaque identity одного unique child playlist resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HlsCatalogChildId(u64);

impl HlsCatalogChildId {
    const fn from_index(index: usize) -> Self {
        Self(index.saturating_add(1) as u64)
    }
}

/// Безопасная сводка роли unique child proof operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsCatalogChildRole {
    Variant,
    AlternateAudio,
    Shared,
}

/// Bounded proof request без URI, query, parser row и source order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsCatalogChildProbe {
    child: HlsCatalogChildId,
    role: HlsCatalogChildRole,
    reference: hls_playlist_core::ExactReference,
}

/// Presentation profile для immutable discovery snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsCatalogPresentation {
    Vod,
    Live,
}

/// Capability intersection получает только content-proven neutral tracks.
pub trait HlsCatalogCapabilityProofPort {
    fn prove_video(
        &mut self,
        track: &media_core::TrackInfo,
    ) -> Result<VideoTrackDescriptor, HlsCatalogCapabilityRejection>;

    fn prove_audio(
        &mut self,
        track: &media_core::TrackInfo,
    ) -> Result<AudioTrackDescriptor, HlsCatalogCapabilityRejection>;
}

/// Safe capability rejection без provider resource identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HlsCatalogCapabilityRejection {
    #[error("video/audio capability intersection rejected child track")]
    Unsupported,
    #[error("proven track metadata cannot be represented by neutral descriptor")]
    InvalidDescriptor,
}

impl HlsCatalogChildProbe {
    pub const fn child(&self) -> HlsCatalogChildId {
        self.child
    }

    pub const fn role(&self) -> HlsCatalogChildRole {
        self.role
    }
}

/// Полностью доказанная фактическая track shape child-а.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HlsCatalogTrackProof {
    VideoOnly(VideoTrackDescriptor),
    AudioOnly(AudioTrackDescriptor),
    Muxed {
        video: VideoTrackDescriptor,
        audio: AudioTrackDescriptor,
    },
}

/// Snapshot-local proof that independently opened children share one presentation timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HlsCatalogAlignmentProof(u64);

impl HlsCatalogAlignmentProof {
    /// Создаёт opaque identity из provider-owned duration/timeline validation.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Evidence, возвращаемое только после всех provider-owned proof stages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsCatalogChildProof {
    pub container: HlsRequiredContainer,
    pub tracks: HlsCatalogTrackProof,
    /// Sparse A/V edge допустим только между одинаковыми alignment proofs.
    pub alignment: HlsCatalogAlignmentProof,
}

/// Изолируемая ошибка одного non-authoritative sibling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HlsCatalogSiblingRejectionReason {
    #[error("child transport unavailable")]
    TransportUnavailable,
    #[error("child media playlist/profile invalid")]
    InvalidChildManifest,
    #[error("child container content unsupported")]
    UnsupportedContainer,
    #[error("child demux track shape unsupported")]
    UnsupportedTrackShape,
    #[error("child codec/capability intersection rejected")]
    CapabilityRejected,
    #[error("manifest metadata conflicts with probed media")]
    ManifestEvidenceConflict,
    #[error("manifest metadata exceeds neutral descriptor bounds")]
    DescriptorBounds,
    #[error("URI-less embedded audio was not present in the proven main child")]
    MissingEmbeddedAudio,
    #[error("semantic sibling identity is ambiguous")]
    AmbiguousSemanticIdentity,
}

/// Proof-port outcome отличает sibling isolation от whole-job fences.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HlsCatalogChildProofError {
    #[error("HLS catalog proof cancelled")]
    Cancelled,
    #[error("HLS catalog proof generation became stale")]
    StaleGeneration,
    #[error("HLS catalog child rejected: {0}")]
    Rejected(HlsCatalogSiblingRejectionReason),
}

/// Provider composition hook, вызываемый не более одного раза на exact unique child.
pub trait HlsCatalogChildProofPort {
    fn prove_child(
        &mut self,
        request: HlsCatalogChildProbe,
    ) -> Result<HlsCatalogChildProof, HlsCatalogChildProofError>;
}

/// Безопасная bounded diagnostic одной отброшенной sibling row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HlsCatalogSiblingRejection {
    child: HlsCatalogChildId,
    reason: HlsCatalogSiblingRejectionReason,
}

impl HlsCatalogSiblingRejection {
    pub const fn child(&self) -> HlsCatalogChildId {
        self.child
    }

    pub const fn reason(&self) -> HlsCatalogSiblingRejectionReason {
        self.reason
    }
}

/// Caller-owned bounded policy HLS catalog-а.
#[derive(Clone, Copy, Debug)]
pub struct HlsCatalogBuildPolicy {
    pub catalog_limit: ComponentVariantCatalogLimit,
    pub compatibility_edge_limit: ComponentVariantEdgeLimit,
    pub maximum_unique_children: NonZeroUsize,
}

/// Pure provider build request; master document уже обязан пройти parsing/profile validation.
pub struct HlsCatalogBuildRequest<'a> {
    pub master: &'a MasterPlaylist,
    pub catalog_identity: ComponentVariantCatalogIdentity,
    pub provider_default: &'a HlsVariantSelectionIntent,
    /// Exact ordinal действует только внутри текущего parsed master snapshot-а.
    pub provider_default_variant_index: Option<usize>,
    pub policy: HlsCatalogBuildPolicy,
}

/// Provider-owned discovery request; raw manifest material остаётся внутри HLS crate.
pub struct HlsCatalogDiscoveryRequest<'a> {
    pub open: &'a crate::HlsVodOpenRequest,
    pub catalog_identity: ComponentVariantCatalogIdentity,
    pub presentation: HlsCatalogPresentation,
    /// Native root admission может передать exact current-snapshot default без reopen coupling.
    pub provider_default_variant_index: Option<usize>,
    pub policy: HlsCatalogBuildPolicy,
}

/// Media playlist не получает fake alternatives; master публикуется только после proofs.
#[derive(Debug)]
pub enum HlsCatalogDiscoveryOutcome {
    Unavailable,
    Installed(Box<HlsCatalogSnapshot>),
}

/// Fatal discovery failure относится к authoritative document/job, а не sibling.
#[derive(Debug, thiserror::Error)]
pub enum HlsCatalogDiscoveryError {
    #[error("authoritative HLS catalog manifest failed: {0}")]
    Open(#[from] HlsVodOpenError),
    #[error("HLS catalog construction failed: {0}")]
    Build(#[from] HlsCatalogBuildError),
}

/// Truthful immutable provider snapshot; runtime-private rows не пересекают boundary.
#[derive(Debug)]
pub struct HlsCatalogSnapshot {
    catalog: ComponentVariantCatalog,
    provider_default: ComponentVariantSelection,
    sibling_rejections: Box<[HlsCatalogSiblingRejection]>,
    runtime: reopen::HlsCatalogRuntimeMap,
}

impl HlsCatalogSnapshot {
    pub const fn catalog(&self) -> &ComponentVariantCatalog {
        &self.catalog
    }

    pub const fn provider_default_selection(&self) -> &ComponentVariantSelection {
        &self.provider_default
    }

    pub const fn sibling_rejections(&self) -> &[HlsCatalogSiblingRejection] {
        &self.sibling_rejections
    }

    /// Rematch-ит active internal selection после endpoint refresh без provider-default fallback.
    pub fn rematch_semantic(
        &self,
        request: ComponentVariantSemanticSelectionRequest,
    ) -> Result<ComponentVariantSelection, ComponentVariantError> {
        self.catalog.rematch_semantic(request)
    }

    /// Преобразует exact neutral selection в opaque provider reopen intent.
    pub fn reopen_exact(
        &self,
        selection: &ComponentVariantSelection,
    ) -> Result<HlsCatalogReopenSelection, HlsCatalogReopenError> {
        self.runtime.resolve_exact(&self.catalog, selection)
    }

    /// Semantic rematch не делает fallback и сразу возвращает fresh opaque reopen intent.
    pub fn reopen_semantic(
        &self,
        request: ComponentVariantSemanticSelectionRequest,
    ) -> Result<HlsCatalogReopenSelection, HlsCatalogReopenError> {
        let selection = self.catalog.rematch_semantic(request)?;
        self.reopen_exact(&selection)
    }
}

/// Fatal catalog failure; child rejection fatal только для authoritative default.
#[derive(Debug, thiserror::Error)]
pub enum HlsCatalogBuildError {
    #[error("HLS provider default selection is invalid: {0}")]
    ProviderDefaultSelection(#[source] HlsVodOpenError),
    #[error("HLS catalog unique child budget exceeded: {provided} > {maximum}")]
    UniqueChildLimitExceeded { provided: usize, maximum: usize },
    #[error("HLS catalog proof cancelled")]
    Cancelled,
    #[error("HLS catalog proof generation became stale")]
    StaleGeneration,
    #[error("authoritative HLS provider default child was rejected: {reason}")]
    ProviderDefaultRejected {
        reason: HlsCatalogSiblingRejectionReason,
    },
    #[error("HLS master has no truthful selectable rows after sibling isolation")]
    NoSelectableRows,
    #[error("neutral HLS catalog rejected provider topology: {0}")]
    Catalog(#[from] ComponentVariantError),
    #[error("HLS semantic identity construction failed")]
    SemanticIdentity,
}
