//! Passive vocabulary install state machine-а.
//!
//! Controller owner сохраняет transition authority, reservation operations и exact
//! порядок `Ready` → authorization → `Installed`; здесь находятся только payload-ы
//! фаз и чистые read-only projections.

use player_core::{MediaInstallRequestId, PlaybackIntentRevision};
use playlist_core::{
    AutomaticTraversalPlan, PrepareReservedMutationError, QueueRevisionSnapshot,
    ReservedQueueMutation,
};

use super::super::PlaylistController;
use super::intents::{BarrierRaceIntent, DeferredControllerIntent, DesiredQueueModes};
use super::token::GuardedInstallToken;
use crate::media_open::{MediaOpenClientKey, MediaOpenRequestId};
use crate::playlist_runtime::identity::PendingTargetOrigin;

/// Controller выбирает command semantics; coordinator остаётся policy-neutral executor-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerMediaOpenDisposition {
    Start,
    Coalesce,
    Supersede {
        expected_request_id: MediaOpenRequestId,
    },
}

/// Opaque controller command не содержит queue target/priority для coordinator-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerMediaOpenCommand {
    Start {
        client_key: MediaOpenClientKey,
    },
    Coalesce {
        client_key: MediaOpenClientKey,
    },
    Supersede {
        expected_request_id: MediaOpenRequestId,
        client_key: MediaOpenClientKey,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerMediaOpenCommandError {
    WorkerUnavailable,
    InstallCommitLinearizing,
    FatalInvariant,
}

/// Admission результата coordinator-а не превращает ожидаемый busy/supersede race в fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistInstallAdmissionError {
    Busy,
    StaleSupersede,
    InstallCommitLinearizing,
    FatalInvariant,
}

/// Все queue-specific данные остаются в controller после neutral coordinator admission.
pub(crate) struct PlaylistInstallRequest {
    pub request_id: MediaOpenRequestId,
    pub player_request_id: MediaInstallRequestId,
    pub target_item_id: Option<playlist_core::PlaylistItemId>,
    pub origin: PendingTargetOrigin,
    pub intent_revision: PlaybackIntentRevision,
    pub expected_queue_revision: QueueRevisionSnapshot,
    pub mutation: PlaylistInstallMutation,
}

