//! Transactional metadata Sort orchestration отдельно от Add/visible jobs.

mod runtime;
#[cfg(test)]
mod tests;

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use bounded_work_executor::{
    BoundedExecutor, ExecutorConfig, SubmitError, TaskFailure, TaskHandle, TaskPoll,
};
use playlist_core::{
    CachedPlaylistMetadata, CanonicalSortPreparationCancelled, CanonicalSortPreparationStatistics,
    CanonicalSortSnapshot, PlaylistItemId, PlaylistLocator, PlaylistMediaKind,
    PlaylistMetadataPatch, PlaylistSortKey, PreparedCanonicalSort, SortCanonicalQueue,
};
use playlist_discovery::{
    AdmissionBatchId, BatchApplySemantics, DiscoveryCancellationCause, DiscoveryDiagnostic,
    DiscoveryEvent, DiscoveryExecutor, DiscoveryFailureCounts, DiscoveryFinalOutcome,
    DiscoveryJobHandle, DiscoveryProgress, DiscoveryRecord, DiscoveryRecordKey, DiscoveryRequest,
    DiscoveryRequestRevision, DiscoverySubmitError,
};

use super::mapping::draft_from_record;
use crate::app_wake::AppWakePort;
use crate::playlist_runtime::controller::PlaylistController;
use crate::playlist_runtime::view::PlaylistStructuralRevision;

const SORT_CPU_WORKER_THREADS: NonZeroUsize = NonZeroUsize::new(1).unwrap();
const SORT_CPU_QUEUE_CAPACITY: NonZeroUsize = NonZeroUsize::new(1).unwrap();

pub(super) fn start_cpu_executor() -> Option<BoundedExecutor> {
    BoundedExecutor::start(ExecutorConfig::new(
        SORT_CPU_WORKER_THREADS,
        SORT_CPU_QUEUE_CAPACITY,
        "playlist-sort-cpu",
    ))
    .ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MetadataSortJobId(u64);

/// Typed first-writer-wins результат user cancel intent-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetadataSortCancelOutcome {
    Requested,
    AlreadyRequested,
    AlreadyInvalidated,
    StaleJob,
}

#[derive(Debug)]
pub(crate) enum MetadataSortStartError {
    RuntimeShuttingDown,
    LoadDecisionPending,
    AlreadyActive,
    JobIdExhausted,
    DiscoveryExecutorUnavailable,
    CpuExecutorUnavailable,
    DiscoverySubmit(DiscoverySubmitError),
    CpuSubmit(SubmitError),
}

impl std::fmt::Display for MetadataSortStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RuntimeShuttingDown => formatter.write_str("playlist runtime shutting down"),
            Self::LoadDecisionPending => formatter.write_str("playlist load decision pending"),
            Self::AlreadyActive => formatter.write_str("metadata sort already active"),
            Self::JobIdExhausted => formatter.write_str("metadata sort job ID exhausted"),
            Self::DiscoveryExecutorUnavailable => {
                formatter.write_str("discovery executor unavailable")
            }
            Self::CpuExecutorUnavailable => formatter.write_str("CPU executor unavailable"),
            Self::DiscoverySubmit(error) => write!(formatter, "discovery submit failed: {error}"),
            Self::CpuSubmit(error) => write!(formatter, "CPU submit failed: {error:?}"),
        }
    }
}

