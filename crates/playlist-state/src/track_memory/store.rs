//! Файловый владелец `track-memory.json`: инспекция, карантин и атомарная запись.
//!
//! Семантика зеркалит resume-sidecar: чтение ничего не меняет, повреждённый файл переносится
//! в карантин отдельным явным вызовом, файл более новой схемы не перезаписывается.

use std::path::{Path, PathBuf};

use crate::atomic_write::{
    AtomicWriteOutcome, NotReplacedCause, NotReplacedFailure, NotReplacedStage,
    write_serialized_json_atomic,
};
use crate::envelope::scan_envelope;
use crate::identity::{
    OpenSourceError, inspected_identity, metadata_matches, open_regular_nofollow,
};
use crate::{
    InspectedFileIdentity, PlaylistStateStore, ProtectedStateCause, QuarantineFileName,
    QuarantineOutcome,
};

use super::dto::{
    CURRENT_TRACK_MEMORY_SCHEMA_VERSION, InvalidTrackMemoryPayload,
    OLDEST_SUPPORTED_TRACK_MEMORY_SCHEMA_VERSION, restore_track_memory, serialize_track_memory,
};
use super::memory::{TrackMemory, TrackMemoryCapacity};

/// Стабильное имя файла рядом с `config.toml`.
pub const TRACK_MEMORY_FILENAME: &str = "track-memory.json";

/// Предел поддерживаемого файла.
///
/// Запись в pretty JSON занимает обычно 0.5-1 KiB (отпечаток 64 символа, две-три подсказки,
/// путь), то есть жёсткий потолок в 10 000 записей — около 7-10 MiB. 16 MiB даёт запас
/// ~1.6x, при этом файл остаётся «человеческим» для проверки. Запись с рекордно длинными
/// полями у всех 10 000 видений не влезет — тогда `write` честно отказывает
/// (`SerializedStateTooLarge`), а не пишет файл, который потом сочтут повреждённым.
pub const MAX_TRACK_MEMORY_BYTES: u64 = 16 * 1024 * 1024;

/// Бюджет доказательства конверта больше поддерживаемого: так v1-файл сверх лимита считается
/// повреждённым (карантин), а не «защищённым», и пользователь не теряет возможность сохранять.
const MAX_TRACK_MEMORY_ENVELOPE_BYTES: u64 = 2 * MAX_TRACK_MEMORY_BYTES;

/// Почему файл v1 признан повреждённым.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackMemoryCorruptCause {
    /// Файл больше [`MAX_TRACK_MEMORY_BYTES`].
    SupportedFileTooLarge,
    /// Не разбирается как схема v1 (синтаксис, неизвестные поля, неверные типы).
    InvalidV1Payload,
    /// Схема соблюдена, но значения нарушают инварианты (диапазон, длина, дубликаты).
    InvalidDomainValue,
    /// Файл вообще не похож на память выбора: не JSON-объект либо нет доказуемого
    /// `schema_version`. Для этого хранилища (удобство, не ценные данные) такой файл
    /// переносится в карантин, а не блокирует память навсегда.
    UnreadableEnvelope,
}

/// Итог чтения без побочных эффектов.
#[derive(Debug)]
pub enum TrackMemoryInspection {
    /// Файла нет: пустая память, запись создаст его.
    Missing,
    /// Файл прочитан и проверен.
    Loaded(TrackMemory),
    /// Файл повреждён: нужен карантин, после него можно писать заново.
    CorruptNeedsQuarantine {
        inspected_identity: InspectedFileIdentity,
        cause: TrackMemoryCorruptCause,
    },
    /// Файл новее этой версии: перезаписывать нельзя, сессия работает с пустой памятью.
    NewerSchemaSaveBlocked { schema_version: u64 },
    /// Файл нельзя безопасно прочитать/распознать: запись запрещена, чтобы ничего не затереть.
    ProtectedSaveBlocked { cause: ProtectedStateCause },
}

