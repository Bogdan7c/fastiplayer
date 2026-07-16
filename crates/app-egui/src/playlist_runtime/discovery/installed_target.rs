//! Orchestration sibling discovery после exact install выбранного target-а.

use std::path::PathBuf;

use playlist_discovery::LocalMediaKind;

use super::InstalledTargetDiscoveryStartError;
use crate::playlist_runtime::PlaylistRuntime;
use crate::playlist_runtime::controller::{
    ControllerInitialQueuePlaybackAction, StablePlaybackIntent,
};
use crate::playlist_runtime::identity::ActiveMediaIdentity;

impl PlaylistRuntime {
    /// CLI/startup target уже получил свой playback intent и только запускает sibling expansion.
    pub(crate) fn start_sibling_discovery_for_installed_target(
        &mut self,
        target_path: PathBuf,
        opened_media_kind: LocalMediaKind,
    ) -> Result<(), InstalledTargetDiscoveryStartError> {
        self.start_installed_target_discovery(target_path, opened_media_kind, None)
    }

    /// In-app picker удерживает target paused и запускает playback после доказанного начала queue.
    pub(crate) fn start_sibling_discovery_then_play_from_beginning(
        &mut self,
        target_path: PathBuf,
        opened_media_kind: LocalMediaKind,
        desired_intent: StablePlaybackIntent,
    ) -> Result<(), InstalledTargetDiscoveryStartError> {
        self.start_installed_target_discovery(target_path, opened_media_kind, Some(desired_intent))
    }

    /// Общий owner-boundary различает уже запущенный startup target и paused in-app target.
    fn start_installed_target_discovery(
        &mut self,
        target_path: PathBuf,
        opened_media_kind: LocalMediaKind,
        desired_initial_intent: Option<StablePlaybackIntent>,
    ) -> Result<(), InstalledTargetDiscoveryStartError> {
        let controller = self
            .controller
            .as_mut()
            .ok_or(InstalledTargetDiscoveryStartError::LoadDecisionPending)?;
        let target_item_id = controller
            .active_media()
            .and_then(ActiveMediaIdentity::item_id)
            .ok_or(InstalledTargetDiscoveryStartError::MissingCommittedTarget)?;
        let continuation = controller
            .begin_discovery_continuation()
            .map_err(|_| InstalledTargetDiscoveryStartError::DiscoveryContinuationUnavailable)?;
        let initial_playback_guard = desired_initial_intent
            .map(|desired_intent| controller.begin_initial_queue_playback(desired_intent))
            .transpose()
            .map_err(InstalledTargetDiscoveryStartError::InitialPlaybackGuard)?;
        let policy = self.settings.future_discovery_policy();
        self.discovery.start(
            target_item_id,
            target_path,
            opened_media_kind,
            policy,
            continuation,
            initial_playback_guard,
        );
        Ok(())
    }

    /// Возвращает ровно одно guarded действие старта с начала новой очереди.
    pub(crate) fn take_initial_queue_playback_action(
        &mut self,
    ) -> Option<ControllerInitialQueuePlaybackAction> {
        let controller = self.controller.as_mut()?;
        self.discovery
            .initial_playback
            .take_ready_action(controller)
    }
}
