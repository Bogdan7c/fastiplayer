use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::info;

use crate::{AppConfig, ConfigError, ConfigPaths, ConfigResult};

mod atomic;
mod backup_name;
mod migrations;
mod outcome;
mod recovery;

pub use outcome::{
    BrokenConfigProblem, BrokenConfigRecovery, ConfigLoadOrigin, ConfigSaveOutcome,
    ConfigSavePolicy, ConfigSaveTarget, ConfigSessionOnlyReason, ConfigWriteProblem,
};
pub use recovery::load_or_recover_at;

#[cfg(test)]
mod tests;

/// Config, загруженный из user path или созданный из defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    /// Валидированная конфигурация приложения.
    pub config: AppConfig,

    /// Путь, из которого config прочитан или куда был записан.
    pub path: PathBuf,

    /// Откуда взялась конфигурация текущего запуска.
    pub origin: ConfigLoadOrigin,

    /// Можно ли в этот запуск сохранять изменения настроек в файл.
    pub save_policy: ConfigSavePolicy,
}

impl LoadedConfig {
    /// Цель сохранения настроек с тем правом записи, которое решено при загрузке.
    #[must_use]
    pub fn save_target(&self) -> ConfigSaveTarget {
        match &self.save_policy {
            ConfigSavePolicy::WriteToFile => ConfigSaveTarget::writable(self.path.clone()),
            ConfigSavePolicy::SessionOnly(reason) => {
                ConfigSaveTarget::session_only(self.path.clone(), reason.clone())
            }
        }
    }
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
        Ok(metadata) if metadata.is_file() => load_existing_config(path),
        Ok(_) => Err(ConfigError::ConfigPathIsNotFile { path }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => create_default_config(path),
        Err(source) => Err(ConfigError::InspectConfigFile { path, source }),
    }
}

/// Загружает существующий config без попытки создать defaults.
pub fn load_from_path(path: impl AsRef<Path>) -> ConfigResult<LoadedConfig> {
    let path = path.as_ref().to_path_buf();
    load_existing_config(path)
}

/// Валидирует config и атомарно заменяет TOML-файл сгенерированным pretty TOML.
pub fn save_validated_atomic_at(path: impl AsRef<Path>, config: &AppConfig) -> ConfigResult<()> {
    atomic::save_validated(path.as_ref(), config)
}

/// Читает, парсит и валидирует существующий config-файл.
fn load_existing_config(path: PathBuf) -> ConfigResult<LoadedConfig> {
    let toml_text = fs::read_to_string(&path).map_err(|source| ConfigError::ReadConfigFile {
        path: path.clone(),
        source,
    })?;
    let config = parse_config_text(&path, &toml_text)?;

    Ok(LoadedConfig {
        config,
        path,
        origin: ConfigLoadOrigin::LoadedExisting,
        save_policy: ConfigSavePolicy::WriteToFile,
    })
}

/// Создаёт default config атомарно и возвращает уже валидированную структуру.
///
/// Строгий путь: ошибка записи остаётся ошибкой. Мягкое поведение при старте
/// приложения — в `recovery::load_or_recover_at`.
fn create_default_config(path: PathBuf) -> ConfigResult<LoadedConfig> {
    let config = AppConfig::default();
    config.validate()?;
    let toml_text = config.to_pretty_toml()?;

    atomic::write_initial_config(&path, &toml_text)?;
    info!(path = %path.display(), "Создан default config fastiplayer");
    Ok(LoadedConfig {
        config,
        path,
        origin: ConfigLoadOrigin::CreatedDefault,
        save_policy: ConfigSavePolicy::WriteToFile,
    })
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