impl TrackMemoryInspection {
    /// Можно ли сразу писать в файл. После `CorruptNeedsQuarantine` — только после успешного
    /// карантина.
    #[must_use]
    pub const fn allows_saving_immediately(&self) -> bool {
        matches!(self, Self::Missing | Self::Loaded(_))
    }
}

/// Процессный владелец пути, инспекции, карантина и атомарной замены.
pub struct TrackMemoryStore {
    operations: PlaylistStateStore,
}

impl TrackMemoryStore {
    /// Путь приходит из `ConfigPaths::track_memory_file()`.
    pub fn new(track_memory_path: impl Into<PathBuf>) -> Self {
        Self {
            operations: PlaylistStateStore::new(track_memory_path),
        }
    }

    /// Точный путь (для диагностики и тестов; содержимое не читается).
    #[must_use]
    pub fn track_memory_path(&self) -> &Path {
        self.operations.state_path()
    }

    /// Читает и строго проверяет файл. `capacity` — ёмкость создаваемой памяти: записи сверх
    /// неё (например, после уменьшения настройки) сразу вытесняются, самые давние первыми.
    #[must_use]
    pub fn inspect(&self, capacity: TrackMemoryCapacity) -> TrackMemoryInspection {
        let _operation_guard = match self.operations.lock_operations() {
            Ok(guard) => guard,
            Err(()) => {
                return TrackMemoryInspection::ProtectedSaveBlocked {
                    cause: ProtectedStateCause::ReadFailed(std::io::ErrorKind::Other),
                };
            }
        };
        inspect_track_memory(self.track_memory_path(), capacity)
    }

    /// Явный карантин: повторно проверяет личность файла под тем же замком.
    pub fn apply_quarantine(
        &self,
        inspected_identity: &InspectedFileIdentity,
        quarantine_file_name: &QuarantineFileName,
    ) -> QuarantineOutcome {
        self.operations
            .apply_quarantine(inspected_identity, quarantine_file_name)
    }

    /// Один снимок памяти — одна атомарная замена файла (права 0600).
    ///
    /// Не знает, безопасно ли писать: решение о блокировке принимает владелец по итогу
    /// [`Self::inspect`] (см. [`TrackMemoryInspection::allows_saving_immediately`]).
    pub fn write(&self, memory: &TrackMemory) -> AtomicWriteOutcome {
        let serialized_json = match serialize_track_memory(memory, MAX_TRACK_MEMORY_BYTES) {
            Ok(bytes) => bytes,
            Err(error) => {
                return AtomicWriteOutcome::NotReplaced(NotReplacedFailure {
                    stage: NotReplacedStage::Serialize,
                    cause: NotReplacedCause::Serialization(error),
                });
            }
        };
        let _operation_guard = match self.operations.lock_operations() {
            Ok(guard) => guard,
            Err(()) => {
                return AtomicWriteOutcome::NotReplaced(NotReplacedFailure {
                    stage: NotReplacedStage::LockStore,
                    cause: NotReplacedCause::Io(std::io::ErrorKind::Other),
                });
            }
        };
        write_serialized_json_atomic(self.track_memory_path(), &serialized_json)
    }
}

fn protected(cause: ProtectedStateCause) -> TrackMemoryInspection {
    TrackMemoryInspection::ProtectedSaveBlocked { cause }
}

/// Причины, по которым у файла нет доказуемого конверта: это мусор, а не чужая схема.
const fn envelope_is_unprovable_garbage(cause: ProtectedStateCause) -> bool {
    matches!(
        cause,
        ProtectedStateCause::InvalidEnvelope
            | ProtectedStateCause::MissingSchemaVersion
            | ProtectedStateCause::NonIntegerSchemaVersion
            | ProtectedStateCause::DuplicateSchemaVersion
    )
}

