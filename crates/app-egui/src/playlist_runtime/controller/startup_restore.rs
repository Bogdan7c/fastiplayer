//! D04/D22 restored-current preparation и bounded paused fallback policy.

use std::num::NonZeroU64;
use std::sync::Arc;

use player_core::{MediaInstallRequestId, PlaybackIntent, PlaybackIntentRevision};
use playlist_core::{
    AutomaticEndedIntent, AutomaticTraversalAdvance, AutomaticTraversalPlan,
    AutomaticTraversalStart, PlaylistItemId, RepeatMode,
};

use super::PlaylistController;
use super::automatic_lifecycle::{AutomaticStopCause, PlaylistErrorBehavior};
use super::install::{
    PlaylistInstallAdmissionError, PlaylistInstallMutation, PlaylistInstallRequest,
};
use super::transport::PlannedPlaylistInstall;
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::identity::{
    PendingTargetOrigin, PlaylistItemErrorCategory, PlaylistItemErrorPhase,
};

/// Startup open всегда явно говорит, оставлять начало или восстанавливать checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupPosition {
    KeepStart,
    Restore(std::time::Duration),
}

/// Opaque startup target сохраняет exact traversal mutation до `Installed`.
pub(crate) struct StartupRestoreTarget {
    pub(crate) locator: playlist_core::PlaylistLocator,
    pub(crate) position: StartupPosition,
    pub(super) install: PlannedPlaylistInstall,
}

impl StartupRestoreTarget {
    /// Correlation ID существующей persisted row; allocator здесь не вызывается.
    pub(crate) const fn item_id(&self) -> PlaylistItemId {
        self.install.item_id
    }

    /// Restore и каждый D22 fallback несут неизменный paused intent.
    pub(crate) const fn playback_intent(&self) -> PlaybackIntent {
        self.install.playback_intent
    }

    /// Position policy читается strong-open транзакцией до передачи domain install plan.
    pub(crate) const fn position(&self) -> StartupPosition {
        self.position
    }

    pub(crate) fn set_position(&mut self, position: StartupPosition) {
        self.position = position;
    }
}

/// Результат D22 preparation failure для одного restored target.
pub(crate) enum StartupRestoreFailureOutcome {
    Stopped { cause: AutomaticStopCause },
    OpenItem { target: StartupRestoreTarget },
}

impl PlaylistController {
    /// Pre-barrier media-open terminal продолжает тот же D22 chain; enqueue-win остаётся fatal.
    pub(crate) fn report_startup_restore_install_failure(
        &mut self,
        request_id: MediaOpenRequestId,
        safe_summary: Arc<str>,
    ) -> Option<StartupRestoreFailureOutcome> {
        let request = self.take_awaiting_startup_restore_failure(request_id)?;
        let item_id = request.target_item_id?;
        let locator = self.queue.item(item_id)?.locator().clone();
        Some(self.report_startup_restore_failure(
            StartupRestoreTarget {
                locator,
                position: StartupPosition::KeepStart,
                install: PlannedPlaylistInstall {
                    item_id,
                    playback_intent: PlaybackIntent::StartPaused,
                    intent_revision: request.intent_revision,
                    pending_origin: request.origin,
                    expected_queue_revision: request.expected_queue_revision,
                    mutation: request.mutation,
                },
            },
            safe_summary,
        ))
    }

    /// Коррелирует opaque restore target с player staging, не меняя traversal до Installed.
    pub(crate) fn accept_startup_restore_install(
        &mut self,
        request_id: MediaOpenRequestId,
        player_request_id: MediaInstallRequestId,
        target: StartupRestoreTarget,
    ) -> Result<(), PlaylistInstallAdmissionError> {
        let planned = target.install;
        self.accept_install_request(PlaylistInstallRequest {
            request_id,
            player_request_id,
            target_item_id: Some(planned.item_id),
            origin: planned.pending_origin,
            intent_revision: planned.intent_revision,
            expected_queue_revision: planned.expected_queue_revision,
            mutation: planned.mutation,
        })
    }

