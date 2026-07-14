use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

use crate::types::{InspectedFileIdentity, InspectedSourceClassification, PlatformFileId};

/// Ошибка безопасного no-follow открытия source.
pub(crate) enum OpenSourceError {
    Missing,
    NotRegularFile,
    Io(io::ErrorKind),
}

/// Открывает source для inspection, отвергая symlink на поддерживаемых Unix OS.
pub(crate) fn open_regular_nofollow(path: &Path) -> Result<File, OpenSourceError> {
    let path_metadata = path
        .symlink_metadata()
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => OpenSourceError::Missing,
            error_kind => OpenSourceError::Io(error_kind),
        })?;
    if !path_metadata.file_type().is_file() {
        return Err(OpenSourceError::NotRegularFile);
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|error| OpenSourceError::Io(error.kind()))?;
    let handle_metadata = file
        .metadata()
        .map_err(|error| OpenSourceError::Io(error.kind()))?;
    if !handle_metadata.is_file() || !same_available_file_id(&path_metadata, &handle_metadata) {
        return Err(OpenSourceError::NotRegularFile);
    }
    Ok(file)
}

/// Собирает identity из metadata того же handle, который дал inspected bytes.
pub(crate) fn inspected_identity(
    metadata: &Metadata,
    content_sha256: [u8; 32],
) -> InspectedFileIdentity {
    InspectedFileIdentity {
        classification: InspectedSourceClassification::NoFollowRegularFile,
        platform_file_id: platform_file_id(metadata),
        length_bytes: metadata.len(),
        modified_at: metadata.modified().ok(),
        content_sha256,
    }
}

/// Быстрая metadata часть revalidation; digest сравнивается после полного read.
pub(crate) fn metadata_matches(expected: &InspectedFileIdentity, actual: &Metadata) -> bool {
    expected.classification == InspectedSourceClassification::NoFollowRegularFile
        && expected.platform_file_id == platform_file_id(actual)
        && expected.length_bytes == actual.len()
        && expected.modified_at == actual.modified().ok()
}

#[cfg(unix)]
fn platform_file_id(metadata: &Metadata) -> PlatformFileId {
    use std::os::unix::fs::MetadataExt;
    PlatformFileId::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn platform_file_id(_metadata: &Metadata) -> PlatformFileId {
    PlatformFileId::Unavailable
}

#[cfg(unix)]
fn same_available_file_id(left: &Metadata, right: &Metadata) -> bool {
    platform_file_id(left) == platform_file_id(right)
}

#[cfg(not(unix))]
fn same_available_file_id(_left: &Metadata, _right: &Metadata) -> bool {
    true
}