/// Файл без доказуемого конверта: строим его личность (SHA-256 всего содержимого) и отдаём
/// на карантин. Если прочитать целиком не удалось или файл менялся под руками — остаётся
/// защита от записи (`ProtectedSaveBlocked`), как раньше.
fn quarantine_or_protect_unreadable(
    source: &mut std::fs::File,
    metadata_before: &std::fs::Metadata,
    cause: ProtectedStateCause,
) -> TrackMemoryInspection {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom};

    if metadata_before.len() > MAX_TRACK_MEMORY_ENVELOPE_BYTES
        || source.seek(SeekFrom::Start(0)).is_err()
    {
        return protected(cause);
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    let mut total: u64 = 0;
    loop {
        match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                total += read as u64;
                if total > MAX_TRACK_MEMORY_ENVELOPE_BYTES {
                    return protected(cause);
                }
                hasher.update(&buffer[..read]);
            }
            Err(error) => return protected(ProtectedStateCause::ReadFailed(error.kind())),
        }
    }
    let Ok(metadata_after) = source.metadata() else {
        return protected(cause);
    };
    let identity_before = inspected_identity(metadata_before, hasher.clone().finalize().into());
    if !metadata_matches(&identity_before, &metadata_after) {
        return protected(cause);
    }
    TrackMemoryInspection::CorruptNeedsQuarantine {
        inspected_identity: identity_before,
        cause: TrackMemoryCorruptCause::UnreadableEnvelope,
    }
}

fn inspect_track_memory(path: &Path, capacity: TrackMemoryCapacity) -> TrackMemoryInspection {
    let mut source = match open_regular_nofollow(path) {
        Ok(source) => source,
        Err(OpenSourceError::Missing) => return TrackMemoryInspection::Missing,
        Err(OpenSourceError::NotRegularFile) => {
            return protected(ProtectedStateCause::SourceIsNotRegularFile);
        }
        Err(OpenSourceError::Io(error_kind)) => {
            return protected(ProtectedStateCause::ReadFailed(error_kind));
        }
    };
    let metadata_before = match source.metadata() {
        Ok(metadata) => metadata,
        Err(error) => return protected(ProtectedStateCause::ReadFailed(error.kind())),
    };
    let proof = match scan_envelope(&mut source, MAX_TRACK_MEMORY_ENVELOPE_BYTES) {
        Ok(proof) => proof,
        Err(cause) if envelope_is_unprovable_garbage(cause) => {
            return quarantine_or_protect_unreadable(&mut source, &metadata_before, cause);
        }
        Err(cause) => return protected(cause),
    };
    let metadata_after = match source.metadata() {
        Ok(metadata) => metadata,
        Err(error) => return protected(ProtectedStateCause::ReadFailed(error.kind())),
    };
    // Файл не должен меняться, пока мы его читали: иначе доказательство не относится к байтам.
    let identity_before = inspected_identity(&metadata_before, proof.content_sha256);
    if !metadata_matches(&identity_before, &metadata_after) {
        return protected(ProtectedStateCause::InvalidEnvelope);
    }
    if proof.schema_version > CURRENT_TRACK_MEMORY_SCHEMA_VERSION {
        return TrackMemoryInspection::NewerSchemaSaveBlocked {
            schema_version: proof.schema_version,
        };
    }
    if proof.schema_version < OLDEST_SUPPORTED_TRACK_MEMORY_SCHEMA_VERSION {
        return protected(ProtectedStateCause::UnsupportedSchemaVersion);
    }

    let identity = inspected_identity(&metadata_after, proof.content_sha256);
    if metadata_before.len() > MAX_TRACK_MEMORY_BYTES {
        return TrackMemoryInspection::CorruptNeedsQuarantine {
            inspected_identity: identity,
            cause: TrackMemoryCorruptCause::SupportedFileTooLarge,
        };
    }
    match restore_track_memory(&proof.inspected_bytes, capacity) {
        Ok(memory) => TrackMemoryInspection::Loaded(memory),
        Err(invalid) => TrackMemoryInspection::CorruptNeedsQuarantine {
            inspected_identity: identity,
            cause: match invalid {
                InvalidTrackMemoryPayload::Schema => TrackMemoryCorruptCause::InvalidV1Payload,
                InvalidTrackMemoryPayload::DomainValue => {
                    TrackMemoryCorruptCause::InvalidDomainValue
                }
            },
        },
    }
}