    /// Возвращает persisted current без выбора первого элемента при `current=None`.
    pub(crate) fn startup_restored_current(&self) -> Option<StartupRestoreTarget> {
        let item_id = self.queue.traversal_current()?.item_id();
        let item = self.queue.item(item_id)?;
        Some(StartupRestoreTarget {
            locator: item.locator().clone(),
            position: StartupPosition::KeepStart,
            install: self.planned_startup_restore_install(
                item_id,
                PlaylistInstallMutation::Reserved(
                    playlist_core::ReservedQueueMutation::select_committed(item_id),
                ),
            ),
        })
    }

    /// D22 сохраняет unavailable row и строит bounded domain traversal только при Skip.
    pub(crate) fn report_startup_restore_failure(
        &mut self,
        failed: StartupRestoreTarget,
        safe_summary: Arc<str>,
    ) -> StartupRestoreFailureOutcome {
        let failed_item_id = failed.install.item_id;
        self.upsert_runtime_error(
            failed_item_id,
            PlaylistItemErrorPhase::Preparation,
            PlaylistItemErrorCategory::Unavailable,
            safe_summary,
            None,
            None,
        );
        if self.repeat_mode == RepeatMode::RepeatOne {
            return StartupRestoreFailureOutcome::Stopped {
                cause: AutomaticStopCause::RepeatOneError,
            };
        }
        if self.error_behavior == PlaylistErrorBehavior::Stop {
            return StartupRestoreFailureOutcome::Stopped {
                cause: AutomaticStopCause::ErrorPolicy,
            };
        }

        let traversal = match failed.install.mutation {
            PlaylistInstallMutation::Reserved(_) => self
                .queue
                .begin_automatic_error_traversal(AutomaticEndedIntent::new(self.repeat_mode)),
            PlaylistInstallMutation::AutomaticTraversal(plan) => {
                match self.queue.advance_automatic_traversal_after_failure(*plan) {
                    AutomaticTraversalAdvance::OpenItem { item_id, plan } => {
                        return self.startup_restore_open_item(item_id, plan);
                    }
                    AutomaticTraversalAdvance::AllFailed { attempted_count } => {
                        return StartupRestoreFailureOutcome::Stopped {
                            cause: AutomaticStopCause::AllCandidatesFailed { attempted_count },
                        };
                    }
                }
            }
            PlaylistInstallMutation::ManualNavigation => {
                return StartupRestoreFailureOutcome::Stopped {
                    cause: AutomaticStopCause::StructuralInvalidation,
                };
            }
        };
        match traversal {
            AutomaticTraversalStart::OpenItem { item_id, plan } => {
                self.startup_restore_open_item(item_id, plan)
            }
            AutomaticTraversalStart::ReplayCurrent { .. } => {
                StartupRestoreFailureOutcome::Stopped {
                    cause: AutomaticStopCause::RepeatOneError,
                }
            }
            AutomaticTraversalStart::Stop(reason) => StartupRestoreFailureOutcome::Stopped {
                cause: AutomaticStopCause::Domain(reason),
            },
        }
    }

    fn startup_restore_open_item(
        &self,
        item_id: PlaylistItemId,
        plan: Box<AutomaticTraversalPlan>,
    ) -> StartupRestoreFailureOutcome {
        let Some(item) = self.queue.item(item_id) else {
            return StartupRestoreFailureOutcome::Stopped {
                cause: AutomaticStopCause::StructuralInvalidation,
            };
        };
        StartupRestoreFailureOutcome::OpenItem {
            target: StartupRestoreTarget {
                locator: item.locator().clone(),
                position: StartupPosition::KeepStart,
                install: self.planned_startup_restore_install(
                    item_id,
                    PlaylistInstallMutation::AutomaticTraversal(plan),
                ),
            },
        }
    }

    fn planned_startup_restore_install(
        &self,
        item_id: PlaylistItemId,
        mutation: PlaylistInstallMutation,
    ) -> PlannedPlaylistInstall {
        PlannedPlaylistInstall {
            item_id,
            playback_intent: PlaybackIntent::StartPaused,
            intent_revision: PlaybackIntentRevision::from_non_zero(
                NonZeroU64::new(self.stable_intent_revision)
                    .expect("controller stable intent revision remains non-zero"),
            ),
            pending_origin: PendingTargetOrigin::RestoredCurrent,
            expected_queue_revision: self.queue.revision_snapshot(),
            mutation,
        }
    }
}
