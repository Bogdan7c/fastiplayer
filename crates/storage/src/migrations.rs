use rusqlite::{Connection, Transaction};

use crate::{StorageError, StorageResult};

/// Текущая версия SQLite schema storage-слоя.
pub const CURRENT_STORAGE_SCHEMA_VERSION: u32 = 1;

/// Описание migration, уже применённой к базе.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    /// Версия schema после применения migration.
    pub version: u32,

    /// Короткое описание изменения schema.
    pub description: &'static str,
}

/// Итог запуска migration framework.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// Версия schema до запуска migrations.
    pub original_version: u32,

    /// Версия schema после успешного commit.
    pub current_version: u32,

    /// Список migrations, применённых в текущем запуске.
    pub applied_migrations: Vec<AppliedMigration>,
}

/// Внутреннее описание migration.
struct Migration {
    /// Целевая версия schema.
    version: u32,

    /// Человекочитаемое описание для log/error.
    description: &'static str,

    /// Функция, выполняющая SQL внутри общей transaction.
    apply: fn(&Transaction<'_>) -> rusqlite::Result<()>,
}

/// Список migrations в порядке применения.
const MIGRATIONS: &[Migration] = &[Migration {
    version: CURRENT_STORAGE_SCHEMA_VERSION,
    description: "initial history/progress/capability cache schema",
    apply: apply_initial_schema,
}];

/// Возвращает текущую версию schema из `PRAGMA user_version`.
pub fn current_schema_version(connection: &Connection) -> StorageResult<u32> {
    let raw_version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(|source| StorageError::ReadSchemaVersion { source })?;

    if raw_version < 0 || raw_version > i64::from(u32::MAX) {
        return Err(StorageError::SchemaVersionOutOfRange {
            version: raw_version,
        });
    }

    Ok(raw_version as u32)
}

/// Применяет все migrations, которых ещё нет в database file.
pub(crate) fn migrate(connection: &mut Connection) -> StorageResult<MigrationReport> {
    migrate_with_migrations(connection, MIGRATIONS)
}

/// Применяет переданный список migrations одной transaction.
fn migrate_with_migrations(
    connection: &mut Connection,
    migrations: &[Migration],
) -> StorageResult<MigrationReport> {
    validate_migration_order(migrations)?;

    let original_version = current_schema_version(connection)?;
    let supported_version = migrations
        .last()
        .map(|migration| migration.version)
        .unwrap_or(0);

    if original_version > supported_version {
        return Err(StorageError::UnsupportedSchemaVersion {
            database_version: original_version,
            supported_version,
        });
    }

    let pending_migrations = migrations
        .iter()
        .filter(|migration| migration.version > original_version)
        .collect::<Vec<_>>();

    if pending_migrations.is_empty() {
        return Ok(MigrationReport {
            original_version,
            current_version: original_version,
            applied_migrations: Vec::new(),
        });
    }

    let transaction = connection
        .transaction()
        .map_err(|source| StorageError::database("begin storage migrations transaction", source))?;
    let mut applied_migrations = Vec::with_capacity(pending_migrations.len());

    for migration in pending_migrations {
        (migration.apply)(&transaction).map_err(|source| StorageError::MigrationFailed {
            version: migration.version,
            description: migration.description,
            source,
        })?;
        set_schema_version(&transaction, migration.version).map_err(|source| {
            StorageError::MigrationFailed {
                version: migration.version,
                description: migration.description,
                source,
            }
        })?;
        applied_migrations.push(AppliedMigration {
            version: migration.version,
            description: migration.description,
        });
    }

    let current_version = applied_migrations
        .last()
        .map(|migration| migration.version)
        .unwrap_or(original_version);

    transaction
        .commit()
        .map_err(|source| StorageError::CommitMigrations { source })?;

    Ok(MigrationReport {
        original_version,
        current_version,
        applied_migrations,
    })
}

/// Проверяет, что migrations идут строго по возрастанию.
fn validate_migration_order(migrations: &[Migration]) -> StorageResult<()> {
    let mut previous_version = 0;

    for migration in migrations {
        if migration.version <= previous_version {
            return Err(StorageError::InvalidMigrationOrder {
                previous_version,
                next_version: migration.version,
            });
        }

        previous_version = migration.version;
    }

    Ok(())
}

/// Записывает новую версию schema в `PRAGMA user_version`.
fn set_schema_version(transaction: &Transaction<'_>, version: u32) -> rusqlite::Result<()> {
    transaction.execute_batch(&format!("PRAGMA user_version = {version};"))
}

/// Создаёт базовые таблицы Phase 6.
fn apply_initial_schema(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE playback_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_uri TEXT NOT NULL UNIQUE,
            title TEXT,
            duration_ms INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
            first_opened_at INTEGER NOT NULL,
            last_opened_at INTEGER NOT NULL,
            open_count INTEGER NOT NULL DEFAULT 1 CHECK (open_count > 0),
            updated_at INTEGER NOT NULL
        );

