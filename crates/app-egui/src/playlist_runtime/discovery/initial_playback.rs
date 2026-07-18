//! Runtime owner отложенного старта с первого элемента новой directory queue.

use playlist_discovery::{
    AdmissionAdvanced, AdmissionDirection, DirectoryManifest, DiscoveryCancellationCause,
    DiscoveryFinalOutcome, ManifestCandidateKey,
};

use super::ActiveDiscoveryScope;
use crate::playlist_runtime::controller::{
    ControllerInitialQueuePlaybackAction, InitialQueuePlaybackGuard,
    InitialQueuePlaybackGuardError, InitialQueuePlaybackPlanError, PlaylistController,
    SiblingDiscoveryScopeId,
};

/// Typed failure сохраняет различие load gate, committed identity и revision exhaustion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstalledTargetDiscoveryStartError {
    /// Playlist load decision ещё не создал controller.
    LoadDecisionPending,
    /// Exact Installed target не связан с committed Item ID.
    MissingCommittedTarget,
    /// Controller не смог выделить следующий discovery continuation.
    DiscoveryContinuationUnavailable,
    /// Paused initial-playback guard не соответствует текущей lineage.
    InitialPlaybackGuard(InitialQueuePlaybackGuardError),
}

impl std::fmt::Display for InstalledTargetDiscoveryStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoadDecisionPending => formatter.write_str("playlist load decision is pending"),
            Self::MissingCommittedTarget => {
                formatter.write_str("installed explicit target has no committed Item ID")
            }
            Self::DiscoveryContinuationUnavailable => {
                formatter.write_str("cannot allocate discovery continuation")
            }
            Self::InitialPlaybackGuard(error) => {
                write!(formatter, "cannot guard initial queue playback: {error:?}")
            }
        }
    }
}

impl std::error::Error for InstalledTargetDiscoveryStartError {}

/// Pending guard остаётся привязан к одному exact discovery scope.
struct PendingInitialPlayback {
    scope_id: Option<SiblingDiscoveryScopeId>,
    guard: InitialQueuePlaybackGuard,
    ready: bool,
}

/// Process-lifetime state хранит максимум один guarded pending intent.
#[derive(Default)]
pub(super) struct InitialQueuePlaybackCoordinator {
    pending: Option<PendingInitialPlayback>,
}

impl InitialQueuePlaybackCoordinator {
    /// Новый explicit open полностью supersede-ит прежний pending slot.
    pub(super) fn arm_waiting(
        &mut self,
        scope_id: SiblingDiscoveryScopeId,
        guard: InitialQueuePlaybackGuard,
    ) {
        self.pending = Some(PendingInitialPlayback {
            scope_id: Some(scope_id),
            guard,
            ready: false,
        });
    }

    /// Target-only fallback не ждёт marker-а отсутствующего discovery job-а.
    pub(super) fn arm_ready_without_scope(&mut self, guard: InitialQueuePlaybackGuard) {
        self.pending = Some(PendingInitialPlayback {
            scope_id: None,
            guard,
            ready: true,
        });
    }

    /// Manifest доказывает немедленный старт, когда explicit target уже первый raw candidate.
    pub(super) fn observe_manifest(
        &mut self,
        scope_id: SiblingDiscoveryScopeId,
        manifest: &DirectoryManifest,
        target_key: ManifestCandidateKey,
    ) {
        let target_is_first = manifest
            .records()
            .first()
            .is_some_and(|record| record.candidate_key() == target_key);
        if target_is_first {
            self.mark_scope_ready(scope_id);
        }
    }

    /// Exhausted Before frontier означает, что новых committed строк перед первой уже не будет.
    pub(super) fn observe_admission_advanced(
        &mut self,
        active: &ActiveDiscoveryScope,
        marker: AdmissionAdvanced,
    ) {
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        if pending.scope_id != Some(active.scope_id)
            || marker.job_id() != active.job.id()
            || marker.request_revision() != active.request_revision
            || marker.policy_revision() != Some(active.policy_revision)
            || marker.direction() != AdmissionDirection::Before
            || !marker.exhausted()
        {
            return;
        }
        self.mark_scope_ready(active.scope_id);
    }

