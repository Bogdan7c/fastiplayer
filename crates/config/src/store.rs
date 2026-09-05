use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::info;

use crate::{AppConfig, ConfigError, ConfigPaths, ConfigResult};

mod atomic;
mod migrations;

#[cfg(test)]
mod tests;

/// Config, загруженный из user path или созданный из defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    /// Валидированная конфигурация приложения.
    pub config: AppConfig,

    /// Путь, из которого config прочитан или куда был записан.
    pub path: PathBuf,

    /// `true`, если файла не было и crate создал defaults.
    pub created: bool,
}

/// Загружает config из стандартного user path или создаёт default-файл.
pub fn load_or_create() -> ConfigResult<LoadedConfig> {
    let paths = ConfigPaths::discover()?;
    load_or_create_at(paths.config_file)
}

/// Загружает config из конкретного пути или создаёт default-файл.
pub fn load_or_create_at(path: impl AsRef<Path>) -> ConfigResult<LoadedConfig> {
    let path = path.as_ref().to_path_buf();

    match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => load_existing_config(path, false),
        Ok(_) => Err(ConfigError::ConfigPathIsNotFile { path }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => create_default_config(path),
        Err(source) => Err(ConfigError::InspectConfigFile { path, source }),
    }
}

/// Загружает существующий config без попытки создать defaults.
pub fn load_from_path(path: impl AsRef<Path>) -> ConfigResult<LoadedConfig> {
    let path = path.as_ref().to_path_buf();
    load_existing_config(path, false)
}

/// Валидирует config и атомарно заменяет TOML-файл сгенерированным pretty TOML.
pub fn save_validated_atomic_at(path: impl AsRef<Path>, config: &AppConfig) -> ConfigResult<()> {
    atomic::save_validated(path.as_ref(), config)
}

/// Читает, парсит и валидирует существующий config-файл.
fn load_existing_config(path: PathBuf, created: bool) -> ConfigResult<LoadedConfig> {
    let toml_text = fs::read_to_string(&path).map_err(|source| ConfigError::ReadConfigFile {
        path: path.clone(),
        source,
    })?;
    let config = parse_config_text(&path, &toml_text)?;

    Ok(LoadedConfig {
        config,
        path,
        created,
    })
}

/// Создаёт default config в новом файле и возвращает уже валидированную структуру.
fn create_default_config(path: PathBuf) -> ConfigResult<LoadedConfig> {
    atomic::create_parent_dir_if_needed(&path)?;

    let config = AppConfig::default();
    config.validate()?;
    let toml_text = config.to_pretty_toml()?;

    match atomic::write_new_config_file(&path, &toml_text) {
        Ok(()) => {
            info!(path = %path.display(), "Создан default config fastiplayer");
            Ok(LoadedConfig {
                config,
                path,
                created: true,
            })
        }
        Err(ConfigError::CreateConfigFile { source, .. })
            if source.kind() == io::ErrorKind::AlreadyExists =>
        {
            load_existing_config(path, false)
        }
        Err(error) => Err(error),
    }
}

/// Превращает TOML text в validated `AppConfig`.
fn parse_config_text(path: &Path, toml_text: &str) -> ConfigResult<AppConfig> {
    let mut toml_document = toml::from_str::<toml::Value>(toml_text).map_err(|source| {
        ConfigError::ParseConfigFile {
            path: path.to_path_buf(),
            source,
        }
    })?;

    migrations::normalize_document(&mut toml_document);

    let mut config: AppConfig =
        toml_document
            .try_into()
            .map_err(|source| ConfigError::ParseConfigFile {
                path: path.to_path_buf(),
                source,
            })?;

    migrations::upgrade_config(&mut config);

    config
        .validate()
        .map_err(|source| ConfigError::ValidateConfigFile {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
    Ok(config)
}
