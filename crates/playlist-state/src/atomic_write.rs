use std::io;
use std::path::Path;
use std::sync::Arc;

use atomic_file_store::{
    AtomicFileWriteCause, AtomicFileWriteFailure, AtomicFileWriteOutcome, AtomicFileWriteStage,
    DirectorySyncError,
};

use crate::{ImmutableSaveSnapshot, PlaylistStateStore, StateSerializationError};

/// Этап playlist-state save, сохраняющий прежнюю public vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotReplacedStage {
    /// Immutable snapshot не удалось закодировать в bounded JSON.
    Serialize,
    /// Target path не содержит пригодного имени файла.
    ValidateTargetPath,
    /// Не удалось захватить process-local store lock.
    LockStore,
    /// Не удалось создать новый owned temp-файл.
    CreateTempFile,
    /// Не удалось полностью записать JSON.
    WriteTempFile,
    /// Buffered bytes не удалось передать файловому handle.
    FlushTempFile,
    /// Temp content/metadata не удалось синхронизировать.
    SyncTempFile,
    /// Same-directory atomic replace не состоялся.
    RenameTempFile,
}

/// Privacy-safe причина pre-replace failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotReplacedCause {
    /// Ошибка filesystem boundary без раскрытия пути.
    Io(io::ErrorKind),
    /// Snapshot нарушил serialization contract.
    Serialization(StateSerializationError),
    /// Исчерпаны уникальные temp candidate names.
    TempNameAttemptsExhausted,
}

/// Typed pre-replace failure не маскирует target mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotReplacedFailure {
    /// Точный этап playlist-state save protocol.
    pub stage: NotReplacedStage,
    /// Safe классификация причины.
    pub cause: NotReplacedCause,
}

/// Directory-sync failure после уже успешного rename.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurabilityUnconfirmedCause {
    /// Родительский path нельзя открыть как directory handle.
    OpenDirectory(io::ErrorKind),
    /// Filesystem/OS отклонил sync directory metadata.
    SyncDirectory(io::ErrorKind),
}

/// Итог одной physical playlist-state write попытки.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomicWriteOutcome {
    /// Target не заменён и старое содержимое остаётся authoritative.
    NotReplaced(NotReplacedFailure),
    /// Rename состоялся, но crash durability directory entry не подтверждена.
    ReplacedDurabilityUnconfirmed(DurabilityUnconfirmedCause),
    /// Temp file и parent directory успешно sync-нуты доступными OS primitives.
    Durable,
}

/// Итог targeted retry, который никогда не переписывает snapshot повторно.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurabilityRetryOutcome {
    /// Parent directory sync теперь подтверждён.
    Durable,
    /// Directory sync всё ещё не подтверждён.
    StillUnconfirmed(DurabilityUnconfirmedCause),
}

/// Injectable writer boundary сохраняет worker tests независимыми от filesystem.
pub(crate) trait SnapshotWriter: Send + Sync {
    /// Выполняет максимум одну physical write.
    fn write_snapshot(&self, snapshot: &ImmutableSaveSnapshot) -> AtomicWriteOutcome;

    /// Повторяет только parent-directory sync после successful rename.
    fn retry_directory_durability(&self) -> DurabilityRetryOutcome;
}

/// Playlist adapter сериализует snapshot и разделяет store lock с quarantine.
pub(crate) struct AtomicSnapshotWriter {
    /// Store остаётся владельцем точного path и process-local operation mutex.
    store: Arc<PlaylistStateStore>,
}

impl AtomicSnapshotWriter {
    /// Связывает writer с тем же store owner, что inspection/quarantine.
    pub(crate) fn new(store: Arc<PlaylistStateStore>) -> Self {
        // Adapter не создаёт отдельного filesystem policy state.
        Self { store }
    }
}

impl SnapshotWriter for AtomicSnapshotWriter {
    fn write_snapshot(&self, snapshot: &ImmutableSaveSnapshot) -> AtomicWriteOutcome {
        // JSON materialization остаётся playlist-state responsibility.
        let serialized_json = match snapshot.serialize_json() {
            Ok(serialized_json) => serialized_json,
            Err(serialization_error) => {
                return AtomicWriteOutcome::NotReplaced(NotReplacedFailure {
                    stage: NotReplacedStage::Serialize,
                    cause: NotReplacedCause::Serialization(serialization_error),
                });
            }
        };

        // Mutex сериализует writer с inspection/quarantine того же store.
        let _operation_guard = match self.store.lock_operations() {
            Ok(operation_guard) => operation_guard,
            Err(()) => {
                return AtomicWriteOutcome::NotReplaced(NotReplacedFailure {
                    stage: NotReplacedStage::LockStore,
                    cause: NotReplacedCause::Io(io::ErrorKind::Other),
                });
            }
        };

        // Neutral crate получает только exact target и готовые bytes.
        write_serialized_json_atomic(self.store.state_path(), &serialized_json)
    }

