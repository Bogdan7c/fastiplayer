use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{ImmutableSaveSnapshot, PlaylistStateStore, StateSerializationError};

/// Bounded число collision-safe попыток создать owned temp-файл.
const MAX_TEMP_FILE_CREATE_ATTEMPTS: u64 = 32;

/// Process-local nonce не даёт двум последовательным attempts выбрать одно имя.
static NEXT_TEMP_FILE_NONCE: AtomicU64 = AtomicU64::new(1);

/// Этап до atomic replace, на котором target гарантированно не был заменён.
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
    /// Точный этап протокола.
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

/// Итог одной physical write попытки.
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

/// Injectable writer boundary для deterministic worker tests.
pub(crate) trait SnapshotWriter: Send + Sync + 'static {
    /// Выполняет максимум одну physical write.
    fn write_snapshot(&self, snapshot: &ImmutableSaveSnapshot) -> AtomicWriteOutcome;

    /// Повторяет только parent-directory sync после successful rename.
    fn retry_directory_durability(&self) -> DurabilityRetryOutcome;
}

/// Production writer использует общий lock `PlaylistStateStore`.
pub(crate) struct AtomicSnapshotWriter {
    store: Arc<PlaylistStateStore>,
}

impl AtomicSnapshotWriter {
    /// Связывает writer с тем же store owner, что inspection/quarantine.
    pub(crate) fn new(store: Arc<PlaylistStateStore>) -> Self {
        Self { store }
    }
}

impl SnapshotWriter for AtomicSnapshotWriter {
    fn write_snapshot(&self, snapshot: &ImmutableSaveSnapshot) -> AtomicWriteOutcome {
        let serialized_json = match snapshot.serialize_json() {
            Ok(serialized_json) => serialized_json,
            Err(serialization_error) => {
                return AtomicWriteOutcome::NotReplaced(NotReplacedFailure {
                    stage: NotReplacedStage::Serialize,
                    cause: NotReplacedCause::Serialization(serialization_error),
                });
            }
        };

        let _operation_guard = match self.store.lock_operations() {
            Ok(operation_guard) => operation_guard,
            Err(()) => {
                return AtomicWriteOutcome::NotReplaced(NotReplacedFailure {
                    stage: NotReplacedStage::LockStore,
                    cause: NotReplacedCause::Io(io::ErrorKind::Other),
                });
            }
        };

        write_serialized_json_atomic(self.store.state_path(), &serialized_json)
    }

    fn retry_directory_durability(&self) -> DurabilityRetryOutcome {
        let _operation_guard = match self.store.lock_operations() {
            Ok(operation_guard) => operation_guard,
            Err(()) => {
                return DurabilityRetryOutcome::StillUnconfirmed(
                    DurabilityUnconfirmedCause::OpenDirectory(io::ErrorKind::Other),
                );
            }
        };

        match sync_parent_directory(self.store.state_path()) {
            Ok(()) => DurabilityRetryOutcome::Durable,
            Err(cause) => DurabilityRetryOutcome::StillUnconfirmed(cause),
        }
    }
}

/// Выполняет same-directory replace и удаляет только созданный этим вызовом temp.
pub(crate) fn write_serialized_json_atomic(
    target_path: &Path,
    serialized_json: &[u8],
) -> AtomicWriteOutcome {
    let Some(target_file_name) = target_path.file_name() else {
        return AtomicWriteOutcome::NotReplaced(NotReplacedFailure {
            stage: NotReplacedStage::ValidateTargetPath,
            cause: NotReplacedCause::Io(io::ErrorKind::InvalidInput),
        });
    };
    if target_file_name.is_empty() {
        return AtomicWriteOutcome::NotReplaced(NotReplacedFailure {
            stage: NotReplacedStage::ValidateTargetPath,
            cause: NotReplacedCause::Io(io::ErrorKind::InvalidInput),
        });
    }

    let parent_directory = target_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let (temp_path, mut temp_file) =
        match create_owned_temp_file(parent_directory, target_file_name) {
            Ok(created_temp) => created_temp,
            Err(failure) => return AtomicWriteOutcome::NotReplaced(failure),
        };
    let mut owned_temp = OwnedTempFile::new(temp_path);

    if let Err(error) = temp_file.write_all(serialized_json) {
        return AtomicWriteOutcome::NotReplaced(io_failure(NotReplacedStage::WriteTempFile, error));
    }
    if let Err(error) = temp_file.flush() {
        return AtomicWriteOutcome::NotReplaced(io_failure(NotReplacedStage::FlushTempFile, error));
    }
    if let Err(error) = temp_file.sync_all() {
        return AtomicWriteOutcome::NotReplaced(io_failure(NotReplacedStage::SyncTempFile, error));
    }

    drop(temp_file);
    if let Err(error) = fs::rename(owned_temp.path(), target_path) {
        return AtomicWriteOutcome::NotReplaced(io_failure(
            NotReplacedStage::RenameTempFile,
            error,
        ));
    }
    owned_temp.disarm_after_rename();

    match sync_parent_directory(target_path) {
        Ok(()) => AtomicWriteOutcome::Durable,
        Err(cause) => AtomicWriteOutcome::ReplacedDurabilityUnconfirmed(cause),
    }
}

