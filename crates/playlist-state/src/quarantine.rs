use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::identity::{OpenSourceError, metadata_matches, open_regular_nofollow};
use crate::types::{InspectedFileIdentity, QuarantineFailureCause, QuarantineOutcome};

/// Validated collision target, который caller создаёт из injected clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuarantineFileName(OsString);

impl QuarantineFileName {
    /// Deterministic filename `playlist-state.corrupt-<timestamp>.json`.
    pub fn from_timestamp(timestamp: SystemTime) -> Self {
        Self::from_timestamp_with_stem(timestamp, "playlist-state")
    }

    /// Deterministic filename `playlist-resume.corrupt-<timestamp>.json`.
    pub fn resume_from_timestamp(timestamp: SystemTime) -> Self {
        Self::from_timestamp_with_stem(timestamp, "playlist-resume")
    }

    /// Общий formatter оставляет выбор artifact stem у typed public constructor-а.
    fn from_timestamp_with_stem(timestamp: SystemTime, artifact_stem: &str) -> Self {
        let timestamp_label = match timestamp.duration_since(UNIX_EPOCH) {
            Ok(duration) => format!("{}-{:09}", duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                format!(
                    "before-{}-{:09}",
                    duration.as_secs(),
                    duration.subsec_nanos()
                )
            }
        };
        Self(OsString::from(format!(
            "{artifact_stem}.corrupt-{timestamp_label}.json"
        )))
    }

    fn is_single_filename_component(&self) -> bool {
        let mut components = Path::new(&self.0).components();
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
    }
}

/// Повторно доказывает source identity/content и только затем переносит файл.
pub(crate) fn apply_quarantine(
    source_path: &Path,
    inspected_identity: &InspectedFileIdentity,
    quarantine_file_name: &QuarantineFileName,
) -> QuarantineOutcome {
    if !quarantine_file_name.is_single_filename_component() {
        return QuarantineOutcome::FailedSaveBlocked {
            cause: QuarantineFailureCause::InvalidQuarantineFileName,
        };
    }

    let mut source = match open_regular_nofollow(source_path) {
        Ok(source) => source,
        Err(OpenSourceError::Missing | OpenSourceError::NotRegularFile) => {
            return QuarantineOutcome::SourceChanged;
        }
        Err(OpenSourceError::Io(error_kind)) => {
            return QuarantineOutcome::FailedSaveBlocked {
                cause: QuarantineFailureCause::RevalidationReadFailed(error_kind),
            };
        }
    };
    let metadata = match source.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            return QuarantineOutcome::FailedSaveBlocked {
                cause: QuarantineFailureCause::RevalidationReadFailed(error.kind()),
            };
        }
    };
    if !metadata_matches(inspected_identity, &metadata) {
        return QuarantineOutcome::SourceChanged;
    }

    let (actual_length, actual_digest) = match hash_reader(&mut source) {
        Ok(identity) => identity,
        Err(error) => {
            return QuarantineOutcome::FailedSaveBlocked {
                cause: QuarantineFailureCause::RevalidationReadFailed(error.kind()),
            };
        }
    };
    if actual_length != inspected_identity.length_bytes
        || actual_digest != inspected_identity.content_sha256
    {
        return QuarantineOutcome::SourceChanged;
    }

    let quarantine_path = source_path.with_file_name(&quarantine_file_name.0);
    match quarantine_path.symlink_metadata() {
        Ok(_) => {
            return QuarantineOutcome::FailedSaveBlocked {
                cause: QuarantineFailureCause::DestinationAlreadyExists,
            };
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return QuarantineOutcome::FailedSaveBlocked {
                cause: QuarantineFailureCause::MoveFailed(error.kind()),
            };
        }
    }
    match move_without_collision_overwrite(source_path, &quarantine_path) {
        Ok(()) => QuarantineOutcome::Applied { quarantine_path },
        Err(MoveError::DestinationExists) => QuarantineOutcome::FailedSaveBlocked {
            cause: QuarantineFailureCause::DestinationAlreadyExists,
        },
        Err(MoveError::Io(error_kind)) => QuarantineOutcome::FailedSaveBlocked {
            cause: QuarantineFailureCause::MoveFailed(error_kind),
        },
    }
}

fn hash_reader(reader: &mut impl Read) -> io::Result<(u64, [u8; 32])> {
    let mut hasher = Sha256::new();
    let mut total_bytes = 0_u64;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read_count = reader.read(&mut buffer)?;
        if read_count == 0 {
            break;
        }
        total_bytes = total_bytes
            .checked_add(read_count as u64)
            .ok_or_else(|| io::Error::other("playlist state length overflow"))?;
        hasher.update(&buffer[..read_count]);
    }
    let digest = hasher.finalize();
    let mut exact_digest = [0_u8; 32];
    exact_digest.copy_from_slice(&digest);
    Ok((total_bytes, exact_digest))
}

enum MoveError {
    DestinationExists,
    Io(io::ErrorKind),
}

/// Linux предоставляет atomic no-clobber rename, но даже он не сравнивает
/// identity с inspected snapshot внутри одной filesystem transaction.
#[cfg(target_os = "linux")]
fn move_without_collision_overwrite(source: &Path, destination: &Path) -> Result<(), MoveError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| MoveError::Io(io::ErrorKind::InvalidInput))?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| MoveError::Io(io::ErrorKind::InvalidInput))?;

    // SAFETY: обе C strings NUL-terminated и живут до завершения syscall;
    // AT_FDCWD означает, что paths разрешаются так же, как std::fs::rename.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EEXIST) {
        return Err(MoveError::DestinationExists);
    }
    if matches!(
        error.raw_os_error(),
        Some(libc::ENOSYS) | Some(libc::EINVAL)
    ) {
        return link_then_unlink(
            source_path(source.as_bytes()),
            source_path(destination.as_bytes()),
        );
    }
    Err(MoveError::Io(error.kind()))
}

#[cfg(target_os = "linux")]
fn source_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

/// Windows rename не заменяет existing destination; остальные platforms
/// используют no-clobber hard-link + unlink fallback.
#[cfg(windows)]
fn move_without_collision_overwrite(source: &Path, destination: &Path) -> Result<(), MoveError> {
    fs::rename(source, destination).map_err(map_move_io_error)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn move_without_collision_overwrite(source: &Path, destination: &Path) -> Result<(), MoveError> {
    link_then_unlink(source, destination)
}

fn link_then_unlink(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<(), MoveError> {
    let source = source.as_ref();
    let destination = destination.as_ref();
    fs::hard_link(source, destination).map_err(map_move_io_error)?;
    if let Err(remove_error) = fs::remove_file(source) {
        let _cleanup_result = fs::remove_file(destination);
        return Err(MoveError::Io(remove_error.kind()));
    }
    Ok(())
}

fn map_move_io_error(error: io::Error) -> MoveError {
    if error.kind() == io::ErrorKind::AlreadyExists {
        MoveError::DestinationExists
    } else {
        MoveError::Io(error.kind())
    }
}
