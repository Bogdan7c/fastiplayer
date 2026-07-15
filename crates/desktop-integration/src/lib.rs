//! Desktop integration boundary для media controls.
//!
//! Crate намеренно видит только public `player-core` contracts: команды уходят через
//! `PlayerCommand`, а состояние читается из read-only `PlayerSnapshot`. Linux MPRIS
//! живёт в platform backend и не протекает в `player-core`.

#![forbid(unsafe_code)]

mod command;
mod error;
mod event;
mod platform;
mod runtime;
mod shutdown;
mod snapshot;

pub use command::{
    DesktopCommandSink, MPRIS_CURRENT_TRACK_ID, MprisCommand, MprisSetPositionRequest,
    map_mpris_command_to_player_commands,
};
pub use error::{DesktopIntegrationError, DesktopIntegrationResult};
pub use event::{DesktopBackendKind, DesktopIntegrationEvent};
pub use runtime::{DesktopIntegration, LatestSnapshotHandle, LatestSnapshotSource};
pub use shutdown::{DesktopIntegrationShutdownOutcome, DesktopIntegrationShutdownTransportFailure};
pub use snapshot::{
    DesktopMetadata, DesktopPlaybackStatus, DesktopSnapshotChange, DesktopSnapshotView,
};
