use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Результат операций storage-слоя.
pub type StorageResult<T> = Result<T, StorageError>;

/// Ошибка SQLite storage-слоя с контекстом для log и UI.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Платформа не вернула стандартную директорию данных для текущего пользователя.
    #[error("не удалось определить пользовательскую data-директорию rustiplayer")]
    ProjectDirsUnavailable,

    /// Не удалось проверить путь к SQLite-файлу перед открытием.
    #[error("не удалось проверить SQLite-файл {path}: {source}")]
    InspectDatabaseFile {
        /// Путь, который проверял storage layer.
        path: PathBuf,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
    },

    /// По ожидаемому пути лежит директория или другой не-файл.
    #[error("ожидался SQLite-файл, но путь {path} не является обычным файлом")]
    DatabasePathIsNotFile {
        /// Некорректный путь к базе данных.
        path: PathBuf,
    },

    /// Не удалось создать директорию для SQLite-файла.
    #[error("не удалось создать директорию storage {path}: {source}")]
    CreateStorageDir {
        /// Директория, которую пытался создать storage layer.
        path: PathBuf,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
    },

    /// `rusqlite` не смог открыть database file.
    #[error("не удалось открыть SQLite database {path}: {source}")]
    OpenDatabase {
        /// Путь к SQLite-файлу.
        path: PathBuf,

        /// Ошибка rusqlite.
        #[source]
        source: rusqlite::Error,
    },

    /// Не удалось настроить connection-level PRAGMA.
    #[error("не удалось настроить SQLite connection: {source}")]
    ConfigureConnection {
        /// Ошибка rusqlite.
        #[source]
        source: rusqlite::Error,
    },

    /// Не удалось прочитать `PRAGMA user_version`.
    #[error("не удалось прочитать версию SQLite schema: {source}")]
    ReadSchemaVersion {
        /// Ошибка rusqlite.
        #[source]
        source: rusqlite::Error,
    },

    /// Версия schema лежит вне поддерживаемого диапазона.
    #[error("SQLite schema version {version} лежит вне поддерживаемого диапазона")]
    SchemaVersionOutOfRange {
        /// Некорректное значение из `PRAGMA user_version`.
        version: i64,
    },

    /// База создана более новой версией приложения.
    #[error(
        "SQLite schema version {database_version} новее поддерживаемой версии {supported_version}; downgrade не поддерживается"
    )]
    UnsupportedSchemaVersion {
        /// Версия schema в базе.
        database_version: u32,

        /// Максимальная версия schema в текущем бинаре.
        supported_version: u32,
    },

    /// Список migrations в коде нарушает строгую последовательность версий.
    #[error("некорректный порядок storage migrations: {previous_version} -> {next_version}")]
    InvalidMigrationOrder {
        /// Предыдущая версия migration.
        previous_version: u32,

        /// Следующая версия migration.
        next_version: u32,
    },

    /// Конкретная migration упала; транзакция должна откатить все её изменения.
    #[error("не удалось применить migration {version} ({description}): {source}")]
    MigrationFailed {
        /// Целевая версия migration.
        version: u32,

        /// Короткое описание migration для диагностики.
        description: &'static str,

        /// Ошибка rusqlite.
        #[source]
        source: rusqlite::Error,
    },

    /// Не удалось закоммитить транзакцию migrations.
    #[error("не удалось закоммитить storage migrations: {source}")]
    CommitMigrations {
        /// Ошибка rusqlite.
        #[source]
        source: rusqlite::Error,
    },

    /// Некорректное значение в публичном storage API.
    #[error("некорректное значение storage-поля `{field}`: {message}")]
    InvalidValue {
        /// Имя поля storage model.
        field: &'static str,

        /// Объяснение ограничения.
        message: String,
    },

    /// Ошибка конкретной SQLite операции после успешной migration.
    #[error("SQLite operation `{operation}` failed: {source}")]
    DatabaseOperation {
        /// Короткое имя операции для log.
        operation: &'static str,

        /// Ошибка rusqlite.
        #[source]
        source: rusqlite::Error,
    },
}

impl StorageError {
    /// Создаёт ошибку для обычной SQL-операции после открытия базы.
    pub(crate) fn database(operation: &'static str, source: rusqlite::Error) -> Self {
        Self::DatabaseOperation { operation, source }
    }

    /// Создаёт validation error с русским объяснением ограничения.
    pub(crate) fn invalid_value(field: &'static str, message: impl Into<String>) -> Self {
        Self::InvalidValue {
            field,
            message: message.into(),
        }
    }
}
