use std::io;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::atomic_write::{
    AtomicSnapshotWriter, AtomicWriteOutcome, DurabilityRetryOutcome, SnapshotWriter,
};
use crate::{ImmutableSaveSnapshot, PlaylistStateStore, SaveRevision};

mod mailbox;
mod types;

use mailbox::{WorkerExitReporter, WorkerMailbox};
pub use types::*;

/// Bounded normal command queue; shutdown admission дополнительно bounded deadline-ом.
const SAVE_COMMAND_CAPACITY: usize = 8;
/// Первая retry задержка после filesystem failure.
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
/// Верхняя граница exponential retry не допускает unbounded silence/overflow.
const MAXIMUM_RETRY_DELAY: Duration = Duration::from_secs(60);
/// Малый bounded admission wait не используется как completion polling.
const SHUTDOWN_ADMISSION_YIELD: Duration = Duration::from_millis(1);

/// Process-lifetime handle одного latest-only writer.
pub struct SaveWorker {
    command_sender: Option<SyncSender<WorkerCommand>>,
    mailbox: Arc<WorkerMailbox>,
    highest_submitted_revision: Mutex<Option<SaveRevision>>,
    join_handle: Option<JoinHandle<()>>,
}

impl SaveWorker {
    /// Запускает production writer только после explicit writable decision.
    pub fn start(
        access: SaveWorkerAccess,
        store: Arc<PlaylistStateStore>,
        debounce: SaveDebounce,
        wake_port: Arc<dyn SaveWakePort>,
    ) -> Result<SaveWorkerStartOutcome, SaveWorkerStartError> {
        if let SaveWorkerAccess::SaveBlocked(reason) = access {
            return Ok(SaveWorkerStartOutcome::SaveBlocked(reason));
        }

        let writer: Arc<dyn SnapshotWriter> = Arc::new(AtomicSnapshotWriter::new(store));
        Self::start_with_dependencies(debounce, wake_port, writer)
            .map(SaveWorkerStartOutcome::Started)
    }

