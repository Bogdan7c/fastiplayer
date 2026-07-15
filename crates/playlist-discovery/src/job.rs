//! Immutable requests и job-owned discovery/frontier state.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use crate::frontier::{SiblingAdmissionFrontier, TerminalCandidate};
use crate::mailbox::{JobMailbox, WakeCoordinator};
use crate::readiness_ack::PendingReadinessAcks;
use crate::request::{
    DiscoveryRequest, ReprioritizeHint, ReprioritizeOutcome, WorkPlan, WorkUnit, build_work_plan,
};
use crate::stream::AdmittedBatchContext;
use crate::{
    AdmissionAckOutcome, AdmissionAdvanced, AdmissionBatchId, AdmissionDirection, AdmittedBatch,
    BatchApplySemantics, CandidateSourceDiagnostic, DISCOVERY_DIAGNOSTIC_LIMIT, DirectoryManifest,
    DiscoveryCancellation, DiscoveryCancellationCause, DiscoveryDiagnostic, DiscoveryEvent,
    DiscoveryFailureCounts, DiscoveryFinalOutcome, DiscoveryFinalSummary, DiscoveryJobId,
    DiscoveryJobKind, DiscoveryPriority, DiscoveryProgress, DiscoveryRecord, DiscoveryRecordKey,
    DiscoveryRequestRevision, FrontierReady, LocalMediaKind, ManifestCandidateKey,
    ProbeDiagnosticKind, ProbeOneLocalMediaError, ProbedLocalMedia, SiblingDiscoveryPolicySnapshot,
    VERIFIED_RECORD_BUFFER_LIMIT,
};

pub(crate) trait JobSchedulerPort: Send + Sync {
    fn reschedule(&self, job_id: DiscoveryJobId);
    fn reprioritize_queued(
        &self,
        job_id: DiscoveryJobId,
        preferred_keys: &[ManifestCandidateKey],
    ) -> ReprioritizeOutcome;
    fn remove_queued(&self, job_id: DiscoveryJobId) -> usize;
}

struct JobState {
    pending_work: VecDeque<WorkUnit>,
    active_work: usize,
    completion_flushes_pending: usize,
    processed: usize,
    verified: usize,
    failed: usize,
    failure_counts: DiscoveryFailureCounts,
    owned_uncommitted_records: usize,
    diagnostics: Vec<DiscoveryDiagnostic>,
    omitted_diagnostics: usize,
    frontier: Option<SiblingAdmissionFrontier>,
    nondirectional_buffer: VecDeque<DiscoveryRecord>,
    pending_acks: PendingReadinessAcks,
    readiness_batch_assigned: HashSet<AdmissionDirection>,
    ready_directions: HashSet<AdmissionDirection>,
    next_batch_id: u64,
    terminal_published: bool,
    forced_outcome: Option<DiscoveryFinalOutcome>,
}

pub(crate) struct JobInner {
    id: DiscoveryJobId,
    kind: DiscoveryJobKind,
    priority: DiscoveryPriority,
    request_revision: DiscoveryRequestRevision,
    policy: Option<SiblingDiscoveryPolicySnapshot>,
    opened_media_kind: Option<LocalMediaKind>,
    total: usize,
    outstanding_work_limit: usize,
    speculative_execution_permit_limit: usize,
    cancellation: DiscoveryCancellation,
    mailbox: Arc<JobMailbox>,
    admission_flush: Mutex<()>,
    state: Mutex<JobState>,
    manifest: Option<Arc<DirectoryManifest>>,
    scheduler_port: Mutex<Option<Arc<dyn JobSchedulerPort>>>,
}

