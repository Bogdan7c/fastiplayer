//! Маленький sidecar последней подтверждённой позиции текущего playlist item.
//!
//! Формат и I/O изолированы от большого `playlist-state.json`: частые timeline
//! checkpoint-ы физически не могут переписать очередь или allocator watermark.

mod worker;

use std::fmt;
use std::path::Path;
use std::time::Duration;

use playlist_core::{PlaylistItemId, PlaylistLocator};
use serde::{Deserialize, Serialize};

use crate::atomic_write::{
    AtomicWriteOutcome, NotReplacedCause, NotReplacedFailure, NotReplacedStage,
    write_serialized_json_atomic,
};
use crate::envelope::scan_envelope;
use crate::identity::{
    OpenSourceError, inspected_identity, metadata_matches, open_regular_nofollow,
};
use crate::locator_fingerprint::{
    UnsupportedNativePathEncoding, decode_lower_hex_32, encode_lower_hex,
    locator_fingerprint_with_domain,
};
use crate::{
    InspectedFileIdentity, PlaylistStateStore, ProtectedStateCause, QuarantineFileName,
    QuarantineOutcome, StateSerializationError,
};

pub use worker::{
    ResumeSaveRevision, ResumeSubmitOutcome, ResumeWorker, ResumeWorkerShutdownOutcome,
    ResumeWorkerStartError, ResumeWriteReport, ResumeWriteSnapshot,
};

/// Стабильное имя маленького sidecar рядом с queue state.
pub const PLAYLIST_RESUME_FILENAME: &str = "playlist-resume.json";

/// Первая строгая schema sidecar-а.
pub const CURRENT_PLAYLIST_RESUME_SCHEMA_VERSION: u64 = 1;

/// Sidecar не должен превращаться в скрытую историю или cache.
const MAX_PLAYLIST_RESUME_BYTES: u64 = 64 * 1024;

/// Envelope budget больше supported v1: так proven v1 oversize остаётся corrupt, а не protected.
const MAX_PLAYLIST_RESUME_ENVELOPE_BYTES: u64 = 128 * 1024;

/// Domain separator не позволяет спутать fingerprint с SHA-256 другого artifact-а.
const LOCATOR_FINGERPRINT_DOMAIN: &[u8] = b"fastiplayer/playlist-resume/locator/v1";

/// Exact checkpoint одного stable item.
#[derive(Clone, PartialEq, Eq)]
pub struct ResumeCheckpoint {
    item_id: PlaylistItemId,
    locator_fingerprint: [u8; 32],
    position: Duration,
}

impl ResumeCheckpoint {
    /// Строит secret-safe correlation из exact locator bytes.
    pub fn for_locator(
        item_id: PlaylistItemId,
        locator: &PlaylistLocator,
        position: Duration,
    ) -> Result<Self, ResumeCheckpointBuildError> {
        Ok(Self {
            item_id,
            locator_fingerprint: locator_fingerprint(locator)?,
            position,
        })
    }

    /// Stable row identity не заменяет fingerprint exact locator-а.
    #[must_use]
    pub const fn item_id(&self) -> PlaylistItemId {
        self.item_id
    }

    /// Возвращает только media time; locator bytes наружу не раскрываются.
    #[must_use]
    pub const fn position(&self) -> Duration {
        self.position
    }

    /// Проверяет обе части correlation и не форматирует secret locator.
    pub fn matches(
        &self,
        item_id: PlaylistItemId,
        locator: &PlaylistLocator,
    ) -> Result<bool, ResumeCheckpointBuildError> {
        Ok(self.item_id == item_id && self.locator_fingerprint == locator_fingerprint(locator)?)
    }

    #[cfg(test)]
    fn fingerprint_hex(&self) -> String {
        encode_lower_hex(&self.locator_fingerprint)
    }
}

impl fmt::Debug for ResumeCheckpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResumeCheckpoint")
            .field("item_id", &self.item_id)
            .field("locator_fingerprint", &"<sha256>")
            .field("position", &self.position)
            .finish()
    }
}

/// Fingerprint может отказаться только от platform-native path без exact encoding API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeCheckpointBuildError {
    UnsupportedNativePathEncoding,
}

impl fmt::Display for ResumeCheckpointBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("exact native path encoding is not available for locator fingerprint")
    }
}

impl std::error::Error for ResumeCheckpointBuildError {}