        CREATE INDEX playback_history_last_opened_at_idx
            ON playback_history(last_opened_at DESC);

        CREATE TABLE playback_progress (
            source_uri TEXT PRIMARY KEY,
            position_ms INTEGER NOT NULL CHECK (position_ms >= 0),
            duration_ms INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
            completed INTEGER NOT NULL DEFAULT 0 CHECK (completed IN (0, 1)),
            updated_at INTEGER NOT NULL
        );

        CREATE INDEX playback_progress_updated_at_idx
            ON playback_progress(updated_at DESC);

        CREATE TABLE capability_cache (
            cache_key TEXT PRIMARY KEY,
            backend TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            schema_version INTEGER NOT NULL CHECK (schema_version > 0),
            probed_at INTEGER NOT NULL,
            expires_at INTEGER
        );

        CREATE INDEX capability_cache_backend_idx
            ON capability_cache(backend);

        CREATE INDEX capability_cache_expires_at_idx
            ON capability_cache(expires_at);
        "#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что initial migration создаёт таблицы и выставляет user_version.
    #[test]
    fn initial_migration_creates_phase_6_tables() {
        let mut connection = Connection::open_in_memory().expect("in-memory sqlite opened");

        let report = migrate(&mut connection).expect("initial migration applied");

        assert_eq!(report.original_version, 0);
        assert_eq!(report.current_version, CURRENT_STORAGE_SCHEMA_VERSION);
        assert_eq!(report.applied_migrations.len(), 1);
        assert_eq!(
            current_schema_version(&connection).expect("schema version read"),
            CURRENT_STORAGE_SCHEMA_VERSION
        );
        assert!(table_exists(&connection, "playback_history"));
        assert!(table_exists(&connection, "playback_progress"));
        assert!(table_exists(&connection, "capability_cache"));
    }

    /// Проверяет, что ошибка второй migration откатывает первую.
    #[test]
    fn failed_migration_rolls_back_entire_batch() {
        let mut connection = Connection::open_in_memory().expect("in-memory sqlite opened");
        let migrations = [
            Migration {
                version: 1,
                description: "create temporary table",
                apply: |transaction| {
                    transaction.execute_batch("CREATE TABLE rollback_probe (id INTEGER);")
                },
            },
            Migration {
                version: 2,
                description: "broken migration",
                apply: |transaction| transaction.execute_batch("CREATE TABLE broken_sql ("),
            },
        ];

        let error = migrate_with_migrations(&mut connection, &migrations)
            .expect_err("broken migration rejected");

        assert!(matches!(
            error,
            StorageError::MigrationFailed { version: 2, .. }
        ));
        assert_eq!(
            current_schema_version(&connection).expect("schema version read"),
            0
        );
        assert!(!table_exists(&connection, "rollback_probe"));
    }

    /// Проверяет защиту от запуска старого бинаря на новой database schema.
    #[test]
    fn newer_database_version_is_rejected() {
        let mut connection = Connection::open_in_memory().expect("in-memory sqlite opened");
        connection
            .execute_batch("PRAGMA user_version = 999;")
            .expect("schema version set");

        let error = migrate(&mut connection).expect_err("downgrade rejected");

        assert!(matches!(
            error,
            StorageError::UnsupportedSchemaVersion {
                database_version: 999,
                supported_version: CURRENT_STORAGE_SCHEMA_VERSION
            }
        ));
    }

    /// Проверяет существование таблицы через sqlite catalog.
    fn table_exists(connection: &Connection, table_name: &str) -> bool {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [table_name],
                |row| row.get::<_, bool>(0),
            )
            .expect("table existence query works")
    }
}
