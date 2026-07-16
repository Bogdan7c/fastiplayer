//! App-owned Manual Add и demand-driven visible metadata jobs поверх единого executor-а.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use natural_sort_key::PreparedNaturalKey;
use playlist_core::{
    LocalSourceFingerprint, MetadataPatchItemOutcome, PlaylistItemId, PlaylistLocator,
    PlaylistMetadataPatch,
};
use playlist_discovery::{
    AdmissionBatchId, BatchApplySemantics, DiscoveryCancellationCause, DiscoveryDiagnostic,
    DiscoveryEvent, DiscoveryExecutor, DiscoveryFinalOutcome, DiscoveryJobHandle, DiscoveryJobId,
    DiscoveryProgress, DiscoveryRecord, DiscoveryRecordKey, DiscoveryRequest,
    DiscoveryRequestRevision, DiscoverySubmitError, LocalMediaFingerprint, VisibleRefreshLocator,
};

use super::mapping::draft_from_record;
use crate::playlist_runtime::controller::PlaylistController;
use crate::playlist_runtime::view::PlaylistStructuralRevision;

/// Максимум demand hints, удерживаемых при быстром scroll.
const VISIBLE_REFRESH_PENDING_LIMIT: usize = 256;
/// Один speculative batch не монополизирует общий executor.
const VISIBLE_REFRESH_BATCH_LIMIT: usize = 64;
/// Successful freshness keys ограничены process-lifetime LRU-like окном.
const VISIBLE_REFRESH_VALID_CACHE_LIMIT: usize = 512;

/// App-visible identity Manual Add job-а без раскрытия executor internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ManualAddJobId(DiscoveryJobId);

/// Ошибка запуска не маскирует lifecycle/backpressure executor-а.
#[derive(Debug)]
pub(crate) enum ManualAddStartError {
    RuntimeShuttingDown,
    LoadDecisionPending,
    ExecutorUnavailable,
    Submit(DiscoverySubmitError),
}

/// Почему terminal Manual Add не создал либо создал одну mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManualAddTerminalOutcome {
    Appended,
    NoSuccessfulItems,
    NoCapacity,
    Cancelled,
    SupersededQueueGeneration,
    ExecutorDisconnected,
    CommitRejected,
}

/// Bounded terminal accounting для Session 19 UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManualAddCompletion {
    pub(crate) job_id: ManualAddJobId,
    pub(crate) outcome: ManualAddTerminalOutcome,
    pub(crate) requested: usize,
    pub(crate) added: usize,
    pub(crate) unsupported_container: usize,
    pub(crate) no_audio_video_tracks: usize,
    pub(crate) probe_failed: usize,
    pub(crate) capacity_rejected: usize,
    pub(crate) diagnostics: Arc<[DiscoveryDiagnostic]>,
    pub(crate) omitted_diagnostics: usize,
}

/// Shared bounded progress/read model переживает hide/recreation UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistDiscoveryJobsReadModel {
    pub(crate) manual_progress: Option<DiscoveryProgress>,
    pub(crate) latest_manual_completion: Option<ManualAddCompletion>,
    pub(crate) active_manual_jobs: usize,
    pub(crate) visible_active: bool,
    pub(crate) visible_pending: usize,
    pub(crate) visible_dropped: usize,
    pub(crate) visible_stale: usize,
    pub(crate) visible_commit_rejected: usize,
}

/// Результат visibility hint не притворяется filesystem completion-ом.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VisibleRefreshRequestOutcome {
    pub(crate) accepted: usize,
    pub(crate) coalesced: usize,
    pub(crate) dropped_by_bound: usize,
}

