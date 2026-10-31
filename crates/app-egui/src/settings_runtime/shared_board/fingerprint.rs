//! «Отпечаток» содержимого config-файла без чтения байтов.
//!
//! Нужен, чтобы не зациклиться на одном и том же содержимом: если файл соседа нельзя
//! принять (битый, от новой версии, маршрут отказал), копия запоминает отпечаток и на
//! повторные события того же содержимого больше не реагирует. Новая запись — новый
//! отпечаток и новая попытка.
//!
//! Отпечаток — метаданные файла (`stat`), снятые ДО чтения: атомарная запись создаёт
//! новый inode, правка на месте меняет время изменения. Если файл поменялся после снятия
//! отпечатка, придёт новое событие слежения, и копия посмотрит ещё раз.

use std::fs;
use std::path::Path;

/// Отпечаток содержимого config-файла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ConfigFileFingerprint {
    /// Устройство и inode: атомарная запись (temp + rename) всегда даёт новый inode.
    #[cfg(unix)]
    device_and_inode: (u64, u64),
    /// Длина файла в байтах.
    length: u64,
    /// Время изменения содержимого (ns; на Unix — ещё и время изменения inode).
    modified: (i128, i128),
}

impl ConfigFileFingerprint {
    /// Снимает отпечаток; `None` — файла нет или его метаданные не читаются.
    pub(super) fn of(config_file: &Path) -> Option<Self> {
        let metadata = fs::metadata(config_file).ok()?;
        Some(Self::from_metadata(&metadata))
    }

    #[cfg(unix)]
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;

        let nanoseconds =
            |seconds: i64, nanos: i64| i128::from(seconds) * 1_000_000_000 + i128::from(nanos);
        Self {
            device_and_inode: (metadata.dev(), metadata.ino()),
            length: metadata.size(),
            modified: (
                nanoseconds(metadata.mtime(), metadata.mtime_nsec()),
                nanoseconds(metadata.ctime(), metadata.ctime_nsec()),
            ),
        }
    }

    #[cfg(not(unix))]
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since_epoch| since_epoch.as_nanos() as i128);
        Self {
            length: metadata.len(),
            modified: (modified, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_content_keeps_the_fingerprint_and_a_rewrite_changes_it() {
        let directory = tempfile::tempdir().expect("tempdir");
        let config_file = directory.path().join("config.toml");
        assert_eq!(ConfigFileFingerprint::of(&config_file), None);

        fs::write(&config_file, b"broken = [").expect("write");
        let first = ConfigFileFingerprint::of(&config_file).expect("fingerprint");
        assert_eq!(ConfigFileFingerprint::of(&config_file), Some(first));

        let replacement = directory.path().join("replacement.toml");
        fs::write(&replacement, b"broken = [").expect("write replacement");
        fs::rename(&replacement, &config_file).expect("atomic replace");

        assert_ne!(
            ConfigFileFingerprint::of(&config_file),
            Some(first),
            "an atomic rewrite with the same bytes is a new attempt"
        );
    }
}
