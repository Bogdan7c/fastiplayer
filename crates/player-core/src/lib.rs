//! Core-контракт и runtime state machine плеера.
//!
//! После Phase 4 этот crate владеет media pipeline и playback tick:
//! demux loop, audio throttle, video backpressure и A/V scheduler не живут в UI shell.

#![forbid(unsafe_code)]

mod command;
mod diagnostics;
mod error;
mod event;
mod media_opening;
mod pipeline;
mod seek_controller;
mod seek_state;
mod session;
mod snapshot;
mod state;
mod tick;
mod video_backend;
mod worker;

pub use command::{
    MediaOpenRequest, MediaSource, PlayerCommand, QualityId, QualitySelection, ScrubCommitPolicy,
    SeekMode, SeekRequest, SeekTarget,
};
pub(crate) use diagnostics::PlaybackDiagnostics;
pub use diagnostics::{
    LatencyCounterSnapshot, PipelineLatencyCountersSnapshot, PipelineLatencySampleSnapshot,
    PipelineLatencyStage, PipelinePauseCountersSnapshot, PipelinePauseReason,
    PipelinePauseSnapshot, PipelineQueueDepthSnapshot, PlaybackDiagnosticsLogSummary,
    PlaybackDiagnosticsSnapshot, TextureSlotPressureSnapshot, VideoDropAttributionSnapshot,
    VideoDropCountersSnapshot, VideoDropReason,
};
pub use error::{PlayerError, PlayerErrorKind, PlayerResult};
pub use event::{
    BufferingState, CapabilitySummary, FramePresentationInfo, MediaSummary, PlayerEvent,
};
pub use media_core::TrackId;
pub(crate) use pipeline::{PendingAudioPacket, PendingVideoPacket, PlaybackPipeline};
pub use seek_controller::{
    PlaybackResumeIntent, SeekController, SeekControllerDiagnostics, SeekControllerMode,
};
pub use session::PlayerSession;
pub use snapshot::{
    AudioBufferSnapshot, BackendSnapshot, FrameCounters, PlayerSnapshot, QualitySummary,
    QueueSnapshot, TexturePoolSnapshot, TrackSelectionSnapshot, TrackSummarySnapshot,
    VideoFrameSnapshot,
};
pub use state::PlaybackState;
pub use tick::{
    PlayerPipelinePause, PlayerTickConfig, PlayerTickContext, PlayerTickPacket, PlayerTickResult,
    PlayerVideoDropReason, PlayerVideoFrameDrop,
};
pub use video_backend::{StartedVideoBackend, VideoBackendFactory, WgpuVideoBackendFactory};
pub use worker::{
    PlayerCommandSender, PlayerPresentFrame, PlayerRenderError, PlayerRenderErrorKind,
    PlayerWorker, PlayerWorkerConfig, PlayerWorkerEvent, PlayerWorkerJoinError,
    PlayerWorkerSendError, PresentFrameLease, PresentFrameTextureViews,
};