/// Создаёт unique temp через atomic `create_new` в target directory.
fn create_owned_temp_file(
    parent_directory: &Path,
    target_file_name: &std::ffi::OsStr,
) -> Result<(PathBuf, File), NotReplacedFailure> {
    let initial_nonce =
        NEXT_TEMP_FILE_NONCE.fetch_add(MAX_TEMP_FILE_CREATE_ATTEMPTS, Ordering::Relaxed);
    for attempt_offset in 0..MAX_TEMP_FILE_CREATE_ATTEMPTS {
        let candidate_nonce = initial_nonce.wrapping_add(attempt_offset);
        let temp_path = parent_directory.join(temp_file_name(target_file_name, candidate_nonce));
        match open_user_only_temp(&temp_path) {
            Ok(temp_file) => return Ok((temp_path, temp_file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(io_failure(NotReplacedStage::CreateTempFile, error));
            }
        }
    }

    Err(NotReplacedFailure {
        stage: NotReplacedStage::CreateTempFile,
        cause: NotReplacedCause::TempNameAttemptsExhausted,
    })
}

/// Формирует имя без lossy преобразования исходного filename.
fn temp_file_name(target_file_name: &std::ffi::OsStr, nonce: u64) -> OsString {
    let mut temp_file_name = OsString::from(".");
    temp_file_name.push(target_file_name);
    temp_file_name.push(format!(".save-{}-{nonce}.tmp", std::process::id()));
    temp_file_name
}

/// На Unix mode применяется при самом создании, без окна широких permissions.
#[cfg(unix)]
fn open_user_only_temp(temp_path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(temp_path)
}

/// На остальных target-ах используются максимально строгие platform defaults.
#[cfg(not(unix))]
fn open_user_only_temp(temp_path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp_path)
}

/// Sync-ит directory entry; unsupported filesystem честно возвращает warning.
fn sync_parent_directory(target_path: &Path) -> Result<(), DurabilityUnconfirmedCause> {
    let parent_directory = target_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory_handle = OpenOptions::new()
        .read(true)
        .open(parent_directory)
        .map_err(|error| DurabilityUnconfirmedCause::OpenDirectory(error.kind()))?;
    directory_handle
        .sync_all()
        .map_err(|error| DurabilityUnconfirmedCause::SyncDirectory(error.kind()))
}

/// Преобразует raw I/O error в privacy-safe typed outcome.
fn io_failure(stage: NotReplacedStage, error: io::Error) -> NotReplacedFailure {
    NotReplacedFailure {
        stage,
        cause: NotReplacedCause::Io(error.kind()),
    }
}

/// RAII cleanup знает только exact path, который успешно создал этот writer.
struct OwnedTempFile {
    path: PathBuf,
    remove_on_drop: bool,
}

impl OwnedTempFile {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            remove_on_drop: true,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm_after_rename(&mut self) {
        self.remove_on_drop = false;
    }
}

impl Drop for OwnedTempFile {
    fn drop(&mut self) {
        if self.remove_on_drop
            && let Err(_cleanup_error) = fs::remove_file(&self.path)
        {
            // Drop не может вернуть вторую ошибку поверх исходного failure.
            // Temp остаётся user-only и принадлежит только этому writer-у;
            // следующий startup не удаляет его по wildcard.
        }
    }
}

#[cfg(test)]
mod tests;
