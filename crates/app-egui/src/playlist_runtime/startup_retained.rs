//! Bounded D65 intents, принятые после allocator gate во время startup install.
//!
//! Это не command FIFO: один queue plan переопределяет предыдущий по правилам
//! `StartupMutationDraft`, а terminal drain применяет победителя exactly once.

use std::time::Instant;

use playlist_core::PlaylistItemDraft;

use super::PlaylistRuntime;
use super::controller::{ControllerAppendError, ControllerAppendOutcome};
use super::removal_undo::RuntimeRemovalOutcome;
use super::startup::{StartupDraftError, StartupMutationDraft, StartupQueuePlan};

/// Process-lifetime owner post-gate draft-а одной startup install транзакции.
#[derive(Default)]
pub(super) struct StartupRetainedActionOwner {
    /// Retention открывается только вокруг renderer-bound startup install.
    active: bool,
    /// Те же bounded latest-wins правила, что у pre-gate draft-а.
    draft: StartupMutationDraft,
    /// Clear сохраняет исходное action-time для корректного Undo deadline.
    clear_requested_at: Option<Instant>,
}

/// Exactly-once результат применения победившего retained queue intent-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetainedStartupApplyOutcome {
    /// Structural intent отсутствовал либо был уже committed первым вызовом.
    NoAction,
    /// Победивший Clear прошёл обычный removal/domain boundary.
    Cleared,
    /// Bounded Add aggregate committed одной domain mutation.
    Added { item_count: usize },
    /// Prepared media replacement остаётся у своего preparation owner-а.
    MediaReplacementPending,
}

/// Terminal apply не сворачивает domain/fatal distinctions в `bool`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RetainedStartupApplyError {
    #[error("retained startup action lost canonical controller")]
    MissingController,
    #[error("retained startup Add failed: {0:?}")]
    Append(ControllerAppendError),
    #[error("retained startup Clear failed: {0:?}")]
    Clear(RuntimeRemovalOutcome),
}

impl StartupRetainedActionOwner {
    /// Новая startup install получает чистый bounded slot.
    pub(super) fn begin(&mut self) {
        self.active = true;
        self.draft = StartupMutationDraft::default();
        self.clear_requested_at = None;
    }

    /// `true` только пока old startup install ещё требует terminal resolution.
    pub(super) const fn is_active(&self) -> bool {
        self.active
    }

    /// Clear заменяет более ранние Add/media intents.
    pub(super) fn record_clear(&mut self, requested_at: Instant) -> Result<(), StartupDraftError> {
        self.draft.record_clear()?;
        self.clear_requested_at = Some(requested_at);
        Ok(())
    }

    /// Последний media replacement заменяет более ранний queue plan.
    pub(super) fn record_media_replacement(&mut self) -> Result<(), StartupDraftError> {
        self.draft.record_media_replacement()?;
        self.clear_requested_at = None;
        Ok(())
    }

    /// Prepared Adds coalesce-ятся до общего cap без Item ID allocation.
    pub(super) fn record_prepared_add(
        &mut self,
        drafts: Vec<PlaylistItemDraft>,
    ) -> Result<(), StartupDraftError> {
        self.draft.record_prepared_add(drafts)?;
        self.clear_requested_at = None;
        Ok(())
    }

    /// Успешный первый вызов не должен быть replay-нут после terminal old install-а.
    pub(super) fn mark_queue_action_committed(&mut self) {
        self.draft = StartupMutationDraft::default();
        self.clear_requested_at = None;
    }

    /// Fatal terminal закрывает retention без попытки мутировать uncertain domain state.
    pub(super) fn discard(&mut self) {
        self.active = false;
        self.mark_queue_action_committed();
    }

    /// Забирает победивший plan exactly once после authoritative terminal resolution.
    fn take_terminal(&mut self) -> Option<(StartupMutationDraft, Option<Instant>)> {
        if !self.active {
            return None;
        }
        self.active = false;
        Some((
            std::mem::take(&mut self.draft),
            self.clear_requested_at.take(),
        ))
    }
}

