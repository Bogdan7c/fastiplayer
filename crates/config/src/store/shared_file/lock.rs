//! Замок записи config-а: файл `config.toml.lock` рядом с config.
//!
//! Решение владельца 135 («общая доска»): config пишут несколько копий плеера, и каждая
//! перед записью берёт этот замок на время «перечитать → вписать свои ключи → записать»
//! (миллисекунды). Под замком же идут уборка брошенных temp и перенос битого файла в
//! `.bak` при старте: без замка уборка могла бы удалить temp соседа посреди его записи.
//!
//! Устройство:
//! - замок — эксклюзивный `flock` (`std::fs::File::try_lock`) на отдельном файле. Сам
//!   `config.toml` для этого не годится: атомарная запись заменяет его rename-ом, после
//!   которого путь указывает на новый inode, и замок на старом уже никого не исключает;
//! - ждём с явным сроком от вызывающего кода: короткие попытки с растущей паузой, по
//!   истечении срока — понятный отказ «занят», а не вечное ожидание замёрзшего соседа;
//! - проверки безопасности как у lease единственного экземпляра
//!   (`app-egui/src/app_instance/linux.rs`): открываем без перехода по символической
//!   ссылке, файл обязан быть обычным, принадлежать нашему пользователю и иметь права
//!   0600; после захвата путь повторно сверяется с тем же inode;
//! - **файл-замок никогда не удаляется**. Удаление ломает взаимное исключение: копия A
//!   держит замок на inode 1, копия B уже открыла тот же путь (inode 1) и ждёт, A удаляет
//!   файл, копия C создаёт новый inode 2 и берёт замок на нём — B и C «держат замок»
//!   одновременно. Пустой файл в каталоге config-а ничего не стоит.
//!
//! Замок освобождается при `Drop` (закрытие дескриптора), в том числе при аварийном
//! завершении процесса: `flock` снимает ядро.
//!
//! Особый случай — каталог config-а не принимает новых файлов (нет прав на запись,
//! файловая система только для чтения) и замка в нём ещё нет: создать замок нельзя, но и
//! исключать некого — в такой каталог не пишет никто. Тогда операция идёт без замка
//! ([`ConfigWriteGuard::DirectoryAcceptsNoNewFiles`]), а её собственная попытка записи
//! вернёт точную ошибку («defaults не записаны», «копию сделать нельзя»).

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use super::outcome::{ConfigLockFailure, SharedConfigLockProblem, SharedConfigLockStage};

/// Первая пауза между попытками взять занятый замок.
///
/// Сосед держит замок миллисекунды (перечитать, слить, записать маленький TOML), поэтому
/// начинаем с одной миллисекунды, чтобы не ждать лишнего.
const FIRST_RETRY_PAUSE: Duration = Duration::from_millis(1);

/// Самая длинная пауза между попытками: дольше ждать между проверками незачем, а частые
/// пробуждения при замёрзшем соседе не нагружают процессор (50 проверок в секунду).
const MAX_RETRY_PAUSE: Duration = Duration::from_millis(20);

/// Права файла-замка: только владелец читает и пишет.
#[cfg(unix)]
const LOCK_FILE_MODE: u32 = 0o600;

/// Право менять каталог config-а на время одной операции.
///
/// Держится до конца операции (перечитать → записать, уборка при старте) и
/// освобождается при `Drop`.
#[derive(Debug)]
pub(in crate::store) enum ConfigWriteGuard {
    /// Эксклюзивный замок взят: соседние писатели ждут.
    Locked {
        /// Само владение дескриптором удерживает `flock` до закрытия.
        _lock_file: File,
    },
    /// Каталог config-а не принимает новых файлов (нет прав на запись, файловая система
    /// только для чтения), поэтому и файл-замок в нём создать нельзя. Исключать некого:
    /// в такой каталог не может писать ни одна копия плеера, и любая попытка изменить
    /// его сама вернёт точную ошибку (например, «defaults не записаны»).
    DirectoryAcceptsNoNewFiles,
}

/// Берёт эксклюзивный замок записи, ожидая не дольше `lock_wait`.
///
/// `lock_wait == 0` — одна попытка без ожидания.
pub(in crate::store) fn acquire_config_write_lock(
    lock_path: &Path,
    lock_wait: Duration,
) -> Result<ConfigWriteGuard, ConfigLockFailure> {
    match create_lock_directory(lock_path) {
        LockFilePreparation::Ready => {}
        LockFilePreparation::DirectoryAcceptsNoNewFiles => {
            return Ok(ConfigWriteGuard::DirectoryAcceptsNoNewFiles);
        }
        LockFilePreparation::Failed(problem) => {
            return Err(ConfigLockFailure::Unavailable(problem));
        }
    }
    let lock_presence = inspect_lock_path(lock_path).map_err(ConfigLockFailure::Unavailable)?;
    let lock_file = match open_lock_file(lock_path, lock_presence) {
        Ok(lock_file) => lock_file,
        Err(LockFileOpenFailure::DirectoryAcceptsNoNewFiles) => {
            return Ok(ConfigWriteGuard::DirectoryAcceptsNoNewFiles);
        }
        Err(LockFileOpenFailure::Failed(problem)) => {
            return Err(ConfigLockFailure::Unavailable(problem));
        }
    };
    let descriptor_identity = inspect_open_lock_file(&lock_file)?;
    wait_for_exclusive_lock(&lock_file, lock_wait)?;
    revalidate_lock_path(lock_path, &descriptor_identity)
        .map_err(ConfigLockFailure::Unavailable)?;
    Ok(ConfigWriteGuard::Locked {
        _lock_file: lock_file,
    })
}