/// Controller сохраняет domain-owned manual preview вплоть до exact Installed.
pub(crate) enum PlaylistInstallMutation {
    /// Обычный explicit select/replacement reservation.
    Reserved(ReservedQueueMutation),
    /// One-step manual navigation, включая private shuffle history/upcoming preview.
    ManualNavigation,
    /// Opaque fixed-snapshot automatic traversal plan.
    AutomaticTraversal(Box<AutomaticTraversalPlan>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerInstallPhase {
    AwaitingReady,
    ReservedAwaitingAuthorization,
    AuthorizationDispatchPending,
    AuthorizationInFlight,
}

/// Post-player-receipt playback intent, который может завершить playlist install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstalledPlaybackIntentCompletion {
    /// Caller не владеет authoritative playback-intent receipt этого install-а.
    PreserveCurrent,
    /// Player уже принял exact staged intent; controller применяет его с revision fence.
    Authoritative(super::super::StablePlaybackIntent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistControllerInvariantViolation {
    LoadDecisionPending,
    StaleReadyToCommit,
    UnexpectedInstallPhase,
    MissingAuthorizationResolution,
    MissingInstalledTerminal,
    TerminalRequestMismatch,
    PlayerRequestMismatch,
    DirtyRevisionExhaustedAfterPlayerCommit,
    LineageIdentityExhausted,
    DeferredModeApplicationFailed,
    PostInstalledCandidateReleaseFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallReadyOutcome {
    RequestAuthorization {
        request_id: MediaOpenRequestId,
    },
    ReservationRejected {
        request_id: MediaOpenRequestId,
        error: PrepareReservedMutationError,
    },
    StaleManualNavigationResult {
        request_id: MediaOpenRequestId,
    },
    Fatal(PlaylistControllerInvariantViolation),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AuthorizationDispatchStart {
    pub(super) request_id: MediaOpenRequestId,
}

impl AuthorizationDispatchStart {
    pub(crate) const fn request_id(self) -> MediaOpenRequestId {
        self.request_id
    }
}

pub(in crate::playlist_runtime::controller) enum InstallState {
    AwaitingReady(AwaitingReady),
    ReservedAwaitingAuthorization(GuardedInstall),
    AuthorizationDispatchPending {
        guarded: GuardedInstall,
        race_intent: Option<BarrierRaceIntent>,
    },
    AuthorizationInFlight {
        guarded: GuardedInstall,
        post_commit_intent: Option<DeferredControllerIntent>,
    },
}

impl InstallState {
    pub(in crate::playlist_runtime::controller) const fn holds_reservation(&self) -> bool {
        !matches!(self, Self::AwaitingReady(_))
    }

    pub(super) const fn phase(&self) -> ControllerInstallPhase {
        match self {
            Self::AwaitingReady(_) => ControllerInstallPhase::AwaitingReady,
            Self::ReservedAwaitingAuthorization(_) => {
                ControllerInstallPhase::ReservedAwaitingAuthorization
            }
            Self::AuthorizationDispatchPending { .. } => {
                ControllerInstallPhase::AuthorizationDispatchPending
            }
            Self::AuthorizationInFlight { .. } => ControllerInstallPhase::AuthorizationInFlight,
        }
    }

    pub(in crate::playlist_runtime::controller) const fn request_id(&self) -> MediaOpenRequestId {
        match self {
            Self::AwaitingReady(state) => state.request.request_id,
            Self::ReservedAwaitingAuthorization(state)
            | Self::AuthorizationDispatchPending { guarded: state, .. }
            | Self::AuthorizationInFlight { guarded: state, .. } => state.request_id,
        }
    }

    pub(in crate::playlist_runtime::controller) const fn player_request_id(
        &self,
    ) -> MediaInstallRequestId {
        match self {
            Self::AwaitingReady(state) => state.request.player_request_id,
            Self::ReservedAwaitingAuthorization(state)
            | Self::AuthorizationDispatchPending { guarded: state, .. }
            | Self::AuthorizationInFlight { guarded: state, .. } => state.player_request_id,
        }
    }
}

pub(in crate::playlist_runtime::controller) struct AwaitingReady {
    pub(super) request: PlaylistInstallRequest,
}

pub(in crate::playlist_runtime::controller) struct GuardedInstall {
    pub(super) request_id: MediaOpenRequestId,
    pub(super) player_request_id: MediaInstallRequestId,
    pub(super) target_item_id: Option<playlist_core::PlaylistItemId>,
    pub(super) intent_revision: PlaybackIntentRevision,
    pub(super) token: GuardedInstallToken,
    pub(super) desired_modes: Option<DesiredQueueModes>,
    pub(super) queue_revision_before_commit: QueueRevisionSnapshot,
}

impl PlaylistController {
    /// Возвращает read-only фазу install protocol-а без права выполнить transition.
    pub(crate) fn install_phase(&self) -> Option<ControllerInstallPhase> {
        self.install_state.as_ref().map(InstallState::phase)
    }

    /// Возвращает exact coordinator request текущего install guard-а без раскрытия token-а.
    pub(crate) fn install_request_id(&self) -> Option<MediaOpenRequestId> {
        self.install_state.as_ref().map(InstallState::request_id)
    }

    /// D52 update остаётся разрешённым: controller только подтверждает exact request correlation.
    pub(crate) fn accepts_playback_intent_update(&self, request_id: MediaOpenRequestId) -> bool {
        self.install_state
            .as_ref()
            .is_some_and(|state| state.request_id() == request_id)
    }
}
