//! Non-recursive parent enumeration и explicit-target hidden policy.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::types::{DirectoryManifestBuildError, DirectoryManifestDiagnostic};

/// Lightweight entry до natural key/canonical grouping.
#[derive(Clone, Debug)]
pub(super) struct EnumeratedEntry {
    pub(super) original_path: PathBuf,
    pub(super) is_symlink: bool,
    pub(super) is_explicit_target: bool,
}

/// Каждый unique raw entry учитывается, даже если hidden/non-file policy его skip-ит.
pub(super) enum EnumerationObservation {
    Candidate(EnumeratedEntry),
    Skipped,
}

/// Diagnostics не могут стать вторым unbounded manifest payload.
const MAX_ENUMERATION_DIAGNOSTICS: usize = 64;

/// Делает explicit target абсолютным, не разрешая и не схлопывая его symlink semantics.
pub(super) fn normalize_explicit_target(
    explicit_target: &Path,
) -> Result<PathBuf, DirectoryManifestBuildError> {
    if explicit_target.file_name().is_none() {
        return Err(DirectoryManifestBuildError::InvalidExplicitTarget);
    }
    if explicit_target.is_absolute() {
        Ok(explicit_target.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|current_directory| current_directory.join(explicit_target))
            .map_err(DirectoryManifestBuildError::CurrentDirectory)
    }
}

/// Читает только непосредственные entries parent directory.
pub(super) fn enumerate_parent<F>(
    explicit_target: &Path,
    mut accept_entry: F,
) -> Result<Vec<DirectoryManifestDiagnostic>, DirectoryManifestBuildError>
where
    F: FnMut(EnumerationObservation) -> Result<(), DirectoryManifestBuildError>,
{
    let parent = explicit_target
        .parent()
        .ok_or(DirectoryManifestBuildError::InvalidExplicitTarget)?;
    let explicit_is_symlink = fs::symlink_metadata(explicit_target)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false);
    accept_entry(EnumerationObservation::Candidate(EnumeratedEntry {
        original_path: explicit_target.to_path_buf(),
        is_symlink: explicit_is_symlink,
        is_explicit_target: true,
    }))?;
    let mut diagnostics = Vec::new();
    let mut omitted_diagnostics = 0_usize;
    let directory =
        fs::read_dir(parent).map_err(DirectoryManifestBuildError::ReadParentDirectory)?;

    for entry_result in directory {
        let entry = match entry_result {
            Ok(entry) => entry,
            Err(error) => {
                accept_entry(EnumerationObservation::Skipped)?;
                push_bounded_diagnostic(
                    &mut diagnostics,
                    &mut omitted_diagnostics,
                    DirectoryManifestDiagnostic::EntryReadFailed {
                        error_kind: error.kind(),
                    },
                );
                continue;
            }
        };
        // `DirEntry::path` сохраняет spelling parent-а, включая `..` после symlink.
        let original_path = entry.path();
        if original_path == explicit_target {
            continue;
        }
        if is_hidden_filename(&entry.file_name()) {
            accept_entry(EnumerationObservation::Skipped)?;
            continue;
        }

        match classify_candidate_entry(&entry) {
            Ok(Some(is_symlink)) => {
                accept_entry(EnumerationObservation::Candidate(EnumeratedEntry {
                    original_path,
                    is_symlink,
                    is_explicit_target: false,
                }))?
            }
            Ok(None) => accept_entry(EnumerationObservation::Skipped)?,
            Err(error) => {
                accept_entry(EnumerationObservation::Skipped)?;
                push_bounded_diagnostic(
                    &mut diagnostics,
                    &mut omitted_diagnostics,
                    DirectoryManifestDiagnostic::EntryInspectionFailed {
                        original_locator: original_path,
                        error_kind: error.kind(),
                    },
                );
            }
        }
    }

    if omitted_diagnostics > 0 {
        diagnostics.push(DirectoryManifestDiagnostic::AdditionalFailuresOmitted {
            count: omitted_diagnostics,
        });
    }
    Ok(diagnostics)
}

/// Удерживает первые diagnostics и считает bounded remainder summary.
fn push_bounded_diagnostic(
    diagnostics: &mut Vec<DirectoryManifestDiagnostic>,
    omitted_diagnostics: &mut usize,
    diagnostic: DirectoryManifestDiagnostic,
) {
    if diagnostics.len() < MAX_ENUMERATION_DIAGNOSTICS {
        diagnostics.push(diagnostic);
    } else {
        *omitted_diagnostics = omitted_diagnostics.saturating_add(1);
    }
}

/// Direct files и file symlinks допустимы; directories никогда не обходятся.
fn classify_candidate_entry(entry: &fs::DirEntry) -> io::Result<Option<bool>> {
    let file_type = entry.file_type()?;
    if file_type.is_file() {
        return Ok(Some(false));
    }
    if !file_type.is_symlink() {
        return Ok(None);
    }
    fs::metadata(entry.path()).map(|metadata| metadata.is_file().then_some(true))
}

/// Dot-hidden проверяется на native units без lossy conversion.
fn is_hidden_filename(filename: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        filename.as_bytes().first() == Some(&b'.')
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        filename.encode_wide().next() == Some(u16::from(b'.'))
    }
    #[cfg(not(any(unix, windows)))]
    filename.to_string_lossy().starts_with('.')
}
