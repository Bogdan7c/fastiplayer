//! Shared bounded discovery executor с reserved foreground lane и worker-ом.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;

use source_core::CancellationToken;

use crate::handle::DiscoveryJobHandle;
use crate::job::{JobInner, JobSchedulerPort};
use crate::mailbox::{DiscoveryWakePort, WakeCoordinator};
use crate::request::DISCOVERY_REQUEST_ITEM_LIMIT;
use crate::request::WorkUnit;
use crate::{
    DiscoveryCancellationCause, DiscoveryJobId, DiscoveryPriority, DiscoveryRecordKey,
    DiscoveryRequest, LocalMediaFingerprint, ManifestCandidateKey, ProbeOneLocalMediaError,
    ProbedLocalMedia, ReprioritizeOutcome, probe_one_local_media, read_local_media_fingerprint,
};

/// Общая bounded input capacity work units.
pub const DISCOVERY_INPUT_LIMIT: usize = 256;

/// Slots, которые speculative work никогда не занимает.
pub const FOREGROUND_RESERVED_INPUT_SLOTS: usize = 16;

/// Per-job queued share: 15 speculative jobs заполняют 240 normal slots.
pub const PER_JOB_INPUT_LIMIT: usize = 16;

/// Максимум одновременно зарегистрированных jobs.
pub const ACTIVE_DISCOVERY_JOB_LIMIT: usize = 16;

/// Один active-job slot также зарезервирован foreground intent-у.
pub const SPECULATIVE_ACTIVE_JOB_LIMIT: usize = ACTIVE_DISCOVERY_JOB_LIMIT - 1;

/// Минимум executor threads независимо от single-core report.
pub const MIN_DISCOVERY_WORKER_THREADS: usize = 2;

/// Максимум filesystem probe threads.
pub const MAX_DISCOVERY_WORKER_THREADS: usize = 4;

/// Ровно один worker навсегда не принимает speculative work.
pub const FOREGROUND_ONLY_WORKER_COUNT: usize = 1;

/// Single-file I/O owner, заменяемый deterministic fake-ом в focused tests.
pub trait DiscoveryProbe: Send + Sync + 'static {
    /// Читает только fingerprint; matching cache не должен запускать demux probe.
    fn read_fingerprint(
        &self,
        locator: &Path,
        cancellation: &CancellationToken,
    ) -> Result<LocalMediaFingerprint, ProbeOneLocalMediaError>;

    /// Выполняет один cooperative probe без app/domain side effects.
    fn probe(
        &self,
        locator: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProbedLocalMedia, ProbeOneLocalMediaError>;
}

/// Production adapter существующего Session 08 owner-а.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalMediaProbe;

impl DiscoveryProbe for LocalMediaProbe {
    fn read_fingerprint(
        &self,
        locator: &Path,
        cancellation: &CancellationToken,
    ) -> Result<LocalMediaFingerprint, ProbeOneLocalMediaError> {
        read_local_media_fingerprint(locator, cancellation)
    }

    fn probe(
        &self,
        locator: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProbedLocalMedia, ProbeOneLocalMediaError> {
        probe_one_local_media(locator, cancellation)
    }
}

/// Typed submit backpressure/disconnect outcomes.
#[derive(Debug, thiserror::Error)]
pub enum DiscoverySubmitError {
    /// Process может не создать требуемые bounded workers.
    #[error("не удалось создать discovery worker: {0:?}")]
    WorkerSpawn(io::ErrorKind),
    /// Active job registry достиг именованной границы.
    #[error("достигнут лимит активных discovery jobs")]
    ActiveJobLimitReached,
    /// Reserved/normal input lane не принимает новый work.
    #[error("bounded discovery input заполнен")]
    InputBackpressure,
    /// Executor больше не принимает jobs.
    #[error("discovery executor завершает работу")]
    ShuttingDown,
    /// Process-lifetime job ID space исчерпан.
    #[error("исчерпано пространство discovery job IDs")]
    JobIdExhausted,
    /// Caller передал больше bounded job-local items, чем representable policy разрешает.
    #[error("discovery request item limit {limit} reached; observed {observed}")]
    RequestItemLimitReached {
        /// Именованный hard limit.
        limit: usize,
        /// Exact caller-provided count.
        observed: usize,
    },
}

/// Nonblocking shutdown initiation report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscoveryShutdownReport {
    /// Jobs, которым впервые отправлен typed lifecycle cancel.
    pub cancelled_jobs: usize,
    /// Уже выполняющиеся blocking calls; они завершатся cooperative между stages.
    pub in_flight_work_units: usize,
}