    /// Принимает только строго новую revision и возвращает snapshot при отказе.
    pub fn submit_snapshot(
        &self,
        snapshot: ImmutableSaveSnapshot,
    ) -> Result<SubmitSnapshotOutcome, SubmitSnapshotError> {
        let mut highest_revision = match self.highest_submitted_revision.lock() {
            Ok(highest_revision) => highest_revision,
            Err(_) => {
                return Err(SubmitSnapshotError::SubmissionStatePoisoned(Box::new(
                    snapshot,
                )));
            }
        };
        if highest_revision.is_some_and(|revision| revision >= snapshot.revision()) {
            return Ok(SubmitSnapshotOutcome::NoOpSameOrOlderRevision);
        }
        let submitted_revision = snapshot.revision();
        let Some(command_sender) = &self.command_sender else {
            return Err(SubmitSnapshotError::Disconnected(Box::new(snapshot)));
        };
        match command_sender.try_send(WorkerCommand::Commit(snapshot)) {
            Ok(()) => {
                *highest_revision = Some(submitted_revision);
                Ok(SubmitSnapshotOutcome::Accepted)
            }
            Err(TrySendError::Full(WorkerCommand::Commit(snapshot))) => {
                Err(SubmitSnapshotError::Backpressure(Box::new(snapshot)))
            }
            Err(TrySendError::Disconnected(WorkerCommand::Commit(snapshot))) => {
                Err(SubmitSnapshotError::Disconnected(Box::new(snapshot)))
            }
            // Boundary не паникует даже при нарушении private envelope invariant.
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                Err(SubmitSnapshotError::CommandTypeInvariantLost)
            }
        }
    }

    /// Live-reschedule не читает config и не меняет dirty snapshot.
    pub fn reschedule_debounce(&self, debounce: SaveDebounce) -> Result<(), SaveControlError> {
        self.try_send_control(WorkerCommand::RescheduleDebounce(debounce))
    }

    /// Manual Retry обходит debounce/backoff, сохраняя single-write invariant.
    pub fn retry_now(&self) -> Result<(), SaveControlError> {
        self.try_send_control(WorkerCommand::RetryNow)
    }

    /// Неблокирующе забирает terminal reports exactly once и latest warning update.
    pub fn drain_events(&self) -> Vec<SaveWorkerEvent> {
        let events = self.mailbox.drain();
        // Только реально освобождённый slot будит worker; empty UI poll не
        // занимает bounded command capacity служебными сообщениями.
        if !events.is_empty() {
            let _resume_result = self.try_send_control(WorkerCommand::MailboxDrained);
        }
        events
    }

    /// Выполняет D68 flush newest committed snapshot с единым bounded deadline.
    pub fn shutdown(
        mut self,
        newest_committed: Option<ImmutableSaveSnapshot>,
        timeout: Duration,
    ) -> SaveWorkerShutdownOutcome {
        let deadline = Instant::now().checked_add(timeout);
        let Some(deadline) = deadline else {
            return SaveWorkerShutdownOutcome::TimedOut {
                phase: ShutdownTimeoutPhase::CommandAdmission,
                completion: None,
            };
        };
        let (acknowledgement_sender, acknowledgement_receiver) = mpsc::sync_channel(1);
        let shutdown_command = WorkerCommand::Shutdown {
            newest_committed,
            acknowledgement_sender,
        };
        let Some(command_sender) = self.command_sender.take() else {
            return SaveWorkerShutdownOutcome::TimedOut {
                phase: ShutdownTimeoutPhase::CommandAdmission,
                completion: None,
            };
        };
        if send_shutdown_until_deadline(&command_sender, shutdown_command, deadline).is_err() {
            return SaveWorkerShutdownOutcome::TimedOut {
                phase: ShutdownTimeoutPhase::CommandAdmission,
                completion: None,
            };
        }
        drop(command_sender);

        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return SaveWorkerShutdownOutcome::TimedOut {
                phase: ShutdownTimeoutPhase::CompletionAcknowledgement,
                completion: None,
            };
        };
        let completion = match acknowledgement_receiver.recv_timeout(remaining) {
            Ok(completion) => completion,
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                return SaveWorkerShutdownOutcome::TimedOut {
                    phase: ShutdownTimeoutPhase::CompletionAcknowledgement,
                    completion: None,
                };
            }
        };

        let Some(join_handle) = self.join_handle.take() else {
            return SaveWorkerShutdownOutcome::TimedOut {
                phase: ShutdownTimeoutPhase::ThreadExit,
                completion: Some(completion),
            };
        };
        wait_for_finished_thread(join_handle, completion, deadline)
    }

    fn try_send_control(&self, command: WorkerCommand) -> Result<(), SaveControlError> {
        let Some(command_sender) = &self.command_sender else {
            return Err(SaveControlError::Disconnected);
        };
        command_sender
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => SaveControlError::Backpressure,
                TrySendError::Disconnected(_) => SaveControlError::Disconnected,
            })
    }

    fn start_with_dependencies(
        debounce: SaveDebounce,
        wake_port: Arc<dyn SaveWakePort>,
        writer: Arc<dyn SnapshotWriter>,
    ) -> Result<Self, SaveWorkerStartError> {
        Self::start_with_spawner(debounce, wake_port, writer, &SystemThreadSpawner)
    }

    fn start_with_spawner(
        debounce: SaveDebounce,
        wake_port: Arc<dyn SaveWakePort>,
        writer: Arc<dyn SnapshotWriter>,
        thread_spawner: &dyn WorkerThreadSpawner,
    ) -> Result<Self, SaveWorkerStartError> {
        let (command_sender, command_receiver) = mpsc::sync_channel(SAVE_COMMAND_CAPACITY);
        let mailbox = Arc::new(WorkerMailbox::new(wake_port));
        let worker_mailbox = Arc::clone(&mailbox);
        let worker_job = Box::new(move || {
            run_worker(command_receiver, worker_mailbox, writer, debounce);
        });
        let join_handle = thread_spawner
            .spawn("playlist-state-save", worker_job)
            .map_err(|error| SaveWorkerStartError::ThreadSpawn(error.kind()))?;

        Ok(Self {
            command_sender: Some(command_sender),
            mailbox,
            highest_submitted_revision: Mutex::new(None),
            join_handle: Some(join_handle),
        })
    }
}

