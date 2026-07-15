//! Startup load gate и read-only inspection owner playlist runtime-а.
//!
//! Модуль намеренно не знает renderer/player API. До принятого load decision он
//! хранит только bounded ID-less draft, а filesystem mutation запускает лишь как
//! отдельный quarantine job после supported-corrupt inspection.

use std::io;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use playlist_core::{MAX_PLAYLIST_ITEMS, PlaylistItemDraft, RepeatMode};
use playlist_state::{
    InspectedFileIdentity, InspectionOutcome, PlaylistStateStore, ProtectedStateCause,
    QuarantineFileName, QuarantineOutcome, SaveBlockReason, SaveWorkerAccess,
};

use crate::app_wake::{AppWakePort, OwnerMailboxReceiver, owner_mailbox};
use crate::process_shutdown::{FinishedThreadJoin, ShutdownDeadline, join_thread_until};

#[cfg(test)]
mod tests;

/// Hard cap не позволяет pre-gate draft превратиться в unbounded command queue.
const MAX_STARTUP_DRAFT_ITEMS: usize = MAX_PLAYLIST_ITEMS;

/// Generation read-only inspection/quarantine policy decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StartupDecisionGeneration(u64);

impl StartupDecisionGeneration {
    const INITIAL: Self = Self(1);
}

/// Correlation identity отдельной runtime queue, не merged с protected state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistQueueGeneration(u64);

impl PlaylistQueueGeneration {
    pub(super) const INITIAL: Self = Self(1);
}

/// Generation применения restored items/traversal, отдельная от allocator decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RestoreApplyGeneration(u64);

impl RestoreApplyGeneration {
    const INITIAL: Self = Self(1);

    fn supersede(&mut self) -> Result<(), StartupDraftError> {
        self.0 = self
            .0
            .checked_add(1)
            .ok_or(StartupDraftError::RestoreGenerationExhausted)?;
        Ok(())
    }
}

/// Последний structural intent до allocator gate; FIFO команд не создаётся.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "Clear and media-replacement variants are wired by later UI/startup sessions"
)]
pub(crate) enum StartupQueuePlan {
    /// Если state valid, можно применить его items/current/shuffle traversal.
    RestoreCandidate,
    /// Явный Clear оставляет победившую queue пустой.
    Empty,
    /// Подготовленные ID-less Add drafts становятся одной domain mutation после gate.
    PreparedItems(Vec<PlaylistItemDraft>),
    /// Open/Play/replacement уже supersede-нули restore, но media commit ещё впереди.
    AwaitingMediaReplacement,
}

impl StartupQueuePlan {
    pub(crate) const fn applies_restored_items(&self) -> bool {
        matches!(self, Self::RestoreCandidate)
    }
}

/// Coalesced mode overlay не меняет restore apply generation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StartupDesiredModes {
    repeat_mode: Option<RepeatMode>,
    shuffle_enabled: Option<bool>,
}

impl StartupDesiredModes {
    pub(crate) const fn repeat_mode(self) -> Option<RepeatMode> {
        self.repeat_mode
    }

    pub(crate) const fn shuffle_enabled(self) -> Option<bool> {
        self.shuffle_enabled
    }
}

/// Единственный bounded ID-less state, допустимый до trusted allocator decision.
#[derive(Debug)]
pub(crate) struct StartupMutationDraft {
    restore_generation: RestoreApplyGeneration,
    queue_plan: StartupQueuePlan,
    desired_modes: StartupDesiredModes,
}

impl Default for StartupMutationDraft {
    fn default() -> Self {
        Self {
            restore_generation: RestoreApplyGeneration::INITIAL,
            queue_plan: StartupQueuePlan::RestoreCandidate,
            desired_modes: StartupDesiredModes::default(),
        }
    }
}

impl StartupMutationDraft {
    pub(crate) const fn restore_generation(&self) -> RestoreApplyGeneration {
        self.restore_generation
    }

    pub(crate) fn into_parts(self) -> (StartupQueuePlan, StartupDesiredModes) {
        (self.queue_plan, self.desired_modes)
    }

    /// Clear заменяет любой более ранний prepared Add без накопления command history.
    #[allow(dead_code, reason = "Session 14A wires explicit Clear before the gate")]
    pub(crate) fn record_clear(&mut self) -> Result<(), StartupDraftError> {
        self.supersede_restore_items()?;
        self.queue_plan = StartupQueuePlan::Empty;
        Ok(())
    }

