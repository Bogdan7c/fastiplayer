//! Thin runtime/coordinator facade над app-owned discovery action jobs.

use std::path::PathBuf;

use playlist_core::PlaylistItemId;
use playlist_discovery::DiscoveryCancellationCause;

use super::PlaylistDiscoveryCoordinator;
use super::action_jobs::{
    ManualAddJobId, ManualAddStartError, PlaylistDiscoveryJobsReadModel, VisibleRefreshDemand,
    VisibleRefreshRequestOutcome,
};
use crate::playlist_runtime::PlaylistRuntime;

impl PlaylistRuntime {
    /// Запускает app-owned Manual Add без Item ID reservation до terminal commit.
    #[allow(dead_code, reason = "Session 19 invokes the typed action")]
    pub(crate) fn start_manual_file_add(
        &mut self,
        paths: Vec<PathBuf>,
    ) -> Result<ManualAddJobId, ManualAddStartError> {
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ManualAddStartError::RuntimeShuttingDown);
        }
        self.supersede_startup_media_apply();
        if self.controller.as_ref().is_none() {
            return Err(ManualAddStartError::LoadDecisionPending);
        }
        self.discovery
            .start_manual_add(paths, self.manual_add_queue_generation.value())
    }

    /// Explicit Cancel отбрасывает весь uncommitted Manual Add batch.
    #[allow(dead_code, reason = "Session 19 invokes the typed action")]
    pub(crate) fn cancel_manual_file_add(&mut self, job_id: ManualAddJobId) -> bool {
        self.discovery.cancel_manual_add(job_id)
    }

    /// Inline progress не хранит очередь job IDs и отменяет только Manual Add owner scope.
    pub(crate) fn cancel_all_manual_file_adds(&mut self) -> bool {
        self.discovery.action_jobs.cancel_all_manual_adds()
    }

    /// D27 отменяет sibling scan и связанный wait, но не откатывает committed batches.
    pub(crate) fn cancel_sibling_discovery_from_ui(&mut self) -> bool {
        let active = !matches!(
            self.discovery.status(),
            super::PlaylistDiscoveryStatus::Idle
        );
        let _ = self.cancel_global_playlist_navigation_wait();
        self.discovery
            .cancel_active(DiscoveryCancellationCause::UserCancelled);
        active
    }

    /// D31 принимает только committed native-local rows; URL visibility не создаёт network I/O.
    #[allow(
        dead_code,
        reason = "Session 18/19 visible-row hint invokes this action"
    )]
    pub(crate) fn request_visible_metadata_refresh(
        &mut self,
        item_ids: &[PlaylistItemId],
    ) -> VisibleRefreshRequestOutcome {
        let Some(controller) = self.controller.as_ref() else {
            return VisibleRefreshRequestOutcome::default();
        };
        let expected_structural_revision = controller.view_snapshot().structural_revision();
        let demands = item_ids
            .iter()
            .filter_map(|item_id| {
                let item = controller.queue().item(*item_id)?;
                let locator = item.locator().clone();
                let path = locator
                    .as_local()?
                    .expose_native_path_for_open()?
                    .to_path_buf();
                Some(VisibleRefreshDemand {
                    item_id: *item_id,
                    locator,
                    expected_fingerprint: item.local_fingerprint(),
                    expected_structural_revision,
                    path,
                })
            })
            .collect();
        self.discovery.request_visible_refresh(demands)
    }

    /// Возвращает bounded process-lifetime read model для будущего UI.
    #[allow(
        dead_code,
        reason = "Session 19 renders the process-lifetime read model"
    )]
    pub(crate) fn playlist_discovery_jobs_read_model(&self) -> PlaylistDiscoveryJobsReadModel {
        self.discovery.jobs_read_model()
    }

    /// Clear/replacement/new explicit open отменяют D66 jobs и делают late result stale.
    pub(in crate::playlist_runtime) fn supersede_manual_add_queue_generation(&mut self) {
        self.manual_add_queue_generation.advance();
        self.discovery.cancel_action_jobs_for_queue_replacement();
    }
}

impl PlaylistDiscoveryCoordinator {
    fn start_manual_add(
        &mut self,
        paths: Vec<PathBuf>,
        queue_generation: u64,
    ) -> Result<ManualAddJobId, ManualAddStartError> {
        // D25: новый Add завершает sibling scope, но committed target/batches уже domain-owned.
        self.cancel_active(DiscoveryCancellationCause::Superseded);
        let executor = self
            .executor
            .as_ref()
            .ok_or(ManualAddStartError::ExecutorUnavailable)?;
        self.action_jobs
            .start_manual_add(executor, paths, queue_generation)
    }

    pub(in crate::playlist_runtime) fn cancel_sibling_for_add(&mut self) {
        self.cancel_active(DiscoveryCancellationCause::Superseded);
    }

    fn cancel_manual_add(&mut self, job_id: ManualAddJobId) -> bool {
        self.action_jobs.cancel_manual_add(job_id)
    }

    fn request_visible_refresh(
        &mut self,
        demands: Vec<VisibleRefreshDemand>,
    ) -> VisibleRefreshRequestOutcome {
        self.action_jobs.request_visible_refresh(demands)
    }

    fn jobs_read_model(&self) -> PlaylistDiscoveryJobsReadModel {
        self.action_jobs.read_model()
    }

    pub(crate) const fn manual_probe_progress(
        &self,
    ) -> Option<playlist_discovery::DiscoveryProgress> {
        self.action_jobs.manual_progress()
    }

    pub(super) fn cancel_action_jobs_for_queue_replacement(&mut self) {
        self.action_jobs.cancel_for_queue_replacement();
        self.metadata_sort.cancel_for_queue_replacement();
    }
}
