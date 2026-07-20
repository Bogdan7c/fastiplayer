use std::cell::RefCell;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::{
    AtomicFileSystem, AtomicFileWriteCause, AtomicFileWriteOutcome, AtomicFileWriteStage,
    DirectorySyncError, replace_file_atomically, replace_file_atomically_with,
};

/// Проверяет real std::fs path: полный payload заменяет target атомарной попыткой.
#[test]
fn real_filesystem_replaces_complete_payload() {
    // TempDir изолирует focused filesystem side effects.
    let directory = tempfile::tempdir().expect("temp directory доступен");
    // Caller передаёт точный target, не filesystem policy.
    let target_path = directory.path().join("state.json");
    // Старый target позволяет доказать именно replace.
    fs::write(&target_path, b"old").expect("старый target создан");

    // Neutral boundary получает только готовые bytes.
    let outcome = replace_file_atomically(&target_path, b"{\"revision\":2}");

    // Поддерживаемая local filesystem подтверждает обе sync стадии.
    assert_eq!(outcome, AtomicFileWriteOutcome::Durable);
    // Target содержит только полный новый payload.
    assert_eq!(
        fs::read(&target_path).expect("новый target читается"),
        b"{\"revision\":2}"
    );
}

/// Проверяет, что Unix target наследует user-only mode созданного temp.
#[cfg(unix)]
#[test]
fn real_filesystem_creates_user_only_target_permissions() {
    // Unix metadata test использует отдельный каталог.
    let directory = tempfile::tempdir().expect("temp directory доступен");
    // Target до попытки не существует.
    let target_path = directory.path().join("private.json");

    // Atomic replace создаёт temp сразу с mode 0600.
    let outcome = replace_file_atomically(&target_path, b"private");

    // Directory sync завершает durable path.
    assert_eq!(outcome, AtomicFileWriteOutcome::Durable);
    // Unix extension нужен только для чтения итогового mode.
    use std::os::unix::fs::PermissionsExt;
    // Mask исключает unrelated file-type bits.
    let mode = fs::metadata(&target_path)
        .expect("target metadata доступна")
        .permissions()
        .mode()
        & 0o777;
    // Ни group, ни other не получают доступ.
    assert_eq!(mode, 0o600);
}

/// Все ошибки до rename сохраняют старый target и чистят exact owned temp.
#[test]
fn every_pre_rename_failure_keeps_target_and_cleans_exact_temp() {
    // Матрица проходит отдельные write/flush/sync/rename boundaries.
    for fault in [
        FaultPoint::Write,
        FaultPoint::Flush,
        FaultPoint::SyncTemp,
        FaultPoint::Rename,
    ] {
        // Новый fake не переносит состояние между стадиями.
        let file_system = FaultingFileSystem::new(Some(fault));
        // Путь нужен только как identity для recorded operations.
        let target_path = Path::new("/virtual/state.json");

        // Fault детерминированно срабатывает на выбранной стадии.
        let outcome = replace_file_atomically_with(&file_system, target_path, b"new");
        // Snapshot состояния читается после завершения RAII cleanup.
        let state = file_system.state.borrow();

        // До успешного rename authoritative target не меняется.
        assert!(!state.target_replaced, "fault {fault:?}");
        // Успешно создан ровно один owned candidate.
        assert_eq!(state.created_paths.len(), 1, "fault {fault:?}");
        // Cleanup удаляет ровно тот же exact candidate.
        assert_eq!(state.removed_paths, state.created_paths, "fault {fault:?}");
        // Outcome не смешивает failure с post-rename durability.
        assert!(matches!(
            outcome,
            AtomicFileWriteOutcome::NotReplaced(failure)
                if failure.stage == fault.expected_stage()
        ));
    }
}

