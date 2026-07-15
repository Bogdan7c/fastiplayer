//! Intent boundary process runtime-а для playlist-state persistence.

use std::sync::Arc;

use playlist_state::{PlaylistStateStore, SaveControlError};

use super::view::PlaylistDirtyRevision;
use super::{
    PlaylistPersistenceView, PlaylistRuntime, PlaylistStartupApplyError,
    PlaylistStartupDrainOutcome, StartupOwnerError,
};

impl PlaylistRuntime {
    /// Production bootstrap передаёт concrete store единственному process owner-у.
    pub(crate) fn begin_production_playlist_state_inspection(
        &mut self,
        store: Arc<PlaylistStateStore>,
    ) -> Result<(), StartupOwnerError> {
        self.persistence.install_store(store.clone());
        self.begin_playlist_state_inspection(store)
    }

    /// Event-driven drain применяет startup completion и worker reports одним UI intent-ом.
    pub(crate) fn drain_playlist_persistence(&mut self) -> Result<bool, PlaylistStartupApplyError> {
        let before_startup = self.playlist_startup_view();
        let quarantine_name = self.persistence.next_quarantine_file_name();
        let startup_outcome = self.drain_playlist_state_startup(quarantine_name)?;
        let startup_changed = !matches!(startup_outcome, PlaylistStartupDrainOutcome::NoCompletion)
            && self.playlist_startup_view() != before_startup;
        let save_changed = self.persistence.drain_worker_events();
        Ok(startup_changed || save_changed)
    }

    /// Timed polling остаётся defensive fallback и не определяет correctness delivery.
    pub(crate) fn has_pending_playlist_persistence_work(&self) -> bool {
        !matches!(
            self.playlist_startup_view().phase,
            super::PlaylistStartupPhase::Ready | super::PlaylistStartupPhase::Shutdown
        ) || self.persistence.has_background_work()
    }

    #[allow(dead_code, reason = "read-only model is the Session 14 UI boundary")]
    pub(crate) const fn playlist_persistence_view(&self) -> PlaylistPersistenceView {
        self.persistence.view()
    }

    /// Manual Retry делегируется D69 scheduler-у; app не дублирует backoff policy.
    #[allow(
        dead_code,
        reason = "manual Retry UI intent is wired in a later UI session"
    )]
    pub(crate) fn retry_playlist_state_save(&self) -> Result<(), SaveControlError> {
        self.persistence.retry_now()
    }

    /// Вызывается сразу после gate decision и создаёт не больше одного writer-а.
    pub(super) fn activate_playlist_persistence(&mut self) {
        let super::PlaylistLoadGateState::Open(lineage) = self.load_gate else {
            return;
        };
        if let Err(error) = self
            .persistence
            .start_for_lineage(lineage, self.owner_ports.clone())
        {
            self.persistence.record_worker_start_error(&error);
            return;
        }
        if let Some(controller) = self.controller.as_ref()
            && controller.dirty_revision().get() > 0
        {
            self.persistence.publish_committed_controller(controller);
        }
    }

    /// Любой runtime mutation boundary сравнивает dirty revision и публикует snapshot.
    pub(super) fn publish_controller_mutation_if_dirty(
        &mut self,
        dirty_before: PlaylistDirtyRevision,
    ) {
        let Some(controller) = self.controller.as_ref() else {
            return;
        };
        if controller.dirty_revision() != dirty_before {
            self.persistence.publish_committed_controller(controller);
        }
    }
}
