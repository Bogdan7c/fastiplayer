use std::fmt;
use std::path::PathBuf;
use std::time::SystemTime;

use playlist_core::{PlaylistQueue, RepeatMode};

/// Immutable persistence view одной согласованной domain revision.
#[derive(Clone, Copy)]
pub struct PlaylistStateSnapshot<'queue> {
    queue: &'queue PlaylistQueue,
    repeat_mode: RepeatMode,
}

impl<'queue> PlaylistStateSnapshot<'queue> {
    /// Связывает queue snapshot и runtime repeat mode в один save intent.
    pub const fn new(queue: &'queue PlaylistQueue, repeat_mode: RepeatMode) -> Self {
        Self { queue, repeat_mode }
    }

    /// Возвращает canonical queue без раскрытия её storage.
    pub(crate) const fn queue(self) -> &'queue PlaylistQueue {
        self.queue
    }

    /// Возвращает exact persisted repeat mode.
    pub(crate) const fn repeat_mode(self) -> RepeatMode {
        self.repeat_mode
    }
}

impl fmt::Debug for PlaylistStateSnapshot<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistStateSnapshot")
            .field("queue", self.queue)
            .field("repeat_mode", &self.repeat_mode)
            .finish()
    }
}

/// Полностью validated state, готовый к app-owned apply decision.
pub struct LoadedPlaylistState {
    queue: PlaylistQueue,
    repeat_mode: RepeatMode,
}

impl LoadedPlaylistState {
    /// Создаётся только после successful DTO→domain mapping.
    pub(crate) const fn new(queue: PlaylistQueue, repeat_mode: RepeatMode) -> Self {
        Self { queue, repeat_mode }
    }

    /// Read-only доступ к восстановленной canonical queue.
    pub const fn queue(&self) -> &PlaylistQueue {
        &self.queue
    }

    /// Exact persisted repeat mode без config-default repair.
    pub const fn repeat_mode(&self) -> RepeatMode {
        self.repeat_mode
    }

    /// Передаёт domain ownership app/controller после load decision.
    pub fn into_parts(self) -> (PlaylistQueue, RepeatMode) {
        (self.queue, self.repeat_mode)
    }
}

impl fmt::Debug for LoadedPlaylistState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadedPlaylistState")
            .field("queue", &self.queue)
            .field("repeat_mode", &self.repeat_mode)
            .finish()
    }
}

/// Platform file identity, доступная текущей std/OS boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlatformFileId {
    /// Unix device+inode pair из metadata открытого handle.
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    /// Текущая платформа не предоставляет используемый stable file ID.
    #[cfg(not(unix))]
    Unavailable,
}

/// Classification exact path entry, полученная без symlink follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InspectedSourceClassification {
    /// Source path и открытый handle подтверждены как regular file.
    NoFollowRegularFile,
}

/// Identity exact bytes, которые были inspected read-only.
#[derive(Clone, PartialEq, Eq)]
pub struct InspectedFileIdentity {
    pub(crate) classification: InspectedSourceClassification,
    pub(crate) platform_file_id: PlatformFileId,
    pub(crate) length_bytes: u64,
    pub(crate) modified_at: Option<SystemTime>,
    pub(crate) content_sha256: [u8; 32],
}

impl fmt::Debug for InspectedFileIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InspectedFileIdentity")
            .field("classification", &self.classification)
            .field("platform_file_id", &self.platform_file_id)
            .field("length_bytes", &self.length_bytes)
            .field("modified_at", &self.modified_at)
            .field("content_digest", &"sha256:<redacted>")
            .finish()
    }
}

/// Supported-v1 причина, после которой app может явно запросить quarantine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorruptStateCause {
    /// Полный v1 file превышает отдельный DTO budget.
    SupportedFileTooLarge,
    /// JSON v1 не соответствует строгой disk DTO форме.
    InvalidV1Payload,
    /// Variable-size поле нарушает именованный resource limit.
    ResourceLimitExceeded,
    /// DTO невозможно преобразовать в domain locator/cache/time types.
    InvalidDomainValue,
    /// Queue allocator/current/capacity invariant отклонён `playlist-core`.
    InvalidQueueState,
    /// Shuffle references/cursor/upcoming invariant отклонён `playlist-core`.
    InvalidShuffleTraversal,
}

