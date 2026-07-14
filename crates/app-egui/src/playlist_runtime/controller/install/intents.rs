//! Latest-only transport/lifecycle intents и terminal drain vocabulary D08/D58.

use playlist_core::{ManualNavigationDirection, PlaylistItemId, RepeatMode};

use super::PlaylistControllerInvariantViolation;
use crate::media_open::{MediaOpenRequestId, PlayerDispatchRejection};
use crate::playlist_runtime::identity::{ActiveMediaIdentity, TransportActionOrigin};
use crate::playlist_runtime::view::PlaylistDirtySignal;

/// Persistent queue modes и runtime protected generation coalesce-ятся в один value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DesiredQueueModes {
    pub repeat_mode: RepeatMode,
    pub shuffle_enabled: bool,
    pub protected_runtime_generation: u64,
}

/// Один deferred intent вместо command FIFO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredControllerIntent {
    Transport(DeferredTransportIntent),
    Suspend,
    Shutdown,
}

/// Latest-only post-commit transport intent; это не fast cursor Session 11C.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredTransportIntent {
    PlayItem {
        item_id: PlaylistItemId,
        origin: TransportActionOrigin,
    },
    Navigate {
        direction: ManualNavigationDirection,
        origin: TransportActionOrigin,
    },
    Stop {
        origin: TransportActionOrigin,
    },
    StopAfterCurrent {
        enabled: bool,
        origin: TransportActionOrigin,
    },
}

impl DeferredTransportIntent {
    const fn cancellation_cause(self) -> player_core::MediaInstallCancellationCause {
        match self {
            Self::Stop { .. } => player_core::MediaInstallCancellationCause::TransportStop,
            Self::StopAfterCurrent { .. } => {
                player_core::MediaInstallCancellationCause::StopAfterCurrent
            }
            Self::PlayItem { .. } | Self::Navigate { .. } => {
                player_core::MediaInstallCancellationCause::Superseded
            }
        }
    }
}

impl DeferredControllerIntent {
    fn priority(self) -> u8 {
        match self {
            Self::Transport(_) => 0,
            Self::Suspend => 1,
            Self::Shutdown => 2,
        }
    }

    pub(super) const fn cancellation_cause(self) -> player_core::MediaInstallCancellationCause {
        match self {
            Self::Transport(intent) => intent.cancellation_cause(),
            Self::Suspend => player_core::MediaInstallCancellationCause::LifecycleSuspended,
            Self::Shutdown => player_core::MediaInstallCancellationCause::LifecycleShutdown,
        }
    }
}

/// Dispatch-pending slot подчёркивает, что barrier winner ещё неизвестен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BarrierRaceIntent(pub(super) DeferredControllerIntent);

impl BarrierRaceIntent {
    pub(crate) const fn intent(self) -> DeferredControllerIntent {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleIntentOutcome {
    Immediate {
        intent: DeferredControllerIntent,
        aborted_request_id: Option<MediaOpenRequestId>,
        cancellation_cause: Option<player_core::MediaInstallCancellationCause>,
        mode_dirty: Option<PlaylistDirtySignal>,
    },
    CancelPendingRequest {
        request_id: MediaOpenRequestId,
        cause: player_core::MediaInstallCancellationCause,
        intent: DeferredControllerIntent,
    },
    AwaitAuthorizationResolution {
        request_id: MediaOpenRequestId,
    },
    AwaitInstalled {
        request_id: MediaOpenRequestId,
    },
    NoPendingInstall,
    Fatal(PlaylistControllerInvariantViolation),
}

/// Результат terminal drain уже соблюдает commit/abort -> modes -> intent ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControllerTerminalDrain {
    pub request_id: MediaOpenRequestId,
    pub active_media: Option<ActiveMediaIdentity>,
    pub dirty: Option<PlaylistDirtySignal>,
    pub deferred_intent: Option<DeferredControllerIntent>,
    pub resolution: ControllerTerminalResolution,
}

/// Cancellation/rejection не теряет cause после domain abort-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerTerminalResolution {
    Installed,
    CancelWonBeforePlayerEnqueue {
        cause: player_core::MediaInstallCancellationCause,
    },
    DownstreamRejectedBeforeEnqueue {
        rejection: PlayerDispatchRejection,
    },
}

pub(super) fn select_intent(
    existing: Option<DeferredControllerIntent>,
    incoming: DeferredControllerIntent,
) -> DeferredControllerIntent {
    match existing {
        Some(DeferredControllerIntent::Transport(_))
            if matches!(incoming, DeferredControllerIntent::Transport(_)) =>
        {
            incoming
        }
        Some(current) if current.priority() >= incoming.priority() => current,
        _ => incoming,
    }
}