    /// Terminal outcome либо завершает построение начала, либо отменяет неожиданный autoplay.
    pub(super) fn finish_scope(
        &mut self,
        scope_id: SiblingDiscoveryScopeId,
        outcome: DiscoveryFinalOutcome,
    ) {
        match outcome {
            DiscoveryFinalOutcome::Completed
            | DiscoveryFinalOutcome::LimitReached
            | DiscoveryFinalOutcome::ExecutorDisconnected
            | DiscoveryFinalOutcome::Cancelled(DiscoveryCancellationCause::UserCancelled) => {
                self.mark_scope_ready(scope_id);
            }
            DiscoveryFinalOutcome::Cancelled(
                DiscoveryCancellationCause::Superseded
                | DiscoveryCancellationCause::TransportStop
                | DiscoveryCancellationCause::StructuralInvalidation
                | DiscoveryCancellationCause::LifecycleSuspended
                | DiscoveryCancellationCause::LifecycleShutdown,
            ) => self.cancel_scope(scope_id),
        }
    }

    /// Ошибка manifest/executor оставляет target-only очередь и освобождает fallback start.
    pub(super) fn mark_scope_ready(&mut self, scope_id: SiblingDiscoveryScopeId) {
        if let Some(pending) = self.pending.as_mut()
            && pending.scope_id == Some(scope_id)
        {
            pending.ready = true;
        }
    }

