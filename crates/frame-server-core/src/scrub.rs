use std::time::Duration;

use media_core::{MediaTime, TrackTimestamp};
use video_present_core::VideoPresentFrameResourceDescriptor;

use crate::diagnostics::{
    ScrubDriverDiagnosticReason, ScrubDriverOutcomeKind, ScrubEventDiagnostics, ScrubFailureReason,
};
use crate::request::{
    CancelScrubReason, ScrubCurrentGuards, ScrubGenerationToken, ScrubStaleReason,
    ScrubTargetContext,
};

/// Driver-supplied audio resume budget metadata. Budget calculation stays outside this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioResumeBudgetMetadata {
    pub budget: Duration,
    pub elapsed: Duration,
    pub source: AudioResumeBudgetSource,
}

impl AudioResumeBudgetMetadata {
    #[must_use]
    pub const fn supplied_by_driver(budget: Duration, elapsed: Duration) -> Self {
        Self {
            budget,
            elapsed,
            source: AudioResumeBudgetSource::SuppliedByExternalDriver,
        }
    }

    #[must_use]
    pub const fn timing_unknown_fallback(budget: Duration, elapsed: Duration) -> Self {
        Self {
            budget,
            elapsed,
            source: AudioResumeBudgetSource::TimingUnknownFallback,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioResumeBudgetSource {
    SuppliedByExternalDriver,
    TimingUnknownFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioResumeErrorReason {
    ResumeGateUnavailable,
    OutputClosed,
    DriverRejectedResume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DemuxUnavailableReason {
    SourceGone,
    DemuxerClosed,
    SeekableSourceMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DemuxUnsupportedReason {
    NonSeekableSource,
    DecodePointBeforeUnsupported,
    SelectedVideoTrackMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecoderBackpressureReason {
    PacketQueueFull,
    OutputFloorControlBlocked,
    DecoderControlChannelFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostUploadBackpressureReason {
    ReadyFrameQueueFull,
    UploadSlotsExhausted,
    UploadControlChannelFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceBusyReason {
    PlaybackOwnsDecoder,
    PreviewLeaseStillHeld,
    BackendResourcePressure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubTimeoutReason {
    DriverStepBudgetExceeded,
    FrameReadinessDeadline,
    CommitResumeDeadline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubFatalReason {
    ContextInvariantViolated,
    DriverInvariantViolated,
    BackendContractViolated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubTargetReachStatus {
    BeforeTarget,
    TargetOrAfter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrubProgress {
    pub packets_fed: u32,
    pub frames_drained: u32,
    pub target_status: ScrubTargetReachStatus,
}

/// Renderer-neutral preview frame identity. Pixel data and release ownership stay outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubPreviewFrame {
    pub generation: ScrubGenerationToken,
    pub actual_time: MediaTime,
    pub actual_pts: TrackTimestamp,
    pub resource: VideoPresentFrameResourceDescriptor,
}

impl ScrubPreviewFrame {
    #[must_use]
    pub fn stale_reason_against_generation(
        &self,
        current_generation: ScrubGenerationToken,
    ) -> Option<ScrubStaleReason> {
        self.generation.stale_reason_against(current_generation)
    }
}

/// Readiness status для frame lease/resource, не сам decoder/render lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubFrameReadiness {
    pub generation: ScrubGenerationToken,
    pub state: ScrubFrameReadinessState,
}

impl ScrubFrameReadiness {
    #[must_use]
    pub const fn pending(generation: ScrubGenerationToken) -> Self {
        Self {
            generation,
            state: ScrubFrameReadinessState::Pending,
        }
    }

    #[must_use]
    pub const fn ready(frame: ScrubPreviewFrame) -> Self {
        Self {
            generation: frame.generation,
            state: ScrubFrameReadinessState::Ready { frame },
        }
    }

    #[must_use]
    pub fn stale_reason_against_generation(
        &self,
        current_generation: ScrubGenerationToken,
    ) -> Option<ScrubStaleReason> {
        self.generation.stale_reason_against(current_generation)
    }

    #[must_use]
    pub fn mark_stale_for_generation(self, current_generation: ScrubGenerationToken) -> Self {
        match self.stale_reason_against_generation(current_generation) {
            Some(reason) => Self {
                generation: self.generation,
                state: ScrubFrameReadinessState::Stale { reason },
            },
            None => self,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubFrameReadinessState {
    Pending,
    Ready { frame: ScrubPreviewFrame },
    Stale { reason: ScrubStaleReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedOutcome {
    pub context: ScrubTargetContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodePointSeekedOutcome {
    pub context: ScrubTargetContext,
    pub actual_decode_time: MediaTime,
    pub actual_decode_pts: TrackTimestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressedOutcome {
    pub context: ScrubTargetContext,
    pub progress: ScrubProgress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewFrameReadyOutcome {
    pub context: ScrubTargetContext,
    pub frame: ScrubPreviewFrame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioResumePendingOutcome {
    pub context: ScrubTargetContext,
    pub budget: AudioResumeBudgetMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioResumeTimedOutOutcome {
    pub context: ScrubTargetContext,
    pub budget: AudioResumeBudgetMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioResumeFailedOutcome {
    pub context: ScrubTargetContext,
    pub reason: AudioResumeErrorReason,
    pub budget: Option<AudioResumeBudgetMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinishedOutcome {
    pub context: ScrubTargetContext,
    pub committed_time: MediaTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchedPlaybackOutcome {
    pub context: ScrubTargetContext,
    pub matched_time: MediaTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancelledOutcome {
    pub context: ScrubTargetContext,
    pub reason: CancelScrubReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaleGenerationOutcome {
    pub context: ScrubTargetContext,
    pub reason: ScrubStaleReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceBusyOutcome {
    pub context: ScrubTargetContext,
    pub reason: ResourceBusyReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemuxUnavailableOutcome {
    pub context: ScrubTargetContext,
    pub reason: DemuxUnavailableReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemuxUnsupportedOutcome {
    pub context: ScrubTargetContext,
    pub reason: DemuxUnsupportedReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderBackpressureOutcome {
    pub context: ScrubTargetContext,
    pub reason: DecoderBackpressureReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostUploadBackpressureOutcome {
    pub context: ScrubTargetContext,
    pub reason: HostUploadBackpressureReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubTimedOutOutcome {
    pub context: ScrubTargetContext,
    pub reason: ScrubTimeoutReason,
    pub elapsed: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FatalOutcome {
    pub context: ScrubTargetContext,
    pub reason: ScrubFatalReason,
}

/// Rich driver result layer. Public UI should consume `ScrubEvent`, not this enum directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubDriverOutcome {
    Prepared(PreparedOutcome),
    DecodePointSeeked(DecodePointSeekedOutcome),
    Progressed(ProgressedOutcome),
    PreviewFrameReady(PreviewFrameReadyOutcome),
    AudioResumePending(AudioResumePendingOutcome),
    AudioResumeTimedOut(AudioResumeTimedOutOutcome),
    AudioResumeFailed(AudioResumeFailedOutcome),
    Finished(FinishedOutcome),
    MatchedPlayback(MatchedPlaybackOutcome),
    Cancelled(CancelledOutcome),
    StaleGeneration(StaleGenerationOutcome),
    ResourceBusy(ResourceBusyOutcome),
    DemuxUnavailable(DemuxUnavailableOutcome),
    DemuxUnsupported(DemuxUnsupportedOutcome),
    DecoderBackpressure(DecoderBackpressureOutcome),
    HostUploadBackpressure(HostUploadBackpressureOutcome),
    TimedOut(ScrubTimedOutOutcome),
    Fatal(FatalOutcome),
}

impl ScrubDriverOutcome {
    #[must_use]
    pub const fn context(&self) -> &ScrubTargetContext {
        match self {
            Self::Prepared(payload) => &payload.context,
            Self::DecodePointSeeked(payload) => &payload.context,
            Self::Progressed(payload) => &payload.context,
            Self::PreviewFrameReady(payload) => &payload.context,
            Self::AudioResumePending(payload) => &payload.context,
            Self::AudioResumeTimedOut(payload) => &payload.context,
            Self::AudioResumeFailed(payload) => &payload.context,
            Self::Finished(payload) => &payload.context,
            Self::MatchedPlayback(payload) => &payload.context,
            Self::Cancelled(payload) => &payload.context,
            Self::StaleGeneration(payload) => &payload.context,
            Self::ResourceBusy(payload) => &payload.context,
            Self::DemuxUnavailable(payload) => &payload.context,
            Self::DemuxUnsupported(payload) => &payload.context,
            Self::DecoderBackpressure(payload) => &payload.context,
            Self::HostUploadBackpressure(payload) => &payload.context,
            Self::TimedOut(payload) => &payload.context,
            Self::Fatal(payload) => &payload.context,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ScrubDriverOutcomeKind {
        match self {
            Self::Prepared(_) => ScrubDriverOutcomeKind::Prepared,
            Self::DecodePointSeeked(_) => ScrubDriverOutcomeKind::DecodePointSeeked,
            Self::Progressed(_) => ScrubDriverOutcomeKind::Progressed,
            Self::PreviewFrameReady(_) => ScrubDriverOutcomeKind::PreviewFrameReady,
            Self::AudioResumePending(_) => ScrubDriverOutcomeKind::AudioResumePending,
            Self::AudioResumeTimedOut(_) => ScrubDriverOutcomeKind::AudioResumeTimedOut,
            Self::AudioResumeFailed(_) => ScrubDriverOutcomeKind::AudioResumeFailed,
            Self::Finished(_) => ScrubDriverOutcomeKind::Finished,
            Self::MatchedPlayback(_) => ScrubDriverOutcomeKind::MatchedPlayback,
            Self::Cancelled(_) => ScrubDriverOutcomeKind::Cancelled,
            Self::StaleGeneration(_) => ScrubDriverOutcomeKind::StaleGeneration,
            Self::ResourceBusy(_) => ScrubDriverOutcomeKind::ResourceBusy,
            Self::DemuxUnavailable(_) => ScrubDriverOutcomeKind::DemuxUnavailable,
            Self::DemuxUnsupported(_) => ScrubDriverOutcomeKind::DemuxUnsupported,
            Self::DecoderBackpressure(_) => ScrubDriverOutcomeKind::DecoderBackpressure,
            Self::HostUploadBackpressure(_) => ScrubDriverOutcomeKind::HostUploadBackpressure,
            Self::TimedOut(_) => ScrubDriverOutcomeKind::TimedOut,
            Self::Fatal(_) => ScrubDriverOutcomeKind::Fatal,
        }
    }

    #[must_use]
    pub fn stale_reason_against(&self, current: ScrubCurrentGuards) -> Option<ScrubStaleReason> {
        match self {
            Self::StaleGeneration(payload) => Some(payload.reason),
            Self::PreviewFrameReady(payload) => payload
                .frame
                .stale_reason_against_generation(current.generation)
                .or_else(|| self.context().stale_reason_against(current)),
            _ => self.context().stale_reason_against(current),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubStartedEvent {
    pub context: ScrubTargetContext,
    pub diagnostics: ScrubEventDiagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubProgressEvent {
    pub context: ScrubTargetContext,
    pub progress: ScrubProgress,
    pub diagnostics: ScrubEventDiagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewFrameReadyEvent {
    pub context: ScrubTargetContext,
    pub frame: ScrubPreviewFrame,
    pub diagnostics: ScrubEventDiagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumePendingEvent {
    pub context: ScrubTargetContext,
    pub budget: AudioResumeBudgetMetadata,
    pub diagnostics: ScrubEventDiagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubCommittedEvent {
    pub context: ScrubTargetContext,
    pub committed_time: MediaTime,
    pub diagnostics: ScrubEventDiagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchedPlaybackEvent {
    pub context: ScrubTargetContext,
    pub matched_time: MediaTime,
    pub diagnostics: ScrubEventDiagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubCancelledEvent {
    pub context: ScrubTargetContext,
    pub reason: CancelScrubReason,
    pub diagnostics: ScrubEventDiagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubFailedEvent {
    pub context: ScrubTargetContext,
    pub reason: ScrubFailureReason,
    pub diagnostics: ScrubEventDiagnostics,
}

/// Нормализованный public event layer для UI/diagnostics consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubEvent {
    Started(ScrubStartedEvent),
    Progress(ScrubProgressEvent),
    PreviewFrameReady(PreviewFrameReadyEvent),
    ResumePending(ResumePendingEvent),
    Committed(ScrubCommittedEvent),
    MatchedPlayback(MatchedPlaybackEvent),
    Cancelled(ScrubCancelledEvent),
    Failed(ScrubFailedEvent),
}

impl ScrubEvent {
    #[must_use]
    pub fn from_driver_outcome(outcome: ScrubDriverOutcome) -> Self {
        let diagnostics = match outcome {
            ScrubDriverOutcome::StaleGeneration(payload) => {
                ScrubEventDiagnostics::with_stale_reason(outcome.kind(), payload.reason)
            }
            ScrubDriverOutcome::AudioResumeFailed(payload) => {
                ScrubEventDiagnostics::with_driver_reason(
                    outcome.kind(),
                    ScrubDriverDiagnosticReason::AudioResumeError(payload.reason),
                )
            }
            ScrubDriverOutcome::ResourceBusy(payload) => ScrubEventDiagnostics::with_driver_reason(
                outcome.kind(),
                ScrubDriverDiagnosticReason::ResourceBusy(payload.reason),
            ),
            ScrubDriverOutcome::DemuxUnavailable(payload) => {
                ScrubEventDiagnostics::with_driver_reason(
                    outcome.kind(),
                    ScrubDriverDiagnosticReason::DemuxUnavailable(payload.reason),
                )
            }
            ScrubDriverOutcome::DemuxUnsupported(payload) => {
                ScrubEventDiagnostics::with_driver_reason(
                    outcome.kind(),
                    ScrubDriverDiagnosticReason::DemuxUnsupported(payload.reason),
                )
            }
            ScrubDriverOutcome::DecoderBackpressure(payload) => {
                ScrubEventDiagnostics::with_driver_reason(
                    outcome.kind(),
                    ScrubDriverDiagnosticReason::DecoderBackpressure(payload.reason),
                )
            }
            ScrubDriverOutcome::HostUploadBackpressure(payload) => {
                ScrubEventDiagnostics::with_driver_reason(
                    outcome.kind(),
                    ScrubDriverDiagnosticReason::HostUploadBackpressure(payload.reason),
                )
            }
            ScrubDriverOutcome::TimedOut(payload) => ScrubEventDiagnostics::with_driver_reason(
                outcome.kind(),
                ScrubDriverDiagnosticReason::Timeout(payload.reason),
            ),
            ScrubDriverOutcome::Fatal(payload) => ScrubEventDiagnostics::with_driver_reason(
                outcome.kind(),
                ScrubDriverDiagnosticReason::Fatal(payload.reason),
            ),
            _ => ScrubEventDiagnostics::new(outcome.kind()),
        };

        match outcome {
            ScrubDriverOutcome::Prepared(payload) => Self::Started(ScrubStartedEvent {
                context: payload.context,
                diagnostics,
            }),
            ScrubDriverOutcome::DecodePointSeeked(payload) => Self::Progress(ScrubProgressEvent {
                context: payload.context,
                progress: ScrubProgress {
                    packets_fed: 0,
                    frames_drained: 0,
                    target_status: ScrubTargetReachStatus::BeforeTarget,
                },
                diagnostics,
            }),
            ScrubDriverOutcome::Progressed(payload) => Self::Progress(ScrubProgressEvent {
                context: payload.context,
                progress: payload.progress,
                diagnostics,
            }),
            ScrubDriverOutcome::PreviewFrameReady(payload) => {
                Self::PreviewFrameReady(PreviewFrameReadyEvent {
                    context: payload.context,
                    frame: payload.frame,
                    diagnostics,
                })
            }
            ScrubDriverOutcome::AudioResumePending(payload) => {
                Self::ResumePending(ResumePendingEvent {
                    context: payload.context,
                    budget: payload.budget,
                    diagnostics,
                })
            }
            ScrubDriverOutcome::Finished(payload) => Self::Committed(ScrubCommittedEvent {
                context: payload.context,
                committed_time: payload.committed_time,
                diagnostics,
            }),
            ScrubDriverOutcome::MatchedPlayback(payload) => {
                Self::MatchedPlayback(MatchedPlaybackEvent {
                    context: payload.context,
                    matched_time: payload.matched_time,
                    diagnostics,
                })
            }
            ScrubDriverOutcome::Cancelled(payload) => Self::Cancelled(ScrubCancelledEvent {
                context: payload.context,
                reason: payload.reason,
                diagnostics,
            }),
            ScrubDriverOutcome::StaleGeneration(payload) => Self::Cancelled(ScrubCancelledEvent {
                context: payload.context,
                reason: CancelScrubReason::StaleContext,
                diagnostics,
            }),
            ScrubDriverOutcome::AudioResumeTimedOut(payload) => Self::Failed(ScrubFailedEvent {
                context: payload.context,
                reason: ScrubFailureReason::AudioResumeTimedOut,
                diagnostics,
            }),
            ScrubDriverOutcome::AudioResumeFailed(payload) => Self::Failed(ScrubFailedEvent {
                context: payload.context,
                reason: ScrubFailureReason::AudioResumeFailed,
                diagnostics,
            }),
            ScrubDriverOutcome::ResourceBusy(payload) => Self::Failed(ScrubFailedEvent {
                context: payload.context,
                reason: ScrubFailureReason::ResourceBusy,
                diagnostics,
            }),
            ScrubDriverOutcome::DemuxUnavailable(payload) => Self::Failed(ScrubFailedEvent {
                context: payload.context,
                reason: ScrubFailureReason::DemuxUnavailable,
                diagnostics,
            }),
            ScrubDriverOutcome::DemuxUnsupported(payload) => Self::Failed(ScrubFailedEvent {
                context: payload.context,
                reason: ScrubFailureReason::DemuxUnsupported,
                diagnostics,
            }),
            ScrubDriverOutcome::DecoderBackpressure(payload) => Self::Failed(ScrubFailedEvent {
                context: payload.context,
                reason: ScrubFailureReason::DecoderBackpressure,
                diagnostics,
            }),
            ScrubDriverOutcome::HostUploadBackpressure(payload) => Self::Failed(ScrubFailedEvent {
                context: payload.context,
                reason: ScrubFailureReason::HostUploadBackpressure,
                diagnostics,
            }),
            ScrubDriverOutcome::TimedOut(payload) => Self::Failed(ScrubFailedEvent {
                context: payload.context,
                reason: ScrubFailureReason::Timeout,
                diagnostics,
            }),
            ScrubDriverOutcome::Fatal(payload) => Self::Failed(ScrubFailedEvent {
                context: payload.context,
                reason: ScrubFailureReason::Fatal,
                diagnostics,
            }),
        }
    }
}
