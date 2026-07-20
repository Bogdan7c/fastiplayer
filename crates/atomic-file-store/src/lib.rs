//! Нейтральный atomic replace одного файла с честным durability outcome.
//!
//! Crate владеет всей filesystem-политикой протокола: создаёт уникальный
//! same-directory temp через `create_new`, задаёт Unix mode `0600`, записывает,
//! flush/sync-ит файл, делает rename, sync-ит родительский каталог и удаляет
//! через RAII только temp, созданный текущей попыткой.
//!
//! Caller выбирает только точный target и готовые bytes. Формат данных, mutex
//! между соседними операциями, retry/backoff и пользовательские предупреждения
//! остаются ответственностью caller-а.

#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Ограничивает число collision-safe попыток создать owned temp-файл.
const MAX_TEMP_FILE_CREATE_ATTEMPTS: u64 = 32;

/// Не повторяет temp name между последовательными попытками текущего процесса.
static NEXT_TEMP_FILE_NONCE: AtomicU64 = AtomicU64::new(1);

/// Этап до atomic replace, на котором target гарантированно не был заменён.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomicFileWriteStage {
    /// Target path не содержит пригодного имени файла.
    ValidateTargetPath,
    /// Новый owned temp-файл создать не удалось.
    CreateTempFile,
    /// Все переданные bytes записать не удалось.
    WriteTempFile,
    /// Buffered bytes передать файловому handle не удалось.
    FlushTempFile,
    /// Temp content и metadata синхронизировать не удалось.
    SyncTempFile,
    /// Same-directory replace не состоялся.
    RenameTempFile,
}

/// Privacy-safe причина failure до replace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomicFileWriteCause {
    /// Filesystem error представлен только безопасным классом без пути.
    Io(io::ErrorKind),
    /// Все bounded candidate names уже существовали.
    TempNameAttemptsExhausted,
}

/// Failure, после которого прежний target остаётся authoritative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtomicFileWriteFailure {
    /// Точный этап файлового протокола.
    pub stage: AtomicFileWriteStage,
    /// Безопасная классификация причины.
    pub cause: AtomicFileWriteCause,
}

/// Причина неподтверждённой durability после успешного rename.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectorySyncError {
    /// Родительский path нельзя открыть как directory handle.
    OpenDirectory(io::ErrorKind),
    /// Filesystem или OS отклонили sync directory metadata.
    SyncDirectory(io::ErrorKind),
}

/// Итог одной physical atomic-replace попытки.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomicFileWriteOutcome {
    /// Target не заменён, поэтому прежнее содержимое authoritative.
    NotReplaced(AtomicFileWriteFailure),
    /// Rename состоялся, но crash durability directory entry не подтверждена.
    ReplacedDurabilityUnconfirmed(DirectorySyncError),
    /// Temp и parent directory sync подтверждены доступными OS primitives.
    Durable,
}

/// Заменяет один target готовыми bytes по единому durability protocol.
///
/// Функция не создаёт parent directories, не выбирает target path, не
/// сериализует данные и не вводит retry policy. После
/// [`AtomicFileWriteOutcome::NotReplaced`] старый target не был заменён. После
/// [`AtomicFileWriteOutcome::ReplacedDurabilityUnconfirmed`] новый target уже
/// видим и повторная запись старого payload не является rollback-ом.
#[must_use]
pub fn replace_file_atomically(target_path: &Path, contents: &[u8]) -> AtomicFileWriteOutcome {
    // Production adapter централизует прямые вызовы стандартной filesystem API.
    replace_file_atomically_with(&StdFileSystem, target_path, contents)
}

/// Повторяет только sync родительского каталога уже заменённого target.
///
/// Эта операция никогда не создаёт temp и никогда не переписывает target.
pub fn sync_parent_directory(target_path: &Path) -> Result<(), DirectorySyncError> {
    // Retry использует тот же production adapter, что post-rename sync.
    StdFileSystem.sync_parent_directory(target_path)
}

/// Узкий private adapter позволяет тестам детерминированно ломать каждый этап.
trait AtomicFileSystem {
    /// Writable handle конкретной реализации filesystem.
    type TempFile: Write;

    /// Создаёт новый user-only temp или возвращает точную I/O ошибку.
    fn create_new_user_only(&self, temp_path: &Path) -> io::Result<Self::TempFile>;

    /// Подтверждает content/metadata temp перед rename.
    fn sync_temp_file(&self, temp_file: &Self::TempFile) -> io::Result<()>;

    /// Выполняет replace в пределах родительского каталога.
    fn rename(&self, source_path: &Path, target_path: &Path) -> io::Result<()>;

    /// Подтверждает directory entry после rename или при targeted retry.
    fn sync_parent_directory(&self, target_path: &Path) -> Result<(), DirectorySyncError>;

    /// Удаляет ровно один owned temp path во время RAII cleanup.
    fn remove_file(&self, temp_path: &Path) -> io::Result<()>;
}

