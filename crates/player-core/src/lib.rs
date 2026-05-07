//! Минимальный core-контракт плеера.
//!
//! Phase 1 намеренно не переносит media pipeline из `app-egui`.
//! Этот crate фиксирует публичные типы команд, событий, snapshot'ов и состояний,
//! чтобы следующие фазы могли переносить логику маленькими проверяемыми шагами.

#![forbid(unsafe_code)]

mod command;
mod error;
mod event;
mod session;
mod snapshot;
mod state;

pub use command::{
    MediaOpenRequest, MediaSource, PlayerCommand, QualityId, QualitySelection, SeekMode,
    SeekRequest,
};
pub use error::{PlayerError, PlayerErrorKind, PlayerResult};
pub use event::{
    BufferingState, CapabilitySummary, FramePresentationInfo, MediaSummary, PlayerEvent,
};
pub use media_core::TrackId;
pub use session::PlayerSession;
pub use snapshot::{
    AudioBufferSnapshot, BackendSnapshot, FrameCounters, PlayerSnapshot, QualitySummary,
    QueueSnapshot, TexturePoolSnapshot, TrackSelectionSnapshot, VideoFrameSnapshot,
};
pub use state::PlaybackState;
