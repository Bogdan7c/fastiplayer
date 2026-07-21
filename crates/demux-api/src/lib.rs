//! Нейтральная граница выбора, открытия и композиции container demuxer-ов.
//!
//! Crate не знает transport provider-ов, yt-dlp, player, decoder или конкретный
//! container backend. Конкретные demux crate-ы регистрируют здесь factory, а
//! consumers получают только существующий [`media_core::Demuxer`] runtime contract.

#![forbid(unsafe_code)]

mod composite;
mod identity;
mod input;
mod probe;
mod registry;

pub use composite::{
    CompositeAvDemuxer, CompositeAvDemuxerError, CompositeAvPublicTrackIds,
    CompositeAvTrackSelection, CompositeComponent, CompositeComponentLeadPolicy,
    CompositeComponentLeadPolicyError, CompositeComponentReadError, CompositeComponentSeekError,
};
pub use identity::{
    DemuxContainerId, DemuxFactoryId, DemuxFixtureId, DemuxIdentityError, DemuxMimeType,
    DemuxSourceExtension,
};
pub use input::{
    DemuxByteSource, DemuxByteStream, DemuxInput, DemuxInputCapabilities, DemuxInputCapability,
    OrderedSegment, OrderedSegmentKind, OrderedSegmentReadError, OrderedSegmentSequence,
    OrderedSegmentSource,
};
pub use probe::{
    DemuxHintRelationship, DemuxHints, DemuxProbeConfidence, DemuxProbeDecision, DemuxProbeMatch,
    DemuxProbeRejection, DemuxProbeRequest, DemuxSniffBudget, DemuxSniffBudgetError,
};
pub use registry::{
    DemuxContainerRegistration, DemuxFactory, DemuxFactoryDescriptor, DemuxFactoryOpenError,
    DemuxOpenError, DemuxOpenRequest, DemuxProbeSelection, DemuxRegistry, DemuxRegistryError,
};
