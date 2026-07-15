//! One-slot D53–D57 cursor поверх domain-owned manual navigation preview.

#[cfg(test)]
mod tests;

use player_core::MediaInstallCancellationCause;
use playlist_core::{
    DiscardedManualNavigationPreview, ManualNavigationDirection, ManualNavigationIntent,
    ManualNavigationNoItem, ManualNavigationOutcome, ManualNavigationPreview,
    ManualNavigationPreviewError, ManualNavigationPreviewState, PlaylistItemId, PlaylistQueue,
};

use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::identity::{ActiveMediaIdentity, TransportActionOrigin};

use super::PlaylistController;
use super::install::{
    ControllerInstallPhase, DeferredTransportIntent, PlaylistControllerInvariantViolation,
};
use super::transport::{
    ManualNavigationWaitId, PlannedPlaylistInstall, SiblingDiscoveryScopeId, TransportGuardOutcome,
};

/// Состояние origin, которое Session 12 сможет обновить после exact clean `Ended`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManualNavigationOriginState {
    Active,
    Ended,
}

/// Terminal действие после discard не запускает automatic policy повторно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManualNavigationTerminalAction {
    KeepActive,
    StopEndedOrigin,
}

/// Причина D57 остаётся отдельной от explicit user cancellation и supersede.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ManualNavigationInvalidation {
    pub cause: MediaInstallCancellationCause,
    pub request_id: Option<MediaOpenRequestId>,
    pub terminal_action: ManualNavigationTerminalAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManualNavigationFailureOutcome {
    AwaitingUserAfterFailure { item_id: PlaylistItemId },
    StaleRequest { request_id: MediaOpenRequestId },
    NotManualNavigation,
}