/// Corrupt v1 можно quarantine-ить независимо от большой очереди.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeCorruptCause {
    SupportedFileTooLarge,
    InvalidV1Payload,
    InvalidDomainValue,
}

/// Read-only inspection сохраняет protected/newer distinction.
#[derive(Debug)]
pub enum ResumeInspectionOutcome {
    Missing,
    Loaded(Option<ResumeCheckpoint>),
    CorruptNeedsQuarantine {
        inspected_identity: InspectedFileIdentity,
        cause: ResumeCorruptCause,
    },
    NewerSchemaSaveBlocked {
        schema_version: u64,
    },
    ProtectedSaveBlocked {
        cause: ProtectedStateCause,
    },
}

/// Process-local serialized owner sidecar path, inspection, quarantine и atomic replace.
pub struct PlaylistResumeStore {
    operations: PlaylistStateStore,
}

impl PlaylistResumeStore {
    /// Caller передаёт trusted exact path из `ConfigPaths`.
    pub fn new(resume_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            operations: PlaylistStateStore::new(resume_path),
        }
    }

    /// Возвращает exact target только для diagnostics/tests без чтения содержимого.
    #[must_use]
    pub fn resume_path(&self) -> &Path {
        self.operations.state_path()
    }

    /// Строгая inspection не меняет source.
    #[must_use]
    pub fn inspect(&self) -> ResumeInspectionOutcome {
        let _operation_guard = match self.operations.lock_operations() {
            Ok(guard) => guard,
            Err(()) => {
                return ResumeInspectionOutcome::ProtectedSaveBlocked {
                    cause: ProtectedStateCause::ReadFailed(std::io::ErrorKind::Other),
                };
            }
        };
        inspect_resume(self.resume_path())
    }

    /// Explicit quarantine повторно проверяет identity под тем же operation lock.
    pub fn apply_quarantine(
        &self,
        inspected_identity: &InspectedFileIdentity,
        quarantine_file_name: &QuarantineFileName,
    ) -> QuarantineOutcome {
        self.operations
            .apply_quarantine(inspected_identity, quarantine_file_name)
    }

    /// Один immutable snapshot превращается ровно в один atomic replace.
    fn write_snapshot(&self, snapshot: &ResumeWriteSnapshot) -> AtomicWriteOutcome {
        let serialized_json = match serialize_resume(snapshot.checkpoint()) {
            Ok(bytes) => bytes,
            Err(()) => {
                return AtomicWriteOutcome::NotReplaced(NotReplacedFailure {
                    stage: NotReplacedStage::Serialize,
                    cause: NotReplacedCause::Serialization(
                        StateSerializationError::JsonEncodingFailed,
                    ),
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
        write_serialized_json_atomic(self.resume_path(), &serialized_json)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct RequiredNullable<T>(Option<T>);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeFileV1 {
    schema_version: u64,
    checkpoint: RequiredNullable<ResumeCheckpointV1>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeCheckpointV1 {
    item_id: u64,
    locator_fingerprint_sha256: String,
    position: ResumePositionV1,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumePositionV1 {
    seconds: u64,
    nanoseconds: u32,
}

fn serialize_resume(checkpoint: Option<&ResumeCheckpoint>) -> Result<Vec<u8>, ()> {
    let checkpoint = checkpoint.map(|checkpoint| ResumeCheckpointV1 {
        item_id: checkpoint.item_id.expose_value_for_persistence(),
        locator_fingerprint_sha256: encode_lower_hex(&checkpoint.locator_fingerprint),
        position: ResumePositionV1 {
            seconds: checkpoint.position.as_secs(),
            nanoseconds: checkpoint.position.subsec_nanos(),
        },
    });
    let mut bytes = serde_json::to_vec_pretty(&ResumeFileV1 {
        schema_version: CURRENT_PLAYLIST_RESUME_SCHEMA_VERSION,
        checkpoint: RequiredNullable(checkpoint),
    })
    .map_err(|_| ())?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn inspect_resume(resume_path: &Path) -> ResumeInspectionOutcome {
    let mut source = match open_regular_nofollow(resume_path) {
        Ok(source) => source,
        Err(OpenSourceError::Missing) => return ResumeInspectionOutcome::Missing,
        Err(OpenSourceError::NotRegularFile) => {
            return ResumeInspectionOutcome::ProtectedSaveBlocked {
                cause: ProtectedStateCause::SourceIsNotRegularFile,
            };
        }
        Err(OpenSourceError::Io(error_kind)) => {
            return ResumeInspectionOutcome::ProtectedSaveBlocked {
                cause: ProtectedStateCause::ReadFailed(error_kind),
            };
        }
    };
    let metadata_before = match source.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            return ResumeInspectionOutcome::ProtectedSaveBlocked {
                cause: ProtectedStateCause::ReadFailed(error.kind()),
            };
        }
    };
    let proof = match scan_envelope(&mut source, MAX_PLAYLIST_RESUME_ENVELOPE_BYTES) {
        Ok(proof) => proof,
        Err(cause) => return ResumeInspectionOutcome::ProtectedSaveBlocked { cause },
    };
    let metadata_after = match source.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            return ResumeInspectionOutcome::ProtectedSaveBlocked {
                cause: ProtectedStateCause::ReadFailed(error.kind()),
            };
        }
    };
    let identity_before = inspected_identity(&metadata_before, proof.content_sha256);
    if !metadata_matches(&identity_before, &metadata_after) {
        return ResumeInspectionOutcome::ProtectedSaveBlocked {
            cause: ProtectedStateCause::InvalidEnvelope,
        };
    }
    if proof.schema_version > CURRENT_PLAYLIST_RESUME_SCHEMA_VERSION {
        return ResumeInspectionOutcome::NewerSchemaSaveBlocked {
            schema_version: proof.schema_version,
        };
    }
    if proof.schema_version != CURRENT_PLAYLIST_RESUME_SCHEMA_VERSION {
        return ResumeInspectionOutcome::ProtectedSaveBlocked {
            cause: ProtectedStateCause::UnsupportedSchemaVersion,
        };
    }

    let identity = inspected_identity(&metadata_after, proof.content_sha256);
    if metadata_before.len() > MAX_PLAYLIST_RESUME_BYTES {
        return ResumeInspectionOutcome::CorruptNeedsQuarantine {
            inspected_identity: identity,
            cause: ResumeCorruptCause::SupportedFileTooLarge,
        };
    }
    let required_checkpoint_is_present =
        serde_json::from_slice::<serde_json::Value>(&proof.inspected_bytes)
            .ok()
            .and_then(|value| {
                value
                    .as_object()
                    .map(|object| object.contains_key("checkpoint"))
            })
            .unwrap_or(false);
    if !required_checkpoint_is_present {
        return ResumeInspectionOutcome::CorruptNeedsQuarantine {
            inspected_identity: identity,
            cause: ResumeCorruptCause::InvalidV1Payload,
        };
    }
    let file: ResumeFileV1 = match serde_json::from_slice(&proof.inspected_bytes) {
        Ok(file) => file,
        Err(_) => {
            return ResumeInspectionOutcome::CorruptNeedsQuarantine {
                inspected_identity: identity,
                cause: ResumeCorruptCause::InvalidV1Payload,
            };
        }
    };
    match restore_checkpoint(file.checkpoint.0) {
        Ok(checkpoint) => ResumeInspectionOutcome::Loaded(checkpoint),
        Err(()) => ResumeInspectionOutcome::CorruptNeedsQuarantine {
            inspected_identity: identity,
            cause: ResumeCorruptCause::InvalidDomainValue,
        },
    }
}

fn restore_checkpoint(dto: Option<ResumeCheckpointV1>) -> Result<Option<ResumeCheckpoint>, ()> {
    let Some(dto) = dto else {
        return Ok(None);
    };
    if dto.position.nanoseconds >= 1_000_000_000 {
        return Err(());
    }
    let item_id = PlaylistItemId::from_persistence_value(dto.item_id).map_err(|_| ())?;
    let locator_fingerprint = decode_lower_hex_32(&dto.locator_fingerprint_sha256).ok_or(())?;
    Ok(Some(ResumeCheckpoint {
        item_id,
        locator_fingerprint,
        position: Duration::new(dto.position.seconds, dto.position.nanoseconds),
    }))
}

/// Resume-fingerprint: общий кодировщик локатора со своим domain separator.
fn locator_fingerprint(locator: &PlaylistLocator) -> Result<[u8; 32], ResumeCheckpointBuildError> {
    locator_fingerprint_with_domain(LOCATOR_FINGERPRINT_DOMAIN, locator).map_err(
        |UnsupportedNativePathEncoding| ResumeCheckpointBuildError::UnsupportedNativePathEncoding,
    )
}

#[cfg(test)]
mod tests;
