//! Core-контракт и runtime state machine плеера.
//!
//! После Phase 4 этот crate владеет media pipeline и playback tick:
//! demux loop, audio throttle, video backpressure и A/V scheduler не живут в UI shell.

#![forbid(unsafe_code)]

mod audio_boundary;
mod command;
mod decoder_boundary;
mod diagnostics;
mod error;
mod event;
mod media_install;
mod media_opening;
mod pipeline;
mod playback_rate;
mod render_lease_bridge;
mod runtime_settings;
mod seek_state;
mod session;
mod snapshot;
mod state;
mod worker;
mod worker_scheduler;

pub use audio_core::{
    AudioChannelLayout, AudioChannelLayoutError, AudioChannelPosition, AudioDecoder,
    AudioDecoderConfig, AudioDecoderError, AudioDecoderFactory, AudioDecoderHandle,
    AudioOutputClockTiming, AudioOutputFactory, AudioOutputInputFrameCount, AudioOutputSpec,
    AudioOutputStreamFrameCount, AudioOutputWriteError, AudioOutputWriteIntent,
    AudioOutputWriteReport, AudioPacketTimeBase, AudioPacketTiming, AudioTempoChannelCount,
    AudioTempoDecodedMedia, AudioTempoFrameCount, AudioTempoFrameSpan,
    AudioTempoOutputProgressMapping, AudioTempoPcmFormat, AudioTempoProcessReport,
    AudioTempoProcessor, AudioTempoProcessorConfig, AudioTempoProcessorError,
    AudioTempoProcessorFactory, AudioTempoProcessorHandle, AudioTempoRatio,
    AudioTempoRatioInvalidReason, AudioTempoReportFrameCounts, AudioTempoSampleRateHz,
    AudioTempoSegment, AudioTempoSegmentId, AudioTempoStretchedOutput, EncodedAudioPacket,
    PlayerAudioClock, PlayerAudioOutput,
};
pub use codec_core::VideoDecodeRequirement;
pub use command::{
    MediaOpenRequest, MediaSource, PlaybackRateAudioTempoRejectReason, PlayerCommand,
    PlayerCommandOutcome, PlayerCommandReject, QualityId, QualitySelection, ScrubCommitOutcome,
    ScrubCommitPolicy, SeekMode, SeekRequest, SeekTarget, VisibleScrubPreviewUnavailableReason,
};
#[cfg(test)]
pub(crate) use decoder_boundary::DecodeBackpressureReason;
pub(crate) use decoder_boundary::{
    DecodeSendError, DecodeThreadError, DecoderResourceSnapshot, PlayerDecodePacket,
    PlayerVideoDecoderThreadHandle, VideoDecoderEndOfStreamDrainResult,
    VideoDecoderEndOfStreamDrainState, VideoPrerollOutputFloor, VideoPrerollOutputFloorClear,
    VideoPrerollOutputFloorResult, VideoStreamConfigResult, VideoStreamDecodeConfig,
};
pub use decoder_boundary::{
    PlayerVideoDecoderThreadConfig, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup, StartedVideoBackend,
};
pub(crate) use diagnostics::{
    ActiveSeekDiagnosticsSnapshot, PlaybackDiagnostics, SeekProgressBlocker,
};
pub use diagnostics::{
    DecoderControlChannelPressureSnapshot, DecoderFramePublishPressureSnapshot,
    LatencyCounterSnapshot, PipelineLatencyCountersSnapshot, PipelineLatencySampleSnapshot,
    PipelineLatencyStage, PipelinePauseCountersSnapshot, PipelinePauseReason,
    PipelinePauseSnapshot, PipelineQueueDepthSnapshot, PlaybackDiagnosticsLogSummary,
    PlaybackDiagnosticsSnapshot, SeekBootstrapDiagnosticsSnapshot, TextureSlotPressureSnapshot,
    VideoDropAttributionSnapshot, VideoDropCountersSnapshot, VideoDropReason,
    WorkerFrameTimingSnapshot, WorkerWakeupDiagnosticsSnapshot, WorkerWakeupReason,
};
pub use error::{PlayerError, PlayerErrorKind, PlayerResult};
pub use event::{
    BufferingState, CapabilitySummary, CorrelatedPlayerEvent, FramePresentationInfo, MediaSummary,
    PlayerEvent, SeekAudioResumeInfo, SeekCommitInfo, SeekTargetFramePresentation,
    VideoBackendSelectionRequest,
};
pub use frame_server_core::{
    BackendRevision, DeferredLiveScrubSettingsChange, LiveScrubDiagnostics,
    LiveScrubSettingsSnapshot, MatchedPlaybackEvent, PlaybackGeneration, PreviewFrameReadyEvent,
    ResumePendingEvent, ScrubCommittedEvent, ScrubDiagnosticsSnapshot, ScrubDriverOutcomeKind,
    ScrubEvent, ScrubEventDiagnostics, ScrubEventFrameIdentity, ScrubExactnessPolicy,
    ScrubFrameTiming, ScrubGeneration, ScrubGenerationToken, ScrubNoVideoFrameReason,
    ScrubPreviewFrame, ScrubRequestKind, ScrubTarget, ScrubTargetContext, ScrubTrackSelection,
    SourceRevision,
};
pub use media_core::TrackId;
pub use media_install::{
    AcceptedMediaInstallTerminalError, AuthorizeInstallCommit, CancelMediaInstall,
    InstalledMediaRestoreFailureStage, InstalledMediaStateRestore,
    InstalledMediaStateRestoreOutcome, InstalledMediaStateRestoreReceipt,
    InstalledMediaStateRestoreReceiptError, InstalledPositionRestore, InstalledSubtitleRestore,
    InstalledTrackRestore, MediaInstallCancellationCause, MediaInstallCommitPoint,
    MediaInstallCompletion, MediaInstallControl, MediaInstallControlOutcome, MediaInstallFailure,
    MediaInstallFailureStage, MediaInstallPhase, MediaInstallPhaseCompletionPort,
    MediaInstallReceipt, MediaInstallReceiptSignal, MediaInstallReceiptWaitError,
    MediaInstallRequestId, MediaInstallVideoResourcePort, MediaInstanceId, PlaybackIntent,
    PlaybackIntentRevision, PlaybackIntentUpdate, PlaybackIntentUpdateOutcome,
    PlaybackIntentUpdateReceipt,
};
pub use media_opening::{MediaSourceInfo, MediaSourceKind, PreparedMedia, PreparedMediaSource};
pub(crate) use pipeline::{PendingAudioPacket, PendingVideoPacket, PlaybackPipeline};
pub use playback_rate::{PlaybackRate, PlaybackRateValidationError};
pub use runtime_settings::{
    PlayerRuntimeAcceptedChange, PlayerRuntimeApplyError, PlayerRuntimeApplyGroup,
    PlayerRuntimeApplyGroupReport, PlayerRuntimeApplyOutcome, PlayerRuntimeApplyReport,
    PlayerRuntimeApplyResult, PlayerRuntimeAudioOutputRecreateUpdate,
    PlayerRuntimeBoundaryActivity, PlayerRuntimeDecoderThreadConfigUpdate,
    PlayerRuntimeDefaultVolumeUpdate, PlayerRuntimeFrameServerPolicyUpdate, PlayerRuntimeSettingId,
    PlayerRuntimeSettingsUpdate, PlayerRuntimeTickConfigUpdate,
    PlayerRuntimeVideoBackendPreference, PlayerRuntimeVideoBackendUpdate,
    PlayerVideoBackendInstallIntent,
};
pub use seek_state::PlaybackResumeIntent;
pub use session::{
    PlayerPipelinePause, PlayerSession, PlayerTickConfig, PlayerTickContext, PlayerTickPacket,
    PlayerTickResult, PlayerVideoDropReason, PlayerVideoFrameDrop,
};
pub(crate) use session::{
    PlayerWorkerWakeupPlan, SchedulerTimingDiagnosticsSnapshot, scheduler_timing_diagnostics,
};
pub use snapshot::{
    AudioBufferSnapshot, BackendSnapshot, FrameCounters, MediaInfoSnapshot, PlayerSnapshot,
    QualitySummary, QueueSnapshot, TexturePoolSnapshot, TrackSelectionSnapshot,
    TrackSummarySnapshot, VideoFrameSnapshot,
};
pub use state::PlaybackState;
pub use worker::{
    MediaInstallControlReceipt, MediaInstallControlReceiptError, PlayerCommandSender,
    PlayerRenderError, PlayerRenderErrorKind, PlayerWorker, PlayerWorkerConfig, PlayerWorkerEvent,
    PlayerWorkerJoinError, PlayerWorkerSendError,
};
