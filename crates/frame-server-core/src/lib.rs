#![forbid(unsafe_code)]

//! Нейтральные контракты будущего frame server-а.
//!
//! Этот crate описывает scrub protocol, но не исполняет его. Реальные flush,
//! demux seek, decode feed/drain и audio resume gate остаются у внешнего owner-а
//! конкретного пайплайна. Для main-video таким owner-ом станет `player-core`.

pub mod config;
pub mod diagnostics;
pub mod error;
pub mod request;
pub mod scheduler;
pub mod scrub;
pub mod state_machine;

pub use config::{
    DEFAULT_LIVE_SCRUB_MAX_HZ, DEFAULT_MAX_FEED_AND_DRAIN_DRIVER_STEPS,
    DEFAULT_RESUME_PENDING_EVENT_INTERVAL, DEFAULT_STALE_OUTCOME_CANCEL_THRESHOLD,
    FrameServerConfig, LiveScrubDecodeMode, MAX_LIVE_SCRUB_MAX_HZ, ValidatedFrameServerConfig,
};
pub use diagnostics::{
    CountSummary, DecoderBackpressureReasonCounters, DeferredLiveScrubSettingsChange,
    DurationSummary, HostUploadBackpressureReasonCounters, LiveScrubDiagnostics,
    LiveScrubSettingsSnapshot, ResourceBusyReasonCounters, ScrubDiagnosticsRecorder,
    ScrubDiagnosticsSnapshot, ScrubDriverDiagnosticReason, ScrubDriverDiagnosticReasonCounters,
    ScrubDriverOutcomeCounters, ScrubDriverOutcomeKind, ScrubEventDiagnostics, ScrubFailureReason,
    ScrubPublicPhase, ScrubRequestKindCounters, ScrubRequestLifecycleCounters,
    ScrubResourcePressureCounters, ScrubSchedulerDiagnosticCounters,
};
pub use error::FrameServerConfigError;
pub use request::{
    BackendRevision, CancelScrubIntent, CancelScrubReason, FeedAndDrainIntent,
    FeedAndDrainStopCondition, FinishScrubIntent, FinishScrubPolicy,
    MainVideoRealPreviewEntrypoint, PlaybackGeneration, PrepareTargetIntent, ScrubCurrentGuards,
    ScrubExactnessPolicy, ScrubGeneration, ScrubGenerationToken, ScrubIntent, ScrubIntentKind,
    ScrubPriority, ScrubRequestKind, ScrubStaleReason, ScrubTarget, ScrubTargetContext,
    ScrubTrackSelection, SeekDecodePointBeforeIntent, SourceRevision,
};
pub use scheduler::{
    FrameScheduler, SchedulerAction, SchedulerActiveWork, SchedulerDiagnostic, SchedulerUpdate,
};
pub use scrub::{
    AudioResumeBudgetMetadata, AudioResumeBudgetSource, AudioResumeErrorReason,
    AudioResumeFailedOutcome, AudioResumePendingOutcome, AudioResumeTimedOutOutcome,
    CancelledOutcome, DecodePointSeekedOutcome, DecoderBackpressureOutcome,
    DecoderBackpressureReason, DemuxUnavailableOutcome, DemuxUnavailableReason,
    DemuxUnsupportedOutcome, DemuxUnsupportedReason, ExactFrameReadyOutcome, FatalOutcome,
    FinishedOutcome, HostUploadBackpressureOutcome, HostUploadBackpressureReason,
    MatchedPlaybackEvent, MatchedPlaybackOutcome, PreTargetReleasedOutcome, PreparedOutcome,
    PreviewFrameReadyEvent, PreviewFrameReadyOutcome, ProgressedOutcome, ResourceBusyOutcome,
    ResourceBusyReason, ResumePendingEvent, ScrubCancelledEvent, ScrubCommittedEvent,
    ScrubDriverOutcome, ScrubEvent, ScrubEventFrameIdentity, ScrubFailedEvent, ScrubFatalReason,
    ScrubFrameReadiness, ScrubFrameReadinessState, ScrubFrameTiming, ScrubNoVideoFrameReason,
    ScrubPreviewFrame, ScrubProgress, ScrubProgressEvent, ScrubStartedEvent,
    ScrubTargetReachStatus, ScrubTimedOutOutcome, ScrubTimeoutReason, StaleGenerationOutcome,
};
pub use state_machine::{
    ScrubExecutionPolicy, ScrubProtocolPhase, ScrubStateMachine, ScrubStep, ScrubTargetUpdate,
    ScrubTargetUpdateGuards,
};

#[cfg(test)]
mod diagnostics_tests;
#[cfg(test)]
mod scheduler_tests;
#[cfg(test)]
mod state_machine_tests;
#[cfg(test)]
mod tests;
