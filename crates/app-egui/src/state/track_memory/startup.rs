//! Запуск памяти выбора процесса: чтение файла, карантин повреждённого, фоновая запись.
//!
//! Выполняется один раз при сборке `AppShell` (маленький файл, ограниченный размером; та же
//! схема, что у чтения config и resume-checkpoint-а). Результат — общий для всех `AppState`
//! процесса [`TrackMemoryHandle`]: пересоздание `AppState` после suspend не теряет память
//! и не перечитывает файл.
//!
//! # Исходы (типизированно, чтобы тесты и лог различали случаи)
//! - файла нет / прочитан — обычная работа;
//! - файл повреждён — карантин (`track-memory.corrupt-<время>.json`), чистая память,
//!   предупреждение в лог БЕЗ путей; файл пересоздастся при первом изменении;
//! - файл новее программы или небезопасен — память работает только в оперативке, файл не
//!   трогается;
//! - поток записи не запустился — тоже только оперативка.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use playlist_state::{
    QuarantineFileName, QuarantineOutcome, TrackMemory, TrackMemoryCapacity,
    TrackMemoryCorruptCause, TrackMemoryInspection, TrackMemoryStore, TrackMemoryWriter,
};

use super::owner::{Persistence, TrackMemoryOwner, WritingState};

/// Общий доступ к владельцу памяти из `AppShell` (запуск, выход) и `AppState` (работа).
///
/// Все обращения — из потока интерфейса, поэтому замок не конкурирует; отравленный замок
/// (паника в чужом коде) не должен ронять плеер, данные памяти при этом остаются целыми.
#[derive(Clone)]
pub(crate) struct TrackMemoryHandle(Arc<Mutex<TrackMemoryOwner>>);

impl TrackMemoryHandle {
    pub(crate) fn new(owner: TrackMemoryOwner) -> Self {
        Self(Arc::new(Mutex::new(owner)))
    }

    /// Доступ к владельцу.
    pub(crate) fn lock(&self) -> MutexGuard<'_, TrackMemoryOwner> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Итог запуска памяти (для лога и тестов).
#[derive(Debug)]
pub(crate) enum TrackMemoryStartOutcome {
    /// Файла не было: память пуста, файл появится при первом изменении.
    StartedEmpty,
    /// Файл прочитан.
    Loaded { remembered_videos: usize },
    /// Файл был повреждён и убран в карантин; память пуста.
    RecoveredFromCorruptFile { cause: TrackMemoryCorruptCause },
    /// Файл повреждён, но карантин не удался: файл не трогаем, память только в оперативке.
    QuarantineFailed {
        cause: TrackMemoryCorruptCause,
        quarantine: QuarantineOutcome,
    },
    /// Файл от более новой версии: не перезаписываем.
    NewerSchema { schema_version: u64 },
    /// Файл небезопасно читать или распознать: не перезаписываем.
    FileProtected,
    /// Поток записи не запустился: память только в оперативке.
    WriterUnavailable { kind: std::io::ErrorKind },
}

/// Запускает память выбора и пишет предупреждения (без путей).
pub(crate) fn start_track_memory(
    store: Arc<TrackMemoryStore>,
    capacity: TrackMemoryCapacity,
    now: SystemTime,
) -> (TrackMemoryHandle, TrackMemoryStartOutcome) {
    let (memory, writes_allowed, outcome) = inspect_and_recover(&store, capacity, now);
    let (persistence, outcome) = if writes_allowed {
        match TrackMemoryWriter::start(Arc::clone(&store)) {
            Ok(writer) => (
                Persistence::Writing(WritingState {
                    writer,
                    next_revision: 1,
                    save_due_at: None,
                    last_logged_revision: None,
                }),
                outcome,
            ),
            Err(error) => (
                Persistence::InMemoryOnly,
                TrackMemoryStartOutcome::WriterUnavailable { kind: error.0 },
            ),
        }
    } else {
        (Persistence::InMemoryOnly, outcome)
    };
    log_start_outcome(&outcome);
    (
        TrackMemoryHandle::new(TrackMemoryOwner::new(memory, persistence)),
        outcome,
    )
}

/// Читает файл и при необходимости убирает повреждённый в карантин.
/// Возвращает память, разрешена ли запись и исход.
fn inspect_and_recover(
    store: &TrackMemoryStore,
    capacity: TrackMemoryCapacity,
    now: SystemTime,
) -> (TrackMemory, bool, TrackMemoryStartOutcome) {
    match store.inspect(capacity) {
        TrackMemoryInspection::Missing => (
            TrackMemory::new(capacity),
            true,
            TrackMemoryStartOutcome::StartedEmpty,
        ),
        TrackMemoryInspection::Loaded(memory) => {
            let remembered_videos = memory.len();
            (
                memory,
                true,
                TrackMemoryStartOutcome::Loaded { remembered_videos },
            )
        }
        TrackMemoryInspection::CorruptNeedsQuarantine {
            inspected_identity,
            cause,
        } => {
            let quarantine = store.apply_quarantine(
                &inspected_identity,
                &QuarantineFileName::track_memory_from_timestamp(now),
            );
            match quarantine {
                QuarantineOutcome::Applied { .. } => (
                    TrackMemory::new(capacity),
                    true,
                    TrackMemoryStartOutcome::RecoveredFromCorruptFile { cause },
                ),
                QuarantineOutcome::SourceChanged | QuarantineOutcome::FailedSaveBlocked { .. } => (
                    TrackMemory::new(capacity),
                    false,
                    TrackMemoryStartOutcome::QuarantineFailed { cause, quarantine },
                ),
            }
        }
        TrackMemoryInspection::NewerSchemaSaveBlocked { schema_version } => (
            TrackMemory::new(capacity),
            false,
            TrackMemoryStartOutcome::NewerSchema { schema_version },
        ),
        TrackMemoryInspection::ProtectedSaveBlocked { .. } => (
            TrackMemory::new(capacity),
            false,
            TrackMemoryStartOutcome::FileProtected,
        ),
    }
}

/// Предупреждения запуска. Пути не пишем: только причина.
fn log_start_outcome(outcome: &TrackMemoryStartOutcome) {
    match outcome {
        TrackMemoryStartOutcome::StartedEmpty => {
            tracing::debug!("No track memory file yet; starting empty");
        }
        TrackMemoryStartOutcome::Loaded { remembered_videos } => {
            tracing::debug!(remembered_videos, "Track memory loaded");
        }
        TrackMemoryStartOutcome::RecoveredFromCorruptFile { cause } => {
            tracing::warn!(
                ?cause,
                "Corrupted track memory was set aside; starting with an empty memory"
            );
        }
        TrackMemoryStartOutcome::QuarantineFailed { cause, quarantine } => {
            tracing::warn!(
                ?cause,
                ?quarantine,
                "Corrupted track memory could not be set aside; it stays untouched and choices are kept in memory only"
            );
        }
        TrackMemoryStartOutcome::NewerSchema { schema_version } => {
            tracing::warn!(
                schema_version,
                "Track memory was written by a newer version; choices are kept in memory only"
            );
        }
        TrackMemoryStartOutcome::FileProtected => {
            tracing::warn!(
                "Track memory file cannot be read safely; choices are kept in memory only"
            );
        }
        TrackMemoryStartOutcome::WriterUnavailable { kind } => {
            tracing::warn!(
                ?kind,
                "Track memory writer did not start; choices are kept in memory only"
            );
        }
    }
}