/// Retry различает отсутствие D55 target-а и уже выполняющийся install.
pub(crate) enum ManualNavigationRetryOutcome {
    StartInstall { install: PlannedPlaylistInstall },
    InstallAlreadyInProgress { request_id: MediaOpenRequestId },
    NoFailedTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreConcreteProbeRejectionOutcome {
    ContinueWaiting {
        wait_id: ManualNavigationWaitId,
        scope_id: SiblingDiscoveryScopeId,
    },
    StaleWait {
        wait_id: ManualNavigationWaitId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManualNavigationCancelOutcome {
    NoManualNavigation,
    Discarded(ManualNavigationInvalidation),
    CancelPending {
        request_id: MediaOpenRequestId,
        cause: MediaInstallCancellationCause,
        terminal_action: ManualNavigationTerminalAction,
    },
    AwaitAuthorizationResolution {
        request_id: MediaOpenRequestId,
    },
    AwaitInstalled {
        request_id: MediaOpenRequestId,
    },
    Fatal(PlaylistControllerInvariantViolation),
}

/// Результат продолжения единственного logical cursor-а.
pub(super) enum CursorStepOutcome {
    OpenItem {
        item_id: PlaylistItemId,
    },
    NoItem(ManualNavigationNoItem),
    Invalidated {
        error: ManualNavigationPreviewError,
        terminal_action: ManualNavigationTerminalAction,
    },
}

struct CursorContext {
    origin: TransportActionOrigin,
    active_origin: Option<ActiveMediaIdentity>,
    origin_state: ManualNavigationOriginState,
    request_id: Option<MediaOpenRequestId>,
}

struct CursorPreview {
    preview: ManualNavigationPreview,
    context: CursorContext,
}

/// Cursor владеет ровно одним preview; FIFO target-ов принципиально отсутствует.
#[derive(Default)]
pub(super) struct ManualNavigationCursor {
    preview: Option<CursorPreview>,
    prepared_context: Option<CursorContext>,
    retired_request_ids: [Option<MediaOpenRequestId>; 2],
}

impl ManualNavigationCursor {
    pub(super) fn has_state(&self) -> bool {
        self.preview.is_some() || self.prepared_context.is_some()
    }
    pub(super) fn begin(
        &mut self,
        preview: ManualNavigationPreview,
        origin: TransportActionOrigin,
        active_origin: Option<ActiveMediaIdentity>,
    ) {
        self.preview = Some(CursorPreview {
            preview,
            context: CursorContext {
                origin,
                active_origin,
                origin_state: ManualNavigationOriginState::Active,
                request_id: None,
            },
        });
        self.prepared_context = None;
    }

    pub(super) fn bind_request(&mut self, request_id: MediaOpenRequestId) -> bool {
        let context = self
            .preview
            .as_mut()
            .map(|cursor| &mut cursor.context)
            .or(self.prepared_context.as_mut());
        let Some(context) = context else {
            return false;
        };
        if let Some(previous_request_id) = context.request_id.replace(request_id)
            && previous_request_id != request_id
        {
            self.remember_retired_request(previous_request_id);
        }
        true
    }

    pub(super) fn matches_target(&self, item_id: Option<PlaylistItemId>) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|cursor| Some(cursor.preview.latest_target_item_id()) == item_id)
    }

    pub(super) fn request_id(&self) -> Option<MediaOpenRequestId> {
        self.preview
            .as_ref()
            .and_then(|cursor| cursor.context.request_id)
            .or_else(|| {
                self.prepared_context
                    .as_ref()
                    .and_then(|context| context.request_id)
            })
    }

    pub(super) fn is_retired_request(&self, request_id: MediaOpenRequestId) -> bool {
        self.retired_request_ids.contains(&Some(request_id))
    }

    pub(super) fn latest_target_item_id(&self) -> Option<PlaylistItemId> {
        self.preview
            .as_ref()
            .map(|cursor| cursor.preview.latest_target_item_id())
    }

    pub(super) fn state(&self) -> Option<ManualNavigationPreviewState> {
        self.preview.as_ref().map(|cursor| cursor.preview.state())
    }

    pub(super) fn origin(&self) -> Option<TransportActionOrigin> {
        self.preview
            .as_ref()
            .map(|cursor| cursor.context.origin)
            .or_else(|| self.prepared_context.as_ref().map(|context| context.origin))
    }

    pub(super) fn continue_in_direction(
        &mut self,
        queue: &PlaylistQueue,
        direction: ManualNavigationDirection,
        repeat_mode: playlist_core::RepeatMode,
    ) -> CursorStepOutcome {
        let Some(cursor) = self.preview.take() else {
            return CursorStepOutcome::NoItem(ManualNavigationNoItem::EmptyQueue);
        };
        let intent = match direction {
            ManualNavigationDirection::Next => ManualNavigationIntent::next(repeat_mode),
            ManualNavigationDirection::Previous => ManualNavigationIntent::previous(repeat_mode),
        };
        match queue.continue_manual_navigation(cursor.preview, intent) {
            Ok(ManualNavigationOutcome::OpenItem { item_id, preview }) => {
                self.preview = Some(CursorPreview {
                    preview,
                    context: cursor.context,
                });
                CursorStepOutcome::OpenItem { item_id }
            }
            Ok(ManualNavigationOutcome::NoItem(reason)) => {
                self.retire_context(cursor.context);
                CursorStepOutcome::NoItem(reason)
            }
            Err(error) => {
                let terminal_action = terminal_action(cursor.context.origin_state);
                self.retire_context(cursor.context);
                CursorStepOutcome::Invalidated {
                    error,
                    terminal_action,
                }
            }
        }
    }

    pub(super) fn take_for_prepare(&mut self) -> Option<ManualNavigationPreview> {
        let cursor = self.preview.take()?;
        self.prepared_context = Some(cursor.context);
        Some(cursor.preview)
    }

    pub(super) fn restore_after_abort(&mut self, preview: ManualNavigationPreview) {
        let context = self
            .prepared_context
            .take()
            .expect("manual token always has matching cursor context");
        self.preview = Some(CursorPreview { preview, context });
    }

    pub(super) fn mark_failed_after_abort(&mut self, preview: ManualNavigationPreview) {
        self.restore_after_abort(preview);
        let marked = self.mark_preview_failed_by_move();
        debug_assert!(marked, "restored manual preview must become D55 failure");
    }

    pub(super) fn mark_prepared_target_failed(&mut self) -> bool {
        self.mark_preview_failed_by_move()
    }

    fn mark_preview_failed_by_move(&mut self) -> bool {
        let Some(mut cursor) = self.preview.take() else {
            return false;
        };
        if let Some(request_id) = cursor.context.request_id.take() {
            self.remember_retired_request(request_id);
        }
        cursor.preview = cursor.preview.mark_latest_target_failed();
        self.preview = Some(cursor);
        true
    }

    pub(super) fn is_awaiting_user_after_failure(&self) -> bool {
        matches!(
            self.state(),
            Some(ManualNavigationPreviewState::AwaitingUserAfterFailure(_))
        )
    }

    pub(super) fn observe_origin_ended(&mut self, active: ActiveMediaIdentity) -> bool {
        let context = self
            .preview
            .as_mut()
            .map(|cursor| &mut cursor.context)
            .or(self.prepared_context.as_mut());
        let Some(context) = context else {
            return false;
        };
        if context.active_origin != Some(active)
            || context.origin_state == ManualNavigationOriginState::Ended
        {
            return false;
        }
        context.origin_state = ManualNavigationOriginState::Ended;
        true
    }

    pub(super) fn discard(
        &mut self,
        queue: &PlaylistQueue,
        cause: MediaInstallCancellationCause,
        request_id: Option<MediaOpenRequestId>,
    ) -> Option<ManualNavigationInvalidation> {
        let cursor = self.preview.take()?;
        let _discarded: DiscardedManualNavigationPreview =
            queue.discard_manual_navigation(cursor.preview);
        let terminal_action = terminal_action(cursor.context.origin_state);
        self.retire_context(cursor.context);
        Some(ManualNavigationInvalidation {
            cause,
            request_id,
            terminal_action,
        })
    }

    pub(super) fn commit_finished(&mut self) {
        if let Some(context) = self.prepared_context.take() {
            self.retire_context(context);
        }
        self.preview = None;
    }

    fn retire_context(&mut self, context: CursorContext) {
        if let Some(request_id) = context.request_id {
            self.remember_retired_request(request_id);
        }
    }

    fn remember_retired_request(&mut self, request_id: MediaOpenRequestId) {
        if self.retired_request_ids.contains(&Some(request_id)) {
            return;
        }
        self.retired_request_ids[0] = self.retired_request_ids[1];
        self.retired_request_ids[1] = Some(request_id);
    }
}

impl PlaylistController {
    /// Probe rejection до concrete row остаётся D50 search, а не становится D55 failure.
    pub(crate) fn report_pre_concrete_probe_rejection(
        &self,
        wait_id: ManualNavigationWaitId,
        scope_id: SiblingDiscoveryScopeId,
    ) -> PreConcreteProbeRejectionOutcome {
        match self.pending_manual_traversal {
            Some(wait)
                if wait.wait_id == wait_id
                    && wait.scope_id == scope_id
                    && self.active_media == Some(wait.active_media) =>
            {
                PreConcreteProbeRejectionOutcome::ContinueWaiting { wait_id, scope_id }
            }
            _ => PreConcreteProbeRejectionOutcome::StaleWait { wait_id },
        }
    }