    /// Open/Play/replacement освобождают старые ID-less drafts и ждут media commit.
    #[allow(dead_code, reason = "Session 17 wires startup media precedence")]
    pub(crate) fn record_media_replacement(&mut self) -> Result<(), StartupDraftError> {
        self.supersede_restore_items()?;
        self.queue_plan = StartupQueuePlan::AwaitingMediaReplacement;
        Ok(())
    }

    /// Добавляет bounded prepared batch к единственному aggregate plan-у.
    pub(crate) fn record_prepared_add(
        &mut self,
        mut drafts: Vec<PlaylistItemDraft>,
    ) -> Result<(), StartupDraftError> {
        if drafts.is_empty() {
            return Ok(());
        }
        let retained_count = match &self.queue_plan {
            StartupQueuePlan::PreparedItems(existing) => existing.len(),
            StartupQueuePlan::RestoreCandidate
            | StartupQueuePlan::Empty
            | StartupQueuePlan::AwaitingMediaReplacement => 0,
        };
        let combined_count = retained_count
            .checked_add(drafts.len())
            .ok_or(StartupDraftError::PreparedItemsCapacityExceeded)?;
        if combined_count > MAX_STARTUP_DRAFT_ITEMS {
            return Err(StartupDraftError::PreparedItemsCapacityExceeded);
        }

        self.supersede_restore_items()?;
        match &mut self.queue_plan {
            StartupQueuePlan::PreparedItems(existing) => existing.append(&mut drafts),
            StartupQueuePlan::RestoreCandidate
            | StartupQueuePlan::Empty
            | StartupQueuePlan::AwaitingMediaReplacement => {
                self.queue_plan = StartupQueuePlan::PreparedItems(drafts);
            }
        }
        Ok(())
    }

    /// Latest repeat choice coalesce-ится и не supersede-ит restored items.
    pub(crate) fn set_repeat_mode(&mut self, repeat_mode: RepeatMode) {
        self.desired_modes.repeat_mode = Some(repeat_mode);
    }

    /// Latest shuffle choice coalesce-ится и не supersede-ит restored items.
    pub(crate) fn set_shuffle_enabled(&mut self, shuffle_enabled: bool) {
        self.desired_modes.shuffle_enabled = Some(shuffle_enabled);
    }

    fn supersede_restore_items(&mut self) -> Result<(), StartupDraftError> {
        if self.queue_plan.applies_restored_items() {
            self.restore_generation.supersede()?;
        }
        Ok(())
    }
}

/// Pre-gate mutation rejection не маскируется allocator/domain ошибкой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupDraftError {
    PreparedItemsCapacityExceeded,
    RestoreGenerationExhausted,
}

/// Runtime lineage policy после load/quarantine decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistLineagePersistence {
    Persistent,
    NonPersistent {
        queue_generation: PlaylistQueueGeneration,
        save_block: SaveBlockReason,
    },
}

impl PlaylistLineagePersistence {
    #[allow(dead_code, reason = "Session 14.3 passes this decision to SaveWorker")]
    pub(crate) const fn save_worker_access(self) -> SaveWorkerAccess {
        match self {
            Self::Persistent => SaveWorkerAccess::Writable,
            Self::NonPersistent { save_block, .. } => SaveWorkerAccess::SaveBlocked(save_block),
        }
    }
}

/// Read-only UI warning содержит только typed privacy-safe категории.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistStartupWarning {
    CorruptStateQuarantined {
        cause: playlist_state::CorruptStateCause,
    },
    CorruptStateSourceChanged {
        cause: playlist_state::CorruptStateCause,
    },
    CorruptStateQuarantineFailed {
        corrupt_cause: playlist_state::CorruptStateCause,
        failure_cause: playlist_state::QuarantineFailureCause,
    },
    NewerSchema {
        schema_version: u64,
    },
    UnrecognizedVersion {
        cause: ProtectedStateCause,
    },
}

/// Loading view отличает закрытый allocator gate от реально пустой queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistStartupPhase {
    PendingLoadDecision,
    Inspecting,
    ApplyingQuarantine,
    Ready,
    Shutdown,
}

/// Read-only startup/persistence policy model для будущего UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistStartupView {
    pub(crate) phase: PlaylistStartupPhase,
    pub(crate) restore_generation: RestoreApplyGeneration,
    pub(crate) persistence: Option<PlaylistLineagePersistence>,
    pub(crate) warning: Option<PlaylistStartupWarning>,
}

/// Fake-able store port сохраняет общий production `PlaylistStateStore` mutex.
pub(crate) trait PlaylistStartupStateStore: Send + Sync {
    fn inspect_state(&self) -> InspectionOutcome;