/// Итог подготовки каталога под файл-замок.
enum LockFilePreparation {
    /// Каталог есть.
    Ready,
    /// Каталога нет, и создать его нельзя (нет прав, файловая система только для чтения).
    DirectoryAcceptsNoNewFiles,
    /// Другая ошибка (например, на пути лежит обычный файл вместо каталога).
    Failed(SharedConfigLockProblem),
}

/// Создаёт каталог config-а, если его ещё нет (первый запуск, тесты с пустым каталогом).
fn create_lock_directory(lock_path: &Path) -> LockFilePreparation {
    let Some(directory) = lock_path
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
    else {
        return LockFilePreparation::Ready;
    };
    match fs::create_dir_all(directory) {
        Ok(()) => LockFilePreparation::Ready,
        Err(error) if refuses_new_files(&error) => LockFilePreparation::DirectoryAcceptsNoNewFiles,
        Err(error) => {
            LockFilePreparation::Failed(io_problem(SharedConfigLockStage::CreateDirectory, &error))
        }
    }
}

/// До открытия отсекает известный опасный случай: по пути лежит не обычный файл
/// (символическая ссылка, каталог, FIFO) или чужой файл. Окно гонки между этой
/// проверкой и `open` закрывают флаг «не переходить по ссылке» и повторные проверки
/// дескриптора и пути.
///
/// Возвращает, был ли файл-замок на месте до открытия.
fn inspect_lock_path(lock_path: &Path) -> Result<LockFilePresence, SharedConfigLockProblem> {
    match fs::symlink_metadata(lock_path) {
        Ok(path_metadata) => {
            validate_lock_metadata(&path_metadata).map(|()| LockFilePresence::Existing)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(LockFilePresence::Absent),
        Err(error) => Err(io_problem(SharedConfigLockStage::InspectPath, &error)),
    }
}

/// Был ли файл-замок на месте до открытия.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockFilePresence {
    /// Замок уже лежал (создан прошлыми запусками или соседом).
    Existing,
    /// Замка нет: открытие его создаст.
    Absent,
}

/// Отказ ОС создать файл: нет прав на запись в каталог или файловая система только для
/// чтения.
fn refuses_new_files(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
    )
}

/// Почему не удалось открыть файл-замок.
enum LockFileOpenFailure {
    /// Замка ещё нет, а каталог не принимает новых файлов.
    DirectoryAcceptsNoNewFiles,
    /// Замок нельзя использовать.
    Failed(SharedConfigLockProblem),
}

/// Открывает или создаёт файл-замок для записи (`flock` на Windows требует запись).
///
/// Отказ в правах значит «каталог не принимает файлов», только если замка до открытия
/// не было: существующий замок без права записи — испорченный замок, а не каталог
/// только для чтения, и без него писать нельзя.
fn open_lock_file(
    lock_path: &Path,
    lock_presence: LockFilePresence,
) -> Result<File, LockFileOpenFailure> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    restrict_open_options(&mut options);
    options.open(lock_path).map_err(|error| {
        if lock_presence == LockFilePresence::Absent && refuses_new_files(&error) {
            LockFileOpenFailure::DirectoryAcceptsNoNewFiles
        } else if is_symlink_refusal(&error) {
            // Символическая ссылка, подложенная после проверки пути, даёт ELOOP из-за
            // O_NOFOLLOW: это не «ошибка диска», а подменённый замок.
            LockFileOpenFailure::Failed(SharedConfigLockProblem::NotRegularFile)
        } else {
            LockFileOpenFailure::Failed(io_problem(SharedConfigLockStage::Open, &error))
        }
    })
}

/// Проверяет открытый дескриптор и возвращает его идентичность для повторной сверки.
fn inspect_open_lock_file(lock_file: &File) -> Result<LockFileIdentity, ConfigLockFailure> {
    let descriptor_metadata = lock_file.metadata().map_err(|error| {
        ConfigLockFailure::Unavailable(io_problem(SharedConfigLockStage::InspectDescriptor, &error))
    })?;
    validate_lock_metadata(&descriptor_metadata).map_err(ConfigLockFailure::Unavailable)?;
    restrict_lock_file_permissions(lock_file).map_err(ConfigLockFailure::Unavailable)?;
    Ok(LockFileIdentity::of(&descriptor_metadata))
}