struct ScheduledWork {
    job: Arc<JobInner>,
    work: WorkUnit,
}

struct SchedulerState {
    foreground: VecDeque<ScheduledWork>,
    speculative: VecDeque<ScheduledWork>,
    jobs: HashMap<DiscoveryJobId, Weak<JobInner>>,
    next_job_id: u64,
    in_flight: usize,
    running_by_job_lane: HashMap<(DiscoveryJobId, DiscoveryPriority), usize>,
    last_foreground_job: Option<DiscoveryJobId>,
    last_speculative_job: Option<DiscoveryJobId>,
    accepting: bool,
}

impl SchedulerState {
    fn queued_count(&self) -> usize {
        self.foreground.len() + self.speculative.len()
    }

    fn prune_finished_jobs(&mut self) {
        self.jobs.retain(|_, weak_job| {
            weak_job
                .upgrade()
                .is_some_and(|job| !job.is_terminal_published())
        });
    }
}

struct Scheduler {
    state: Mutex<SchedulerState>,
    work_available: Condvar,
}

impl Scheduler {
    fn new() -> Self {
        Self {
            state: Mutex::new(SchedulerState {
                foreground: VecDeque::new(),
                speculative: VecDeque::new(),
                jobs: HashMap::new(),
                next_job_id: 1,
                in_flight: 0,
                running_by_job_lane: HashMap::new(),
                last_foreground_job: None,
                last_speculative_job: None,
                accepting: true,
            }),
            work_available: Condvar::new(),
        }
    }

    fn enqueue_one(&self, job: &Arc<JobInner>) -> Result<bool, DiscoverySubmitError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.accepting {
            return Err(DiscoverySubmitError::ShuttingDown);
        }
        let Some(work) = job.take_work() else {
            return Ok(false);
        };
        let queued_count = state.queued_count();
        let allowed = match work.priority {
            DiscoveryPriority::Foreground => queued_count < DISCOVERY_INPUT_LIMIT,
            DiscoveryPriority::Speculative => {
                queued_count < DISCOVERY_INPUT_LIMIT - FOREGROUND_RESERVED_INPUT_SLOTS
            }
        };
        if !allowed {
            job.return_unstarted_work(work);
            return Err(DiscoverySubmitError::InputBackpressure);
        }
        let priority = work.priority;
        let scheduled = ScheduledWork {
            job: Arc::clone(job),
            work,
        };
        match priority {
            DiscoveryPriority::Foreground => state.foreground.push_back(scheduled),
            DiscoveryPriority::Speculative => state.speculative.push_back(scheduled),
        }
        drop(state);
        self.work_available.notify_all();
        Ok(true)
    }

    fn refill_job(&self, job: &Arc<JobInner>) {
        while job.has_schedulable_work() {
            if self.enqueue_one(job).is_err() {
                break;
            }
        }
    }

    fn refill_job_by_id(&self, job_id: DiscoveryJobId) {
        let job = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .jobs
            .get(&job_id)
            .and_then(Weak::upgrade);
        if let Some(job) = job {
            self.refill_job(&job);
        }
    }

    fn take_work(&self, foreground_only: bool) -> Option<ScheduledWork> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            let position = if foreground_only {
                schedulable_position(
                    &state.foreground,
                    &state.running_by_job_lane,
                    state.last_foreground_job,
                )
            } else {
                schedulable_position(
                    &state.speculative,
                    &state.running_by_job_lane,
                    state.last_speculative_job,
                )
            };
            let scheduled = if foreground_only {
                position.and_then(|position| state.foreground.remove(position))
            } else {
                position.and_then(|position| state.speculative.remove(position))
            };
            if let Some(scheduled) = scheduled {
                state.in_flight += 1;
                let priority = scheduled.work.priority;
                *state
                    .running_by_job_lane
                    .entry((scheduled.job.id(), priority))
                    .or_default() += 1;
                match priority {
                    DiscoveryPriority::Foreground => {
                        state.last_foreground_job = Some(scheduled.job.id());
                    }
                    DiscoveryPriority::Speculative => {
                        state.last_speculative_job = Some(scheduled.job.id());
                    }
                }
                return Some(scheduled);
            }
            if !state.accepting {
                return None;
            }
            state = self
                .work_available
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn finish_work(&self, job_id: DiscoveryJobId, priority: DiscoveryPriority) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.in_flight = state.in_flight.saturating_sub(1);
        let running_key = (job_id, priority);
        if let Some(running) = state.running_by_job_lane.get_mut(&running_key) {
            *running = running.saturating_sub(1);
            if *running == 0 {
                state.running_by_job_lane.remove(&running_key);
            }
        }
        drop(state);
        self.work_available.notify_all();
    }

    fn remove_queued(&self, job_id: DiscoveryJobId) -> usize {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = state.queued_count();
        state
            .foreground
            .retain(|scheduled| scheduled.job.id() != job_id);
        state
            .speculative
            .retain(|scheduled| scheduled.job.id() != job_id);
        let removed = before.saturating_sub(state.queued_count());
        drop(state);
        self.work_available.notify_all();
        removed
    }

    fn reprioritize_queued(
        &self,
        job_id: DiscoveryJobId,
        preferred_keys: &[ManifestCandidateKey],
    ) -> ReprioritizeOutcome {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut promoted = Vec::new();
        let mut stale = 0;
        for candidate_key in preferred_keys {
            let expected_key = DiscoveryRecordKey::Manifest(*candidate_key);
            let foreground_position = state.foreground.iter().position(|scheduled| {
                scheduled.job.id() == job_id && scheduled.work.key == expected_key
            });
            let speculative_position = state.speculative.iter().position(|scheduled| {
                scheduled.job.id() == job_id && scheduled.work.key == expected_key
            });
            let scheduled = foreground_position
                .and_then(|position| state.foreground.remove(position))
                .or_else(|| {
                    speculative_position.and_then(|position| state.speculative.remove(position))
                });
            if let Some(mut scheduled) = scheduled {
                scheduled.work.priority = DiscoveryPriority::Foreground;
                promoted.push(scheduled);
            } else {
                stale += 1;
            }
        }
        let reprioritized = promoted.len();
        for scheduled in promoted.into_iter().rev() {
            state.foreground.push_front(scheduled);
        }
        drop(state);
        self.work_available.notify_all();
        ReprioritizeOutcome {
            reprioritized,
            stale,
        }
    }
}