impl PlaylistRuntime {
    /// Открывает post-gate retention рядом с началом renderer-bound startup install.
    pub(crate) fn begin_startup_action_retention(&mut self) {
        self.startup_retained_actions.begin();
    }

    /// `true`, если structural ingress должен сохранить payload до terminal old install-а.
    pub(crate) const fn startup_action_retention_is_active(&self) -> bool {
        self.startup_retained_actions.is_active()
    }

    /// Сохраняет Clear в bounded latest-wins slot.
    pub(crate) fn retain_startup_clear(
        &mut self,
        requested_at: Instant,
    ) -> Result<(), StartupDraftError> {
        self.startup_retained_actions.record_clear(requested_at)
    }

    /// Сохраняет marker подготовленного replacement; payload остаётся у media owner-а.
    pub(crate) fn retain_startup_media_replacement(&mut self) -> Result<(), StartupDraftError> {
        self.startup_retained_actions.record_media_replacement()
    }

    /// Сохраняет ID-less Add aggregate до terminal old install-а.
    pub(crate) fn retain_startup_prepared_add(
        &mut self,
        drafts: Vec<PlaylistItemDraft>,
    ) -> Result<(), StartupDraftError> {
        self.startup_retained_actions.record_prepared_add(drafts)
    }

    /// Отмечает action, который уже прошёл normal domain boundary с первого раза.
    pub(crate) fn mark_retained_startup_queue_action_committed(&mut self) {
        self.startup_retained_actions.mark_queue_action_committed();
    }

    /// Fatal/missing/post-barrier failure не разрешает применять retained mutations.
    pub(crate) fn discard_retained_startup_actions(&mut self) {
        self.startup_retained_actions.discard();
    }

