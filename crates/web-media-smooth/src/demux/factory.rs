//! Injected ISO-BMFF adapter boundary для ordinary video и window-aware audio.

use std::fmt;

use demux_api::{DemuxSniffBudget, OrderedSegmentSource, PresentationWindowOrderedSegmentSource};
use media_core::Demuxer;
use source_core::CancellationToken;

/// Named ownership handoff ordinary video adapter-у.
pub struct SmoothVideoDemuxOpenRequest {
    source: Box<dyn OrderedSegmentSource>,
    cancellation: CancellationToken,
    sniff_budget: DemuxSniffBudget,
}

impl SmoothVideoDemuxOpenRequest {
    /// Создаёт запрос только внутри validated Smooth P4 composition.
    pub(crate) fn new(
        source: Box<dyn OrderedSegmentSource>,
        cancellation: CancellationToken,
        sniff_budget: DemuxSniffBudget,
    ) -> Self {
        Self {
            source,
            cancellation,
            sniff_budget,
        }
    }

    /// Передаёт concrete adapter-у все named части без positional bool/string.
    #[must_use]
    pub fn into_parts(self) -> SmoothVideoDemuxOpenParts {
        SmoothVideoDemuxOpenParts {
            source: self.source,
            cancellation: self.cancellation,
            sniff_budget: self.sniff_budget,
        }
    }
}

impl fmt::Debug for SmoothVideoDemuxOpenRequest {
    /// Source/transport state остаётся opaque.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothVideoDemuxOpenRequest")
            .finish_non_exhaustive()
    }
}

/// Named owned parts ordinary video adapter-а.
pub struct SmoothVideoDemuxOpenParts {
    /// Ленивый finite ordered source.
    pub source: Box<dyn OrderedSegmentSource>,
    /// Общая cancellation generation.
    pub cancellation: CancellationToken,
    /// Caller-owned bounded sniff policy.
    pub sniff_budget: DemuxSniffBudget,
}

/// Named ownership handoff window-aware audio adapter-у.
pub struct SmoothAudioDemuxOpenRequest {
    source: Box<dyn PresentationWindowOrderedSegmentSource>,
    cancellation: CancellationToken,
    sniff_budget: DemuxSniffBudget,
}

impl SmoothAudioDemuxOpenRequest {
    /// Создаёт запрос только внутри validated Smooth P4 composition.
    pub(crate) fn new(
        source: Box<dyn PresentationWindowOrderedSegmentSource>,
        cancellation: CancellationToken,
        sniff_budget: DemuxSniffBudget,
    ) -> Self {
        Self {
            source,
            cancellation,
            sniff_budget,
        }
    }

    /// Передаёт concrete adapter-у все named части.
    #[must_use]
    pub fn into_parts(self) -> SmoothAudioDemuxOpenParts {
        SmoothAudioDemuxOpenParts {
            source: self.source,
            cancellation: self.cancellation,
            sniff_budget: self.sniff_budget,
        }
    }
}

impl fmt::Debug for SmoothAudioDemuxOpenRequest {
    /// Source/transport state остаётся opaque.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothAudioDemuxOpenRequest")
            .finish_non_exhaustive()
    }
}

/// Named owned parts window-aware audio adapter-а.
pub struct SmoothAudioDemuxOpenParts {
    /// Ленивый finite presentation-window source.
    pub source: Box<dyn PresentationWindowOrderedSegmentSource>,
    /// Общая cancellation generation.
    pub cancellation: CancellationToken,
    /// Caller-owned bounded sniff policy.
    pub sniff_budget: DemuxSniffBudget,
}

/// Composition-injected S28A adapter factory без app/player knowledge.
pub trait SmoothIsoBmffDemuxFactory: Send + Sync {
    /// Открывает ordinary single-track video adapter.
    fn open_video(
        &self,
        request: SmoothVideoDemuxOpenRequest,
    ) -> anyhow::Result<Box<dyn Demuxer + Send>>;

    /// Открывает presentation-window-aware single-track audio adapter.
    fn open_audio(
        &self,
        request: SmoothAudioDemuxOpenRequest,
    ) -> anyhow::Result<Box<dyn Demuxer + Send>>;
}
