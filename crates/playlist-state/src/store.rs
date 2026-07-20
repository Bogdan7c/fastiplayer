use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::dto::{self, DtoLoadError};
use crate::envelope::scan_envelope;
use crate::identity::{OpenSourceError, inspected_identity, open_regular_nofollow};
use crate::quarantine::{self, QuarantineFileName};
use crate::types::{CorruptStateCause, InspectionOutcome, ProtectedStateCause, QuarantineOutcome};
use crate::{
    CURRENT_PLAYLIST_STATE_SCHEMA_VERSION, MAX_STATE_ENVELOPE_SCAN_BYTES, MAX_SUPPORTED_STATE_BYTES,
};

/// Process-local owner path и сериализации destructive state operations.
///
/// Future Session 07 writer обязан разделять этот же owner lock. Несколько
/// независимых `PlaylistStateStore` для одного path caller создавать не должен.
pub struct PlaylistStateStore {
    state_path: PathBuf,
    operation_lock: Mutex<()>,
}

impl PlaylistStateStore {
    /// Caller передаёт exact path; crate не читает AppConfig/ConfigPaths.
    pub fn new(state_path: impl Into<PathBuf>) -> Self {
        Self {
            state_path: state_path.into(),
            operation_lock: Mutex::new(()),
        }
    }

    /// Возвращает configured target без попытки вычислить config directory.
    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    /// Сериализует writer/quarantine/inspection operations одним owner lock.
    pub(crate) fn lock_operations(&self) -> Result<MutexGuard<'_, ()>, ()> {
        self.operation_lock.lock().map_err(|_| ())
    }

    /// Выполняет только read-only inspection и никогда не quarantine-ит source.
    pub fn inspect_state(&self) -> InspectionOutcome {
        let _operation_guard = match self.operation_lock.lock() {
            Ok(guard) => guard,
            Err(_) => {
                return InspectionOutcome::UnrecognizedVersionSaveBlocked {
                    cause: ProtectedStateCause::ReadFailed(std::io::ErrorKind::Other),
                };
            }
        };
        inspect_state_with_limits(
            &self.state_path,
            MAX_STATE_ENVELOPE_SCAN_BYTES,
            MAX_SUPPORTED_STATE_BYTES,
        )
    }

    /// Явный app-policy action для exact identity из `CorruptNeedsQuarantine`.
    ///
    /// Portable atomic compare-identity-and-rename не существует: после финальной
    /// revalidation и до rename остаётся внешнее TOCTOU-окно. Mutex исключает
    /// только in-process writer/quarantine overlap. Linux дополнительно использует
    /// `renameat2(RENAME_NOREPLACE)` для atomic no-clobber move.
    pub fn apply_quarantine(
        &self,
        inspected_identity: &crate::InspectedFileIdentity,
        quarantine_file_name: &QuarantineFileName,
    ) -> QuarantineOutcome {
        let _operation_guard = match self.operation_lock.lock() {
            Ok(guard) => guard,
            Err(_) => {
                return QuarantineOutcome::FailedSaveBlocked {
                    cause: crate::QuarantineFailureCause::MoveFailed(std::io::ErrorKind::Other),
                };
            }
        };
        quarantine::apply_quarantine(&self.state_path, inspected_identity, quarantine_file_name)
    }
}

fn inspect_state_with_limits(
    state_path: &Path,
    envelope_limit_bytes: u64,
    supported_state_limit_bytes: u64,
) -> InspectionOutcome {
    let mut source = match open_regular_nofollow(state_path) {
        Ok(source) => source,
        Err(OpenSourceError::Missing) => return InspectionOutcome::Missing,
        Err(OpenSourceError::NotRegularFile) => {
            return InspectionOutcome::UnrecognizedVersionSaveBlocked {
                cause: ProtectedStateCause::SourceIsNotRegularFile,
            };
        }
        Err(OpenSourceError::Io(error_kind)) => {
            return InspectionOutcome::UnrecognizedVersionSaveBlocked {
                cause: ProtectedStateCause::ReadFailed(error_kind),
            };
        }
    };
    let metadata_before_scan = match source.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            return InspectionOutcome::UnrecognizedVersionSaveBlocked {
                cause: ProtectedStateCause::ReadFailed(error.kind()),
            };
        }
    };
    let proof = match scan_envelope(&mut source, envelope_limit_bytes) {
        Ok(proof) => proof,
        Err(cause) => {
            return InspectionOutcome::UnrecognizedVersionSaveBlocked { cause };
        }
    };
    let metadata_after_scan = match source.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            return InspectionOutcome::UnrecognizedVersionSaveBlocked {
                cause: ProtectedStateCause::ReadFailed(error.kind()),
            };
        }
    };
    let pre_scan_identity = inspected_identity(&metadata_before_scan, proof.content_sha256);
    if !crate::identity::metadata_matches(&pre_scan_identity, &metadata_after_scan) {
        return InspectionOutcome::UnrecognizedVersionSaveBlocked {
            cause: ProtectedStateCause::InvalidEnvelope,
        };
    }

    if proof.schema_version > CURRENT_PLAYLIST_STATE_SCHEMA_VERSION {
        return InspectionOutcome::NewerSchemaSaveBlocked {
            schema_version: proof.schema_version,
        };
    }
    if !matches!(
        proof.schema_version,
        1 | CURRENT_PLAYLIST_STATE_SCHEMA_VERSION
    ) {
        return InspectionOutcome::UnrecognizedVersionSaveBlocked {
            cause: ProtectedStateCause::UnsupportedSchemaVersion,
        };
    }

    let identity = inspected_identity(&metadata_after_scan, proof.content_sha256);
    if proof.inspected_bytes.len() as u64 != metadata_before_scan.len() {
        return InspectionOutcome::UnrecognizedVersionSaveBlocked {
            cause: ProtectedStateCause::InvalidEnvelope,
        };
    }
    if metadata_before_scan.len() > supported_state_limit_bytes {
        return InspectionOutcome::CorruptNeedsQuarantine {
            inspected_identity: identity,
            cause: CorruptStateCause::SupportedFileTooLarge,
        };
    }

    match dto::deserialize_supported(proof.schema_version, &proof.inspected_bytes) {
        Ok(state) => InspectionOutcome::Loaded(state),
        Err(error) => InspectionOutcome::CorruptNeedsQuarantine {
            inspected_identity: identity,
            cause: match error {
                DtoLoadError::InvalidPayload if proof.schema_version == 1 => {
                    CorruptStateCause::InvalidV1Payload
                }
                DtoLoadError::InvalidPayload => CorruptStateCause::InvalidV2Payload,
                DtoLoadError::ResourceLimit => CorruptStateCause::ResourceLimitExceeded,
                DtoLoadError::DomainValue => CorruptStateCause::InvalidDomainValue,
                DtoLoadError::QueueState => CorruptStateCause::InvalidQueueState,
                DtoLoadError::ShuffleTraversal => CorruptStateCause::InvalidShuffleTraversal,
            },
        },
    }
}

#[cfg(test)]
pub(crate) fn inspect_state_with_test_limits(
    state_path: &Path,
    envelope_limit_bytes: u64,
    supported_state_limit_bytes: u64,
) -> InspectionOutcome {
    inspect_state_with_limits(
        state_path,
        envelope_limit_bytes,
        supported_state_limit_bytes,
    )
}
