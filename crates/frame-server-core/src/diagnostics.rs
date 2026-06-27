use crate::request::{ScrubStaleReason, ScrubTargetContext};
use crate::scrub::{
    AudioResumeErrorReason, DecoderBackpressureReason, DemuxUnavailableReason,
    DemuxUnsupportedReason, HostUploadBackpressureReason, ResourceBusyReason, ScrubFatalReason,
    ScrubTimeoutReason,
};

/// Driver-only outcome kind, который можно положить в diagnostics без раскрытия payload-а UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubDriverOutcomeKind {
    Prepared,
    DecodePointSeeked,
    Progressed,
    PreviewFrameReady,
    AudioResumePending,
    AudioResumeTimedOut,
    AudioResumeFailed,
    Finished,
    MatchedPlayback,
    Cancelled,
    StaleGeneration,
    ResourceBusy,
    DemuxUnavailable,
    DemuxUnsupported,
    DecoderBackpressure,
    HostUploadBackpressure,
    TimedOut,
    Fatal,
}

/// Public phase для event consumers, без driver lifecycle деталей.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubPublicPhase {
    Started,
    Progress,
    PreviewFrameReady,
    ResumePending,
    Committed,
    MatchedPlayback,
    Cancelled,
    Failed,
}

/// Нормализованная public failure category. Driver payload остаётся в outcome/diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubFailureReason {
    AudioResumeTimedOut,
    AudioResumeFailed,
    DemuxUnavailable,
    DemuxUnsupported,
    DecoderBackpressure,
    HostUploadBackpressure,
    ResourceBusy,
    Timeout,
    Fatal,
}

/// Typed driver detail для diagnostics. Public event reason остаётся normalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubDriverDiagnosticReason {
    AudioResumeError(AudioResumeErrorReason),
    DemuxUnavailable(DemuxUnavailableReason),
    DemuxUnsupported(DemuxUnsupportedReason),
    DecoderBackpressure(DecoderBackpressureReason),
    HostUploadBackpressure(HostUploadBackpressureReason),
    ResourceBusy(ResourceBusyReason),
    Timeout(ScrubTimeoutReason),
    Fatal(ScrubFatalReason),
    StaleGeneration(ScrubStaleReason),
}

/// Diagnostics, которые связывают public event с исходным driver outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrubEventDiagnostics {
    pub driver_outcome: ScrubDriverOutcomeKind,
    pub driver_reason: Option<ScrubDriverDiagnosticReason>,
    pub stale_reason: Option<ScrubStaleReason>,
}

impl ScrubEventDiagnostics {
    #[must_use]
    pub const fn new(driver_outcome: ScrubDriverOutcomeKind) -> Self {
        Self {
            driver_outcome,
            driver_reason: None,
            stale_reason: None,
        }
    }

    #[must_use]
    pub const fn with_driver_reason(
        driver_outcome: ScrubDriverOutcomeKind,
        driver_reason: ScrubDriverDiagnosticReason,
    ) -> Self {
        Self {
            driver_outcome,
            driver_reason: Some(driver_reason),
            stale_reason: None,
        }
    }

    #[must_use]
    pub const fn with_stale_reason(
        driver_outcome: ScrubDriverOutcomeKind,
        stale_reason: ScrubStaleReason,
    ) -> Self {
        Self {
            driver_outcome,
            driver_reason: Some(ScrubDriverDiagnosticReason::StaleGeneration(stale_reason)),
            stale_reason: Some(stale_reason),
        }
    }
}

/// Общая часть event payload-а для future diagnostics snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrubEventEnvelope {
    pub context: ScrubTargetContext,
    pub phase: ScrubPublicPhase,
    pub diagnostics: ScrubEventDiagnostics,
}