/// Stale-safe row demand, построенный runtime-ом из committed controller state.
#[derive(Clone)]
pub(crate) struct VisibleRefreshDemand {
    pub(super) item_id: PlaylistItemId,
    pub(super) locator: PlaylistLocator,
    pub(super) expected_fingerprint: Option<LocalSourceFingerprint>,
    pub(super) expected_structural_revision: PlaylistStructuralRevision,
    pub(super) path: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct VisibleRefreshKey {
    item_id: PlaylistItemId,
    expected_fingerprint: Option<LocalSourceFingerprint>,
}

impl VisibleRefreshDemand {
    fn key(&self) -> VisibleRefreshKey {
        VisibleRefreshKey {
            item_id: self.item_id,
            expected_fingerprint: self.expected_fingerprint,
        }
    }
}

struct ManualAddJob {
    handle: DiscoveryJobHandle,
    queue_generation: u64,
    requested: usize,
    records: Vec<DiscoveryRecord>,
    batch_ids: Vec<AdmissionBatchId>,
}

struct VisibleRefreshJob {
    handle: DiscoveryJobHandle,
    demands: Vec<VisibleRefreshDemand>,
    records: Vec<DiscoveryRecord>,
    batch_ids: Vec<AdmissionBatchId>,
}

/// Единственный app action owner поверх shared `DiscoveryExecutor`.
pub(super) struct DiscoveryActionJobs {
    next_request_revision: u64,
    manual_jobs: Vec<ManualAddJob>,
    manual_progress: Option<DiscoveryProgress>,
    latest_manual_completion: Option<ManualAddCompletion>,
    visible_pending: VecDeque<VisibleRefreshDemand>,
    visible_active: Option<VisibleRefreshJob>,
    visible_valid_cache: VecDeque<VisibleRefreshKey>,
    visible_dropped: usize,
    visible_stale: usize,
    visible_commit_rejected: usize,
}

impl DiscoveryActionJobs {
    pub(super) const fn manual_progress(&self) -> Option<DiscoveryProgress> {
        self.manual_progress
    }

    pub(super) fn new() -> Self {
        Self {
            next_request_revision: 1,
            manual_jobs: Vec::new(),
            manual_progress: None,
            latest_manual_completion: None,
            visible_pending: VecDeque::new(),
            visible_active: None,
            visible_valid_cache: VecDeque::new(),
            visible_dropped: 0,
            visible_stale: 0,
            visible_commit_rejected: 0,
        }
    }

    pub(super) fn start_manual_add(
        &mut self,
        executor: &DiscoveryExecutor,
        paths: Vec<PathBuf>,
        queue_generation: u64,
    ) -> Result<ManualAddJobId, ManualAddStartError> {
        let paths = natural_order(paths);
        let requested = paths.len();
        let request_revision = self.allocate_request_revision();
        let handle = executor
            .submit(DiscoveryRequest::ManualBatch {
                locators: paths.clone(),
                request_revision,
            })
            .map_err(ManualAddStartError::Submit)?;
        let job_id = ManualAddJobId(handle.id());
        self.manual_jobs.push(ManualAddJob {
            handle,
            queue_generation,
            requested,
            records: Vec::new(),
            batch_ids: Vec::new(),
        });
        Ok(job_id)
    }

    pub(super) fn cancel_manual_add(&mut self, job_id: ManualAddJobId) -> bool {
        self.manual_jobs
            .iter()
            .find(|job| job.handle.id() == job_id.0)
            .is_some_and(|job| job.handle.cancel(DiscoveryCancellationCause::UserCancelled))
    }

    /// UI cancel относится ко всем ещё не committed Manual Add batches.
    pub(super) fn cancel_all_manual_adds(&mut self) -> bool {
        let mut changed = false;
        for job in &mut self.manual_jobs {
            changed |= job.handle.cancel(DiscoveryCancellationCause::UserCancelled);
        }
        changed
    }

    pub(super) fn request_visible_refresh(
        &mut self,
        demands: Vec<VisibleRefreshDemand>,
    ) -> VisibleRefreshRequestOutcome {
        let mut outcome = VisibleRefreshRequestOutcome {
            accepted: 0,
            coalesced: 0,
            dropped_by_bound: 0,
        };
        for demand in demands {
            let key = demand.key();
            if self.visible_valid_cache.contains(&key)
                || self
                    .visible_pending
                    .iter()
                    .any(|queued| queued.key() == key)
                || self
                    .visible_active
                    .as_ref()
                    .is_some_and(|active| active.demands.iter().any(|running| running.key() == key))
            {
                outcome.coalesced += 1;
                continue;
            }
            if self.visible_pending.len() == VISIBLE_REFRESH_PENDING_LIMIT {
                outcome.dropped_by_bound += 1;
                self.visible_dropped = self.visible_dropped.saturating_add(1);
                continue;
            }
            self.visible_pending.push_back(demand);
            outcome.accepted += 1;
        }
        outcome
    }

    pub(super) fn read_model(&self) -> PlaylistDiscoveryJobsReadModel {
        PlaylistDiscoveryJobsReadModel {
            manual_progress: self.manual_progress,
            latest_manual_completion: self.latest_manual_completion.clone(),
            active_manual_jobs: self.manual_jobs.len(),
            visible_active: self.visible_active.is_some(),
            visible_pending: self.visible_pending.len(),
            visible_dropped: self.visible_dropped,
            visible_stale: self.visible_stale,
            visible_commit_rejected: self.visible_commit_rejected,
        }
    }