    /// Concrete target failure сохраняет D55 preview и не запускает automatic continuation.
    pub(crate) fn report_manual_navigation_target_failure(
        &mut self,
        request_id: MediaOpenRequestId,
    ) -> ManualNavigationFailureOutcome {
        if self.manual_navigation_cursor.is_retired_request(request_id) {
            return ManualNavigationFailureOutcome::StaleRequest { request_id };
        }
        let Some((phase, current_request_id)) = self.manual_navigation_install_phase() else {
            return ManualNavigationFailureOutcome::NotManualNavigation;
        };
        if current_request_id != request_id || phase != ControllerInstallPhase::AwaitingReady {
            return ManualNavigationFailureOutcome::StaleRequest { request_id };
        }
        if let Err(violation) = self.retire_awaiting_manual_navigation_request(request_id) {
            self.set_fatal(violation);
            return ManualNavigationFailureOutcome::StaleRequest { request_id };
        }
        if !self.manual_navigation_cursor.mark_prepared_target_failed() {
            return ManualNavigationFailureOutcome::NotManualNavigation;
        }
        let item_id = self
            .manual_navigation_cursor
            .latest_target_item_id()
            .expect("failed manual preview retains concrete target");
        ManualNavigationFailureOutcome::AwaitingUserAfterFailure { item_id }
    }

    /// Retry повторяет exact failed target без cursor step или automatic reevaluation.
    pub(crate) fn retry_failed_manual_navigation(&mut self) -> ManualNavigationRetryOutcome {
        if let Some(state) = self.install_state.as_ref() {
            return ManualNavigationRetryOutcome::InstallAlreadyInProgress {
                request_id: state.request_id(),
            };
        }
        if !self
            .manual_navigation_cursor
            .is_awaiting_user_after_failure()
        {
            return ManualNavigationRetryOutcome::NoFailedTarget;
        }
        let item_id = self
            .manual_navigation_cursor
            .latest_target_item_id()
            .expect("failed preview retains target");
        let origin = self
            .manual_navigation_cursor
            .origin()
            .expect("failed preview retains transport origin");
        ManualNavigationRetryOutcome::StartInstall {
            install: self.planned_manual_install(item_id, origin),
        }
    }