/// Stateless production adapter над `std::fs`.
struct StdFileSystem;

impl AtomicFileSystem for StdFileSystem {
    /// Production temp handle является обычным файлом.
    type TempFile = File;

    fn create_new_user_only(&self, temp_path: &Path) -> io::Result<Self::TempFile> {
        // Platform-specific helper сохраняет create-new и permission policy вместе.
        open_user_only_temp(temp_path)
    }

    fn sync_temp_file(&self, temp_file: &Self::TempFile) -> io::Result<()> {
        // `sync_all` подтверждает и содержимое, и metadata temp перед rename.
        temp_file.sync_all()
    }

    fn rename(&self, source_path: &Path, target_path: &Path) -> io::Result<()> {
        // Temp всегда создаётся рядом с target, поэтому cross-filesystem move не нужен.
        fs::rename(source_path, target_path)
    }

    fn sync_parent_directory(&self, target_path: &Path) -> Result<(), DirectorySyncError> {
        // Отдельный handle нужен для durability изменения directory entry.
        let directory_handle = OpenOptions::new()
            .read(true)
            .open(parent_directory(target_path))
            .map_err(|error| DirectorySyncError::OpenDirectory(error.kind()))?;
        // Unsupported filesystem остаётся честным typed failure, а не success.
        directory_handle
            .sync_all()
            .map_err(|error| DirectorySyncError::SyncDirectory(error.kind()))
    }

    fn remove_file(&self, temp_path: &Path) -> io::Result<()> {
        // Cleanup получает exact path и не выполняет directory scan или wildcard.
        fs::remove_file(temp_path)
    }
}

/// Выполняет protocol через production или focused-test filesystem adapter.
fn replace_file_atomically_with<FileSystem: AtomicFileSystem>(
    file_system: &FileSystem,
    target_path: &Path,
    contents: &[u8],
) -> AtomicFileWriteOutcome {
    // Непригодный target отклоняется до любого filesystem side effect.
    let Some(target_file_name) = target_path.file_name() else {
        return invalid_target_path_failure();
    };
    // Пустое имя также не может безопасно породить sibling temp.
    if target_file_name.is_empty() {
        return invalid_target_path_failure();
    }

    // Temp создаётся только в том же каталоге, что и будущий target.
    let parent_directory = parent_directory(target_path);
    // Только успешно созданный текущей попыткой path становится owned.
    let (temp_path, mut temp_file) =
        match create_owned_temp_file(file_system, parent_directory, target_file_name) {
            Ok(created_temp) => created_temp,
            Err(failure) => return AtomicFileWriteOutcome::NotReplaced(failure),
        };
    // Guard удалит exact temp при любом раннем return или panic unwind.
    let mut owned_temp = OwnedTempFile::new(file_system, temp_path);

    // `write_all` исключает silently accepted partial payload.
    if let Err(error) = temp_file.write_all(contents) {
        return io_failure_outcome(AtomicFileWriteStage::WriteTempFile, error);
    }
    // Явный flush сохраняет прежний protocol и поддерживает buffered adapters.
    if let Err(error) = temp_file.flush() {
        return io_failure_outcome(AtomicFileWriteStage::FlushTempFile, error);
    }
    // Rename запрещён, пока temp durability не подтверждена.
    if let Err(error) = file_system.sync_temp_file(&temp_file) {
        return io_failure_outcome(AtomicFileWriteStage::SyncTempFile, error);
    }

    // Handle закрывается до rename для одинаковой семантики на поддерживаемых OS.
    drop(temp_file);
    // Только этот вызов меняет authoritative target.
    if let Err(error) = file_system.rename(owned_temp.path(), target_path) {
        return io_failure_outcome(AtomicFileWriteStage::RenameTempFile, error);
    }
    // После rename прежнего temp path больше нет, cleanup нужно отключить.
    owned_temp.disarm_after_rename();

    // Post-rename failure не маскируется как NotReplaced.
    match file_system.sync_parent_directory(target_path) {
        Ok(()) => AtomicFileWriteOutcome::Durable,
        Err(cause) => AtomicFileWriteOutcome::ReplacedDurabilityUnconfirmed(cause),
    }
}