struct SchedulerPort {
    scheduler: Weak<Scheduler>,
}

impl JobSchedulerPort for SchedulerPort {
    fn reschedule(&self, job_id: DiscoveryJobId) {
        if let Some(scheduler) = self.scheduler.upgrade() {
            scheduler.refill_job_by_id(job_id);
        }
    }

    fn reprioritize_queued(
        &self,
        job_id: DiscoveryJobId,
        preferred_keys: &[ManifestCandidateKey],
    ) -> ReprioritizeOutcome {
        self.scheduler.upgrade().map_or(
            ReprioritizeOutcome {
                reprioritized: 0,
                stale: preferred_keys.len(),
            },
            |scheduler| scheduler.reprioritize_queued(job_id, preferred_keys),
        )
    }

    fn remove_queued(&self, job_id: DiscoveryJobId) -> usize {
        self.scheduler
            .upgrade()
            .map_or(0, |scheduler| scheduler.remove_queued(job_id))
    }
}

/// Process-lifetime reusable executor; Drop не блокирует UI на join.
pub struct DiscoveryExecutor {
    scheduler: Arc<Scheduler>,
    wake_coordinator: Arc<WakeCoordinator>,
    worker_count: usize,
}

impl DiscoveryExecutor {
    /// Создаёт production executor с `available_parallelism.clamp(2, 4)`.
    pub fn start(wake_port: Arc<dyn DiscoveryWakePort>) -> Result<Self, DiscoverySubmitError> {
        Self::start_with_probe(Arc::new(LocalMediaProbe), wake_port)
    }

    /// Создаёт executor с injectable fake probe, сохраняя production budgets.
    pub fn start_with_probe(
        probe: Arc<dyn DiscoveryProbe>,
        wake_port: Arc<dyn DiscoveryWakePort>,
    ) -> Result<Self, DiscoverySubmitError> {
        let reported_parallelism = thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(MIN_DISCOVERY_WORKER_THREADS);
        let worker_count =
            reported_parallelism.clamp(MIN_DISCOVERY_WORKER_THREADS, MAX_DISCOVERY_WORKER_THREADS);
        Self::start_with_worker_count(probe, wake_port, worker_count)
    }

