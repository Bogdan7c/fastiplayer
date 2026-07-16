//! Runtime terminal serialization, Undo ordering и persistence publication.

use super::*;
use crate::playlist_runtime::PlaylistRuntime;
use crate::playlist_runtime::controller::ControllerCanonicalSortError;

impl PlaylistRuntime {
    /// Запускает one-shot Sort; UI wiring остаётся следующей session.
    #[allow(dead_code, reason = "Session 19 вызывает typed Sort action")]
    pub(crate) fn start_metadata_sort(
        &mut self,
        intent: SortCanonicalQueue,
    ) -> Result<MetadataSortJobId, MetadataSortStartError> {
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(MetadataSortStartError::RuntimeShuttingDown);
        }
        let Some(controller) = self.controller.as_ref() else {
            return Err(MetadataSortStartError::LoadDecisionPending);
        };
        self.discovery.metadata_sort.start(
            self.discovery.executor.as_ref(),
            self.discovery.cpu_executor.as_ref(),
            controller,
            intent,
        )
    }

    #[allow(dead_code, reason = "Session 19 вызывает typed Sort action")]
    pub(crate) fn cancel_metadata_sort(
        &mut self,
        job_id: MetadataSortJobId,
    ) -> MetadataSortCancelOutcome {
        self.discovery.metadata_sort.cancel(job_id)
    }

    #[allow(dead_code, reason = "Session 19 отображает read model")]
    pub(crate) fn metadata_sort_read_model(&self) -> MetadataSortReadModel {
        self.discovery.metadata_sort.read_model()
    }

    pub(in crate::playlist_runtime) fn drain_metadata_sort(&mut self) -> bool {
        let Some(controller) = self.controller.as_ref() else {
            return false;
        };
        let structural_revision = controller.view_snapshot().structural_revision();
        let terminal = self
            .discovery
            .metadata_sort
            .drain(self.discovery.cpu_executor.as_ref(), structural_revision);
        let Some(terminal) = terminal else {
            return false;
        };
        let dirty_before = self
            .controller
            .as_ref()
            .expect("controller presence was checked before terminal apply")
            .dirty_revision();
        let completion = match terminal {
            MetadataSortTerminal::Prepared {
                job_id,
                expected_structural_revision,
                prepared,
                patches,
                failure_counts,
                diagnostics,
                omitted_diagnostics,
            } => {
                let statistics = prepared.statistics();
                match self
                    .controller
                    .as_ref()
                    .expect("controller presence was checked before terminal apply")
                    .preflight_canonical_sort(
                        expected_structural_revision,
                        prepared,
                        patches.clone(),
                    ) {
                    Ok(commit) => {
                        if commit.changed_persistent_state() {
                            self.invalidate_removal_undo_for_persistent_mutation();
                        }
                        let outcome = self
                            .controller
                            .as_mut()
                            .expect("controller presence was checked before terminal apply")
                            .commit_canonical_sort(commit);
                        let terminal_outcome = if outcome.domain.reordered() {
                            MetadataSortTerminalOutcome::Sorted
                        } else if outcome.domain.metadata().changed_metadata() {
                            MetadataSortTerminalOutcome::MetadataOnly
                        } else {
                            MetadataSortTerminalOutcome::AlreadyInCanonicalOrder
                        };
                        MetadataSortCompletion {
                            job_id,
                            outcome: terminal_outcome,
                            metadata_updated: outcome.domain.metadata().applied_count(),
                            failure_counts,
                            diagnostics,
                            omitted_diagnostics,
                            statistics: Some(statistics),
                        }
                    }
                    Err(error) => self.apply_metadata_sort_salvage(
                        job_id,
                        sort_error_terminal(error),
                        patches,
                        failure_counts,
                        diagnostics,
                        omitted_diagnostics,
                    ),
                }
            }
            MetadataSortTerminal::Salvage {
                job_id,
                outcome,
                patches,
                failure_counts,
                diagnostics,
                omitted_diagnostics,
            } => self.apply_metadata_sort_salvage(
                job_id,
                outcome,
                patches,
                failure_counts,
                diagnostics,
                omitted_diagnostics,
            ),
        };
        self.publish_controller_mutation_if_dirty(dirty_before);
        self.discovery.metadata_sort.record_completion(completion);
        true
    }

    pub(super) fn apply_metadata_sort_salvage(
        &mut self,
        job_id: MetadataSortJobId,
        terminal_outcome: MetadataSortTerminalOutcome,
        patches: Vec<PlaylistMetadataPatch>,
        failure_counts: DiscoveryFailureCounts,
        diagnostics: Arc<[DiscoveryDiagnostic]>,
        omitted_diagnostics: usize,
    ) -> MetadataSortCompletion {
        let metadata_preflight = self
            .controller
            .as_ref()
            .expect("salvage requires an installed controller")
            .preflight_metadata_patches(patches);
        let (metadata_updated, apply_failed) = match metadata_preflight {
            Ok(commit) => {
                if commit.changed_persistent_state() {
                    self.invalidate_removal_undo_for_persistent_mutation();
                }
                let outcome = self
                    .controller
                    .as_mut()
                    .expect("salvage requires an installed controller")
                    .commit_metadata_patches(commit);
                (outcome.domain.applied_count(), false)
            }
            Err(error) => {
                tracing::warn!(?error, "Metadata Sort salvage preflight отклонён");
                (0, true)
            }
        };
        let outcome = if apply_failed {
            MetadataSortTerminalOutcome::Failed
        } else if metadata_updated == 0
            && !matches!(
                terminal_outcome,
                MetadataSortTerminalOutcome::Cancelled | MetadataSortTerminalOutcome::Invalidated
            )
        {
            MetadataSortTerminalOutcome::NoChanges
        } else {
            terminal_outcome
        };
        MetadataSortCompletion {
            job_id,
            outcome,
            metadata_updated,
            failure_counts,
            diagnostics,
            omitted_diagnostics,
            statistics: None,
        }
    }
}

fn sort_error_terminal(error: ControllerCanonicalSortError) -> MetadataSortTerminalOutcome {
    match error {
        ControllerCanonicalSortError::StaleStructuralRevision
        | ControllerCanonicalSortError::Domain(
            playlist_core::ApplyPreparedCanonicalSortError::StaleQueueRevision,
        ) => MetadataSortTerminalOutcome::Invalidated,
        _ => MetadataSortTerminalOutcome::Failed,
    }
}
