//! Desktop integration boundary для media controls.
//!
//! Crate владеет только neutral desktop vocabulary и Linux MPRIS codec. Player,
//! playlist/controller и process lease остаются за app composition root.

#![forbid(unsafe_code)]

mod command;
mod error;
mod event;
mod platform;
mod runtime;
mod shutdown;
mod snapshot;

pub use command::{
    DesktopCommand, DesktopCommandRequestId, DesktopCommandSink, DesktopLoopStatus,
    DesktopTimelineSeekOutcome, DesktopTrackKey, DesktopTransportAction, EffectiveVolume,
    EffectiveVolumeError, TimelineSeekRequestId,
};
pub use error::{DesktopIntegrationError, DesktopIntegrationResult};
pub use event::{DesktopBackendKind, DesktopIntegrationEvent};
pub use runtime::{DesktopIntegration, LatestSnapshotHandle, LatestSnapshotSource};
pub use shutdown::{DesktopIntegrationShutdownOutcome, DesktopIntegrationShutdownTransportFailure};
pub use snapshot::{
    DesktopCapabilities, DesktopControlRevision, DesktopMetadata, DesktopPlaybackStatus,
    DesktopSeeked, DesktopSnapshotChange, DesktopSnapshotRevision, DesktopSnapshotView,
};