    fn apply_quarantine(
        &self,
        inspected_identity: &InspectedFileIdentity,
        quarantine_file_name: &QuarantineFileName,
    ) -> QuarantineOutcome;
}

impl PlaylistStartupStateStore for PlaylistStateStore {
    fn inspect_state(&self) -> InspectionOutcome {
        PlaylistStateStore::inspect_state(self)
    }

    fn apply_quarantine(
        &self,
        inspected_identity: &InspectedFileIdentity,
        quarantine_file_name: &QuarantineFileName,
    ) -> QuarantineOutcome {
        PlaylistStateStore::apply_quarantine(self, inspected_identity, quarantine_file_name)
    }
}

/// Lossless terminal payload одного read/quarantine background job-а.
pub(crate) enum StartupJobCompletion {
    Inspection {
        generation: StartupDecisionGeneration,
        outcome: InspectionOutcome,
    },
    Quarantine {
        generation: StartupDecisionGeneration,
        corrupt_cause: playlist_state::CorruptStateCause,
        outcome: QuarantineOutcome,
    },
}

struct StartupJob {
    receiver: OwnerMailboxReceiver<(), StartupJobCompletion>,
    /// `Option` позволяет join helper-у забрать handle только после завершения потока.
    join_handle: Option<JoinHandle<()>>,
}

/// Terminal результат async inspection/quarantine owner-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistStartupShutdownOutcome {
    /// Активный job завершён и joined либо job никогда не запускался.
    Completed,
    /// Owner уже полностью завершён предыдущим вызовом.
    AlreadyCompleted,
    /// Общий deadline истёк; job и его `JoinHandle` остались внутри owner-а.
    TimedOut,
    /// Завершённый startup thread сообщил Rust panic при join.
    ThreadPanicked,
}

/// Process-lifetime async mechanism; policy apply остаётся у `PlaylistRuntime`.
pub(crate) struct PlaylistStartupOwner {
    wake_port: AppWakePort,
    decision_generation: StartupDecisionGeneration,
    phase: PlaylistStartupPhase,
    draft: StartupMutationDraft,
    persistence: Option<PlaylistLineagePersistence>,
    warning: Option<PlaylistStartupWarning>,
    store: Option<Arc<dyn PlaylistStartupStateStore>>,
    job: Option<StartupJob>,
    shutdown_started: bool,
    shutdown_complete: bool,
}

impl PlaylistStartupOwner {
    pub(crate) fn new(wake_port: AppWakePort) -> Self {
        Self {
            wake_port,
            decision_generation: StartupDecisionGeneration::INITIAL,
            phase: PlaylistStartupPhase::PendingLoadDecision,
            draft: StartupMutationDraft::default(),
            persistence: None,
            warning: None,
            store: None,
            job: None,
            shutdown_started: false,
            shutdown_complete: false,
        }
    }

    pub(crate) fn begin_inspection(
        &mut self,
        store: Arc<dyn PlaylistStartupStateStore>,
    ) -> Result<(), StartupOwnerError> {
        if !matches!(self.phase, PlaylistStartupPhase::PendingLoadDecision) || self.job.is_some() {
            return Err(StartupOwnerError::InvalidPhase);
        }
        let generation = self.decision_generation;
        let worker_store = store.clone();
        let (publisher, receiver) = owner_mailbox(self.wake_port.clone());
        let join_handle = thread::Builder::new()
            .name("playlist-state-inspect".to_owned())
            .spawn(move || {
                let completion = StartupJobCompletion::Inspection {
                    generation,
                    outcome: worker_store.inspect_state(),
                };
                match publisher.publish_completion(completion) {
                    Ok(_) => {}
                    Err(_) => {
                        // Runtime shutdown may intentionally abandon a read-only result.
                    }
                }
            })
            .map_err(|error| StartupOwnerError::ThreadSpawn(error.kind()))?;

        self.store = Some(store);
        self.job = Some(StartupJob {
            receiver,
            join_handle: Some(join_handle),
        });
        self.phase = PlaylistStartupPhase::Inspecting;
        Ok(())
    }