    /// Stale scope не может удалить guard более нового explicit open-а.
    pub(super) fn cancel_scope(&mut self, scope_id: SiblingDiscoveryScopeId) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.scope_id == Some(scope_id))
        {
            self.pending = None;
        }
    }

    /// Shutdown/new open очищает ещё не применённый guarded intent.
    pub(super) fn cancel_all(&mut self) {
        self.pending = None;
    }

    /// Строит action в точке применения, после уже обработанных UI/transport intents этого frame-а.
    pub(super) fn take_ready_action(
        &mut self,
        controller: &mut PlaylistController,
    ) -> Option<ControllerInitialQueuePlaybackAction> {
        let pending = self.pending.take()?;
        if !pending.ready {
            self.pending = Some(pending);
            return None;
        }

        match controller.finish_initial_queue_playback(pending.guard) {
            Ok(action) => Some(action),
            Err(InitialQueuePlaybackPlanError::Superseded) => {
                // Более новый пользовательский intent намеренно побеждает deferred autoplay.
                None
            }
            Err(error) => {
                tracing::warn!(
                    error = ?error,
                    "Не удалось построить guarded start с начала новой очереди"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::path::PathBuf;

    use player_core::MediaInstanceId;
    use playlist_core::{
        CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistItemId, PlaylistMediaKind,
    };

    use super::*;
    use crate::playlist_runtime::controller::ControllerAppendOutcome;
    use crate::playlist_runtime::identity::{ActiveMediaIdentity, ActiveMediaLineageId};
    use crate::playlist_runtime::{PlaylistBindingGeneration, StablePlaybackIntent};

    /// Строит non-zero identity для локальных coordinator tests.
    fn non_zero(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("test identity must be non-zero")
    }

    /// Создаёт минимальную native-local строку без filesystem I/O.
    fn local_draft(path: &str) -> PlaylistItemDraft {
        PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from(path)),
            None,
            CachedPlaylistMetadata::new(path, PlaylistMediaKind::Video),
        )
    }

    /// Возвращает controller с paused target в середине committed queue.
    fn installed_middle_target() -> (PlaylistController, Vec<PlaylistItemId>) {
        let mut controller = PlaylistController::new();
        let outcome = controller
            .append(vec![
                local_draft("/media/new/01.mkv"),
                local_draft("/media/new/02.mkv"),
                local_draft("/media/new/03.mkv"),
            ])
            .expect("test append must succeed");
        let ControllerAppendOutcome::Added { item_ids, .. } = outcome else {
            panic!("test append must add rows");
        };
        let target_item_id = item_ids[1];
        controller
            .queue
            .set_traversal_current(target_item_id)
            .expect("target must be committed");
        controller.active_media = Some(ActiveMediaIdentity::installed(
            Some(target_item_id),
            ActiveMediaLineageId::from_non_zero(non_zero(1)),
            MediaInstanceId::from_non_zero(non_zero(2)),
            PlaylistBindingGeneration(3),
        ));
        (controller, item_ids)
    }

    /// Создаёт guard через controller boundary, а не конструирует opaque поля вручную.
    fn guarded_middle_target() -> (
        PlaylistController,
        Vec<PlaylistItemId>,
        InitialQueuePlaybackGuard,
    ) {
        let (mut controller, item_ids) = installed_middle_target();
        let guard = controller
            .begin_initial_queue_playback(StablePlaybackIntent::Playing)
            .expect("paused installed target must create guard");
        (controller, item_ids, guard)
    }

    #[test]
    fn matching_scope_releases_exactly_one_first_item_action() {
        let (mut controller, item_ids, guard) = guarded_middle_target();
        let scope_id = SiblingDiscoveryScopeId::from_non_zero(non_zero(10));
        let unrelated_scope_id = SiblingDiscoveryScopeId::from_non_zero(non_zero(11));
        let mut coordinator = InitialQueuePlaybackCoordinator::default();
        coordinator.arm_waiting(scope_id, guard);

        coordinator.mark_scope_ready(unrelated_scope_id);
        assert!(coordinator.take_ready_action(&mut controller).is_none());
        coordinator.mark_scope_ready(scope_id);

        let Some(ControllerInitialQueuePlaybackAction::InstallFirst { install, .. }) =
            coordinator.take_ready_action(&mut controller)
        else {
            panic!("matching scope must plan the first committed row");
        };
        assert_eq!(install.item_id, item_ids[0]);
        assert!(coordinator.take_ready_action(&mut controller).is_none());
    }

    #[test]
    fn target_only_fallback_does_not_wait_for_missing_discovery_scope() {
        let (mut controller, item_ids, guard) = guarded_middle_target();
        let mut coordinator = InitialQueuePlaybackCoordinator::default();
        coordinator.arm_ready_without_scope(guard);

        let Some(ControllerInitialQueuePlaybackAction::InstallFirst { install, .. }) =
            coordinator.take_ready_action(&mut controller)
        else {
            panic!("target-only fallback must still select the first committed row");
        };
        assert_eq!(install.item_id, item_ids[0]);
    }

    #[test]
    fn structural_cancellation_drops_deferred_start() {
        let (mut controller, _, guard) = guarded_middle_target();
        let scope_id = SiblingDiscoveryScopeId::from_non_zero(non_zero(20));
        let mut coordinator = InitialQueuePlaybackCoordinator::default();
        coordinator.arm_waiting(scope_id, guard);

        coordinator.finish_scope(
            scope_id,
            DiscoveryFinalOutcome::Cancelled(DiscoveryCancellationCause::StructuralInvalidation),
        );

        assert!(coordinator.take_ready_action(&mut controller).is_none());
    }

    #[test]
    fn newer_transport_intent_wins_before_ready_action_is_built() {
        let (mut controller, _, guard) = guarded_middle_target();
        let mut coordinator = InitialQueuePlaybackCoordinator::default();
        coordinator.arm_ready_without_scope(guard);
        let _newer_dispatch = controller
            .record_stable_transport_intent(
                StablePlaybackIntent::Paused,
                crate::playlist_runtime::TransportActionOrigin::Ui,
            )
            .expect("newer user intent revision must advance");

        assert!(coordinator.take_ready_action(&mut controller).is_none());
    }

    #[test]
    fn explicit_cancel_keeps_committed_target_only_fallback_playable() {
        let (mut controller, item_ids, guard) = guarded_middle_target();
        let scope_id = SiblingDiscoveryScopeId::from_non_zero(non_zero(30));
        let mut coordinator = InitialQueuePlaybackCoordinator::default();
        coordinator.arm_waiting(scope_id, guard);

        coordinator.finish_scope(
            scope_id,
            DiscoveryFinalOutcome::Cancelled(DiscoveryCancellationCause::UserCancelled),
        );

        let Some(ControllerInitialQueuePlaybackAction::InstallFirst { install, .. }) =
            coordinator.take_ready_action(&mut controller)
        else {
            panic!("user cancellation must release playback from committed queue beginning");
        };
        assert_eq!(install.item_id, item_ids[0]);
    }
}