impl JobInner {
    pub(crate) fn new(
        id: DiscoveryJobId,
        request: DiscoveryRequest,
        wake_coordinator: Arc<WakeCoordinator>,
        outstanding_work_limit: usize,
        speculative_execution_permit_limit: usize,
    ) -> Arc<Self> {
        let kind = request.kind();
        let priority = request.priority();
        let request_revision = request.request_revision();
        let work_plan = build_work_plan(request);
        let WorkPlan {
            pending_work,
            frontier,
            manifest,
            policy,
            opened_media_kind,
        } = work_plan;
        let total = pending_work.len();
        let job = Arc::new(Self {
            id,
            kind,
            priority,
            request_revision,
            policy,
            opened_media_kind,
            total,
            outstanding_work_limit,
            speculative_execution_permit_limit,
            cancellation: DiscoveryCancellation::default(),
            mailbox: JobMailbox::new(wake_coordinator),
            admission_flush: Mutex::new(()),
            state: Mutex::new(JobState {
                pending_work,
                active_work: 0,
                completion_flushes_pending: 0,
                processed: 0,
                verified: 0,
                failed: 0,
                failure_counts: DiscoveryFailureCounts::default(),
                owned_uncommitted_records: 0,
                diagnostics: Vec::new(),
                omitted_diagnostics: 0,
                frontier,
                nondirectional_buffer: VecDeque::new(),
                pending_acks: PendingReadinessAcks::default(),
                readiness_batch_assigned: HashSet::new(),
                ready_directions: HashSet::new(),
                next_batch_id: 1,
                terminal_published: false,
                forced_outcome: None,
            }),
            manifest,
            scheduler_port: Mutex::new(None),
        });
        job.publish_progress();
        job.publish_terminal_if_complete();
        job
    }

    pub(crate) const fn id(&self) -> DiscoveryJobId {
        self.id
    }

    pub(crate) const fn priority(&self) -> DiscoveryPriority {
        self.priority
    }

    pub(crate) fn cancellation(&self) -> DiscoveryCancellation {
        self.cancellation.clone()
    }

    pub(crate) fn set_scheduler_port(&self, scheduler_port: Arc<dyn JobSchedulerPort>) {
        *self
            .scheduler_port
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(scheduler_port);
    }

    fn request_reschedule(&self) {
        let scheduler_port = self
            .scheduler_port
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(scheduler_port) = scheduler_port {
            scheduler_port.reschedule(self.id);
        }
    }

    fn remove_queued_work(&self) {
        let scheduler_port = self
            .scheduler_port
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let removed =
            scheduler_port.map_or(0, |scheduler_port| scheduler_port.remove_queued(self.id));
        if removed != 0 {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.active_work = state.active_work.saturating_sub(removed);
        }
    }