/// Injectable OS boundary позволяет доказать typed spawn failure без panic.
trait WorkerThreadSpawner {
    fn spawn(
        &self,
        thread_name: &str,
        worker_job: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<JoinHandle<()>>;
}

struct SystemThreadSpawner;

impl WorkerThreadSpawner for SystemThreadSpawner {
    fn spawn(
        &self,
        thread_name: &str,
        worker_job: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<JoinHandle<()>> {
        thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(worker_job)
    }
}

/// Internal commands остаются bounded и не содержат mutable domain references.
enum WorkerCommand {
    Commit(ImmutableSaveSnapshot),
    RescheduleDebounce(SaveDebounce),
    RetryNow,
    MailboxDrained,
    Shutdown {
        newest_committed: Option<ImmutableSaveSnapshot>,
        acknowledgement_sender: SyncSender<ShutdownCompletion>,
    },
}

/// Monotonic scheduler clock с production `Instant` origin.
struct WorkerClock {
    origin: Instant,
}

impl WorkerClock {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }

    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// Scheduled filesystem operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingAttemptKind {
    FullWrite,
    DirectoryDurabilityRetry,
}

/// Due action сохраняет exact kind и monotonic deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingAttempt {
    kind: PendingAttemptKind,
    deadline: Duration,
}

/// Pure latest-only state machine; blocking I/O выполняется снаружи методов transition.
struct WorkerState {
    debounce: SaveDebounce,
    latest_dirty: Option<ImmutableSaveSnapshot>,
    latest_durable_revision: Option<SaveRevision>,
    durability_pending_revision: Option<SaveRevision>,
    pending_attempt: Option<PendingAttempt>,
    next_retry_delay: Duration,
    warning: Option<SaveWarning>,
}

impl WorkerState {
    fn new(debounce: SaveDebounce) -> Self {
        Self {
            debounce,
            latest_dirty: None,
            latest_durable_revision: None,
            durability_pending_revision: None,
            pending_attempt: None,
            next_retry_delay: INITIAL_RETRY_DELAY,
            warning: None,
        }
    }

    fn accept_snapshot(&mut self, snapshot: ImmutableSaveSnapshot, now: Duration) {
        if self
            .latest_dirty
            .as_ref()
            .is_some_and(|current| current.revision() >= snapshot.revision())
            || self
                .latest_durable_revision
                .is_some_and(|revision| revision >= snapshot.revision())
        {
            return;
        }
        self.latest_dirty = Some(snapshot);
        self.durability_pending_revision = None;
        self.next_retry_delay = INITIAL_RETRY_DELAY;
        self.pending_attempt = Some(PendingAttempt {
            kind: PendingAttemptKind::FullWrite,
            deadline: saturating_deadline(now, self.debounce.duration()),
        });
    }

    fn reschedule_debounce(&mut self, debounce: SaveDebounce, now: Duration) {
        self.debounce = debounce;
        if self.latest_dirty.is_some()
            && self
                .pending_attempt
                .is_some_and(|attempt| attempt.kind == PendingAttemptKind::FullWrite)
        {
            self.pending_attempt = Some(PendingAttempt {
                kind: PendingAttemptKind::FullWrite,
                deadline: saturating_deadline(now, debounce.duration()),
            });
        }
    }

    fn retry_now(&mut self, now: Duration) {
        if self.latest_dirty.is_none() {
            return;
        }
        let kind = if self.durability_pending_revision.is_some() {
            PendingAttemptKind::DirectoryDurabilityRetry
        } else {
            PendingAttemptKind::FullWrite
        };
        self.pending_attempt = Some(PendingAttempt {
            kind,
            deadline: now,
        });
    }

    fn due_attempt(&self, now: Duration) -> Option<PendingAttemptKind> {
        self.pending_attempt
            .filter(|attempt| attempt.deadline <= now)
            .map(|attempt| attempt.kind)
    }

