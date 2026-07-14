use std::fmt;
use std::io;
use std::time::Duration;

use crate::atomic_write::{
    AtomicWriteOutcome, DurabilityRetryOutcome, DurabilityUnconfirmedCause, NotReplacedFailure,
};
use crate::{ImmutableSaveSnapshot, SaveRevision};

use super::SaveWorker;

/// Validated quiet period после последней committed mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveDebounce(Duration);

impl SaveDebounce {
    /// Минимум D10d.
    pub const MINIMUM: Duration = Duration::from_millis(250);
    /// Максимум D10d.
    pub const MAXIMUM: Duration = Duration::from_secs(30);

    /// Проверяет inclusive D10d range; zero не имеет hidden semantics.
    pub fn new(duration: Duration) -> Result<Self, SaveDebounceValidationError> {
        if !(Self::MINIMUM..=Self::MAXIMUM).contains(&duration) {
            return Err(SaveDebounceValidationError { duration });
        }
        Ok(Self(duration))
    }

    /// Возвращает уже validated duration worker timer-у.
    pub const fn duration(self) -> Duration {
        self.0
    }
}

/// Invalid debounce не запускает background owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveDebounceValidationError {
    duration: Duration,
}

impl SaveDebounceValidationError {
    /// Возвращает отклонённое значение для config diagnostics.
    pub const fn duration(self) -> Duration {
        self.duration
    }
}

impl fmt::Display for SaveDebounceValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "playlist-state debounce {:?} вне диапазона {:?}..={:?}",
            self.duration,
            SaveDebounce::MINIMUM,
            SaveDebounce::MAXIMUM
        )
    }
}

impl std::error::Error for SaveDebounceValidationError {}

/// D10b/D81 причина, по которой writer нельзя создавать в этой startup lineage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveBlockReason {
    /// На диске доказана более новая schema.
    NewerSchema,
    /// Версию нельзя безопасно распознать.
    UnrecognizedVersion,
    /// Top-level version key дублируется/конфликтует.
    DuplicateVersion,
    /// Explicit quarantine не удалось выполнить.
    QuarantineFailed,
    /// Source изменился после inspection.
    QuarantineSourceChanged,
}

/// Startup decision передаётся caller-ом после explicit inspect/quarantine workflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveWorkerAccess {
    /// Target разрешено заменять в текущей lineage.
    Writable,
    /// Target защищён; новый reload/startup workflow обязателен.
    SaveBlocked(SaveBlockReason),
}

/// Лёгкий signal port; winit остаётся за пределами crate.
pub trait SaveWakePort: Send + Sync + 'static {
    /// Просит app drain-ить owner mailbox без передачи payload через event loop.
    fn wake_save_worker(&self) -> Result<(), WakePortDisconnected>;
}

/// App-provided wake receiver больше недоступен.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WakePortDisconnected;

/// Результат попытки создать owner thread либо сохранить D10b block.
pub enum SaveWorkerStartOutcome {
    /// Worker запущен и принимает committed snapshots.
    Started(SaveWorker),
    /// Поток не создавался, filesystem не открывался.
    SaveBlocked(SaveBlockReason),
}

/// Fallible OS thread boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveWorkerStartError {
    /// `thread::Builder::spawn` вернул OS error.
    ThreadSpawn(io::ErrorKind),
}

impl fmt::Display for SaveWorkerStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ThreadSpawn(error_kind) => {
                write!(
                    formatter,
                    "не удалось запустить playlist-state worker: {error_kind:?}"
                )
            }
        }
    }
}

impl std::error::Error for SaveWorkerStartError {}

/// Accepted submission semantics не сворачиваются в `bool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitSnapshotOutcome {
    /// Snapshot принят bounded command path.
    Accepted,
    /// Такая или более новая revision уже принята этим handle.
    NoOpSameOrOlderRevision,
}

/// Backpressure/disconnect возвращает ownership snapshot caller-у.
pub enum SubmitSnapshotError {
    /// Bounded command queue заполнена.
    Backpressure(Box<ImmutableSaveSnapshot>),
    /// Worker command receiver завершён.
    Disconnected(Box<ImmutableSaveSnapshot>),
    /// Handle-local monotonic guard poisoned; snapshot не потерян.
    SubmissionStatePoisoned(Box<ImmutableSaveSnapshot>),
    /// Private command envelope нарушил доказанный submit invariant.
    CommandTypeInvariantLost,
}

