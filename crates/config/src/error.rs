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
    #[error("не удалось определить пользовательскую config-директорию fastiplayer")]
    ProjectDirsUnavailable,

    /// Не удалось проверить состояние config-файла перед чтением или созданием.
    #[error("не удалось проверить config-файл {path}: {source}")]
    InspectConfigFile {
        /// Путь к config-файлу.
        path: PathBuf,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
    },

    /// По ожидаемому пути находится директория или другой не-файл.
    #[error("ожидался TOML-файл config, но путь {path} не является обычным файлом")]
    ConfigPathIsNotFile {
        /// Путь, который должен указывать на `config.toml`.
        path: PathBuf,
    },

    /// Не удалось создать директорию для config-файла.
    #[error("не удалось создать директорию config {path}: {source}")]
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
    #[error("не удалось записать config {path}: {failure}")]
    ReplaceConfigFile {
        /// Целевой config-файл.
        path: PathBuf,

        /// Этап протокола записи и безопасная причина.
        failure: ConfigWriteFailure,
    },

    /// Не удалось прочитать существующий config.
    #[error("не удалось прочитать config {path}: {source}")]
    ReadConfigFile {
        /// Путь к config-файлу.
        path: PathBuf,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
    },

    /// TOML синтаксически некорректен или не совпадает со schema structs.
    #[error("config {path} не соответствует TOML-схеме: {source}")]
    ParseConfigFile {
        /// Путь к config-файлу.
        path: PathBuf,

        /// Ошибка TOML/Serde deserialization.
        #[source]
        source: toml::de::Error,
    },

    /// Сгенерированный TOML не смог пройти roundtrip parse обратно в `AppConfig`.
    #[error("сгенерированный config {path} не проходит TOML roundtrip: {source}")]
    ParseSerializedConfig {
        /// Целевой путь config-файла, для которого готовился save.
        path: PathBuf,

        /// Ошибка TOML/Serde deserialization.
        #[source]
        source: toml::de::Error,
    },

    /// Сгенерированный TOML распарсился, но вернул другую структуру config.
    #[error("сгенерированный config {path} изменил значения при TOML roundtrip")]
    SerializedConfigRoundtripMismatch {
        /// Целевой путь config-файла, для которого готовился save.
        path: PathBuf,
    },

    /// Config не удалось сериализовать в TOML.
    #[error("не удалось сериализовать config: {source}")]
    SerializeDefaultConfig {
        /// Ошибка TOML/Serde serialization.
        #[source]
        source: toml::ser::Error,
    },

    /// Config прошёл parsing, но нарушил бизнес-правила validation.
    #[error("некорректное значение config-поля `{field}`: {message}")]
    InvalidValue {
        /// TOML-путь поля, например `audio.volume`.
        field: &'static str,

        /// Человекочитаемое объяснение ограничения.
        message: String,
    },

    /// Config-файл синтаксически корректен, но не прошёл validation.
    #[error("config {path} содержит некорректное значение: {source}")]
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
/// Обёртка нужна, чтобы сообщение об ошибке было по-русски и без имён Rust-типов:
/// `atomic-file-store` намеренно отдаёт только typed этап и класс I/O ошибки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigWriteFailure(pub AtomicFileWriteFailure);

impl fmt::Display for ConfigWriteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self.0.stage {
            AtomicFileWriteStage::ValidateTargetPath => "путь не указывает на файл",
            AtomicFileWriteStage::CreateTempFile => "не создан временный файл",
            AtomicFileWriteStage::WriteTempFile => "не записан временный файл",
            AtomicFileWriteStage::FlushTempFile => "не сброшен буфер временного файла",
            AtomicFileWriteStage::SyncTempFile => "временный файл не сохранён на диск",
            AtomicFileWriteStage::RenameTempFile => "временный файл не переименован в config",
        };
        match self.0.cause {
            AtomicFileWriteCause::Io(error_kind) => {
                write!(formatter, "{stage} ({})", io::Error::from(error_kind))
            }
            AtomicFileWriteCause::TempNameAttemptsExhausted => {
                write!(formatter, "{stage} (все имена временного файла заняты)")
            }
        }
    }
}