    pub(crate) fn start_quarantine(
        &mut self,
        corrupt_cause: playlist_state::CorruptStateCause,
        inspected_identity: InspectedFileIdentity,
        quarantine_file_name: QuarantineFileName,
    ) -> Result<(), StartupOwnerError> {
        if !matches!(self.phase, PlaylistStartupPhase::Inspecting) || self.job.is_some() {
            return Err(StartupOwnerError::InvalidPhase);
        }
        let store = self.store.clone().ok_or(StartupOwnerError::MissingStore)?;
        let generation = self.decision_generation;
        let (publisher, receiver) = owner_mailbox(self.wake_port.clone());
        let join_handle = thread::Builder::new()
            .name("playlist-state-quarantine".to_owned())
            .spawn(move || {
                let outcome = store.apply_quarantine(&inspected_identity, &quarantine_file_name);
                let completion = StartupJobCompletion::Quarantine {
                    generation,
                    corrupt_cause,
                    outcome,
                };
                match publisher.publish_completion(completion) {
                    Ok(_) => {}
                    Err(_) => {
                        // После terminal shutdown writable policy больше не принимается.
                    }
                }
            })
            .map_err(|error| StartupOwnerError::ThreadSpawn(error.kind()))?;

        self.job = Some(StartupJob {
            receiver,
            join_handle: Some(join_handle),
        });
        self.phase = PlaylistStartupPhase::ApplyingQuarantine;
        Ok(())
    }

    pub(crate) fn drain_completion(&mut self) -> Option<StartupJobCompletion> {
        let job = self.job.as_mut()?;
        let completion = job.receiver.drain().completion;
        if completion.is_some() {
            let mut completed_job = self.job.take().expect("startup job exists while draining");
            if let Some(join_handle) = completed_job.join_handle.take() {
                match join_handle.join() {
                    Ok(()) => {}
                    Err(_) => {
                        // Published completion proves policy payload already arrived safely.
                    }
                }
            }
        }
        completion
    }

    pub(crate) const fn decision_generation(&self) -> StartupDecisionGeneration {
        self.decision_generation
    }

    pub(crate) const fn queue_generation(&self) -> PlaylistQueueGeneration {
        PlaylistQueueGeneration(self.decision_generation.0)
    }

    pub(crate) fn draft_mut(&mut self) -> Result<&mut StartupMutationDraft, StartupOwnerError> {
        if matches!(
            self.phase,
            PlaylistStartupPhase::Ready | PlaylistStartupPhase::Shutdown
        ) {
            return Err(StartupOwnerError::InvalidPhase);
        }
        Ok(&mut self.draft)
    }

    pub(crate) fn take_draft(&mut self) -> StartupMutationDraft {
        std::mem::take(&mut self.draft)
    }

    pub(crate) fn mark_ready(
        &mut self,
        persistence: PlaylistLineagePersistence,
        warning: Option<PlaylistStartupWarning>,
    ) {
        self.phase = PlaylistStartupPhase::Ready;
        self.persistence = Some(persistence);
        self.warning = warning;
        self.store = None;
    }

    /// Закрывает policy admission до ожидания и никогда не detach-ит незавершённый job.
    pub(super) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> PlaylistStartupShutdownOutcome {
        if self.shutdown_complete {
            return PlaylistStartupShutdownOutcome::AlreadyCompleted;
        }
        if !self.shutdown_started {
            self.shutdown_started = true;
            self.phase = PlaylistStartupPhase::Shutdown;
            self.decision_generation.0 = self.decision_generation.0.saturating_add(1);
        }

        let join_outcome = match self.job.as_mut() {
            Some(job) => join_thread_until(&mut job.join_handle, deadline),
            None => FinishedThreadJoin::AlreadyJoined,
        };
        match join_outcome {
            FinishedThreadJoin::StillRunning => PlaylistStartupShutdownOutcome::TimedOut,
            FinishedThreadJoin::Panicked => {
                self.job = None;
                self.store = None;
                self.shutdown_complete = true;
                PlaylistStartupShutdownOutcome::ThreadPanicked
            }
            FinishedThreadJoin::AlreadyJoined | FinishedThreadJoin::Joined => {
                self.job = None;
                self.store = None;
                self.shutdown_complete = true;
                PlaylistStartupShutdownOutcome::Completed
            }
        }
    }

    pub(crate) const fn is_shutdown(&self) -> bool {
        matches!(self.phase, PlaylistStartupPhase::Shutdown)
    }

    pub(crate) const fn view(&self) -> PlaylistStartupView {
        PlaylistStartupView {
            phase: self.phase,
            restore_generation: self.draft.restore_generation(),
            persistence: self.persistence,
            warning: self.warning,
        }
    }
}

/// Async owner errors различают lifecycle misuse и OS thread failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupOwnerError {
    InvalidPhase,
    MissingStore,
    ThreadSpawn(io::ErrorKind),
}