    pub(super) fn cancel_for_queue_replacement(&mut self) {
        for job in &self.manual_jobs {
            let _cancelled_now = job
                .handle
                .cancel(DiscoveryCancellationCause::StructuralInvalidation);
        }
        if let Some(active) = &self.visible_active {
            let _cancelled_now = active
                .handle
                .cancel(DiscoveryCancellationCause::StructuralInvalidation);
        }
        self.visible_pending.clear();
        self.visible_valid_cache.clear();
    }

    pub(super) fn begin_shutdown(&mut self) {
        for job in &self.manual_jobs {
            let _cancelled_now = job
                .handle
                .cancel(DiscoveryCancellationCause::LifecycleShutdown);
        }
        if let Some(active) = &self.visible_active {
            let _cancelled_now = active
                .handle
                .cancel(DiscoveryCancellationCause::LifecycleShutdown);
        }
        self.visible_pending.clear();
    }

    pub(super) fn drain(
        &mut self,
        executor: Option<&DiscoveryExecutor>,
        controller: &mut PlaylistController,
        queue_generation: u64,
    ) -> bool {
        let mut visible_change = self.drain_manual(controller, queue_generation);
        visible_change |= self.drain_visible(controller);
        if self.visible_active.is_none() {
            visible_change |= self.start_visible_if_needed(executor);
        }
        visible_change
    }

    fn drain_manual(&mut self, controller: &mut PlaylistController, queue_generation: u64) -> bool {
        let mut visible_change = false;
        let mut index = 0;
        while index < self.manual_jobs.len() {
            let job = &mut self.manual_jobs[index];
            collect_record_events(
                &job.handle,
                &mut job.records,
                &mut job.batch_ids,
                BatchApplySemantics::AccumulateUntilTerminalAtomicApply,
            );
            if let Some(progress) = job.handle.take_progress() {
                self.manual_progress = Some(progress);
                visible_change = true;
            }
            let Some(summary) = job.handle.take_final_summary() else {
                index += 1;
                continue;
            };
            // Terminal и record slots независимы: дочитываем records, опубликованные
            // в том же completion edge после первого mailbox snapshot-а.
            collect_record_events(
                &job.handle,
                &mut job.records,
                &mut job.batch_ids,
                BatchApplySemantics::AccumulateUntilTerminalAtomicApply,
            );
            let mut job = self.manual_jobs.remove(index);
            let completion = finish_manual_job(&mut job, summary, controller, queue_generation);
            self.latest_manual_completion = Some(completion);
            self.manual_progress = None;
            visible_change = true;
        }
        visible_change
    }

