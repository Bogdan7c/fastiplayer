//! Bounded deterministic D63 directory manifest facade.

mod accounting;
mod builder;
mod enumeration;
mod types;

use std::fs;
use std::io;
use std::path::Path;

use accounting::{ManifestLimits, RawManifestAccounting};
use builder::{ManifestBuilder, ValidationIdentity};
use enumeration::{EnumerationObservation, enumerate_parent, normalize_explicit_target};

pub use types::{
    AliasPresentationChoice, CandidateSourceDiagnostic, DirectoryManifestBuildError,
    DirectoryManifestDiagnostic, ManifestAliasDiagnostics, ManifestCandidateKey, ManifestRecord,
    NaturalPosition, RAW_MANIFEST_MAX_ENTRIES, RAW_MANIFEST_MAX_PATH_KEY_BYTES, RawManifestLimit,
    RawManifestLimitReached,
};

/// Immutable deterministic membership snapshot до media probe scheduling.
pub struct DirectoryManifest {
    records: Box<[ManifestRecord]>,
    validation_identities: Box<[ValidationIdentity]>,
    target_position: usize,
    diagnostics: Box<[DirectoryManifestDiagnostic]>,
    accounting: RawManifestAccounting,
}

impl DirectoryManifest {
    /// Возвращает natural-ordered deduplicated records.
    #[must_use]
    pub fn records(&self) -> &[ManifestRecord] {
        &self.records
    }

    /// Возвращает explicit target record, сохранивший natural position.
    #[must_use]
    pub fn explicit_target(&self) -> &ManifestRecord {
        &self.records[self.target_position]
    }

    /// Возвращает typed enumeration diagnostics без probe/result state.
    #[must_use]
    pub fn diagnostics(&self) -> &[DirectoryManifestDiagnostic] {
        &self.diagnostics
    }

    /// Полное число raw candidate entries до canonical alias dedup.
    #[must_use]
    pub const fn raw_entry_count(&self) -> usize {
        self.accounting.entry_count()
    }

    /// Exact retained native path + compact natural-key payload accounting.
    #[must_use]
    pub const fn retained_path_key_bytes(&self) -> usize {
        self.accounting.path_key_bytes()
    }

    /// Проверяет конкретный snapshot locator без rescan и membership mutation.
    pub fn validate_candidate_source(
        &self,
        candidate_key: ManifestCandidateKey,
    ) -> Result<(), CandidateSourceDiagnostic> {
        let index = candidate_key.get() as usize;
        let Some(record) = self.records.get(index) else {
            return Err(CandidateSourceDiagnostic::UnknownCandidateKey { candidate_key });
        };
        let identity = &self.validation_identities[index];

        match fs::canonicalize(record.original_locator()) {
            Ok(current_canonical_path) => {
                let identity_matches = if identity.canonicalization_succeeded {
                    current_canonical_path == identity.expected_canonical_path
                } else {
                    current_canonical_path == record.original_locator()
                };
                if identity_matches {
                    Ok(())
                } else {
                    Err(CandidateSourceDiagnostic::SourceChangedAfterSnapshot { candidate_key })
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Err(CandidateSourceDiagnostic::MissingAfterSnapshot { candidate_key })
            }
            Err(error) => Err(CandidateSourceDiagnostic::UnavailableAfterSnapshot {
                candidate_key,
                error_kind: error.kind(),
            }),
        }
    }
}

/// Строит полный D63 snapshot непосредственных siblings явного local target.
pub fn build_directory_manifest(
    explicit_target: &Path,
) -> Result<DirectoryManifest, DirectoryManifestBuildError> {
    let explicit_target = normalize_explicit_target(explicit_target)?;
    let mut builder = ManifestBuilder::new(ManifestLimits::PRODUCTION);
    let diagnostics = enumerate_parent(&explicit_target, |observation| match observation {
        EnumerationObservation::Candidate(entry) => builder.push(entry),
        EnumerationObservation::Skipped => builder.observe_skipped_entry(),
    })?;
    let built = builder.finish()?;

    Ok(DirectoryManifest {
        records: built.records,
        validation_identities: built.validation_identities,
        target_position: built.target_position,
        diagnostics: diagnostics.into_boxed_slice(),
        accounting: built.accounting,
    })
}

#[cfg(test)]
mod tests;