    #[cfg(test)]
    pub(crate) fn start_with_test_worker_count(
        probe: Arc<dyn DiscoveryProbe>,
        wake_port: Arc<dyn DiscoveryWakePort>,
        worker_count: usize,
    ) -> Result<Self, DiscoverySubmitError> {
        assert!(
            (MIN_DISCOVERY_WORKER_THREADS..=MAX_DISCOVERY_WORKER_THREADS).contains(&worker_count)
        );
        Self::start_with_worker_count(probe, wake_port, worker_count)
    }

    fn start_with_worker_count(
        probe: Arc<dyn DiscoveryProbe>,
        wake_port: Arc<dyn DiscoveryWakePort>,
        worker_count: usize,
    ) -> Result<Self, DiscoverySubmitError> {
        let scheduler = Arc::new(Scheduler::new());
        let wake_coordinator = WakeCoordinator::new(wake_port);

        for worker_index in 0..worker_count {
            let foreground_only = worker_index < FOREGROUND_ONLY_WORKER_COUNT;
            let worker_scheduler = Arc::clone(&scheduler);
            let worker_probe = Arc::clone(&probe);
            thread::Builder::new()
                .name(format!("playlist-discovery-{worker_index}"))
                .spawn(move || {
                    worker_loop(worker_scheduler, worker_probe, foreground_only);
                })
                .map_err(|error| {
                    let mut state = scheduler
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.accepting = false;
                    drop(state);
                    scheduler.work_available.notify_all();
                    DiscoverySubmitError::WorkerSpawn(error.kind())
                })?;
        }

        Ok(Self {
            scheduler,
            wake_coordinator,
            worker_count,
        })
    }

    /// Возвращает фактический clamp-нутый worker budget для diagnostics/tests.
    #[must_use]
    pub const fn worker_count(&self) -> usize {
        self.worker_count
    }

    /// Принимает immutable request и сразу создаёт lossless terminal slot.
    pub fn submit(
        &self,
        request: DiscoveryRequest,
    ) -> Result<DiscoveryJobHandle, DiscoverySubmitError> {
        let request_item_count = request.item_count();
        if request_item_count > DISCOVERY_REQUEST_ITEM_LIMIT {
            return Err(DiscoverySubmitError::RequestItemLimitReached {
                limit: DISCOVERY_REQUEST_ITEM_LIMIT,
                observed: request_item_count,
            });
        }
        let mut state = self
            .scheduler
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.accepting {
            return Err(DiscoverySubmitError::ShuttingDown);
        }
        state.prune_finished_jobs();
        if state.jobs.len() == ACTIVE_DISCOVERY_JOB_LIMIT {
            return Err(DiscoverySubmitError::ActiveJobLimitReached);
        }
        if request.priority() == DiscoveryPriority::Speculative
            && state
                .jobs
                .values()
                .filter_map(Weak::upgrade)
                .filter(|job| job.priority() == DiscoveryPriority::Speculative)
                .count()
                == SPECULATIVE_ACTIVE_JOB_LIMIT
        {
            return Err(DiscoverySubmitError::ActiveJobLimitReached);
        }
        let Some(job_id) = DiscoveryJobId::from_counter(state.next_job_id) else {
            return Err(DiscoverySubmitError::JobIdExhausted);
        };
        state.next_job_id = state.next_job_id.saturating_add(1);
        let general_worker_count = self.worker_count - FOREGROUND_ONLY_WORKER_COUNT;
        let outstanding_work_limit = request.outstanding_work_limit();
        // Даже один большой bulk job оставляет хотя бы один general permit
        // другому job-у; на 2-thread host единственный general permit неделим.
        let execution_permit_limit =
            request.speculative_execution_permit_limit(general_worker_count);
        let job = JobInner::new(
            job_id,
            request,
            self.wake_coordinator.clone(),
            outstanding_work_limit,
            execution_permit_limit,
        );
        job.set_scheduler_port(Arc::new(SchedulerPort {
            scheduler: Arc::downgrade(&self.scheduler),
        }));
        state.jobs.insert(job_id, Arc::downgrade(&job));
        drop(state);

        self.scheduler.refill_job(&job);
        if !job.has_admitted_work_or_is_empty() {
            let mut state = self
                .scheduler
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.jobs.remove(&job_id);
            drop(state);
            let _ = job.cancel(DiscoveryCancellationCause::LifecycleShutdown);
            return Err(DiscoverySubmitError::InputBackpressure);
        }
        Ok(DiscoveryJobHandle { inner: job })
    }

