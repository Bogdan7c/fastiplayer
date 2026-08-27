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
mod progressive;
mod registry;

pub use composite::{
    CompositeAvDemuxer, CompositeAvDemuxerError, CompositeAvPublicTrackIds,
    CompositeAvTrackSelection, CompositeComponent, CompositeComponentLeadPolicy,
    CompositeComponentLeadPolicyError, CompositeComponentReadError, CompositeComponentSeekError,
    CompositePendingPacketTooLargeError,
};
pub use identity::{
    DemuxContainerId, DemuxFactoryId, DemuxFixtureId, DemuxIdentityError, DemuxMimeType,
    DemuxSourceExtension,
};
pub use input::{
    DemuxByteSource, DemuxByteStream, DemuxInput, DemuxInputCapabilities, DemuxInputCapability,
    OrderedResourceMetadata, OrderedResourceReadError, OrderedResourceReadOutcome,
    OrderedResourceRestartableReadInterrupted, OrderedResourceStreamSource, OrderedSegment,
    OrderedSegmentDiscontinuity, OrderedSegmentKind, OrderedSegmentReadError,
    OrderedSegmentSequence, OrderedSegmentSource, PresentationWindowOrderedSegment,
    PresentationWindowOrderedSegmentReadOutcome, PresentationWindowOrderedSegmentSource,
};
pub use probe::{
    DemuxHintRelationship, DemuxHints, DemuxProbeConfidence, DemuxProbeDecision, DemuxProbeMatch,
    DemuxProbeRejection, DemuxProbeRequest, DemuxSniffBudget, DemuxSniffBudgetError,
};
pub use progressive::{
    ProgressiveAsyncSeekEnqueueError, ProgressiveAsyncSeekHandle, ProgressiveAsyncSeekLimits,
    ProgressiveAsyncSeekOutcome, ProgressiveAsyncSeekReceipt, ProgressiveDemuxBufferLimits,
    ProgressiveDemuxPacketTooLargeError, ProgressiveDemuxReadiness, ProgressiveDemuxReadinessPort,
    ProgressiveDemuxStartupError, ProgressiveDemuxWorkerStoppedError, ProgressiveDemuxer,
    ProgressiveRuntimeGeneration, ProgressiveSeekAnchorMismatchError, ProgressiveSeekController,
    ProgressiveSeekFence, ProgressiveSeekRequestId,
};
pub use registry::{
    DemuxContainerRegistration, DemuxFactory, DemuxFactoryDescriptor, DemuxFactoryOpenError,
    DemuxOpenError, DemuxOpenRequest, DemuxProbeSelection, DemuxProbedOpen, DemuxRegistry,
    DemuxRegistryError,
};