/// Причина protected no-touch исхода, где destructive recovery запрещён.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtectedStateCause {
    /// Источник не является no-follow regular file.
    SourceIsNotRegularFile,
    /// Файл нельзя безопасно открыть или прочитать.
    ReadFailed(std::io::ErrorKind),
    /// Hard envelope budget закончился до полного uniqueness proof.
    EnvelopeBudgetExhausted,
    /// Top-level object либо JSON envelope синтаксически не доказан.
    InvalidEnvelope,
    /// Обязательный top-level key отсутствует.
    MissingSchemaVersion,
    /// Значение schema_version не является non-negative integer.
    NonIntegerSchemaVersion,
    /// Top-level schema_version встречается больше одного раза.
    DuplicateSchemaVersion,
    /// Integer version не поддерживается и не является newer schema.
    UnsupportedSchemaVersion,
}

/// Результат read-only inspection; ни один variant не меняет filesystem.
pub enum InspectionOutcome {
    /// State file отсутствует: это нормальная новая lineage.
    Missing,
    /// Supported state полностью validated и готов к app decision.
    Loaded(LoadedPlaylistState),
    /// Supported-v1 source можно quarantine-ить только отдельным вызовом.
    CorruptNeedsQuarantine {
        /// Identity inspected bytes для последующей revalidation.
        inspected_identity: InspectedFileIdentity,
        /// Privacy-safe категория corruption.
        cause: CorruptStateCause,
    },
    /// Более новая schema остаётся на месте и блокирует save.
    NewerSchemaSaveBlocked {
        /// Доказанная top-level schema version.
        schema_version: u64,
    },
    /// Версия/источник не доказаны безопасно; любые writes запрещены.
    UnrecognizedVersionSaveBlocked {
        /// Privacy-safe protected classification.
        cause: ProtectedStateCause,
    },
}

impl fmt::Debug for InspectionOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("Missing"),
            Self::Loaded(state) => formatter.debug_tuple("Loaded").field(state).finish(),
            Self::CorruptNeedsQuarantine {
                inspected_identity,
                cause,
            } => formatter
                .debug_struct("CorruptNeedsQuarantine")
                .field("inspected_identity", inspected_identity)
                .field("cause", cause)
                .finish(),
            Self::NewerSchemaSaveBlocked { schema_version } => formatter
                .debug_struct("NewerSchemaSaveBlocked")
                .field("schema_version", schema_version)
                .finish(),
            Self::UnrecognizedVersionSaveBlocked { cause } => formatter
                .debug_struct("UnrecognizedVersionSaveBlocked")
                .field("cause", cause)
                .finish(),
        }
    }
}

/// Privacy-safe failure сериализации domain snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateSerializationError {
    /// Compound storage ожидает schema v2 и не может быть losslessly flattened в v1.
    CompoundQueueRequiresSchemaV2,
    /// Domain time не помещается в canonical v1 seconds+nanos representation.
    TimeOutOfRange,
    /// Native path encoding недоступен для текущей target platform.
    UnsupportedNativePathEncoding,
    /// Snapshot нарушает v1 variable-size budget.
    ResourceLimitExceeded,
    /// Готовый JSON превышает supported-v1 file budget.
    SerializedStateTooLarge,
    /// serde_json не смог записать private DTO.
    JsonEncodingFailed,
}

impl fmt::Display for StateSerializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::CompoundQueueRequiresSchemaV2 => {
                "compound playlist нельзя без потерь сохранить в schema v1"
            }
            Self::TimeOutOfRange => "playlist state time не помещается в schema v1",
            Self::UnsupportedNativePathEncoding => {
                "native path encoding не поддержан schema v1 на этой платформе"
            }
            Self::ResourceLimitExceeded => "playlist state превышает resource limit schema v1",
            Self::SerializedStateTooLarge => "playlist state превышает file limit schema v1",
            Self::JsonEncodingFailed => "playlist state не удалось сериализовать в JSON",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for StateSerializationError {}

/// Причина quarantine failure, которая оставляет save заблокированным.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuarantineFailureCause {
    /// Generated quarantine name уже существует и не перезаписывается.
    DestinationAlreadyExists,
    /// Source нельзя повторно открыть/прочитать безопасно.
    RevalidationReadFailed(std::io::ErrorKind),
    /// Rename/link-unlink primitive завершился ошибкой.
    MoveFailed(std::io::ErrorKind),
    /// Caller передал имя, которое не является одним filename component.
    InvalidQuarantineFileName,
}

/// Результат отдельного serialized quarantine policy action.
#[derive(Debug)]
pub enum QuarantineOutcome {
    /// Matching source перенесён без collision overwrite.
    Applied {
        /// Новый путь нужен app для user-visible recovery warning.
        quarantine_path: PathBuf,
    },
    /// Source identity/content изменились после inspection; ничего не переименовано.
    SourceChanged,
    /// Recovery не завершена, поэтому writer обязан остаться заблокированным.
    FailedSaveBlocked {
        /// Privacy-safe failure category.
        cause: QuarantineFailureCause,
    },
}