    /// Запрещает новые jobs, typed-cancel-ит pending work и не ждёт blocking syscalls.
    pub fn shutdown(&self) -> DiscoveryShutdownReport {
        let jobs = {
            let mut state = self
                .scheduler
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !state.accepting {
                return DiscoveryShutdownReport {
                    cancelled_jobs: 0,
                    in_flight_work_units: state.in_flight,
                };
            }
            state.accepting = false;
            state
                .jobs
                .values()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        };
        let cancelled_jobs = jobs
            .iter()
            .filter(|job| job.cancel(DiscoveryCancellationCause::LifecycleShutdown))
            .count();
        let in_flight_work_units = self
            .scheduler
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .in_flight;
        self.scheduler.work_available.notify_all();
        DiscoveryShutdownReport {
            cancelled_jobs,
            in_flight_work_units,
        }
    }
}

impl Drop for DiscoveryExecutor {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn worker_loop(scheduler: Arc<Scheduler>, probe: Arc<dyn DiscoveryProbe>, foreground_only: bool) {
    while let Some(scheduled) = scheduler.take_work(foreground_only) {
        let work_priority = scheduled.work.priority;
        // Refill после dequeue сохраняет round-robin tail и не даёт одному job
        // заполнить permits длинным contiguous prefix-ом.
        scheduler.refill_job(&scheduled.job);
        if !scheduled.job.begin_probe_if_active() {
            scheduled.job.abandon_in_flight_work();
        } else if let Err(diagnostic) = scheduled.job.validate_source(&scheduled.work) {
            scheduled
                .job
                .record_source_failure(scheduled.work, diagnostic);
        } else if let Some(expected_fingerprint) = scheduled.work.expected_fingerprint {
            let fingerprint_result = catch_unwind(AssertUnwindSafe(|| {
                probe.read_fingerprint(
                    &scheduled.work.locator,
                    scheduled.job.cancellation().probe_token(),
                )
            }));
            match fingerprint_result {
                Ok(Ok(actual_fingerprint)) if actual_fingerprint == expected_fingerprint => {
                    scheduled.job.complete_fingerprint_unchanged(scheduled.work);
                }
                Ok(Ok(_)) => run_probe(&scheduled.job, scheduled.work, probe.as_ref()),
                Ok(Err(error)) => scheduled.job.complete_work(scheduled.work, Err(error)),
                Err(_) => scheduled.job.fail_executor_disconnected(),
            }
        } else {
            run_probe(&scheduled.job, scheduled.work, probe.as_ref());
        }
        scheduler.finish_work(scheduled.job.id(), work_priority);
        scheduler.refill_job(&scheduled.job);
    }
}

fn run_probe(job: &Arc<JobInner>, work: WorkUnit, probe: &dyn DiscoveryProbe) {
    let probe_result = catch_unwind(AssertUnwindSafe(|| {
        probe.probe(&work.locator, job.cancellation().probe_token())
    }));
    match probe_result {
        Ok(result) => job.complete_work(work, result),
        Err(_) => job.fail_executor_disconnected(),
    }
}

fn schedulable_position(
    queue: &VecDeque<ScheduledWork>,
    running_by_job_lane: &HashMap<(DiscoveryJobId, DiscoveryPriority), usize>,
    last_dispatched_job: Option<DiscoveryJobId>,
) -> Option<usize> {
    let is_schedulable = |scheduled: &ScheduledWork| {
        let priority = scheduled.work.priority;
        running_by_job_lane
            .get(&(scheduled.job.id(), priority))
            .copied()
            .unwrap_or(0)
            < scheduled.job.execution_permit_limit(priority)
    };
    queue
        .iter()
        .position(|scheduled| {
            is_schedulable(scheduled) && Some(scheduled.job.id()) != last_dispatched_job
        })
        .or_else(|| queue.iter().position(is_schedulable))
}