impl std::error::Error for MetadataSortStartError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetadataSortPhase {
    Probing,
    PreparingKeys,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MetadataSortReadModel {
    pub(crate) active_job: Option<MetadataSortJobId>,
    pub(crate) phase: Option<MetadataSortPhase>,
    pub(crate) processed: usize,
    pub(crate) total: usize,
    pub(crate) latest_completion: Option<MetadataSortCompletion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetadataSortTerminalOutcome {
    Sorted,
    AlreadyInCanonicalOrder,
    MetadataOnly,
    Cancelled,
    Invalidated,
    Failed,
    NoChanges,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MetadataSortCompletion {
    pub(crate) job_id: MetadataSortJobId,
    pub(crate) outcome: MetadataSortTerminalOutcome,
    pub(crate) metadata_updated: usize,
    pub(crate) failure_counts: DiscoveryFailureCounts,
    pub(crate) diagnostics: Arc<[DiscoveryDiagnostic]>,
    pub(crate) omitted_diagnostics: usize,
    pub(crate) statistics: Option<CanonicalSortPreparationStatistics>,
}

#[derive(Clone)]
struct MetadataSortDemand {
    item_id: PlaylistItemId,
    locator: PlaylistLocator,
    expected_fingerprint: Option<playlist_core::LocalSourceFingerprint>,
    path: PathBuf,
}

struct ProbePhase {
    handle: DiscoveryJobHandle,
    demands: Vec<MetadataSortDemand>,
    records: Vec<DiscoveryRecord>,
    batch_ids: Vec<AdmissionBatchId>,
}

struct CpuPhase {
    handle: TaskHandle<Result<PreparedCanonicalSort, CanonicalSortPreparationCancelled>>,
}

enum ActivePhase {
    Probe(ProbePhase),
    Cpu(CpuPhase),
}

struct ActiveMetadataSort {
    job_id: MetadataSortJobId,
    expected_structural_revision: PlaylistStructuralRevision,
    snapshot: CanonicalSortSnapshot,
    intent: SortCanonicalQueue,
    patches: Vec<PlaylistMetadataPatch>,
    phase: ActivePhase,
    cancel_outcome: Option<MetadataSortTerminalOutcome>,
    failure_counts: DiscoveryFailureCounts,
    diagnostics: Arc<[DiscoveryDiagnostic]>,
    omitted_diagnostics: usize,
}

pub(super) enum MetadataSortTerminal {
    Prepared {
        job_id: MetadataSortJobId,
        expected_structural_revision: PlaylistStructuralRevision,
        prepared: PreparedCanonicalSort,
        patches: Vec<PlaylistMetadataPatch>,
        failure_counts: DiscoveryFailureCounts,
        diagnostics: Arc<[DiscoveryDiagnostic]>,
        omitted_diagnostics: usize,
    },
    Salvage {
        job_id: MetadataSortJobId,
        outcome: MetadataSortTerminalOutcome,
        patches: Vec<PlaylistMetadataPatch>,
        failure_counts: DiscoveryFailureCounts,
        diagnostics: Arc<[DiscoveryDiagnostic]>,
        omitted_diagnostics: usize,
    },
}

pub(super) struct MetadataSortOwner {
    wake_port: AppWakePort,
    next_job_id: Option<u64>,
    active: Option<ActiveMetadataSort>,
    latest_completion: Option<MetadataSortCompletion>,
    progress: Option<DiscoveryProgress>,
}

impl MetadataSortOwner {
    pub(super) fn new(wake_port: AppWakePort) -> Self {
        Self {
            wake_port,
            next_job_id: Some(1),
            active: None,
            latest_completion: None,
            progress: None,
        }
    }

    pub(super) fn start(
        &mut self,
        discovery_executor: Option<&DiscoveryExecutor>,
        cpu_executor: Option<&BoundedExecutor>,
        controller: &PlaylistController,
        intent: SortCanonicalQueue,
    ) -> Result<MetadataSortJobId, MetadataSortStartError> {
        if self.active.is_some() {
            return Err(MetadataSortStartError::AlreadyActive);
        }
        let cpu_executor = cpu_executor.ok_or(MetadataSortStartError::CpuExecutorUnavailable)?;
        let job_id_value = self
            .next_job_id
            .ok_or(MetadataSortStartError::JobIdExhausted)?;
        let job_id = MetadataSortJobId(job_id_value);
        self.next_job_id = job_id_value.checked_add(1);
        let snapshot = controller.queue().canonical_sort_snapshot();
        let expected_structural_revision = controller.view_snapshot().structural_revision();
        let demands = metadata_demands(controller, intent.key());
        let active = if demands.is_empty() {
            let phase = self.submit_cpu(cpu_executor, snapshot.clone(), Vec::new(), intent)?;
            ActiveMetadataSort {
                job_id,
                expected_structural_revision,
                snapshot,
                intent,
                patches: Vec::new(),
                phase: ActivePhase::Cpu(phase),
                cancel_outcome: None,
                failure_counts: DiscoveryFailureCounts::default(),
                diagnostics: Arc::from([]),
                omitted_diagnostics: 0,
            }
        } else {
            let executor =
                discovery_executor.ok_or(MetadataSortStartError::DiscoveryExecutorUnavailable)?;
            let request_revision = DiscoveryRequestRevision::new(job_id.0);
            let paths = demands.iter().map(|demand| demand.path.clone()).collect();
            let handle = executor
                .submit(DiscoveryRequest::MetadataSortPreparation {
                    locators: paths,
                    request_revision,
                })
                .map_err(MetadataSortStartError::DiscoverySubmit)?;
            ActiveMetadataSort {
                job_id,
                expected_structural_revision,
                snapshot,
                intent,
                patches: Vec::new(),
                phase: ActivePhase::Probe(ProbePhase {
                    handle,
                    demands,
                    records: Vec::new(),
                    batch_ids: Vec::new(),
                }),
                cancel_outcome: None,
                failure_counts: DiscoveryFailureCounts::default(),
                diagnostics: Arc::from([]),
                omitted_diagnostics: 0,
            }
        };
        self.progress = None;
        self.active = Some(active);
        Ok(job_id)
    }

    pub(super) fn cancel(&mut self, job_id: MetadataSortJobId) -> MetadataSortCancelOutcome {
        let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.job_id == job_id)
        else {
            return MetadataSortCancelOutcome::StaleJob;
        };
        if let Some(existing) = active.cancel_outcome {
            return match existing {
                MetadataSortTerminalOutcome::Invalidated => {
                    MetadataSortCancelOutcome::AlreadyInvalidated
                }
                _ => MetadataSortCancelOutcome::AlreadyRequested,
            };
        }
        active.cancel_outcome = Some(MetadataSortTerminalOutcome::Cancelled);
        match &active.phase {
            ActivePhase::Probe(probe) => {
                let _requested = probe
                    .handle
                    .cancel(DiscoveryCancellationCause::UserCancelled);
            }
            ActivePhase::Cpu(cpu) => cpu.handle.cancel(),
        }
        MetadataSortCancelOutcome::Requested
    }

    pub(super) fn cancel_for_queue_replacement(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if active.cancel_outcome.is_some() {
            return;
        }
        active.cancel_outcome = Some(MetadataSortTerminalOutcome::Invalidated);
        match &active.phase {
            ActivePhase::Probe(probe) => {
                let _cancelled = probe
                    .handle
                    .cancel(DiscoveryCancellationCause::StructuralInvalidation);
            }
            ActivePhase::Cpu(cpu) => cpu.handle.cancel(),
        }
    }

    pub(super) fn begin_shutdown(&mut self) {
        self.cancel_for_queue_replacement();
    }

    pub(super) fn read_model(&self) -> MetadataSortReadModel {
        let phase = self.active.as_ref().map(|active| match active.phase {
            ActivePhase::Probe(_) => MetadataSortPhase::Probing,
            ActivePhase::Cpu(_) => MetadataSortPhase::PreparingKeys,
        });
        MetadataSortReadModel {
            active_job: self.active.as_ref().map(|active| active.job_id),
            phase,
            processed: self
                .progress
                .as_ref()
                .map_or(0, |progress| progress.processed),
            total: self.progress.as_ref().map_or_else(
                || {
                    self.active
                        .as_ref()
                        .map_or(0, |active| active.snapshot.item_count())
                },
                |progress| progress.total,
            ),
            latest_completion: self.latest_completion.clone(),
        }
    }

    pub(super) fn record_completion(&mut self, completion: MetadataSortCompletion) {
        self.latest_completion = Some(completion);
        self.progress = None;
    }

    pub(super) fn drain(
        &mut self,
        cpu_executor: Option<&BoundedExecutor>,
        current_structural_revision: PlaylistStructuralRevision,
    ) -> Option<MetadataSortTerminal> {
        let mut active = self.active.take()?;
        if current_structural_revision != active.expected_structural_revision
            && active.cancel_outcome.is_none()
        {
            active.cancel_outcome = Some(MetadataSortTerminalOutcome::Invalidated);
            match &active.phase {
                ActivePhase::Probe(probe) => {
                    let _cancelled = probe
                        .handle
                        .cancel(DiscoveryCancellationCause::StructuralInvalidation);
                }
                ActivePhase::Cpu(cpu) => cpu.handle.cancel(),
            }
        }

        let probe_summary = if let ActivePhase::Probe(probe) = &mut active.phase {
            drain_probe_events(probe);
            if let Some(progress) = probe.handle.take_progress() {
                self.progress = Some(progress);
            }
            let summary = probe.handle.take_final_summary();
            if summary.is_some() {
                active.patches = patches_from_records(&probe.records, &probe.demands);
            }
            summary
        } else {
            None
        };

        if let Some(summary) = probe_summary {
            active.failure_counts = summary.failure_counts;
            active.diagnostics = Arc::from(summary.diagnostics);
            active.omitted_diagnostics = summary.omitted_diagnostics;
            let terminal_outcome = active.cancel_outcome.or(match summary.outcome {
                DiscoveryFinalOutcome::Completed | DiscoveryFinalOutcome::LimitReached => None,
                DiscoveryFinalOutcome::Cancelled(_) => Some(MetadataSortTerminalOutcome::Cancelled),
                DiscoveryFinalOutcome::ExecutorDisconnected => {
                    Some(MetadataSortTerminalOutcome::Failed)
                }
            });
            if let Some(outcome) = terminal_outcome {
                return Some(salvage(&mut active, outcome));
            }
            let Some(cpu_executor) = cpu_executor else {
                return Some(salvage(&mut active, MetadataSortTerminalOutcome::Failed));
            };
            match self.submit_cpu(
                cpu_executor,
                active.snapshot.clone(),
                active.patches.clone(),
                active.intent,
            ) {
                Ok(cpu) => {
                    active.phase = ActivePhase::Cpu(cpu);
                    self.active = Some(active);
                    return None;
                }
                Err(_) => {
                    return Some(salvage(&mut active, MetadataSortTerminalOutcome::Failed));
                }
            }
        }

        let cpu_poll = if let ActivePhase::Cpu(cpu) = &active.phase {
            Some(cpu.handle.try_take())
        } else {
            None
        };
        let terminal = match cpu_poll {
            None | Some(TaskPoll::Pending) => None,
            Some(TaskPoll::Completed(Ok(prepared))) => {
                if let Some(outcome) = active.cancel_outcome {
                    Some(salvage(&mut active, outcome))
                } else {
                    Some(MetadataSortTerminal::Prepared {
                        job_id: active.job_id,
                        expected_structural_revision: active.expected_structural_revision,
                        prepared,
                        patches: std::mem::take(&mut active.patches),
                        failure_counts: active.failure_counts,
                        diagnostics: Arc::clone(&active.diagnostics),
                        omitted_diagnostics: active.omitted_diagnostics,
                    })
                }
            }
            Some(
                TaskPoll::Completed(Err(_)) | TaskPoll::Failed(TaskFailure::CancelledBeforeStart),
            ) => {
                let outcome = active
                    .cancel_outcome
                    .unwrap_or(MetadataSortTerminalOutcome::Cancelled);
                Some(salvage(&mut active, outcome))
            }
            Some(TaskPoll::Failed(TaskFailure::Panicked | TaskFailure::ExecutorStopped)) => {
                Some(salvage(&mut active, MetadataSortTerminalOutcome::Failed))
            }
        };

        if terminal.is_none() {
            self.active = Some(active);
        }
        terminal
    }

    fn submit_cpu(
        &self,
        executor: &BoundedExecutor,
        snapshot: CanonicalSortSnapshot,
        patches: Vec<PlaylistMetadataPatch>,
        intent: SortCanonicalQueue,
    ) -> Result<CpuPhase, MetadataSortStartError> {
        let wake_port = self.wake_port.clone();
        let handle = executor
            .try_submit_with_terminal_notifier(
                move |cancellation| {
                    snapshot.prepare(&patches, intent, || cancellation.is_cancelled())
                },
                move || {
                    let _wake_delivery = wake_port.request_wake();
                },
            )
            .map_err(MetadataSortStartError::CpuSubmit)?;
        Ok(CpuPhase { handle })
    }
}

fn metadata_demands(
    controller: &PlaylistController,
    sort_key: PlaylistSortKey,
) -> Vec<MetadataSortDemand> {
    if sort_key == PlaylistSortKey::NaturalFilename {
        return Vec::new();
    }
    controller
        .queue()
        .iter_playable_items()
        .filter(|item| {
            item.local_fingerprint().is_none()
                || selected_metadata_is_missing(item.cached_metadata(), sort_key)
        })
        .filter_map(|item| {
            let locator = item.locator().clone();
            let path = locator
                .as_local()?
                .expose_native_path_for_open()?
                .to_path_buf();
            Some(MetadataSortDemand {
                item_id: item.item_id(),
                locator,
                expected_fingerprint: item.local_fingerprint(),
                path,
            })
        })
        .collect()
}

fn selected_metadata_is_missing(
    metadata: &CachedPlaylistMetadata,
    sort_key: PlaylistSortKey,
) -> bool {
    match sort_key {
        PlaylistSortKey::NaturalFilename => false,
        PlaylistSortKey::Title => metadata.title().is_none(),
        PlaylistSortKey::Artist => metadata.artists().is_empty(),
        PlaylistSortKey::Album => metadata.album().is_none(),
        PlaylistSortKey::Duration => metadata.duration().is_none(),
        PlaylistSortKey::SmartSequence => match metadata.media_kind() {
            PlaylistMediaKind::Audio => {
                metadata.disc_number().is_none()
                    || metadata.track_number().is_none()
                    || metadata.title().is_none()
            }
            PlaylistMediaKind::Video => {
                metadata.season_number().is_none()
                    || metadata.episode_number().is_none()
                    || metadata.title().is_none()
            }
            PlaylistMediaKind::Unknown => true,
        },
    }
}

fn drain_probe_events(probe: &mut ProbePhase) {
    for event in probe.handle.drain_events() {
        let DiscoveryEvent::AdmittedBatch(batch) = event else {
            continue;
        };
        if batch.apply_semantics() != BatchApplySemantics::AccumulateUntilTerminalAtomicApply {
            continue;
        }
        probe.records.extend_from_slice(batch.records());
        probe.batch_ids.push(batch.batch_id());
    }
}

fn patches_from_records(
    records: &[DiscoveryRecord],
    demands: &[MetadataSortDemand],
) -> Vec<PlaylistMetadataPatch> {
    records
        .iter()
        .filter_map(|record| {
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
        })
        .collect()
}

fn salvage(
    active: &mut ActiveMetadataSort,
    outcome: MetadataSortTerminalOutcome,
) -> MetadataSortTerminal {
    MetadataSortTerminal::Salvage {
        job_id: active.job_id,
        outcome,
        patches: std::mem::take(&mut active.patches),
        failure_counts: active.failure_counts,
        diagnostics: Arc::clone(&active.diagnostics),
        omitted_diagnostics: active.omitted_diagnostics,
    }
}
