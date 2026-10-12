use std::fmt;
use std::io;
use std::path::PathBuf;

use atomic_file_store::{AtomicFileWriteCause, AtomicFileWriteFailure, AtomicFileWriteStage};
use thiserror::Error;

/// Результат операций с пользовательской TOML-конфигурацией.
pub type ConfigResult<T> = Result<T, ConfigError>;

/// Ошибка config-слоя с сообщением, которое можно показать пользователю или записать в log.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Платформа не вернула стандартный config-dir для текущего пользователя.
    #[error("Failed to identify the user config directory of fastiplayer")]
    ProjectDirsUnavailable,

    /// Не удалось проверить состояние config-файла перед чтением или созданием.
    #[error("Failed to check the config file {path}: {source}")]
    InspectConfigFile {
        /// Путь к config-файлу.
        path: PathBuf,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
    },

    /// По ожидаемому пути находится директория или другой не-файл.
    #[error("A TOML config file was expected, but the {path} path is not a regular file.")]
    ConfigPathIsNotFile {
        /// Путь, который должен указывать на `config.toml`.
        path: PathBuf,
    },

    /// Не удалось создать директорию для config-файла.
    #[error("Could not create configuration directory {path}: {source}")]
    CreateConfigDir {
        /// Директория, которую пытались создать.
        path: PathBuf,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
    },

    /// Не удалось атомарно записать config-файл (первое создание defaults или save).
    ///
    /// Протокол записи принадлежит `atomic-file-store`: при этой ошибке прежний
    /// config-файл (если он был) не заменён и остаётся полным.
    #[error("Could not write configuration {path}: {failure}")]
    ReplaceConfigFile {
        /// Целевой config-файл.
        path: PathBuf,

        /// Этап протокола записи и безопасная причина.
        failure: ConfigWriteFailure,
    },

    /// Не удалось прочитать существующий config.
    #[error("Could not read configuration {path}: {source}")]
    ReadConfigFile {
        /// Путь к config-файлу.
        path: PathBuf,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
    },

    /// TOML синтаксически некорректен или не совпадает со schema structs.
    #[error("config {path} does not match the TOML schema: {source}")]
    ParseConfigFile {
        /// Путь к config-файлу.
        path: PathBuf,

        /// Ошибка TOML/Serde deserialization.
        #[source]
        source: toml::de::Error,
    },

    /// Сгенерированный TOML не смог пройти roundtrip parse обратно в `AppConfig`.
    #[error("generated config {path} does not pass TOML roundtrip: {source}")]
    ParseSerializedConfig {
        /// Целевой путь config-файла, для которого готовился save.
        path: PathBuf,

        /// Ошибка TOML/Serde deserialization.
        #[source]
        source: toml::de::Error,
    },

    /// Сгенерированный TOML распарсился, но вернул другую структуру config.
    #[error("The generated config {path} changed the values at TOML roundtrip")]
    SerializedConfigRoundtripMismatch {
        /// Целевой путь config-файла, для которого готовился save.
        path: PathBuf,
    },

    /// Config не удалось сериализовать в TOML.
    #[error("Could not serialize configuration: {source}")]
    SerializeDefaultConfig {
        /// Ошибка TOML/Serde serialization.
        #[source]
        source: toml::ser::Error,
    },

    /// Config прошёл parsing, но нарушил бизнес-правила validation.
    #[error("incorrect config field value {field}: {message}")]
    InvalidValue {
        /// TOML-путь поля, например `audio.volume`.
        field: &'static str,

        /// Человекочитаемое объяснение ограничения.
        message: String,
    },

    /// Config-файл синтаксически корректен, но не прошёл validation.
    #[error("config {path} contains an incorrect value: {source}")]
    ValidateConfigFile {
        /// Путь к config-файлу.
        path: PathBuf,

        /// Конкретная validation error с именем TOML-поля.
        #[source]
        source: Box<ConfigError>,
    },
}

/// Неудачная атомарная запись config-файла с человекочитаемым описанием этапа.
///
/// Обёртка нужна, чтобы сообщение об ошибке было по-английски и без имён Rust-типов:
/// `atomic-file-store` намеренно отдаёт только typed этап и класс I/O ошибки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigWriteFailure(pub AtomicFileWriteFailure);

impl fmt::Display for ConfigWriteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self.0.stage {
            AtomicFileWriteStage::ValidateTargetPath => "path doesn't point to file",
            AtomicFileWriteStage::CreateTempFile => "no temporary file created",
            AtomicFileWriteStage::WriteTempFile => "Temporary file was not written",
            AtomicFileWriteStage::FlushTempFile => "temporary file buffer was not flushed",
            AtomicFileWriteStage::SyncTempFile => "temporary file not saved to disk",
            AtomicFileWriteStage::RenameTempFile => "Temporary file is not renamed to config",
        };
        match self.0.cause {
            AtomicFileWriteCause::Io(error_kind) => {
                write!(formatter, "{stage} ({})", io::Error::from(error_kind))
            }
            AtomicFileWriteCause::TempNameAttemptsExhausted => {
                write!(formatter, "{stage} (all temporary file names are occupied)")
            }
        }
    }
}