/// Создаёт unique temp через atomic `create_new` в target directory.
fn create_owned_temp_file<FileSystem: AtomicFileSystem>(
    file_system: &FileSystem,
    parent_directory: &Path,
    target_file_name: &OsStr,
) -> Result<(PathBuf, FileSystem::TempFile), AtomicFileWriteFailure> {
    // Каждая попытка резервирует собственный bounded диапазон nonce.
    let initial_nonce =
        NEXT_TEMP_FILE_NONCE.fetch_add(MAX_TEMP_FILE_CREATE_ATTEMPTS, Ordering::Relaxed);
    // Коллизия одного имени не разрешает перезаписать чужой temp.
    for attempt_offset in 0..MAX_TEMP_FILE_CREATE_ATTEMPTS {
        // Wrapping сохраняет уникальный progression даже после u64 overflow.
        let candidate_nonce = initial_nonce.wrapping_add(attempt_offset);
        // Имя строится без lossy преобразования исходного filename.
        let temp_path = parent_directory.join(temp_file_name(target_file_name, candidate_nonce));
        // Только atomic create_new даёт ownership текущей попытке.
        match file_system.create_new_user_only(&temp_path) {
            Ok(temp_file) => return Ok((temp_path, temp_file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(io_failure(AtomicFileWriteStage::CreateTempFile, error));
            }
        }
    }

    // Bounded exhaustion не превращается в directory scan или wildcard cleanup.
    Err(AtomicFileWriteFailure {
        stage: AtomicFileWriteStage::CreateTempFile,
        cause: AtomicFileWriteCause::TempNameAttemptsExhausted,
    })
}

/// Возвращает безопасный failure для target без filename.
fn invalid_target_path_failure() -> AtomicFileWriteOutcome {
    // InvalidInput не раскрывает исходный путь в публичной диагностике.
    AtomicFileWriteOutcome::NotReplaced(AtomicFileWriteFailure {
        stage: AtomicFileWriteStage::ValidateTargetPath,
        cause: AtomicFileWriteCause::Io(io::ErrorKind::InvalidInput),
    })
}

/// Выделяет родительский каталог, включая relative target в current directory.
fn parent_directory(target_path: &Path) -> &Path {
    // Пустой parent у `file.json` означает текущий каталог.
    target_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Формирует имя без lossy преобразования исходного filename.
fn temp_file_name(target_file_name: &OsStr, nonce: u64) -> OsString {
    // Leading dot не является политикой cleanup: directory никогда не сканируется.
    let mut temp_file_name = OsString::from(".");
    // Exact platform string target-а сохраняется внутри candidate.
    temp_file_name.push(target_file_name);
    // PID и monotonic nonce уменьшают коллизии между процессами и попытками.
    temp_file_name.push(format!(".save-{}-{nonce}.tmp", std::process::id()));
    // Готовое имя используется только с create_new.
    temp_file_name
}

/// На Unix mode применяется при создании, без окна широких permissions.
#[cfg(unix)]
fn open_user_only_temp(temp_path: &Path) -> io::Result<File> {
    // Unix extension задаёт creation mode вместе с атомарным open.
    use std::os::unix::fs::OpenOptionsExt;

    // Umask может только дополнительно сузить user-only mode.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(temp_path)
}

/// На остальных target-ах используются максимально строгие platform defaults.
#[cfg(not(unix))]
fn open_user_only_temp(temp_path: &Path) -> io::Result<File> {
    // Portable std не предоставляет Unix mode на других платформах.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp_path)
}

/// Преобразует raw I/O error в privacy-safe typed failure.
fn io_failure(stage: AtomicFileWriteStage, error: io::Error) -> AtomicFileWriteFailure {
    // Путь и platform message намеренно не переходят public boundary.
    AtomicFileWriteFailure {
        stage,
        cause: AtomicFileWriteCause::Io(error.kind()),
    }
}

/// Преобразует raw I/O error сразу в pre-rename outcome.
fn io_failure_outcome(stage: AtomicFileWriteStage, error: io::Error) -> AtomicFileWriteOutcome {
    // Общий constructor не позволяет случайно потерять stage.
    AtomicFileWriteOutcome::NotReplaced(io_failure(stage, error))
}

/// RAII cleanup знает только exact path, созданный текущей попыткой.
struct OwnedTempFile<'file_system, FileSystem: AtomicFileSystem> {
    /// Adapter выполняет exact-path removal без directory traversal.
    file_system: &'file_system FileSystem,
    /// Единственный temp path, ownership которого доказан create_new success.
    path: PathBuf,
    /// После rename cleanup отключается, потому что source path исчез.
    remove_on_drop: bool,
}

impl<'file_system, FileSystem: AtomicFileSystem> OwnedTempFile<'file_system, FileSystem> {
    /// Вооружает cleanup сразу после successful create_new.
    fn new(file_system: &'file_system FileSystem, path: PathBuf) -> Self {
        // Все поля задаются вместе, чтобы не было невооружённого owned temp.
        Self {
            file_system,
            path,
            remove_on_drop: true,
        }
    }

    /// Даёт rename-у borrowed exact source path.
    fn path(&self) -> &Path {
        // Caller не получает ownership и не может подменить cleanup target.
        &self.path
    }

    /// Отключает cleanup только после successful rename.
    fn disarm_after_rename(&mut self) {
        // Directory sync failure уже не должен пытаться удалить новый target.
        self.remove_on_drop = false;
    }
}

impl<FileSystem: AtomicFileSystem> Drop for OwnedTempFile<'_, FileSystem> {
    fn drop(&mut self) {
        // До rename удаляется только temp текущей попытки.
        if self.remove_on_drop {
            // Drop не может вернуть cleanup error поверх исходного failure.
            let _cleanup_result = self.file_system.remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests;
