//! Core-контракт и runtime state machine плеера.
//!
//! После Phase 4 этот crate владеет media pipeline и playback tick:
//! demux loop, audio throttle, video backpressure и A/V scheduler не живут в UI shell.

#![forbid(unsafe_code)]

mod command;
mod decoder_boundary;
mod diagnostics;
mod error;
mod event;
mod media_opening;
mod pipeline;
mod render_lease_bridge;
mod scrub_command;
mod scrub_driver;
mod seek_controller;
mod seek_state;
mod session;
mod snapshot;
mod state;
mod tick;
mod video_backend;
mod worker;
mod worker_scheduler;

pub use command::{
    MediaOpenRequest, MediaSource, PlayerCommand, QualityId, QualitySelection, ScrubCommitPolicy,
    ScrubGeneration, SeekMode, SeekRequest, SeekTarget,
};
pub(crate) use command::{ScrubCommitIntent, ScrubUpdateIntent, SessionScrubCommand};
#[cfg(test)]
pub(crate) use decoder_boundary::DecodeBackpressureReason;
pub(crate) use decoder_boundary::{
    DecodeSendError, DecodeThreadError, DecoderResourceSnapshot, PlayerDecodePacket,
    PlayerVideoDecoderThreadHandle,
};
pub use decoder_boundary::{
    PlayerVideoDecoderThreadConfig, WgpuRenderTextureProvider, WgpuRenderTextureProviderHandle,
    WgpuRenderTextureViewLookup, WgpuRenderTextureViews,
};
pub(crate) use diagnostics::{
    ActiveSeekDiagnosticsSnapshot, PlaybackDiagnostics, SeekProgressBlocker,
};
pub use diagnostics::{
    DecoderControlChannelPressureSnapshot, DecoderFramePublishPressureSnapshot,
    LatencyCounterSnapshot, PipelineLatencyCountersSnapshot, PipelineLatencySampleSnapshot,
    PipelineLatencyStage, PipelinePauseCountersSnapshot, PipelinePauseReason,
    PipelinePauseSnapshot, PipelineQueueDepthSnapshot, PlaybackDiagnosticsLogSummary,
    PlaybackDiagnosticsSnapshot, TextureSlotPressureSnapshot, VideoDropAttributionSnapshot,
    VideoDropCountersSnapshot, VideoDropReason, WorkerFrameTimingSnapshot,
    WorkerWakeupDiagnosticsSnapshot, WorkerWakeupReason,
};
pub use error::{PlayerError, PlayerErrorKind, PlayerResult};
pub use event::{
    BufferingState, CapabilitySummary, FramePresentationInfo, MediaSummary, PlayerEvent,
};
pub use media_core::TrackId;
pub use media_opening::{PreparedMedia, PreparedMediaSource};
pub(crate) use pipeline::{PendingAudioPacket, PendingVideoPacket, PlaybackPipeline};
pub use render_lease_bridge::{
    PlayerPresentFrame, PresentFrameLease, PresentFrameTextureViewLookup, PresentFrameTextureViews,
    PresentFrameWgpuTextureViews,
};
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
pub use video_backend::{StartedVideoBackend, VideoBackendFactory};
pub use worker::{
    PlayerCommandSender, PlayerRenderError, PlayerRenderErrorKind, PlayerWorker,
    PlayerWorkerConfig, PlayerWorkerEvent, PlayerWorkerJoinError, PlayerWorkerSendError,
};