    fn drain_visible(&mut self, controller: &mut PlaylistController) -> bool {
        let Some(active) = &mut self.visible_active else {
            return false;
        };
        collect_record_events(
            &active.handle,
            &mut active.records,
            &mut active.batch_ids,
            BatchApplySemantics::MetadataRefreshChunk,
        );
        let Some(summary) = active.handle.take_final_summary() else {
            return active.handle.take_progress().is_some();
        };
        collect_record_events(
            &active.handle,
            &mut active.records,
            &mut active.batch_ids,
            BatchApplySemantics::MetadataRefreshChunk,
        );
        let mut active = self
            .visible_active
            .take()
            .expect("terminal visible job remains owned until atomic consume");
        let mut diagnostic_ordinals = vec![false; active.demands.len()];
        for diagnostic in &summary.diagnostics {
            let DiscoveryRecordKey::Batch(ordinal) = diagnostic.key else {
                continue;
            };
            if let Some(demand) = active.demands.get(ordinal as usize) {
                diagnostic_ordinals[ordinal as usize] = true;
                if demand_still_current(controller, demand) {
                    let recorded = controller.mark_committed_source_unavailable(
                        demand.item_id,
                        Arc::from("Источник временно недоступен"),
                    );
                    if recorded
                        != super::super::controller::RuntimeErrorCorrelationOutcome::Recorded
                    {
                        self.visible_stale = self.visible_stale.saturating_add(1);
                    }
                } else {
                    self.visible_stale = self.visible_stale.saturating_add(1);
                }
            }
        }
        if summary.outcome == DiscoveryFinalOutcome::Completed {
            let mut record_ordinals = vec![false; active.demands.len()];
            let mut patch_ordinals = Vec::new();
            let mut patches = Vec::new();
            for record in &active.records {
                let DiscoveryRecordKey::Batch(ordinal) = record.key() else {
                    continue;
                };
                let Some(demand) = active.demands.get(ordinal as usize) else {
                    self.visible_commit_rejected = self.visible_commit_rejected.saturating_add(1);
                    continue;
                };
                record_ordinals[ordinal as usize] = true;
                if !demand_still_current(controller, demand) {
                    self.visible_stale = self.visible_stale.saturating_add(1);
                    continue;
                }
                if let Some(patch) = visible_patch(record, &active.demands) {
                    patch_ordinals.push(ordinal as usize);
                    patches.push(patch);
                } else {
                    self.visible_commit_rejected = self.visible_commit_rejected.saturating_add(1);
                }
            }
            if !patches.is_empty() {
                match controller.apply_metadata_patches(patches) {
                    Ok(outcome) => {
                        for (ordinal, item_outcome) in patch_ordinals
                            .into_iter()
                            .zip(outcome.domain.item_outcomes().iter())
                        {
                            match item_outcome {
                                MetadataPatchItemOutcome::Applied { .. }
                                | MetadataPatchItemOutcome::NoChange { .. } => {
                                    retain_current_valid_key(
                                        &mut self.visible_valid_cache,
                                        controller,
                                        active.demands[ordinal].item_id,
                                    );
                                }
                                MetadataPatchItemOutcome::NotFound { .. }
                                | MetadataPatchItemOutcome::SourceMismatch { .. } => {
                                    self.visible_stale = self.visible_stale.saturating_add(1);
                                }
                            }
                        }
                    }
                    Err(_) => {
                        self.visible_commit_rejected = self
                            .visible_commit_rejected
                            .saturating_add(patch_ordinals.len());
                    }
                }
            }
            for (ordinal, demand) in active.demands.iter().enumerate() {
                if !record_ordinals[ordinal]
                    && !diagnostic_ordinals[ordinal]
                    && demand_still_current(controller, demand)
                {
                    retain_valid_key(&mut self.visible_valid_cache, demand.key());
                }
            }
            for batch_id in active.batch_ids.drain(..) {
                let _acknowledged = active.handle.acknowledge_admitted_batch(batch_id);
            }
        }
        true
    }

    fn start_visible_if_needed(&mut self, executor: Option<&DiscoveryExecutor>) -> bool {
        let Some(executor) = executor else {
            return false;
        };
        if self.visible_pending.is_empty() {
            return false;
        }
        let take = self.visible_pending.len().min(VISIBLE_REFRESH_BATCH_LIMIT);
        let demands = self.visible_pending.drain(..take).collect::<Vec<_>>();
        let locators = demands
            .iter()
            .map(|demand| {
                VisibleRefreshLocator::new(
                    demand.path.clone(),
                    demand.expected_fingerprint.map(|fingerprint| {
                        LocalMediaFingerprint::new(
                            fingerprint.file_size_bytes(),
                            fingerprint.modified_at(),
                        )
                    }),
                )
            })
            .collect();
        let request_revision = self.allocate_request_revision();
        match executor.submit(DiscoveryRequest::VisibleRefresh {
            locators,
            request_revision,
        }) {
            Ok(handle) => {
                self.visible_active = Some(VisibleRefreshJob {
                    handle,
                    demands,
                    records: Vec::new(),
                    batch_ids: Vec::new(),
                });
            }
            Err(_) => {
                self.visible_dropped = self.visible_dropped.saturating_add(demands.len());
            }
        }
        true
    }