/// Ошибка самого create_new не создаёт ownership и не запускает cleanup.
#[test]
fn create_failure_keeps_target_without_claiming_or_removing_any_temp() {
    // Fake ломает первую filesystem mutation.
    let file_system = FaultingFileSystem::new(Some(FaultPoint::Create));

    // Writer не должен пройти к payload или rename.
    let outcome =
        replace_file_atomically_with(&file_system, Path::new("/virtual/state.json"), b"new");
    // Recorded state читается после возврата outcome.
    let state = file_system.state.borrow();

    // Create вызывался ровно один раз.
    assert_eq!(state.create_calls, 1);
    // Неуспешный create не доказывает ownership candidate-а.
    assert!(state.created_paths.is_empty());
    // Без owned temp RAII ничего не удаляет.
    assert!(state.removed_paths.is_empty());
    // Target остаётся прежним.
    assert!(!state.target_replaced);
    // Typed failure сохраняет exact stage.
    assert!(matches!(
        outcome,
        AtomicFileWriteOutcome::NotReplaced(failure)
            if failure.stage == AtomicFileWriteStage::CreateTempFile
    ));
}

/// Ошибка sync каталога происходит после replace и не запускает cleanup target-а.
#[test]
fn post_rename_failure_reports_durability_unconfirmed_without_rollback() {
    // Fake ломает только последнюю directory-sync стадию.
    let file_system = FaultingFileSystem::new(Some(FaultPoint::SyncDirectory));
    // Virtual path не требует реального filesystem доступа.
    let target_path = Path::new("/virtual/state.json");

    // Rename успевает состояться до injected failure.
    let outcome = replace_file_atomically_with(&file_system, target_path, b"new");
    // Recorded state доказывает отсутствие скрытого rollback.
    let state = file_system.state.borrow();

    // Target уже заменён новым payload.
    assert!(state.target_replaced);
    // Source temp после rename не удаляется вторично.
    assert!(state.removed_paths.is_empty());
    // Directory sync выполняется ровно один раз.
    assert_eq!(state.directory_sync_calls, 1);
    // Typed outcome сохраняет post-rename семантику.
    assert_eq!(
        outcome,
        AtomicFileWriteOutcome::ReplacedDurabilityUnconfirmed(DirectorySyncError::SyncDirectory(
            io::ErrorKind::Other
        ))
    );
}

/// Коллизия пропускается через create_new без wildcard удаления чужого path.
#[test]
fn collision_uses_new_candidate_and_cleanup_never_scans_wildcards() {
    // Первый candidate считается чужой коллизией, rename второго ломается.
    let file_system = FaultingFileSystem::with_collisions(Some(FaultPoint::Rename), 1);
    // Foreign temp похож на generated name, но не был создан попыткой.
    let foreign_temp_path = PathBuf::from("/virtual/.state.json.save-foreign.tmp");
    // Fake хранит foreign path только как доказательство отсутствия wildcard.
    file_system
        .state
        .borrow_mut()
        .foreign_paths
        .push(foreign_temp_path.clone());

    // Attempt должна перейти ко второму candidate.
    let outcome =
        replace_file_atomically_with(&file_system, Path::new("/virtual/state.json"), b"new");
    // Cleanup уже завершён к моменту чтения state.
    let state = file_system.state.borrow();

    // Одна коллизия и один success дают две create_new попытки.
    assert_eq!(state.create_calls, 2);
    // Owned становится только второй успешно созданный path.
    assert_eq!(state.created_paths.len(), 1);
    // Удалён только этот exact owned path.
    assert_eq!(state.removed_paths, state.created_paths);
    // Foreign path не попал в removal log.
    assert!(!state.removed_paths.contains(&foreign_temp_path));
    // Failure остаётся rename-stage.
    assert!(matches!(
        outcome,
        AtomicFileWriteOutcome::NotReplaced(failure)
            if failure.stage == AtomicFileWriteStage::RenameTempFile
    ));
}

