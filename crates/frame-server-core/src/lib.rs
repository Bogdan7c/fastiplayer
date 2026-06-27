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

pub use config::{
    DEFAULT_LIVE_SCRUB_MAX_HZ, DEFAULT_MAX_FEED_AND_DRAIN_DRIVER_STEPS,
    DEFAULT_RESUME_PENDING_EVENT_INTERVAL, DEFAULT_STALE_OUTCOME_CANCEL_THRESHOLD,
    DEFAULT_TIMELINE_HOVER_PREPARE_SLOTS, FrameServerConfig, LiveScrubDecodeMode,
    MAX_LIVE_SCRUB_MAX_HZ, MAX_TIMELINE_HOVER_PREPARE_SLOTS, ValidatedFrameServerConfig,
};
pub use diagnostics::{
    ScrubDriverDiagnosticReason, ScrubDriverOutcomeKind, ScrubEventDiagnostics, ScrubFailureReason,
    ScrubPublicPhase,
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
    DemuxUnsupportedOutcome, DemuxUnsupportedReason, FatalOutcome, FinishedOutcome,
    HostUploadBackpressureOutcome, HostUploadBackpressureReason, MatchedPlaybackEvent,
    MatchedPlaybackOutcome, PreparedOutcome, PreviewFrameReadyEvent, PreviewFrameReadyOutcome,
    ProgressedOutcome, ResourceBusyOutcome, ResourceBusyReason, ResumePendingEvent,
    ScrubCancelledEvent, ScrubCommittedEvent, ScrubDriverOutcome, ScrubEvent, ScrubFailedEvent,
    ScrubFatalReason, ScrubFrameReadiness, ScrubFrameReadinessState, ScrubPreviewFrame,
    ScrubProgress, ScrubProgressEvent, ScrubStartedEvent, ScrubTargetReachStatus,
    ScrubTimedOutOutcome, ScrubTimeoutReason, StaleGenerationOutcome,
};

#[cfg(test)]
mod scheduler_tests;
#[cfg(test)]
mod tests;