    fn time_until_attempt(&self, now: Duration) -> Option<Duration> {
        self.pending_attempt
            .map(|attempt| attempt.deadline.saturating_sub(now))
    }

    fn current_revision(&self) -> Option<SaveRevision> {
        self.latest_dirty
            .as_ref()
            .map(ImmutableSaveSnapshot::revision)
    }

    fn apply_full_write_outcome(
        &mut self,
        revision: SaveRevision,
        outcome: AtomicWriteOutcome,
        now: Duration,
    ) {
        match outcome {
            AtomicWriteOutcome::Durable => self.mark_durable(revision),
            AtomicWriteOutcome::NotReplaced(failure) => {
                self.durability_pending_revision = None;
                self.record_warning(revision, SaveWarningFailure::NotReplaced(failure));
                self.schedule_retry(PendingAttemptKind::FullWrite, now);
            }
            AtomicWriteOutcome::ReplacedDurabilityUnconfirmed(cause) => {
                self.durability_pending_revision = Some(revision);
                self.record_warning(revision, SaveWarningFailure::DurabilityUnconfirmed(cause));
                self.schedule_retry(PendingAttemptKind::DirectoryDurabilityRetry, now);
            }
        }
    }

    fn apply_durability_retry_outcome(
        &mut self,
        revision: SaveRevision,
        outcome: DurabilityRetryOutcome,
        now: Duration,
    ) {
        match outcome {
            DurabilityRetryOutcome::Durable => self.mark_durable(revision),
            DurabilityRetryOutcome::StillUnconfirmed(cause) => {
                self.record_warning(revision, SaveWarningFailure::DurabilityUnconfirmed(cause));
                self.schedule_retry(PendingAttemptKind::DirectoryDurabilityRetry, now);
            }
        }
    }

    fn mark_durable(&mut self, revision: SaveRevision) {
        self.latest_durable_revision = Some(revision);
        self.durability_pending_revision = None;
        self.pending_attempt = None;
        self.next_retry_delay = INITIAL_RETRY_DELAY;
        if self
            .latest_dirty
            .as_ref()
            .is_some_and(|snapshot| snapshot.revision() <= revision)
        {
            self.latest_dirty = None;
            self.warning = None;
        }
    }

    fn schedule_retry(&mut self, kind: PendingAttemptKind, now: Duration) {
        let retry_delay = self.next_retry_delay;
        self.pending_attempt = Some(PendingAttempt {
            kind,
            deadline: saturating_deadline(now, retry_delay),
        });
        self.next_retry_delay = retry_delay
            .checked_mul(2)
            .unwrap_or(MAXIMUM_RETRY_DELAY)
            .min(MAXIMUM_RETRY_DELAY);
    }

    fn record_warning(&mut self, revision: SaveRevision, failure: SaveWarningFailure) {
        let occurrence_count = self
            .warning
            .filter(|warning| warning.revision == revision && warning.failure == failure)
            .map(|warning| warning.occurrence_count.saturating_add(1))
            .unwrap_or(1);
        self.warning = Some(SaveWarning {
            revision,
            failure,
            occurrence_count,
        });
    }
}