impl fmt::Debug for SubmitSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backpressure(snapshot) => formatter
                .debug_tuple("Backpressure")
                .field(&snapshot.revision())
                .finish(),
            Self::Disconnected(snapshot) => formatter
                .debug_tuple("Disconnected")
                .field(&snapshot.revision())
                .finish(),
            Self::SubmissionStatePoisoned(snapshot) => formatter
                .debug_tuple("SubmissionStatePoisoned")
                .field(&snapshot.revision())
                .finish(),
            Self::CommandTypeInvariantLost => formatter.write_str("CommandTypeInvariantLost"),
        }
    }
}

/// Non-snapshot control command delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveControlError {
    /// Bounded queue занята committed snapshot/control work.
    Backpressure,
    /// Worker уже завершён.
    Disconnected,
}

/// Physical operation, завершившая одну scheduled attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveAttemptOutcome {
    /// Полный temp-write/replace protocol.
    FullWrite(AtomicWriteOutcome),
    /// Только повтор directory sync после successful rename.
    DirectoryDurabilityRetry(DurabilityRetryOutcome),
}

/// Lossless terminal report одной выполненной filesystem attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveAttemptReport {
    /// Revision exact immutable snapshot.
    pub revision: SaveRevision,
    /// Distinction full write vs targeted durability retry.
    pub outcome: SaveAttemptOutcome,
}

/// Coalesced user-visible warning без raw path/URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveWarning {
    /// Latest dirty revision, которой относится warning.
    pub revision: SaveRevision,
    /// Последняя safe failure category.
    pub failure: SaveWarningFailure,
    /// Число одинаковых последовательных failures.
    pub occurrence_count: u32,
}

/// Warning сохраняет pre/post rename distinction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveWarningFailure {
    /// Target не был заменён.
    NotReplaced(NotReplacedFailure),
    /// Target заменён, но directory durability не подтверждена.
    DurabilityUnconfirmed(DurabilityUnconfirmedCause),
}

/// Exactly-once mailbox events; progress/warning update coalesce-ится отдельно.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveWorkerEvent {
    /// Одна physical attempt завершилась.
    AttemptCompleted(SaveAttemptReport),
    /// Latest warning изменился либо был очищен.
    WarningChanged(Option<SaveWarning>),
    /// Background owner завершился вне explicit successful shutdown.
    WorkerDisconnected(WorkerDisconnectReason),
    /// Persistence продолжает работать, но event-driven app wake недоступен.
    WakePortDisconnected,
}

/// Thread exit distinctions не теряются в empty poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerDisconnectReason {
    /// Все command senders были dropped без explicit shutdown.
    CommandChannelClosed,
    /// Неожиданный unwind/invariant exit.
    UnexpectedThreadExit,
}

/// D68 exact final persistence state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownPersistenceOutcome {
    /// Caller не передал committed state и worker не имел dirty revision.
    NoCommittedSnapshot,
    /// Latest committed revision уже была durable.
    AlreadyDurable { revision: SaveRevision },
    /// Shutdown выполнил immediate full write либо targeted sync.
    Attempted(SaveAttemptReport),
}

/// Worker acknowledgement отправляется после последнего filesystem access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShutdownCompletion {
    /// Точное состояние newest committed revision на deadline boundary.
    pub persistence: ShutdownPersistenceOutcome,
}

/// Timeout phase показывает, что именно не было подтверждено.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownTimeoutPhase {
    /// Reserved shutdown command не попал в заполненный bounded queue.
    CommandAdmission,
    /// Worker не закончил blocking filesystem operation до deadline.
    CompletionAcknowledgement,
    /// Filesystem закончен, но OS thread exit не подтверждён до deadline.
    ThreadExit,
}

/// Bounded shutdown никогда не выдаёт `JoinHandle` за timed acknowledgement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveWorkerShutdownOutcome {
    /// Flush и thread exit подтверждены.
    Complete(ShutdownCompletion),
    /// Deadline истёк; caller должен перейти к terminal process-exit policy.
    TimedOut {
        /// Где истёк bounded deadline.
        phase: ShutdownTimeoutPhase,
        /// Может присутствовать, если flush уже подтверждён, а thread exit — нет.
        completion: Option<ShutdownCompletion>,
    },
    /// Thread завершился panic-ом после acknowledgement protocol.
    ThreadPanicked(ShutdownCompletion),
}
