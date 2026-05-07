use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Результат операций с пользовательской TOML-конфигурацией.
pub type ConfigResult<T> = Result<T, ConfigError>;

/// Ошибка config-слоя с сообщением, которое можно показать пользователю или записать в log.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Платформа не вернула стандартный config-dir для текущего пользователя.
    #[error("не удалось определить пользовательскую config-директорию rustiplayer")]
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

    /// Не удалось создать новый default config.
    #[error("не удалось создать default config {path}: {source}")]
    CreateConfigFile {
        /// Путь к создаваемому config-файлу.
        path: PathBuf,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
    },

    /// Не удалось записать содержимое config-файла.
    #[error("не удалось записать default config {path}: {source}")]
    WriteConfigFile {
        /// Путь к записываемому config-файлу.
        path: PathBuf,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
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

    /// Default config не удалось сериализовать в TOML.
    #[error("не удалось сериализовать default config: {source}")]
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