/// Пытается взять эксклюзивный замок, пока не истечёт срок ожидания.
fn wait_for_exclusive_lock(lock_file: &File, lock_wait: Duration) -> Result<(), ConfigLockFailure> {
    let started_at = Instant::now();
    let mut retry_pause = FIRST_RETRY_PAUSE;
    loop {
        match lock_file.try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(error)) => {
                return Err(ConfigLockFailure::Unavailable(io_problem(
                    SharedConfigLockStage::Acquire,
                    &error,
                )));
            }
        }
        let waited = started_at.elapsed();
        let Some(remaining) = lock_wait.checked_sub(waited).filter(|left| !left.is_zero()) else {
            return Err(ConfigLockFailure::Busy { waited });
        };
        thread::sleep(retry_pause.min(remaining));
        retry_pause = (retry_pause * 2).min(MAX_RETRY_PAUSE);
    }
}

/// После захвата убеждается, что путь всё ещё указывает на тот же файл, что мы заперли.
///
/// Если файл-замок подменили (удалили и создали заново), наш `flock` стоит на файле,
/// которого по пути уже нет, и соседей он не исключает — писать так нельзя.
fn revalidate_lock_path(
    lock_path: &Path,
    descriptor_identity: &LockFileIdentity,
) -> Result<(), SharedConfigLockProblem> {
    let path_metadata = fs::symlink_metadata(lock_path)
        .map_err(|error| io_problem(SharedConfigLockStage::RevalidatePath, &error))?;
    validate_lock_metadata(&path_metadata)?;
    if LockFileIdentity::of(&path_metadata) != *descriptor_identity {
        return Err(SharedConfigLockProblem::IdentityChanged);
    }
    Ok(())
}

/// Общая проверка метаданных пути и дескриптора: обычный файл нашего пользователя.
fn validate_lock_metadata(metadata: &fs::Metadata) -> Result<(), SharedConfigLockProblem> {
    if !metadata.file_type().is_file() {
        return Err(SharedConfigLockProblem::NotRegularFile);
    }
    if !is_owned_by_current_user(metadata) {
        return Err(SharedConfigLockProblem::OwnerMismatch);
    }
    Ok(())
}

/// Безопасный класс ошибки шага без пути и текста ОС.
fn io_problem(stage: SharedConfigLockStage, error: &io::Error) -> SharedConfigLockProblem {
    SharedConfigLockProblem::Io {
        stage,
        error_kind: error.kind(),
    }
}

/// Идентичность файла-замка: устройство и inode (на Unix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LockFileIdentity {
    /// Номер устройства файловой системы.
    device: u64,
    /// Номер inode внутри устройства.
    inode: u64,
}

impl LockFileIdentity {
    #[cfg(unix)]
    fn of(metadata: &fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    /// Без Unix-идентичности сверка пути вырождается в «обычный файл» (`validate_lock_metadata`).
    #[cfg(not(unix))]
    fn of(_metadata: &fs::Metadata) -> Self {
        Self {
            device: 0,
            inode: 0,
        }
    }
}

/// Unix: не переходить по символической ссылке и не наследовать дескриптор в `exec`.
///
/// `std` и так открывает файлы с `O_CLOEXEC`; флаг указан явно, чтобы инвариант не
/// зависел от внутреннего устройства `std` (дочерний процесс не должен держать замок).
#[cfg(unix)]
fn restrict_open_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options
        .mode(LOCK_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
}

#[cfg(not(unix))]
fn restrict_open_options(_options: &mut OpenOptions) {}

/// Отказ `O_NOFOLLOW` открыть символическую ссылку (`ELOOP`).
#[cfg(unix)]
fn is_symlink_refusal(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn is_symlink_refusal(_error: &io::Error) -> bool {
    false
}

/// Unix: права 0600 и на уже существующем файле (его мог создать старый umask или человек).
#[cfg(unix)]
fn restrict_lock_file_permissions(lock_file: &File) -> Result<(), SharedConfigLockProblem> {
    use std::os::unix::fs::PermissionsExt;
    lock_file
        .set_permissions(fs::Permissions::from_mode(LOCK_FILE_MODE))
        .map_err(|error| io_problem(SharedConfigLockStage::RestrictPermissions, &error))
}

#[cfg(not(unix))]
fn restrict_lock_file_permissions(_lock_file: &File) -> Result<(), SharedConfigLockProblem> {
    Ok(())
}

/// Unix: владелец файла — наш действующий пользователь.
#[cfg(unix)]
fn is_owned_by_current_user(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.uid() == effective_user_id()
}

#[cfg(not(unix))]
fn is_owned_by_current_user(_metadata: &fs::Metadata) -> bool {
    true
}

/// Действующий uid процесса.
///
/// В стандартной библиотеке Rust безопасного способа узнать uid нет, поэтому это
/// единственный `unsafe` crate-а (`#![deny(unsafe_code)]` в `lib.rs`).
#[cfg(unix)]
#[allow(unsafe_code)]
fn effective_user_id() -> u32 {
    // SAFETY: `geteuid` не принимает указателей, не меняет память Rust и всегда успешна.
    unsafe { libc::geteuid() }
}
