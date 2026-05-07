use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tracing::info;

use crate::{
    CapabilityCacheEntry, MigrationReport, OpenedMediaRecord, PlaybackHistoryEntry,
    PlaybackProgressEntry, StorageError, StoragePaths, StorageResult, migrations,
};

/// Storage, открытый приложением на старте.
pub struct OpenedStorage {
    /// Активное SQLite соединение.
    pub connection: StorageConnection,

    /// Путь к SQLite-файлу.
    pub path: PathBuf,

    /// Отчёт о migrations, выполненных при открытии.
    pub migration_report: MigrationReport,
}

/// Тонкая production-обёртка над `rusqlite::Connection`.
pub struct StorageConnection {
    /// Единственное соединение с SQLite в текущем Phase 6.
    connection: Connection,
}

impl StorageConnection {
    /// Записывает или обновляет запись истории открытия media.
    pub fn record_opened_media(&mut self, opened_media: &OpenedMediaRecord) -> StorageResult<()> {
        crate::playback_history::record_opened_media(&mut self.connection, opened_media)
    }

    /// Возвращает последние записи истории открытия.
    pub fn recent_history(&self, limit: usize) -> StorageResult<Vec<PlaybackHistoryEntry>> {
        crate::playback_history::recent_history(&self.connection, limit)
    }

    /// Записывает последнюю известную позицию воспроизведения.
    pub fn save_playback_progress(
        &mut self,
        playback_progress: &PlaybackProgressEntry,
    ) -> StorageResult<()> {
        crate::playback_progress::save_playback_progress(&mut self.connection, playback_progress)
    }

    /// Загружает прогресс для конкретного media source.
    pub fn load_playback_progress(
        &self,
        source_uri: &str,
    ) -> StorageResult<Option<PlaybackProgressEntry>> {
        crate::playback_progress::load_playback_progress(&self.connection, source_uri)
    }

    /// Записывает cache entry с результатами capability probing.
    pub fn save_capability_cache_entry(
        &mut self,
        capability_cache_entry: &CapabilityCacheEntry,
    ) -> StorageResult<()> {
        crate::capability_cache::save_capability_cache_entry(
            &mut self.connection,
            capability_cache_entry,
        )
    }

    /// Загружает cache entry с результатами capability probing.
    pub fn load_capability_cache_entry(
        &self,
        cache_key: &str,
    ) -> StorageResult<Option<CapabilityCacheEntry>> {
        crate::capability_cache::load_capability_cache_entry(&self.connection, cache_key)
    }

    /// Удаляет устаревшие capability cache записи.
    pub fn delete_expired_capability_cache(
        &mut self,
        current_unix_seconds: i64,
    ) -> StorageResult<usize> {
        crate::capability_cache::delete_expired_capability_cache(
            &mut self.connection,
            current_unix_seconds,
        )
    }
}

/// Открывает SQLite storage из стандартного platform data path.
pub fn open_or_create() -> StorageResult<OpenedStorage> {
    let paths = StoragePaths::discover()?;
    open_or_create_at(paths.database_file)
}

/// Открывает SQLite storage по конкретному пути.
pub fn open_or_create_at(path: impl AsRef<Path>) -> StorageResult<OpenedStorage> {
    let path = path.as_ref().to_path_buf();

    ensure_database_path_is_usable(&path)?;
    create_parent_dir_if_needed(&path)?;

    let mut connection = Connection::open(&path).map_err(|source| StorageError::OpenDatabase {
        path: path.clone(),
        source,
    })?;
    configure_connection(&connection)?;
    let migration_report = migrations::migrate(&mut connection)?;

    info!(
        path = %path.display(),
        schema_version = migration_report.current_version,
        applied_migrations = migration_report.applied_migrations.len(),
        "SQLite storage rustiplayer готов"
    );

    Ok(OpenedStorage {
        connection: StorageConnection { connection },
        path,
        migration_report,
    })
}

/// Проверяет, что существующий путь не является директорией.
fn ensure_database_path_is_usable(path: &Path) -> StorageResult<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(StorageError::DatabasePathIsNotFile {
            path: path.to_path_buf(),
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StorageError::InspectDatabaseFile {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Создаёт директорию SQLite-файла, если она задана в пути.
fn create_parent_dir_if_needed(path: &Path) -> StorageResult<()> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };

    fs::create_dir_all(parent).map_err(|source| StorageError::CreateStorageDir {
        path: parent.to_path_buf(),
        source,
    })
}

/// Настраивает connection-level параметры SQLite перед migrations.
fn configure_connection(connection: &Connection) -> StorageResult<()> {
    connection
        .execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            PRAGMA journal_mode = WAL;
            "#,
        )
        .map_err(|source| StorageError::ConfigureConnection { source })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет создание SQLite-файла и parent directory.
    #[test]
    fn open_or_create_at_creates_database_file() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let database_path = temp_dir.path().join("nested").join("rustiplayer.sqlite");

        let opened_storage = open_or_create_at(&database_path).expect("storage opened");

        assert_eq!(opened_storage.path, database_path);
        assert!(opened_storage.path.exists());
        assert_eq!(
            opened_storage.migration_report.current_version,
            migrations::CURRENT_STORAGE_SCHEMA_VERSION
        );
    }

    /// Проверяет отказ, если database path указывает на директорию.
    #[test]
    fn database_path_directory_is_rejected() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");

        let error = match open_or_create_at(temp_dir.path()) {
            Ok(_) => panic!("directory should be rejected"),
            Err(error) => error,
        };

        assert!(matches!(error, StorageError::DatabasePathIsNotFile { .. }));
    }
}