    /// Session 12 передаст сюда только exact matching clean `Ended` edge.
    pub(crate) fn mark_manual_navigation_origin_ended(
        &mut self,
        active: ActiveMediaIdentity,
    ) -> bool {
        self.manual_navigation_cursor.observe_origin_ended(active)
    }

    /// Explicit Cancel использует отдельную cause и не arm-ит future stop для active origin.
    pub(crate) fn cancel_manual_navigation(&mut self) -> ManualNavigationCancelOutcome {
        let outcome = self.cancel_manual_navigation_inner();
        let terminal_action = match &outcome {
            ManualNavigationCancelOutcome::Discarded(invalidation) => {
                Some(invalidation.terminal_action)
            }
            ManualNavigationCancelOutcome::CancelPending {
                terminal_action, ..
            } => Some(*terminal_action),
            _ => None,
        };
        if let Some(action) = terminal_action {
            self.consume_manual_terminal_action(
                action,
                super::automatic_lifecycle::AutomaticStopCause::ManualTraversalCancelled,
            );
        }
        outcome
    }

    fn cancel_manual_navigation_inner(&mut self) -> ManualNavigationCancelOutcome {
        if let Some((phase, request_id)) = self.manual_navigation_install_phase() {
            match phase {
                ControllerInstallPhase::AwaitingReady => {
                    if let Err(violation) =
                        self.retire_awaiting_manual_navigation_request(request_id)
                    {
                        return ManualNavigationCancelOutcome::Fatal(violation);
                    }
                    let Some(discarded) = self.manual_navigation_cursor.discard(
                        &self.queue,
                        MediaInstallCancellationCause::UserCancelled,
                        Some(request_id),
                    ) else {
                        return ManualNavigationCancelOutcome::NoManualNavigation;
                    };
                    return ManualNavigationCancelOutcome::CancelPending {
                        request_id,
                        cause: discarded.cause,
                        terminal_action: discarded.terminal_action,
                    };
                }
                ControllerInstallPhase::ReservedAwaitingAuthorization => {
                    if let Err(violation) =
                        self.abort_reserved_manual_navigation_before_dispatch(request_id)
                    {
                        return ManualNavigationCancelOutcome::Fatal(violation);
                    }
                    return self
                        .manual_navigation_cursor
                        .discard(
                            &self.queue,
                            MediaInstallCancellationCause::UserCancelled,
                            Some(request_id),
                        )
                        .map(ManualNavigationCancelOutcome::Discarded)
                        .unwrap_or(ManualNavigationCancelOutcome::NoManualNavigation);
                }
                ControllerInstallPhase::AuthorizationDispatchPending
                | ControllerInstallPhase::AuthorizationInFlight => {
                    return match self
                        .request_transport_guard(DeferredTransportIntent::CancelManualNavigation)
                    {
                        TransportGuardOutcome::AwaitAuthorizationResolution { request_id } => {
                            ManualNavigationCancelOutcome::AwaitAuthorizationResolution {
                                request_id,
                            }
                        }
                        TransportGuardOutcome::AwaitInstalled { request_id } => {
                            ManualNavigationCancelOutcome::AwaitInstalled { request_id }
                        }
                        TransportGuardOutcome::Fatal(violation) => {
                            ManualNavigationCancelOutcome::Fatal(violation)
                        }
                        TransportGuardOutcome::ExecuteNow { .. }
                        | TransportGuardOutcome::CancelPendingThenExecute { .. } => {
                            unreachable!("dispatch/in-flight cancel waits for authoritative winner")
                        }
                    };
                }
            }
        }
        self.manual_navigation_cursor
            .discard(
                &self.queue,
                MediaInstallCancellationCause::UserCancelled,
                None,
            )
            .map(ManualNavigationCancelOutcome::Discarded)
            .unwrap_or(ManualNavigationCancelOutcome::NoManualNavigation)
    }
}

fn terminal_action(origin_state: ManualNavigationOriginState) -> ManualNavigationTerminalAction {
    match origin_state {
        ManualNavigationOriginState::Active => ManualNavigationTerminalAction::KeepActive,
        ManualNavigationOriginState::Ended => ManualNavigationTerminalAction::StopEndedOrigin,
    }
}
