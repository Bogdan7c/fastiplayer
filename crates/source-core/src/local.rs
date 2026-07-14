use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    ByteSource, CancellationToken, Seekability, SourceError, SourceFingerprint, SourceResult,
    SourceValidators,
};

/// Снимок filesystem identity, снятый с того же открытого handle, что читает demuxer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalFileMetadataSnapshot {
    /// Размер файла в момент успешного `File::metadata`.
    pub file_size_bytes: u64,

    /// Modification time в native `SystemTime`, без потери точности при конвертации.
    pub modified_at: SystemTime,
}

/// Seekable byte source поверх локального файла.
pub struct LocalFileSource {
    /// File handle с собственным cursor-ом.
    file: File,

    /// Путь, нормализованный для diagnostics и fingerprint.
    path: PathBuf,

    /// Размер файла на момент открытия.
    content_length: u64,

    /// Текущий byte cursor source-а.
    position: u64,

    /// Fingerprint, привязанный к path/size/mtime.
    fingerprint: SourceFingerprint,

    /// Neutral filesystem snapshot для D64/D75 envelope до ownership transfer.
    metadata_snapshot: LocalFileMetadataSnapshot,
}

impl LocalFileSource {
    /// Открывает локальный файл как seekable source.
    pub fn open(path: impl AsRef<Path>) -> SourceResult<Self> {
        let original_path = path.as_ref();
        let file = File::open(original_path).map_err(|source| SourceError::LocalIo {
            context: "open",
            source,
        })?;
        let metadata = file.metadata().map_err(|source| SourceError::LocalIo {
            context: "metadata",
            source,
        })?;

        let normalized_path = original_path
            .canonicalize()
            .unwrap_or_else(|_| original_path.to_path_buf());
        let fingerprint = build_local_fingerprint(&normalized_path, &metadata);
        let modified_at = metadata.modified().map_err(|source| SourceError::LocalIo {
            context: "metadata.modified",
            source,
        })?;

        Ok(Self {
            file,
            path: normalized_path,
            content_length: metadata.len(),
            position: 0,
            fingerprint,
            metadata_snapshot: LocalFileMetadataSnapshot {
                file_size_bytes: metadata.len(),
                modified_at,
            },
        })
    }

    /// Возвращает путь source-а для diagnostics.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Возвращает exact size/mtime того же opened handle до передачи source demuxer-у.
    #[must_use]
    pub const fn metadata_snapshot(&self) -> LocalFileMetadataSnapshot {
        self.metadata_snapshot
    }
}

impl ByteSource for LocalFileSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }

        let bytes_read = self
            .file
            .read(output)
            .map_err(|source| SourceError::LocalIo {
                context: "read",
                source,
            })?;
        self.position = self.position.saturating_add(bytes_read as u64);
        Ok(bytes_read)
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|source| SourceError::LocalIo {
                context: "seek",
                source,
            })?;
        self.position = offset;
        Ok(())
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn seekability(&self) -> Seekability {
        Seekability::Seekable
    }

    fn validators(&self) -> SourceValidators {
        SourceValidators::default()
    }

    fn content_length(&self) -> Option<u64> {
        Some(self.content_length)
    }

    fn fingerprint(&self) -> SourceFingerprint {
        self.fingerprint.clone()
    }
}

/// Собирает fingerprint из локальных свойств файла без чтения содержимого.
fn build_local_fingerprint(path: &Path, metadata: &Metadata) -> SourceFingerprint {
    SourceFingerprint::new(format!(
        "local:{}:{}:{}",
        path.display(),
        metadata.len(),
        modified_unix_nanos(metadata)
    ))
}

/// Возвращает mtime в наносекундах Unix epoch с насыщением до `i64`.
fn modified_unix_nanos(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn local_source_reads_and_seeks() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let file_path = temp_dir.path().join("sample.bin");
        let mut file = File::create(&file_path).expect("sample file created");
        file.write_all(b"abcdef").expect("sample file written");

        let mut source = LocalFileSource::open(&file_path).expect("local source opened");
        let metadata_snapshot = source.metadata_snapshot();
        assert_eq!(metadata_snapshot.file_size_bytes, 6);
        assert!(metadata_snapshot.modified_at <= std::time::SystemTime::now());
        let token = CancellationToken::never_cancelled();
        let mut output = [0_u8; 3];

        let bytes_read = source
            .read(&mut output, &token)
            .expect("initial read works");
        assert_eq!(bytes_read, 3);
        assert_eq!(&output, b"abc");
        assert_eq!(source.position(), 3);

        source.seek(2).expect("seek works");
        let bytes_read = source.read(&mut output, &token).expect("second read works");
        assert_eq!(bytes_read, 3);
        assert_eq!(&output, b"cde");
        assert!(source.seekability().is_seekable());
        assert_eq!(source.content_length(), Some(6));
    }
}