    pub(crate) fn take_work(&self) -> Option<WorkUnit> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.cancellation.cause().is_some()
            || state.active_work == self.outstanding_work_limit
            || state.owned_uncommitted_records + state.active_work >= VERIFIED_RECORD_BUFFER_LIMIT
        {
            return None;
        }
        let schedulable_position = state.pending_work.iter().position(|work| {
            state.frontier.as_ref().is_none_or(|frontier| {
                frontier.can_schedule(work.direction, work.directional_offset)
            })
        })?;
        let work = state.pending_work.remove(schedulable_position)?;
        state.active_work += 1;
        Some(work)
    }

    pub(crate) fn return_unstarted_work(&self, work: WorkUnit) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_work = state.active_work.saturating_sub(1);
        state.pending_work.push_front(work);
    }

    pub(crate) fn has_schedulable_work(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.cancellation.cause().is_none()
            && state.pending_work.iter().any(|work| {
                state.frontier.as_ref().is_none_or(|frontier| {
                    frontier.can_schedule(work.direction, work.directional_offset)
                })
            })
            && state.active_work < self.outstanding_work_limit
            && state.owned_uncommitted_records + state.active_work < VERIFIED_RECORD_BUFFER_LIMIT
    }

    pub(crate) const fn execution_permit_limit(&self, priority: DiscoveryPriority) -> usize {
        match priority {
            DiscoveryPriority::Foreground => 1,
            DiscoveryPriority::Speculative => self.speculative_execution_permit_limit,
        }
    }

    pub(crate) fn is_terminal_published(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .terminal_published
    }

    pub(crate) fn has_admitted_work_or_is_empty(&self) -> bool {
        if self.total == 0 {
            return true;
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_work != 0 || state.processed != 0 || state.terminal_published
    }

    pub(crate) fn complete_work(
        &self,
        work: WorkUnit,
        result: Result<ProbedLocalMedia, ProbeOneLocalMediaError>,
    ) {
        let _linearization = self
            .admission_flush
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_work = state.active_work.saturating_sub(1);
        state.processed += 1;

        if self.cancellation.cause().is_some() {
            drop(state);
            self.publish_progress();
            self.publish_terminal_if_complete();
            return;
        }

        let terminal = match result {
            Ok(media) if self.admits(media.media_kind()) => {
                state.verified += 1;
                state.owned_uncommitted_records += 1;
                TerminalCandidate::Eligible(Box::new(DiscoveryRecord::new(
                    work.key,
                    work.locator,
                    media,
                )))
            }
            Ok(_) => TerminalCandidate::Ineligible,
            Err(ProbeOneLocalMediaError::Cancelled) => {
                drop(state);
                drop(_linearization);
                self.cancel(DiscoveryCancellationCause::UserCancelled);
                return;
            }
            Err(error) => {
                state.failed += 1;
                retain_diagnostic(&mut state, work.key, diagnostic_kind(&error));
                TerminalCandidate::Ineligible
            }
        };

        if let Some(frontier) = &mut state.frontier {
            let _ = frontier.record_terminal(work.direction, work.directional_offset, terminal);
        } else if let TerminalCandidate::Eligible(record) = terminal {
            state.nondirectional_buffer.push_back(*record);
        }
        state.completion_flushes_pending += 1;
        drop(state);
        self.flush_admission_locked();
        self.finish_completion_flush();
        self.publish_progress();
        self.publish_terminal_if_complete();
    }

    /// Matching fingerprint завершает visible work без container/demux probe.
    pub(crate) fn complete_fingerprint_unchanged(&self, _work: WorkUnit) {
        let _linearization = self
            .admission_flush
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_work = state.active_work.saturating_sub(1);
        state.processed += 1;
        if self.cancellation.cause().is_none() && state.forced_outcome.is_none() {
            state.completion_flushes_pending += 1;
        }
        drop(state);
        self.flush_admission_locked();
        self.finish_completion_flush();
        self.publish_progress();
        self.publish_terminal_if_complete();
    }

    fn admits(&self, candidate_kind: LocalMediaKind) -> bool {
        match (self.policy, self.opened_media_kind) {
            (Some(policy), Some(opened_kind)) => {
                policy.load_siblings() && policy.filter().admits(opened_kind, candidate_kind)
            }
            _ => true,
        }
    }

    const fn batch_apply_semantics(&self) -> BatchApplySemantics {
        match self.kind {
            DiscoveryJobKind::SiblingDiscovery => BatchApplySemantics::ProgressiveSiblingCommit,
            DiscoveryJobKind::ManualBatch | DiscoveryJobKind::MetadataSortPreparation => {
                BatchApplySemantics::AccumulateUntilTerminalAtomicApply
            }
            DiscoveryJobKind::VisibleRefresh => BatchApplySemantics::MetadataRefreshChunk,
        }
    }

    pub(crate) fn cancel(&self, cause: DiscoveryCancellationCause) -> bool {
        let _linearization = self
            .admission_flush
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.terminal_published || !self.cancellation.cancel(cause) {
            return false;
        }
        drop(state);
        self.remove_queued_work();
        self.discard_uncommitted_state_locked();
        self.publish_terminal_if_complete();
        true
    }

    pub(crate) fn freeze(&self) -> bool {
        let _linearization = self
            .admission_flush
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.cancellation.freeze()
    }

    pub(crate) fn resume(&self) -> bool {
        let _linearization = self
            .admission_flush
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.cancellation.resume() {
            return false;
        }
        self.flush_admission_locked();
        self.request_reschedule();
        self.publish_terminal_if_complete();
        true
    }

    pub(crate) fn reprioritize(&self, hint: ReprioritizeHint) -> ReprioritizeOutcome {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut preferred = VecDeque::new();
        let mut reprioritized = 0;
        let mut unresolved = Vec::new();
        for candidate_key in hint.preferred_keys.iter().copied() {
            let position = state
                .pending_work
                .iter()
                .position(|work| work.key == DiscoveryRecordKey::Manifest(candidate_key));
            if let Some(position) = position {
                if let Some(mut work) = state.pending_work.remove(position) {
                    work.priority = DiscoveryPriority::Foreground;
                    preferred.push_back(work);
                    reprioritized += 1;
                }
            } else {
                unresolved.push(candidate_key);
            }
        }
        preferred.append(&mut state.pending_work);
        state.pending_work = preferred;
        drop(state);
        let scheduler_port = self
            .scheduler_port
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let queued = scheduler_port.map_or(
            ReprioritizeOutcome {
                reprioritized: 0,
                stale: unresolved.len(),
            },
            |scheduler_port| scheduler_port.reprioritize_queued(self.id, &unresolved),
        );
        self.request_reschedule();
        ReprioritizeOutcome {
            reprioritized: reprioritized + queued.reprioritized,
            stale: queued.stale,
        }
    }

    pub(crate) fn acknowledge_batch(&self, batch_id: AdmissionBatchId) -> AdmissionAckOutcome {
        let _linearization = self
            .admission_flush
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.cancellation.cause().is_some() {
            return AdmissionAckOutcome::JobTerminated;
        }
        if self.cancellation.is_frozen() {
            return AdmissionAckOutcome::AdmissionFrozen;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(nearest_candidates) = state.pending_acks.take(batch_id) else {
            return AdmissionAckOutcome::StaleOrAlreadyAcknowledged;
        };
        let mut ready_events = Vec::new();
        for (direction, candidate_key, revision) in nearest_candidates {
            if state.ready_directions.insert(direction) {
                ready_events.push(DiscoveryEvent::FrontierReady(FrontierReady::new(
                    self.id,
                    self.request_revision,
                    self.policy.map(SiblingDiscoveryPolicySnapshot::revision),
                    direction,
                    candidate_key,
                    revision,
                )));
            }
        }
        drop(state);
        for event in ready_events {
            self.mailbox.publish_marker(event);
        }
        AdmissionAckOutcome::Accepted
    }

    pub(crate) fn take_events(&self) -> Vec<DiscoveryEvent> {
        let _linearization = self
            .admission_flush
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let events = self.mailbox.take_events();
        let transferred_record_count = events
            .iter()
            .map(|event| match event {
                DiscoveryEvent::AdmittedBatch(batch) => batch.records().len(),
                DiscoveryEvent::AdmissionAdvanced(_) | DiscoveryEvent::FrontierReady(_) => 0,
            })
            .sum::<usize>();
        if transferred_record_count != 0 {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.owned_uncommitted_records = state
                .owned_uncommitted_records
                .saturating_sub(transferred_record_count);
        }
        self.flush_admission_locked();
        self.request_reschedule();
        events
    }

    pub(crate) fn take_progress(&self) -> Option<DiscoveryProgress> {
        self.mailbox.take_progress()
    }

    pub(crate) fn take_terminal(&self) -> Option<DiscoveryFinalSummary> {
        self.mailbox.take_terminal()
    }

    pub(crate) fn wake_disconnected(&self) -> bool {
        self.mailbox.wake_disconnected()
    }

    fn flush_admission_locked(&self) {
        if self.cancellation.is_frozen() || self.cancellation.cause().is_some() {
            return;
        }
        let available_records = self.mailbox.remaining_record_capacity();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut releases = if let Some(frontier) = &mut state.frontier {
            frontier.release_contiguous(available_records)
        } else {
            let release_count = available_records
                .min(crate::ADMITTED_BATCH_RECORD_LIMIT)
                .min(state.nondirectional_buffer.len());
            if release_count == 0 {
                Vec::new()
            } else {
                vec![crate::frontier::FrontierRelease {
                    direction: AdmissionDirection::NonDirectional,
                    records: state.nondirectional_buffer.drain(..release_count).collect(),
                    revision: state.processed as u64,
                    exhausted: state.processed == self.total,
                    side_accounting: None,
                }]
            }
        };

        let mut events = Vec::new();
        for release in releases.drain(..) {
            if !release.records.is_empty() {
                let Some(batch_id) = AdmissionBatchId::from_counter(state.next_batch_id) else {
                    continue;
                };
                state.next_batch_id = state.next_batch_id.saturating_add(1);
                let mut nearest_candidates = Vec::new();
                if release.direction != AdmissionDirection::NonDirectional
                    && state.readiness_batch_assigned.insert(release.direction)
                    && let Some(DiscoveryRecordKey::Manifest(candidate_key)) =
                        release.records.first().map(DiscoveryRecord::key)
                {
                    nearest_candidates.push((release.direction, candidate_key, release.revision));
                }
                state
                    .pending_acks
                    .retain_if_required(batch_id, nearest_candidates);
                events.push(DiscoveryEvent::AdmittedBatch(AdmittedBatch::new(
                    AdmittedBatchContext {
                        job_id: self.id,
                        request_revision: self.request_revision,
                        policy_revision: self.policy.map(SiblingDiscoveryPolicySnapshot::revision),
                        batch_id,
                        direction: release.direction,
                        frontier_revision: (release.direction
                            != AdmissionDirection::NonDirectional)
                            .then_some(release.revision),
                        side_accounting: release.side_accounting,
                        apply_semantics: self.batch_apply_semantics(),
                    },
                    release.records,
                )));
            }
            events.push(DiscoveryEvent::AdmissionAdvanced(AdmissionAdvanced::new(
                self.id,
                self.request_revision,
                self.policy.map(SiblingDiscoveryPolicySnapshot::revision),
                release.direction,
                release.revision,
                release.exhausted,
            )));
        }
        if state
            .frontier
            .as_ref()
            .is_some_and(SiblingAdmissionFrontier::limit_reached)
        {
            state.pending_work.clear();
        }
        drop(state);
        for event in events {
            match event {
                DiscoveryEvent::AdmittedBatch(batch) => {
                    let published = self.mailbox.publish_batch(batch);
                    debug_assert!(published, "record capacity checked before frontier release");
                }
                marker => self.mailbox.publish_marker(marker),
            }
        }
    }

    fn discard_uncommitted_state_locked(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending_work.clear();
        state.nondirectional_buffer.clear();
        state.pending_acks.clear();
        state.readiness_batch_assigned.clear();
        state.owned_uncommitted_records = 0;
        if let Some(frontier) = &mut state.frontier {
            frontier.clear_unadmitted();
        }
        drop(state);
        self.mailbox.discard_all_events();
    }

    fn publish_progress(&self) {
        let processed = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .processed;
        self.mailbox.publish_progress(DiscoveryProgress {
            job_id: self.id,
            kind: self.kind,
            processed,
            total: self.total,
        });
    }

    fn publish_terminal_if_complete(&self) {
        let cause = self.cancellation.cause();
        let frozen = self.cancellation.is_frozen();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.terminal_published
            || state.active_work != 0
            || state.completion_flushes_pending != 0
            || (frozen && state.owned_uncommitted_records != 0)
        {
            return;
        }
        let frontier_limit_reached = state
            .frontier
            .as_ref()
            .is_some_and(SiblingAdmissionFrontier::limit_reached);
        if cause.is_none() && !frontier_limit_reached && !state.pending_work.is_empty() {
            return;
        }
        state.terminal_published = true;
        let outcome = if let Some(forced_outcome) = state.forced_outcome {
            forced_outcome
        } else if let Some(cause) = cause {
            DiscoveryFinalOutcome::Cancelled(cause)
        } else if frontier_limit_reached {
            DiscoveryFinalOutcome::LimitReached
        } else {
            DiscoveryFinalOutcome::Completed
        };
        let summary = DiscoveryFinalSummary {
            job_id: self.id,
            kind: self.kind,
            request_revision: self.request_revision,
            policy_revision: self.policy.map(SiblingDiscoveryPolicySnapshot::revision),
            outcome,
            verified: state.verified,
            failed: state.failed,
            failure_counts: state.failure_counts,
            diagnostics: state.diagnostics.clone().into_boxed_slice(),
            omitted_diagnostics: state.omitted_diagnostics,
        };
        drop(state);
        let published = self.mailbox.publish_terminal(summary);
        debug_assert!(published, "terminal slot is written exactly once");
    }

    pub(crate) fn validate_source(&self, work: &WorkUnit) -> Result<(), CandidateSourceDiagnostic> {
        let Some(manifest) = &self.manifest else {
            return Ok(());
        };
        let DiscoveryRecordKey::Manifest(candidate_key) = work.key else {
            return Ok(());
        };
        manifest.validate_candidate_source(candidate_key)
    }

    pub(crate) fn record_source_failure(
        &self,
        work: WorkUnit,
        diagnostic: CandidateSourceDiagnostic,
    ) {
        let _linearization = self
            .admission_flush
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let kind = match diagnostic {
            CandidateSourceDiagnostic::UnknownCandidateKey { .. } => {
                ProbeDiagnosticKind::SourceChangedAfterSnapshot
            }
            CandidateSourceDiagnostic::MissingAfterSnapshot { .. } => {
                ProbeDiagnosticKind::MissingAfterSnapshot
            }
            CandidateSourceDiagnostic::SourceChangedAfterSnapshot { .. } => {
                ProbeDiagnosticKind::SourceChangedAfterSnapshot
            }
            CandidateSourceDiagnostic::UnavailableAfterSnapshot { error_kind, .. } => {
                ProbeDiagnosticKind::UnavailableAfterSnapshot(error_kind)
            }
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_work = state.active_work.saturating_sub(1);
        state.processed += 1;
        if self.cancellation.cause().is_some() || state.forced_outcome.is_some() {
            drop(state);
            self.publish_progress();
            self.publish_terminal_if_complete();
            return;
        }
        state.failed += 1;
        retain_diagnostic(&mut state, work.key, kind);
        if let Some(frontier) = &mut state.frontier {
            let _ = frontier.record_terminal(
                work.direction,
                work.directional_offset,
                TerminalCandidate::Ineligible,
            );
        }
        state.completion_flushes_pending += 1;
        drop(state);
        self.flush_admission_locked();
        self.finish_completion_flush();
        self.publish_progress();
        self.publish_terminal_if_complete();
    }

    fn finish_completion_flush(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.completion_flushes_pending = state.completion_flushes_pending.saturating_sub(1);
    }

    pub(crate) fn begin_probe_if_active(&self) -> bool {
        let _linearization = self
            .admission_flush
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.cancellation.cause().is_none() && state.forced_outcome.is_none()
    }

    pub(crate) fn abandon_in_flight_work(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_work = state.active_work.saturating_sub(1);
        drop(state);
        self.publish_terminal_if_complete();
    }

    pub(crate) fn fail_executor_disconnected(&self) {
        let _linearization = self
            .admission_flush
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_work = state.active_work.saturating_sub(1);
        if state.forced_outcome.is_none() {
            state.forced_outcome = Some(DiscoveryFinalOutcome::ExecutorDisconnected);
            let _ = self
                .cancellation
                .cancel(DiscoveryCancellationCause::LifecycleShutdown);
        }
        drop(state);
        self.remove_queued_work();
        self.discard_uncommitted_state_locked();
        self.publish_terminal_if_complete();
    }
}

fn retain_diagnostic(state: &mut JobState, key: DiscoveryRecordKey, kind: ProbeDiagnosticKind) {
    state.failure_counts.record(&kind);
    if state.diagnostics.len() < DISCOVERY_DIAGNOSTIC_LIMIT {
        state.diagnostics.push(DiscoveryDiagnostic { key, kind });
    } else {
        state.omitted_diagnostics = state.omitted_diagnostics.saturating_add(1);
    }
}

fn diagnostic_kind(error: &ProbeOneLocalMediaError) -> ProbeDiagnosticKind {
    match error {
        ProbeOneLocalMediaError::Cancelled => ProbeDiagnosticKind::ProbeFailure,
        ProbeOneLocalMediaError::UnsupportedContainer { .. } => {
            ProbeDiagnosticKind::UnsupportedContainer
        }
        ProbeOneLocalMediaError::NoAudioVideoTracks => ProbeDiagnosticKind::NoAudioVideoTracks,
        ProbeOneLocalMediaError::IoFailure(error) => ProbeDiagnosticKind::IoFailure(error.kind()),
        ProbeOneLocalMediaError::ProbeFailure { .. } => ProbeDiagnosticKind::ProbeFailure,
    }
}
