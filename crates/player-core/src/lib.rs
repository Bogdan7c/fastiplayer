//! Core-контракт и runtime state machine плеера.
//!
//! После Phase 4 этот crate владеет media pipeline и playback tick:
//! demux loop, audio throttle, video backpressure и A/V scheduler не живут в UI shell.

#![forbid(unsafe_code)]

mod command;
mod error;
mod event;
mod pipeline;
mod session;
mod snapshot;
mod state;
mod tick;

pub use command::{
    MediaOpenRequest, MediaSource, PlayerCommand, QualityId, QualitySelection, SeekMode,
    SeekRequest,
};
pub use error::{PlayerError, PlayerErrorKind, PlayerResult};
pub use event::{
    BufferingState, CapabilitySummary, FramePresentationInfo, MediaSummary, PlayerEvent,
};
pub use media_core::TrackId;
pub use pipeline::{PendingAudioPacket, PendingVideoPacket, PlaybackPipeline};
pub use session::PlayerSession;
pub use snapshot::{
    AudioBufferSnapshot, BackendSnapshot, FrameCounters, PlayerSnapshot, QualitySummary,
    QueueSnapshot, TexturePoolSnapshot, TrackSelectionSnapshot, TrackSummarySnapshot,
    VideoFrameSnapshot,
};
pub use state::PlaybackState;
pub use tick::{
    PlayerTickConfig, PlayerTickContext, PlayerTickPacket, PlayerTickResult, PlayerVideoDropReason,
    PlayerVideoFrameDrop,
};