/// Main loop сначала coalesce-ит все доступные commands, затем начинает I/O.
fn run_worker(
    command_receiver: Receiver<WorkerCommand>,
    mailbox: Arc<WorkerMailbox>,
    writer: Arc<dyn SnapshotWriter>,
    debounce: SaveDebounce,
) {
    let mut exit_reporter = WorkerExitReporter::new(Arc::clone(&mailbox));
    let clock = WorkerClock::new();
    let mut state = WorkerState::new(debounce);

    loop {
        let now = clock.now();
        match receive_next_command(&command_receiver, state.time_until_attempt(now)) {
            ReceiveOutcome::Command(command) => {
                if process_command(
                    command,
                    &command_receiver,
                    &mut state,
                    &mailbox,
                    writer.as_ref(),
                    &clock,
                ) {
                    exit_reporter.mark_clean();
                    return;
                }
                continue;
            }
            ReceiveOutcome::DeadlineReached => {}
            ReceiveOutcome::Disconnected => {
                mailbox.publish_disconnect(WorkerDisconnectReason::CommandChannelClosed);
                exit_reporter.mark_clean();
                return;
            }
        }

        if drain_ready_commands(
            &command_receiver,
            &mut state,
            &mailbox,
            writer.as_ref(),
            &clock,
        ) {
            exit_reporter.mark_clean();
            return;
        }
        let now = clock.now();
        let Some(attempt_kind) = state.due_attempt(now) else {
            continue;
        };
        if !mailbox.has_normal_event_capacity() {
            match command_receiver.recv() {
                Ok(command) => {
                    if process_command(
                        command,
                        &command_receiver,
                        &mut state,
                        &mailbox,
                        writer.as_ref(),
                        &clock,
                    ) {
                        exit_reporter.mark_clean();
                        return;
                    }
                }
                Err(_) => {
                    mailbox.publish_disconnect(WorkerDisconnectReason::CommandChannelClosed);
                    exit_reporter.mark_clean();
                    return;
                }
            }
            continue;
        }
        execute_attempt(attempt_kind, &mut state, &mailbox, writer.as_ref(), &clock);
    }
}

/// One command may reveal a queued shutdown, поэтому очередь drain-ится до I/O.
fn process_command(
    first_command: WorkerCommand,
    command_receiver: &Receiver<WorkerCommand>,
    state: &mut WorkerState,
    mailbox: &WorkerMailbox,
    writer: &dyn SnapshotWriter,
    clock: &WorkerClock,
) -> bool {
    let mut next_command = Some(first_command);
    while let Some(command) = next_command {
        match command {
            WorkerCommand::Commit(snapshot) => state.accept_snapshot(snapshot, clock.now()),
            WorkerCommand::RescheduleDebounce(debounce) => {
                state.reschedule_debounce(debounce, clock.now());
            }
            WorkerCommand::RetryNow => state.retry_now(clock.now()),
            WorkerCommand::MailboxDrained => {}
            WorkerCommand::Shutdown {
                newest_committed,
                acknowledgement_sender,
            } => {
                perform_shutdown(
                    newest_committed,
                    acknowledgement_sender,
                    state,
                    mailbox,
                    writer,
                    clock,
                );
                return true;
            }
        }
        next_command = command_receiver.try_recv().ok();
    }
    false
}

/// Deadline wake обрабатывает commands, пришедшие одновременно с timer edge.
fn drain_ready_commands(
    command_receiver: &Receiver<WorkerCommand>,
    state: &mut WorkerState,
    mailbox: &WorkerMailbox,
    writer: &dyn SnapshotWriter,
    clock: &WorkerClock,
) -> bool {
    if let Ok(command) = command_receiver.try_recv() {
        return process_command(command, command_receiver, state, mailbox, writer, clock);
    }
    false
}

/// Выполняет ровно одну operation и публикует ровно один terminal report.
fn execute_attempt(
    attempt_kind: PendingAttemptKind,
    state: &mut WorkerState,
    mailbox: &WorkerMailbox,
    writer: &dyn SnapshotWriter,
    clock: &WorkerClock,
) -> Option<SaveAttemptReport> {
    let revision = state.current_revision()?;
    let outcome = match attempt_kind {
        PendingAttemptKind::FullWrite => {
            let snapshot = state.latest_dirty.as_ref()?;
            let write_outcome = writer.write_snapshot(snapshot);
            state.apply_full_write_outcome(revision, write_outcome, clock.now());
            SaveAttemptOutcome::FullWrite(write_outcome)
        }
        PendingAttemptKind::DirectoryDurabilityRetry => {
            let retry_outcome = writer.retry_directory_durability();
            state.apply_durability_retry_outcome(revision, retry_outcome, clock.now());
            SaveAttemptOutcome::DirectoryDurabilityRetry(retry_outcome)
        }
    };
    let report = SaveAttemptReport { revision, outcome };
    mailbox.publish_attempt(report);
    mailbox.publish_warning(state.warning);
    Some(report)
}