    /// После cancel-win либо полного Installed commit-а применяет winner exactly once.
    pub(crate) fn apply_retained_startup_actions(
        &mut self,
    ) -> Result<RetainedStartupApplyOutcome, RetainedStartupApplyError> {
        let Some((draft, clear_requested_at)) = self.startup_retained_actions.take_terminal()
        else {
            return Ok(RetainedStartupApplyOutcome::NoAction);
        };
        let (queue_plan, _desired_modes) = draft.into_parts();
        match queue_plan {
            StartupQueuePlan::RestoreCandidate => Ok(RetainedStartupApplyOutcome::NoAction),
            StartupQueuePlan::AwaitingMediaReplacement => {
                Ok(RetainedStartupApplyOutcome::MediaReplacementPending)
            }
            StartupQueuePlan::Empty => {
                let requested_at = clear_requested_at.unwrap_or_else(Instant::now);
                match self.clear_playlist(requested_at) {
                    RuntimeRemovalOutcome::Removed { .. } | RuntimeRemovalOutcome::NoChange => {
                        Ok(RetainedStartupApplyOutcome::Cleared)
                    }
                    outcome => Err(RetainedStartupApplyError::Clear(outcome)),
                }
            }
            StartupQueuePlan::PreparedItems(drafts) => {
                let controller = self
                    .controller
                    .as_mut()
                    .ok_or(RetainedStartupApplyError::MissingController)?;
                let dirty_before = controller.dirty_revision();
                let item_count = match controller
                    .append(drafts)
                    .map_err(RetainedStartupApplyError::Append)?
                {
                    ControllerAppendOutcome::Added { item_ids, .. } => item_ids.len(),
                    ControllerAppendOutcome::NoItemsProvided => 0,
                };
                self.publish_controller_mutation_if_dirty(dirty_before);
                Ok(RetainedStartupApplyOutcome::Added { item_count })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::path::PathBuf;
    use std::time::Instant;

    use player_core::{MediaInstallRequestId, MediaInstanceId, PlaybackIntentRevision};
    use playlist_core::{
        CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind, RepeatMode,
    };

    use super::*;
    use crate::app_wake::{AppWakeOwner, AppWakePort};
    use crate::media_open::{
        AuthorizationDispatchResolution, MediaOpenRequestId, PlayerDispatchRejection,
    };
    use crate::playlist_runtime::PlaylistBindingGeneration;
    use crate::playlist_runtime::controller::InstallReadyOutcome;

    fn draft(label: &str) -> PlaylistItemDraft {
        PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from(label)),
            None,
            CachedPlaylistMetadata::new(label.to_owned(), PlaylistMediaKind::Video),
        )
    }

    fn request_id(value: u64) -> MediaOpenRequestId {
        MediaOpenRequestId::from_non_zero(NonZeroU64::new(value).expect("request id"))
    }

    fn player_request_id(value: u64) -> MediaInstallRequestId {
        MediaInstallRequestId::from_non_zero(NonZeroU64::new(value).expect("player request id"))
    }

    fn instance_id(value: u64) -> MediaInstanceId {
        MediaInstanceId::from_non_zero(NonZeroU64::new(value).expect("instance id"))
    }

    fn runtime_with_awaiting_install()
    -> (PlaylistRuntime, MediaOpenRequestId, MediaInstallRequestId) {
        let mut runtime =
            PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
        runtime.resolve_missing_state_for_test();
        runtime
            .controller
            .as_mut()
            .expect("controller")
            .append(vec![draft("restored")])
            .expect("seed queue");
        let request_id = request_id(71);
        let player_request_id = player_request_id(81);
        runtime
            .accept_explicit_target_install(
                request_id,
                player_request_id,
                draft("startup-target"),
                PlaybackIntentRevision::from_non_zero(NonZeroU64::new(1).expect("intent revision")),
            )
            .expect("accept startup target");
        runtime.begin_startup_action_retention();
        (runtime, request_id, player_request_id)
    }

    fn reserve_and_begin_dispatch(runtime: &mut PlaylistRuntime, request_id: MediaOpenRequestId) {
        assert!(matches!(
            runtime
                .controller
                .as_mut()
                .expect("controller")
                .on_ready_to_commit(request_id),
            InstallReadyOutcome::RequestAuthorization { .. }
        ));
        runtime
            .controller
            .as_mut()
            .expect("controller")
            .begin_authorization_dispatch(request_id)
            .expect("begin dispatch");
    }

    #[test]
    fn retained_draft_is_latest_wins_bounded_and_taken_once() {
        let mut owner = StartupRetainedActionOwner::default();
        owner.begin();
        owner
            .record_prepared_add(vec![draft("add-a"), draft("add-b")])
            .expect("retain adds");
        owner
            .record_clear(Instant::now())
            .expect("clear supersedes adds");
        owner
            .record_prepared_add(vec![draft("add-after-clear")])
            .expect("latest add replaces empty plan");

        let (draft, _) = owner.take_terminal().expect("terminal draft");
        let (queue_plan, _) = draft.into_parts();
        let StartupQueuePlan::PreparedItems(items) = queue_plan else {
            panic!("latest Add must be the only winner");
        };
        assert_eq!(items.len(), 1);
        assert!(owner.take_terminal().is_none());
    }

    #[test]
    fn retained_add_cap_rejects_overflow_without_losing_existing_drafts() {
        let mut owner = StartupRetainedActionOwner::default();
        owner.begin();
        let bounded_drafts = (0..super::super::startup::MAX_STARTUP_DRAFT_ITEMS)
            .map(|index| draft(&format!("bounded-add-{index}")))
            .collect();
        owner
            .record_prepared_add(bounded_drafts)
            .expect("exact startup Add cap is accepted");

        assert_eq!(
            owner.record_prepared_add(vec![draft("overflow")]),
            Err(StartupDraftError::PreparedItemsCapacityExceeded)
        );
        let (retained, _) = owner.take_terminal().expect("bounded retained draft");
        let (queue_plan, _) = retained.into_parts();
        let StartupQueuePlan::PreparedItems(items) = queue_plan else {
            panic!("failed overflow must preserve prepared Add winner");
        };
        assert_eq!(items.len(), super::super::startup::MAX_STARTUP_DRAFT_ITEMS);
    }

    #[test]
    fn clear_before_ready_commits_once_and_is_not_replayed() {
        let (mut runtime, _request_id, _player_request_id) = runtime_with_awaiting_install();
        let dirty_before = runtime
            .playlist_controller()
            .expect("controller")
            .dirty_revision();

        assert!(matches!(
            runtime.clear_playlist(Instant::now()),
            RuntimeRemovalOutcome::Removed { .. }
        ));
        let dirty_after = runtime
            .playlist_controller()
            .expect("controller")
            .dirty_revision();
        assert!(dirty_after > dirty_before);
        assert_eq!(
            runtime
                .apply_retained_startup_actions()
                .expect("terminal apply"),
            RetainedStartupApplyOutcome::NoAction
        );
        assert_eq!(
            runtime
                .playlist_controller()
                .expect("controller")
                .dirty_revision(),
            dirty_after
        );
    }

    #[test]
    fn dispatch_rejection_applies_retained_clear_exactly_once() {
        let (mut runtime, request_id, _) = runtime_with_awaiting_install();
        reserve_and_begin_dispatch(&mut runtime, request_id);
        let dirty_before = runtime
            .playlist_controller()
            .expect("controller")
            .dirty_revision();

        assert_eq!(
            runtime.clear_playlist(Instant::now()),
            RuntimeRemovalOutcome::DeferredUntilStartupInstallResolution
        );
        assert_eq!(
            runtime
                .playlist_controller()
                .expect("controller")
                .dirty_revision(),
            dirty_before
        );
        runtime
            .controller
            .as_mut()
            .expect("controller")
            .resolve_authorization_dispatch(
                request_id,
                AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue {
                    rejection: PlayerDispatchRejection::Backpressure,
                },
            )
            .expect("resolve rejection");

        assert_eq!(
            runtime
                .apply_retained_startup_actions()
                .expect("apply clear"),
            RetainedStartupApplyOutcome::Cleared
        );
        assert!(
            runtime
                .playlist_controller()
                .expect("controller")
                .queue()
                .is_empty()
        );
        assert_eq!(
            runtime
                .apply_retained_startup_actions()
                .expect("second drain"),
            RetainedStartupApplyOutcome::NoAction
        );
    }

    #[test]
    fn enqueue_win_commits_old_identity_then_applies_clear_and_modes() {
        let (mut runtime, request_id, player_request_id) = runtime_with_awaiting_install();
        reserve_and_begin_dispatch(&mut runtime, request_id);
        runtime
            .record_startup_repeat_mode(RepeatMode::RepeatOne)
            .expect("retain repeat");
        runtime
            .record_startup_shuffle_enabled(true)
            .expect("retain shuffle");
        assert_eq!(
            runtime.clear_playlist(Instant::now()),
            RuntimeRemovalOutcome::DeferredUntilStartupInstallResolution
        );
        runtime
            .controller
            .as_mut()
            .expect("controller")
            .resolve_authorization_dispatch(
                request_id,
                AuthorizationDispatchResolution::EnqueuedAtPlayerOwner,
            )
            .expect("enqueue wins");
        let installed = runtime
            .controller
            .as_mut()
            .expect("controller")
            .on_installed(
                request_id,
                player_request_id,
                instance_id(91),
                PlaylistBindingGeneration(1),
            )
            .expect("old startup install commits");
        assert!(
            installed
                .active_media
                .as_ref()
                .expect("old startup identity commits before retained action")
                .item_id()
                .is_some()
        );
        assert_eq!(
            runtime
                .playlist_controller()
                .expect("controller")
                .repeat_mode(),
            RepeatMode::RepeatOne
        );
        assert!(
            runtime
                .playlist_controller()
                .expect("controller")
                .queue()
                .shuffle_enabled()
        );

        assert_eq!(
            runtime
                .apply_retained_startup_actions()
                .expect("apply clear"),
            RetainedStartupApplyOutcome::Cleared
        );
        let controller = runtime.playlist_controller().expect("controller");
        assert!(controller.queue().is_empty());
        assert!(
            controller
                .active_media()
                .expect("installed identity")
                .item_id()
                .is_none()
        );
    }

    #[test]
    fn retained_add_allocates_no_id_or_dirty_until_terminal_resolution() {
        let (mut runtime, request_id, _) = runtime_with_awaiting_install();
        reserve_and_begin_dispatch(&mut runtime, request_id);
        let controller = runtime.playlist_controller().expect("controller");
        let next_id_before = controller.queue().next_item_id_snapshot();
        let dirty_before = controller.dirty_revision();

        runtime
            .record_startup_prepared_add(vec![draft("retained-add")])
            .expect("retain add");
        let controller = runtime.playlist_controller().expect("controller");
        assert_eq!(controller.queue().next_item_id_snapshot(), next_id_before);
        assert_eq!(controller.dirty_revision(), dirty_before);
        runtime
            .controller
            .as_mut()
            .expect("controller")
            .resolve_authorization_dispatch(
                request_id,
                AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue {
                    rejection: PlayerDispatchRejection::Backpressure,
                },
            )
            .expect("resolve rejection");

        assert_eq!(
            runtime.apply_retained_startup_actions().expect("apply add"),
            RetainedStartupApplyOutcome::Added { item_count: 1 }
        );
        let controller = runtime.playlist_controller().expect("controller");
        assert!(
            controller
                .queue()
                .next_item_id_snapshot()
                .expose_value_for_persistence()
                > next_id_before.expose_value_for_persistence()
        );
        assert!(controller.dirty_revision() > dirty_before);
    }

    #[test]
    fn ready_mode_setters_mutate_empty_controller_once_and_noop_stays_clean() {
        let mut runtime =
            PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
        runtime.resolve_missing_state_for_test();
        let dirty_before = runtime
            .playlist_controller()
            .expect("ready controller")
            .dirty_revision();

        assert!(
            runtime
                .record_startup_repeat_mode(RepeatMode::RepeatOne)
                .expect("set repeat one")
        );
        let dirty_after_repeat = runtime
            .playlist_controller()
            .expect("ready controller")
            .dirty_revision();
        assert!(dirty_after_repeat > dirty_before);
        assert!(
            !runtime
                .record_startup_repeat_mode(RepeatMode::RepeatOne)
                .expect("repeat no-op")
        );
        assert_eq!(
            runtime
                .playlist_controller()
                .expect("ready controller")
                .dirty_revision(),
            dirty_after_repeat
        );

        assert!(
            runtime
                .record_startup_repeat_mode(RepeatMode::RepeatQueue)
                .expect("set repeat queue")
        );
        assert!(
            runtime
                .record_startup_repeat_mode(RepeatMode::StopAtEnd)
                .expect("set stop at end")
        );
        assert!(
            runtime
                .record_startup_shuffle_enabled(true)
                .expect("enable shuffle")
        );
        let dirty_after_shuffle = runtime
            .playlist_controller()
            .expect("ready controller")
            .dirty_revision();
        assert!(
            !runtime
                .record_startup_shuffle_enabled(true)
                .expect("shuffle no-op")
        );
        assert_eq!(
            runtime
                .playlist_controller()
                .expect("ready controller")
                .dirty_revision(),
            dirty_after_shuffle
        );
    }

    #[test]
    fn pregate_mode_setters_coalesce_without_early_visible_mutation() {
        let mut runtime =
            PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));

        assert!(
            !runtime
                .record_startup_repeat_mode(RepeatMode::RepeatOne)
                .expect("retain first repeat")
        );
        assert!(
            !runtime
                .record_startup_repeat_mode(RepeatMode::RepeatQueue)
                .expect("coalesce repeat")
        );
        assert!(
            !runtime
                .record_startup_shuffle_enabled(true)
                .expect("retain shuffle")
        );
        assert!(runtime.playlist_controller().is_none());

        runtime.resolve_missing_state_for_test();
        let controller = runtime.playlist_controller().expect("ready controller");
        assert_eq!(controller.repeat_mode(), RepeatMode::RepeatQueue);
        assert!(controller.queue().shuffle_enabled());
    }
}
