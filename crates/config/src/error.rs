use std::io;
use std::path::PathBuf;

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

    /// Не удалось создать временный файл рядом с целевым config-файлом.
    #[error("не удалось создать временный config-файл {path}: {source}")]
    CreateConfigTempFile {
        /// Путь временного файла.
        path: PathBuf,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
    },

    /// Не удалось записать весь сгенерированный TOML во временный файл.
    #[error("не удалось записать временный config-файл {path}: {source}")]
    WriteConfigTempFile {
        /// Путь временного файла.
        path: PathBuf,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
    },

    /// Не удалось сбросить userspace buffer временного config-файла.
    #[error("не удалось flush временного config-файла {path}: {source}")]
    FlushConfigTempFile {
        /// Путь временного файла.
        path: PathBuf,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
    },

    /// Не удалось синхронизировать временный config-файл с диском.
    #[error("не удалось sync временного config-файла {path}: {source}")]
    SyncConfigTempFile {
        /// Путь временного файла.
        path: PathBuf,

        /// Исходная I/O ошибка.
        #[source]
        source: io::Error,
    },

    /// Не удалось заменить целевой config временным файлом через rename.
    #[error(
        "не удалось атомарно заменить config {target_path} временным файлом {source_path}: {source}"
    )]
    RenameConfigFile {
        /// Временный файл, уже записанный и синхронизированный.
        source_path: PathBuf,

        /// Целевой config-файл.
        target_path: PathBuf,

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