    fn retry_directory_durability(&self) -> DurabilityRetryOutcome {
        // Retry также разделяет mutex с остальными store operations.
        let _operation_guard = match self.store.lock_operations() {
            Ok(operation_guard) => operation_guard,
            Err(()) => {
                return DurabilityRetryOutcome::StillUnconfirmed(
                    DurabilityUnconfirmedCause::OpenDirectory(io::ErrorKind::Other),
                );
            }
        };

        // Targeted retry не сериализует и не переписывает snapshot.
        match atomic_file_store::sync_parent_directory(self.store.state_path()) {
            Ok(()) => DurabilityRetryOutcome::Durable,
            Err(cause) => {
                DurabilityRetryOutcome::StillUnconfirmed(convert_directory_sync_error(cause))
            }
        }
    }
}

/// Сохраняет внутренний shared helper для queue-state и resume writers.
pub(crate) fn write_serialized_json_atomic(
    target_path: &Path,
    serialized_json: &[u8],
) -> AtomicWriteOutcome {
    // Вся filesystem policy находится внутри neutral crate.
    convert_file_write_outcome(atomic_file_store::replace_file_atomically(
        target_path,
        serialized_json,
    ))
}

/// Переводит neutral filesystem outcome без изменения playlist-state API.
fn convert_file_write_outcome(outcome: AtomicFileWriteOutcome) -> AtomicWriteOutcome {
    // Каждый вариант сохраняет pre/post-rename distinction.
    match outcome {
        AtomicFileWriteOutcome::NotReplaced(failure) => {
            AtomicWriteOutcome::NotReplaced(convert_file_write_failure(failure))
        }
        AtomicFileWriteOutcome::ReplacedDurabilityUnconfirmed(cause) => {
            AtomicWriteOutcome::ReplacedDurabilityUnconfirmed(convert_directory_sync_error(cause))
        }
        AtomicFileWriteOutcome::Durable => AtomicWriteOutcome::Durable,
    }
}

/// Переводит neutral pre-replace failure в прежнюю public vocabulary.
fn convert_file_write_failure(failure: AtomicFileWriteFailure) -> NotReplacedFailure {
    // Stage и cause конвертируются отдельно, чтобы mapping был исчерпывающим.
    NotReplacedFailure {
        stage: convert_file_write_stage(failure.stage),
        cause: convert_file_write_cause(failure.cause),
    }
}

/// Отображает каждый filesystem stage на одноимённый playlist-state stage.
fn convert_file_write_stage(stage: AtomicFileWriteStage) -> NotReplacedStage {
    // Serialize и LockStore не могут прийти из neutral crate.
    match stage {
        AtomicFileWriteStage::ValidateTargetPath => NotReplacedStage::ValidateTargetPath,
        AtomicFileWriteStage::CreateTempFile => NotReplacedStage::CreateTempFile,
        AtomicFileWriteStage::WriteTempFile => NotReplacedStage::WriteTempFile,
        AtomicFileWriteStage::FlushTempFile => NotReplacedStage::FlushTempFile,
        AtomicFileWriteStage::SyncTempFile => NotReplacedStage::SyncTempFile,
        AtomicFileWriteStage::RenameTempFile => NotReplacedStage::RenameTempFile,
    }
}

/// Отображает privacy-safe neutral cause без добавления playlist policy.
fn convert_file_write_cause(cause: AtomicFileWriteCause) -> NotReplacedCause {
    // Playlist-specific Serialization остаётся только в верхнем adapter-е.
    match cause {
        AtomicFileWriteCause::Io(error_kind) => NotReplacedCause::Io(error_kind),
        AtomicFileWriteCause::TempNameAttemptsExhausted => {
            NotReplacedCause::TempNameAttemptsExhausted
        }
    }
}

/// Отображает post-rename directory failure без потери точной стадии.
fn convert_directory_sync_error(cause: DirectorySyncError) -> DurabilityUnconfirmedCause {
    // Оба варианта остаются различимыми для warning/retry read model.
    match cause {
        DirectorySyncError::OpenDirectory(error_kind) => {
            DurabilityUnconfirmedCause::OpenDirectory(error_kind)
        }
        DirectorySyncError::SyncDirectory(error_kind) => {
            DurabilityUnconfirmedCause::SyncDirectory(error_kind)
        }
    }
}

#[cfg(test)]
mod tests;