    fn allocate_request_revision(&mut self) -> DiscoveryRequestRevision {
        let revision = DiscoveryRequestRevision::new(self.next_request_revision);
        self.next_request_revision = self.next_request_revision.saturating_add(1);
        revision
    }
}

fn natural_order(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut keyed = paths
        .into_iter()
        .map(|path| {
            let key = PreparedNaturalKey::from_os_str(
                path.file_name().unwrap_or_else(|| path.as_os_str()),
            );
            (key, path)
        })
        .collect::<Vec<_>>();
    keyed.sort_by(|(left_key, left_path), (right_key, right_path)| {
        left_key
            .cmp(right_key)
            .then_with(|| left_path.cmp(right_path))
    });
    keyed.into_iter().map(|(_, path)| path).collect()
}

fn collect_record_events(
    handle: &DiscoveryJobHandle,
    records: &mut Vec<DiscoveryRecord>,
    batch_ids: &mut Vec<AdmissionBatchId>,
    expected_semantics: BatchApplySemantics,
) {
    for event in handle.drain_events() {
        if let DiscoveryEvent::AdmittedBatch(batch) = event
            && batch.apply_semantics() == expected_semantics
        {
            records.extend_from_slice(batch.records());
            batch_ids.push(batch.batch_id());
        }
    }
}

fn finish_manual_job(
    job: &mut ManualAddJob,
    summary: playlist_discovery::DiscoveryFinalSummary,
    controller: &mut PlaylistController,
    queue_generation: u64,
) -> ManualAddCompletion {
    let (outcome, added, capacity_rejected) = match summary.outcome {
        DiscoveryFinalOutcome::Completed | DiscoveryFinalOutcome::LimitReached
            if job.queue_generation != queue_generation =>
        {
            (ManualAddTerminalOutcome::SupersededQueueGeneration, 0, 0)
        }
        DiscoveryFinalOutcome::Completed | DiscoveryFinalOutcome::LimitReached => {
            job.records.sort_by_key(DiscoveryRecord::key);
            match job
                .records
                .iter()
                .map(draft_from_record)
                .collect::<Result<Vec<_>, _>>()
            {
                Err(_) => (ManualAddTerminalOutcome::CommitRejected, 0, 0),
                Ok(drafts) if drafts.is_empty() => {
                    (ManualAddTerminalOutcome::NoSuccessfulItems, 0, 0)
                }
                Ok(drafts) => match controller.append_capped_tail(drafts) {
                    Ok(commit) => {
                        let outcome = if commit.item_ids.is_empty() {
                            ManualAddTerminalOutcome::NoCapacity
                        } else {
                            ManualAddTerminalOutcome::Appended
                        };
                        (outcome, commit.item_ids.len(), commit.capacity_rejected)
                    }
                    Err(_) => (ManualAddTerminalOutcome::CommitRejected, 0, 0),
                },
            }
        }
        DiscoveryFinalOutcome::Cancelled(_) => (ManualAddTerminalOutcome::Cancelled, 0, 0),
        DiscoveryFinalOutcome::ExecutorDisconnected => {
            (ManualAddTerminalOutcome::ExecutorDisconnected, 0, 0)
        }
    };
    if outcome == ManualAddTerminalOutcome::Appended {
        for batch_id in job.batch_ids.drain(..) {
            let _acknowledged = job.handle.acknowledge_admitted_batch(batch_id);
        }
    }
    ManualAddCompletion {
        job_id: ManualAddJobId(job.handle.id()),
        outcome,
        requested: job.requested,
        added,
        unsupported_container: summary.failure_counts.unsupported_container,
        no_audio_video_tracks: summary.failure_counts.no_audio_video_tracks,
        probe_failed: summary.failure_counts.probe_failed,
        capacity_rejected,
        diagnostics: Arc::from(summary.diagnostics),
        omitted_diagnostics: summary.omitted_diagnostics,
    }
}

fn visible_patch(
    record: &DiscoveryRecord,
    demands: &[VisibleRefreshDemand],
) -> Option<PlaylistMetadataPatch> {
    let DiscoveryRecordKey::Batch(ordinal) = record.key() else {
        return None;
    };
    let demand = demands.get(ordinal as usize)?;
    let draft = draft_from_record(record).ok()?;
    Some(PlaylistMetadataPatch::refreshed_local(
        demand.item_id,
        demand.locator.clone(),
        demand.expected_fingerprint,
        draft.local_fingerprint()?,
        draft.cached_metadata().clone(),
    ))
}

fn retain_valid_key(cache: &mut VecDeque<VisibleRefreshKey>, key: VisibleRefreshKey) {
    if cache.len() == VISIBLE_REFRESH_VALID_CACHE_LIMIT {
        cache.pop_front();
    }
    cache.push_back(key);
}

fn demand_still_current(controller: &PlaylistController, demand: &VisibleRefreshDemand) -> bool {
    controller.view_snapshot().structural_revision() == demand.expected_structural_revision
        && controller.queue().item(demand.item_id).is_some_and(|item| {
            item.locator() == &demand.locator
                && item.local_fingerprint() == demand.expected_fingerprint
        })
}

fn retain_current_valid_key(
    cache: &mut VecDeque<VisibleRefreshKey>,
    controller: &PlaylistController,
    item_id: PlaylistItemId,
) {
    if let Some(item) = controller.queue().item(item_id) {
        retain_valid_key(
            cache,
            VisibleRefreshKey {
                item_id,
                expected_fingerprint: item.local_fingerprint(),
            },
        );
    }
}

#[cfg(test)]
#[path = "action_jobs/tests.rs"]
mod tests;
