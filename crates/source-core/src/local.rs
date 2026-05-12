use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::{
    ByteSource, CancellationToken, Seekability, SourceError, SourceFingerprint, SourceResult,
    SourceRuntimeConfig, SourceValidators,
};

/// FNV-1a offset basis для deterministic non-cryptographic partial hash.
const FNV1A_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a prime для deterministic non-cryptographic partial hash.
const FNV1A_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Версия алгоритма partial hash, включённая в строку для будущих миграций.
const LOCAL_PARTIAL_HASH_VERSION: &str = "fnv1a64-v1";

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
}

/// Identity локального файла для persisted media index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFileIndexIdentity {
    /// Стабильный ключ source-а: canonical path без size/mtime/hash.
    pub source_key: String,

    /// Человекочитаемый URI/path source-а.
    pub source_uri: String,

    /// Размер файла на момент fingerprint-а.
    pub size_bytes: u64,

    /// `mtime` в наносекундах Unix epoch.
    pub modified_unix_nanos: i64,

    /// Partial content hash по configured head/tail sample.
    pub partial_hash_hex: String,
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

        Ok(Self {
            file,
            path: normalized_path,
            content_length: metadata.len(),
            position: 0,
            fingerprint,
        })
    }

    /// Возвращает путь source-а для diagnostics.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Строит persisted-index identity локального файла без открытия decoder/demuxer pipeline.
pub fn build_local_file_index_identity(
    path: impl AsRef<Path>,
    config: &SourceRuntimeConfig,
) -> SourceResult<LocalFileIndexIdentity> {
    let original_path = path.as_ref();
    let mut file = File::open(original_path).map_err(|source| SourceError::LocalIo {
        context: "open index identity",
        source,
    })?;
    let metadata = file.metadata().map_err(|source| SourceError::LocalIo {
        context: "metadata index identity",
        source,
    })?;
    let normalized_path = original_path
        .canonicalize()
        .unwrap_or_else(|_| original_path.to_path_buf());
    let partial_hash_hex = build_local_partial_hash_hex(&mut file, metadata.len(), config)?;
    let source_key = format!("local:{}", normalized_path.display());
    let source_uri = normalized_path.display().to_string();

    Ok(LocalFileIndexIdentity {
        source_key,
        source_uri,
        size_bytes: metadata.len(),
        modified_unix_nanos: modified_unix_nanos(&metadata),
        partial_hash_hex,
    })
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

/// Возвращает mtime в наносекундах Unix epoch с насыщением до SQLite-compatible i64.
fn modified_unix_nanos(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

/// Строит partial hash первых и последних bytes, не читая весь большой файл.
fn build_local_partial_hash_hex(
    file: &mut File,
    file_size_bytes: u64,
    config: &SourceRuntimeConfig,
) -> SourceResult<String> {
    let sample_bytes = config.index_fingerprint_sample_bytes();
    if sample_bytes == 0 {
        return Err(SourceError::InvalidConfig {
            field: "network.index_fingerprint_sample_kb",
            message: "sample должен быть положительным".to_string(),
        });
    }

    let mut hasher = DeterministicFnv1a64::new();
    hasher.write_bytes(LOCAL_PARTIAL_HASH_VERSION.as_bytes());
    hasher.write_u64(file_size_bytes);
    hasher.write_u64(sample_bytes);

    if file_size_bytes <= sample_bytes.saturating_mul(2) {
        hash_file_range(file, 0, file_size_bytes, &mut hasher)?;
    } else {
        hash_file_range(file, 0, sample_bytes, &mut hasher)?;
        hash_file_range(
            file,
            file_size_bytes.saturating_sub(sample_bytes),
            sample_bytes,
            &mut hasher,
        )?;
    }

    Ok(format!(
        "{LOCAL_PARTIAL_HASH_VERSION}:{:016x}",
        hasher.finish()
    ))
}

/// Читает конкретный range файла в hash с явным offset marker-ом.
fn hash_file_range(
    file: &mut File,
    offset: u64,
    length: u64,
    hasher: &mut DeterministicFnv1a64,
) -> SourceResult<()> {
    let buffer_len = usize::try_from(length).map_err(|_| SourceError::InvalidConfig {
        field: "network.index_fingerprint_sample_kb",
        message: "sample не помещается в usize на текущей платформе".to_string(),
    })?;
    let mut buffer = vec![0_u8; buffer_len];

    file.seek(SeekFrom::Start(offset))
        .map_err(|source| SourceError::LocalIo {
            context: "seek index identity sample",
            source,
        })?;
    file.read_exact(&mut buffer)
        .map_err(|source| SourceError::LocalIo {
            context: "read index identity sample",
            source,
        })?;

    hasher.write_u64(offset);
    hasher.write_u64(length);
    hasher.write_bytes(&buffer);
    Ok(())
}

/// Минимальный deterministic hasher, чтобы persisted partial hash не зависел от random keys.
struct DeterministicFnv1a64 {
    /// Текущее состояние FNV-1a.
    state: u64,
}

impl DeterministicFnv1a64 {
    /// Создаёт hasher с фиксированным offset basis.
    const fn new() -> Self {
        Self {
            state: FNV1A_OFFSET_BASIS,
        }
    }

    /// Добавляет little-endian u64 marker.
    fn write_u64(&mut self, value: u64) {
        self.write_bytes(&value.to_le_bytes());
    }

    /// Добавляет bytes в FNV-1a state.
    fn write_bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.state ^= u64::from(*byte);
            self.state = self.state.wrapping_mul(FNV1A_PRIME);
        }
    }

    /// Возвращает итоговый hash.
    const fn finish(&self) -> u64 {
        self.state
    }
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

    #[test]
    fn local_file_index_identity_changes_when_sample_content_changes() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let file_path = temp_dir.path().join("sample.bin");
        let mut file = File::create(&file_path).expect("sample file created");
        file.write_all(b"abcdef").expect("sample file written");
        let config = SourceRuntimeConfig::from_network_config(&rustiplayer_config::NetworkConfig {
            index_fingerprint_sample_kb: 1,
            ..rustiplayer_config::NetworkConfig::default()
        })
        .expect("source config built");

        let first_identity =
            build_local_file_index_identity(&file_path, &config).expect("first identity built");
        std::fs::write(&file_path, b"abcdeg").expect("sample file modified");
        let second_identity =
            build_local_file_index_identity(&file_path, &config).expect("second identity built");

        assert_eq!(first_identity.source_key, second_identity.source_key);
        assert_ne!(
            first_identity.partial_hash_hex,
            second_identity.partial_hash_hex
        );
    }
}