/// Shutdown newest snapshot supersedes queued state и bypass-ит timers.
fn perform_shutdown(
    newest_committed: Option<ImmutableSaveSnapshot>,
    acknowledgement_sender: SyncSender<ShutdownCompletion>,
    state: &mut WorkerState,
    mailbox: &WorkerMailbox,
    writer: &dyn SnapshotWriter,
    clock: &WorkerClock,
) {
    if let Some(snapshot) = newest_committed {
        state.accept_snapshot(snapshot, clock.now());
    }
    state.retry_now(clock.now());

    let persistence = match state.current_revision() {
        Some(revision) => {
            let attempt_kind = state
                .due_attempt(clock.now())
                .unwrap_or(PendingAttemptKind::FullWrite);
            match execute_attempt(attempt_kind, state, mailbox, writer, clock) {
                Some(report) => ShutdownPersistenceOutcome::Attempted(report),
                None => ShutdownPersistenceOutcome::AlreadyDurable { revision },
            }
        }
        None => match state.latest_durable_revision {
            Some(revision) => ShutdownPersistenceOutcome::AlreadyDurable { revision },
            None => ShutdownPersistenceOutcome::NoCommittedSnapshot,
        },
    };
    let completion = ShutdownCompletion { persistence };
    if acknowledgement_sender.try_send(completion).is_err() {
        // Caller уже исчерпал deadline и перешёл к terminal process policy.
        // Worker закончил filesystem access и не retry-ит потерянный receiver.
    }
}

/// recv_timeout используется только для scheduled debounce/retry, не UI completion.
fn receive_next_command(
    command_receiver: &Receiver<WorkerCommand>,
    time_until_attempt: Option<Duration>,
) -> ReceiveOutcome {
    match time_until_attempt {
        Some(wait_duration) => match command_receiver.recv_timeout(wait_duration) {
            Ok(command) => ReceiveOutcome::Command(command),
            Err(RecvTimeoutError::Timeout) => ReceiveOutcome::DeadlineReached,
            Err(RecvTimeoutError::Disconnected) => ReceiveOutcome::Disconnected,
        },
        None => match command_receiver.recv() {
            Ok(command) => ReceiveOutcome::Command(command),
            Err(_) => ReceiveOutcome::Disconnected,
        },
    }
}

enum ReceiveOutcome {
    Command(WorkerCommand),
    DeadlineReached,
    Disconnected,
}

/// Deadline addition saturates instead of wrapping scheduler into busy loop.
fn saturating_deadline(now: Duration, delay: Duration) -> Duration {
    now.checked_add(delay).unwrap_or(Duration::MAX)
}

/// Shutdown admission восстанавливает ownership command после `Full`.
fn send_shutdown_until_deadline(
    command_sender: &SyncSender<WorkerCommand>,
    mut shutdown_command: WorkerCommand,
    deadline: Instant,
) -> Result<(), ()> {
    loop {
        match command_sender.try_send(shutdown_command) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned_command)) => {
                shutdown_command = returned_command;
                if Instant::now() >= deadline {
                    return Err(());
                }
                thread::sleep(SHUTDOWN_ADMISSION_YIELD);
            }
            Err(TrySendError::Disconnected(_)) => return Err(()),
        }
    }
}

/// Join вызывается только после `is_finished`, поэтому сам не служит timeout API.
fn wait_for_finished_thread(
    join_handle: JoinHandle<()>,
    completion: ShutdownCompletion,
    deadline: Instant,
) -> SaveWorkerShutdownOutcome {
    while !join_handle.is_finished() {
        if Instant::now() >= deadline {
            return SaveWorkerShutdownOutcome::TimedOut {
                phase: ShutdownTimeoutPhase::ThreadExit,
                completion: Some(completion),
            };
        }
        thread::yield_now();
    }
    match join_handle.join() {
        Ok(()) => SaveWorkerShutdownOutcome::Complete(completion),
        Err(_) => SaveWorkerShutdownOutcome::ThreadPanicked(completion),
    }
}

#[cfg(test)]
mod tests;