/// Bounded collision exhaustion не удаляет ни один не принадлежащий writer-у path.
#[test]
fn collision_exhaustion_is_typed_and_performs_no_cleanup() {
    // Все разрешённые candidate names считаются уже существующими.
    let file_system = FaultingFileSystem::with_collisions(None, 32);

    // Writer обязан остановиться без fallback scan или overwrite.
    let outcome =
        replace_file_atomically_with(&file_system, Path::new("/virtual/state.json"), b"new");
    // State доказывает bounded поведение.
    let state = file_system.state.borrow();

    // Число попыток совпадает с production bound.
    assert_eq!(state.create_calls, 32);
    // Ни один collision path не становится owned.
    assert!(state.created_paths.is_empty());
    // Поэтому cleanup не получает ни одного path.
    assert!(state.removed_paths.is_empty());
    // Exhaustion остаётся отдельной безопасной причиной.
    assert_eq!(
        outcome,
        AtomicFileWriteOutcome::NotReplaced(super::AtomicFileWriteFailure {
            stage: AtomicFileWriteStage::CreateTempFile,
            cause: AtomicFileWriteCause::TempNameAttemptsExhausted,
        })
    );
}

/// Непригодный target отклоняется до create-new side effect.
#[test]
fn invalid_target_path_fails_before_filesystem_access() {
    // Fake не имеет injected I/O failure.
    let file_system = FaultingFileSystem::new(None);

    // Корневой путь не содержит filename.
    let outcome = replace_file_atomically_with(&file_system, Path::new("/"), b"new");
    // Adapter не должен был увидеть ни одной операции.
    let state = file_system.state.borrow();

    // Failure точно указывает validation stage.
    assert_eq!(
        outcome,
        AtomicFileWriteOutcome::NotReplaced(super::AtomicFileWriteFailure {
            stage: AtomicFileWriteStage::ValidateTargetPath,
            cause: AtomicFileWriteCause::Io(io::ErrorKind::InvalidInput),
        })
    );
    // Никакой temp policy не дошло до filesystem.
    assert_eq!(state.create_calls, 0);
}

/// Инъецируемая граница одной physical операции.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaultPoint {
    /// Ошибка атомарного create_new.
    Create,
    /// Ошибка записи payload.
    Write,
    /// Ошибка явного flush.
    Flush,
    /// Ошибка sync temp.
    SyncTemp,
    /// Ошибка atomic rename.
    Rename,
    /// Ошибка post-rename directory sync.
    SyncDirectory,
}

impl FaultPoint {
    /// Соотносит pre-rename fault с ожидаемым public stage.
    fn expected_stage(self) -> AtomicFileWriteStage {
        // Post-rename fault не используется этой матрицей.
        match self {
            Self::Create => AtomicFileWriteStage::CreateTempFile,
            Self::Write => AtomicFileWriteStage::WriteTempFile,
            Self::Flush => AtomicFileWriteStage::FlushTempFile,
            Self::SyncTemp => AtomicFileWriteStage::SyncTempFile,
            Self::Rename => AtomicFileWriteStage::RenameTempFile,
            Self::SyncDirectory => {
                panic!("post-rename fault не имеет NotReplaced stage")
            }
        }
    }
}

/// Общий recorded state fake filesystem и fake temp handle.
#[derive(Default)]
struct FaultingState {
    /// Единственная configured failure stage.
    fault: Option<FaultPoint>,
    /// Число collision responses до successful create.
    remaining_collisions: usize,
    /// Полное число create_new вызовов.
    create_calls: usize,
    /// Пути, ownership которых доказан successful create.
    created_paths: Vec<PathBuf>,
    /// Пути, переданные exact cleanup.
    removed_paths: Vec<PathBuf>,
    /// Foreign temp-looking paths не принадлежат writer-у.
    foreign_paths: Vec<PathBuf>,
    /// Признак successful rename.
    target_replaced: bool,
    /// Число directory sync попыток.
    directory_sync_calls: usize,
    /// Bytes, принятые fake temp handle.
    written_bytes: Vec<u8>,
}

/// Cloneable in-memory filesystem adapter для focused fault tests.
struct FaultingFileSystem {
    /// Rc позволяет temp handle и adapter-у видеть один recorded state.
    state: Rc<RefCell<FaultingState>>,
}

