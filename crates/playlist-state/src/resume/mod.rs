//! Маленький sidecar последней подтверждённой позиции текущего playlist item.
//!
//! Формат и I/O изолированы от большого `playlist-state.json`: частые timeline
//! checkpoint-ы физически не могут переписать очередь или allocator watermark.

mod worker;

use std::fmt;
use std::path::Path;
use std::time::Duration;

use playlist_core::{
    ForeignPathEncoding, ForeignPathPlatform, LocalLocator, PlaylistItemId, PlaylistLocator,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
const LOCATOR_FINGERPRINT_DOMAIN: &[u8] = b"rustiplayer/playlist-resume/locator/v1";

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
        formatter.write_str("exact native path encoding недоступен для locator fingerprint")
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

fn locator_fingerprint(locator: &PlaylistLocator) -> Result<[u8; 32], ResumeCheckpointBuildError> {
    let mut hasher = Sha256::new();
    update_fingerprint_bytes(&mut hasher, LOCATOR_FINGERPRINT_DOMAIN);
    match locator {
        PlaylistLocator::Url(url) => {
            hasher.update([0x01]);
            update_fingerprint_bytes(&mut hasher, url.expose_secret_for_persistence().as_bytes());
        }
        PlaylistLocator::Local(LocalLocator::Native(path)) => {
            hasher.update([0x02]);
            update_native_path_fingerprint(&mut hasher, path)?;
        }
        PlaylistLocator::Local(LocalLocator::Foreign(path)) => {
            hasher.update([0x03]);
            update_foreign_platform(&mut hasher, path.platform_for_persistence());
            update_foreign_encoding(&mut hasher, path.encoding_for_persistence());
        }
    }
    Ok(hasher.finalize().into())
}

fn update_fingerprint_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(unix)]
fn update_native_path_fingerprint(
    hasher: &mut Sha256,
    path: &Path,
) -> Result<(), ResumeCheckpointBuildError> {
    use std::os::unix::ffi::OsStrExt;

    hasher.update([0x01]);
    update_fingerprint_bytes(hasher, path.as_os_str().as_bytes());
    Ok(())
}

#[cfg(windows)]
fn update_native_path_fingerprint(
    hasher: &mut Sha256,
    path: &Path,
) -> Result<(), ResumeCheckpointBuildError> {
    use std::os::windows::ffi::OsStrExt;

    hasher.update([0x02]);
    let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
    hasher.update((units.len() as u64).to_le_bytes());
    for unit in units {
        hasher.update(unit.to_le_bytes());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn update_native_path_fingerprint(
    _hasher: &mut Sha256,
    _path: &Path,
) -> Result<(), ResumeCheckpointBuildError> {
    Err(ResumeCheckpointBuildError::UnsupportedNativePathEncoding)
}

fn update_foreign_platform(hasher: &mut Sha256, platform: &ForeignPathPlatform) {
    match platform {
        ForeignPathPlatform::Linux => hasher.update([0x01]),
        ForeignPathPlatform::MacOs => hasher.update([0x02]),
        ForeignPathPlatform::Windows => hasher.update([0x03]),
        ForeignPathPlatform::Other(name) => {
            hasher.update([0x04]);
            update_fingerprint_bytes(hasher, name.as_bytes());
        }
    }
}

fn update_foreign_encoding(hasher: &mut Sha256, encoding: &ForeignPathEncoding) {
    match encoding {
        ForeignPathEncoding::Utf8(value) => {
            hasher.update([0x01]);
            update_fingerprint_bytes(hasher, value.as_bytes());
        }
        ForeignPathEncoding::Bytes(bytes) => {
            hasher.update([0x02]);
            update_fingerprint_bytes(hasher, bytes);
        }
        ForeignPathEncoding::Wide(units) => {
            hasher.update([0x03]);
            hasher.update((units.len() as u64).to_le_bytes());
            for unit in units {
                hasher.update(unit.to_le_bytes());
            }
        }
        ForeignPathEncoding::Opaque {
            encoding_name,
            raw_units,
        } => {
            hasher.update([0x04]);
            update_fingerprint_bytes(hasher, encoding_name.as_bytes());
            hasher.update((raw_units.len() as u64).to_le_bytes());
            for unit in raw_units {
                hasher.update(unit.to_le_bytes());
            }
        }
    }
}

fn encode_lower_hex(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_lower_hex_32(encoded: &str) -> Option<[u8; 32]> {
    if encoded.len() != 64 {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (index, chunk) in encoded.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_lower_hex_nibble(chunk[0])?;
        let low = decode_lower_hex_nibble(chunk[1])?;
        decoded[index] = (high << 4) | low;
    }
    Some(decoded)
}

fn decode_lower_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
