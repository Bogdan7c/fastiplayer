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
use crate::url_service_adapter::{
    PlaylistUrlMetadataSource, StartupUrlClassification, classify_playlist_url,
};

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

    /// D31 принимает local refresh и service-owned YtDlp enrichment видимых rows.
    #[allow(
        dead_code,
        reason = "Session 18/19 visible-row hint invokes this action"
    )]
    pub(crate) fn request_visible_metadata_refresh(
        &mut self,
        item_ids: &[PlaylistItemId],
        yt_dlp_config: &rustiplayer_config::YtDlpConfig,
    ) -> VisibleRefreshRequestOutcome {
        let Some(controller) = self.controller.as_ref() else {
            return VisibleRefreshRequestOutcome::default();
        };
        let expected_structural_revision = controller.view_snapshot().structural_revision();
        let local_demands = item_ids
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
        let yt_dlp_demands = item_ids
            .iter()
            .filter_map(|item_id| {
                let item = controller.queue().item(*item_id)?;
                if item
                    .cached_metadata()
                    .title()
                    .is_some_and(|title| !title.trim().is_empty())
                {
                    return None;
                }
                let secret_url = item.locator().as_secret_url()?;
                let StartupUrlClassification::Supported(locator) =
                    classify_playlist_url(secret_url)
                else {
                    return None;
                };
                let PlaylistUrlMetadataSource::YtDlp(yt_dlp_locator) =
                    locator.playlist_metadata_source()?;
                Some(super::yt_dlp_metadata::YtDlpMetadataDemand::new(
                    *item_id,
                    item.locator().clone(),
                    yt_dlp_locator,
                    yt_dlp_config.clone(),
                ))
            })
            .collect();
        let outcome = self.discovery.request_visible_refresh(local_demands);
        let _yt_dlp_outcome = self.discovery.request_yt_dlp_metadata(yt_dlp_demands);
        outcome
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

    pub(in crate::playlist_runtime) fn request_yt_dlp_metadata(
        &mut self,
        demands: Vec<super::YtDlpMetadataDemand>,
    ) -> super::yt_dlp_metadata::YtDlpMetadataRequestOutcome {
        self.yt_dlp_metadata
            .request(demands, std::time::Instant::now())
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
        self.yt_dlp_metadata.cancel_for_queue_replacement();
    }

    #[cfg(test)]
    pub(in crate::playlist_runtime) fn replace_yt_dlp_metadata_resolver_for_test(
        &mut self,
        resolver: std::sync::Arc<dyn super::YtDlpMetadataResolver>,
    ) {
        self.yt_dlp_metadata.replace_resolver_for_test(resolver);
    }
}