impl FaultingFileSystem {
    /// Создаёт fake без предварительных collision.
    fn new(fault: Option<FaultPoint>) -> Self {
        // Общий constructor держит тестовую policy в одном месте.
        Self::with_collisions(fault, 0)
    }

    /// Создаёт fake с заданным числом atomic create_new collision.
    fn with_collisions(fault: Option<FaultPoint>, remaining_collisions: usize) -> Self {
        // Остальные поля получают явные безопасные defaults.
        Self {
            state: Rc::new(RefCell::new(FaultingState {
                fault,
                remaining_collisions,
                ..FaultingState::default()
            })),
        }
    }
}

impl AtomicFileSystem for FaultingFileSystem {
    /// Temp handle пишет в тот же recorded state.
    type TempFile = FaultingTempFile;

    fn create_new_user_only(&self, temp_path: &Path) -> io::Result<Self::TempFile> {
        // Mutable borrow ограничен текущей операцией.
        let mut state = self.state.borrow_mut();
        // Каждый вызов считается независимо от outcome.
        state.create_calls += 1;
        // Create failure не даёт writer-у ownership candidate-а.
        if state.fault == Some(FaultPoint::Create) {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        // Коллизия не создаёт ownership и возвращает canonical ErrorKind.
        if state.remaining_collisions > 0 {
            state.remaining_collisions -= 1;
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        // Successful create фиксирует exact owned candidate.
        state.created_paths.push(temp_path.to_path_buf());
        // Handle разделяет state после освобождения текущего borrow.
        Ok(FaultingTempFile {
            state: Rc::clone(&self.state),
        })
    }

    fn sync_temp_file(&self, _temp_file: &Self::TempFile) -> io::Result<()> {
        // Injected sync failure происходит до rename.
        if self.state.borrow().fault == Some(FaultPoint::SyncTemp) {
            return Err(io::Error::from(io::ErrorKind::Other));
        }
        // Остальные сценарии подтверждают temp.
        Ok(())
    }

    fn rename(&self, _source_path: &Path, _target_path: &Path) -> io::Result<()> {
        // Rename failure оставляет target untouched.
        if self.state.borrow().fault == Some(FaultPoint::Rename) {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        // Successful rename является единственной target mutation.
        self.state.borrow_mut().target_replaced = true;
        // Fake не моделирует дополнительную filesystem policy.
        Ok(())
    }

    fn sync_parent_directory(&self, _target_path: &Path) -> Result<(), DirectorySyncError> {
        // Счётчик доказывает отдельную post-rename стадию.
        self.state.borrow_mut().directory_sync_calls += 1;
        // Injected failure сохраняет typed directory-sync distinction.
        if self.state.borrow().fault == Some(FaultPoint::SyncDirectory) {
            return Err(DirectorySyncError::SyncDirectory(io::ErrorKind::Other));
        }
        // Durable path подтверждается без дополнительных side effects.
        Ok(())
    }

    fn remove_file(&self, temp_path: &Path) -> io::Result<()> {
        // Removal log принимает только path, переданный RAII guard-ом.
        self.state
            .borrow_mut()
            .removed_paths
            .push(temp_path.to_path_buf());
        // Fake cleanup всегда завершается успешно.
        Ok(())
    }
}

/// Writable fake handle с independent write/flush failures.
struct FaultingTempFile {
    /// Handle разделяет recorded state с filesystem adapter-ом.
    state: Rc<RefCell<FaultingState>>,
}

impl Write for FaultingTempFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        // Write failure проверяет RAII cleanup на первом payload boundary.
        if self.state.borrow().fault == Some(FaultPoint::Write) {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }
        // Successful write сохраняет exact bytes для возможных assertions.
        self.state
            .borrow_mut()
            .written_bytes
            .extend_from_slice(buffer);
        // Полный размер сообщает `write_all`, что весь chunk принят.
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // Flush failure остаётся отдельным public stage.
        if self.state.borrow().fault == Some(FaultPoint::Flush) {
            return Err(io::Error::from(io::ErrorKind::Other));
        }
        // Остальные сценарии подтверждают buffered handoff.
        Ok(())
    }
}
