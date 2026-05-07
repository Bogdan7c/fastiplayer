use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tracing::info;

use crate::{AppConfig, ConfigError, ConfigPaths, ConfigResult};

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
    create_parent_dir_if_needed(&path)?;

    let config = AppConfig::default();
    config.validate()?;
    let toml_text = config.to_pretty_toml()?;

    match write_new_config_file(&path, &toml_text) {
        Ok(()) => {
            info!(path = %path.display(), "Создан default config rustiplayer");
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

/// Создаёт директорию config-файла, если путь имеет parent directory.
fn create_parent_dir_if_needed(path: &Path) -> ConfigResult<()> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };

    fs::create_dir_all(parent).map_err(|source| ConfigError::CreateConfigDir {
        path: parent.to_path_buf(),
        source,
    })
}

/// Пишет новый config через `create_new`, чтобы не перетереть пользовательский файл.
fn write_new_config_file(path: &Path, toml_text: &str) -> ConfigResult<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| ConfigError::CreateConfigFile {
            path: path.to_path_buf(),
            source,
        })?;

    file.write_all(toml_text.as_bytes())
        .map_err(|source| ConfigError::WriteConfigFile {
            path: path.to_path_buf(),
            source,
        })
}

/// Превращает TOML text в validated `AppConfig`.
fn parse_config_text(path: &Path, toml_text: &str) -> ConfigResult<AppConfig> {
    let config =
        toml::from_str::<AppConfig>(toml_text).map_err(|source| ConfigError::ParseConfigFile {
            path: path.to_path_buf(),
            source,
        })?;

    config
        .validate()
        .map_err(|source| ConfigError::ValidateConfigFile {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что default schema остаётся самосогласованной.
    #[test]
    fn default_config_is_valid() {
        AppConfig::default()
            .validate()
            .expect("default config valid");
    }

    /// Проверяет первый запуск без существующего config-файла.
    #[test]
    fn missing_config_is_created_with_defaults() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("rustiplayer").join("config.toml");

        let loaded = load_or_create_at(&config_path).expect("default config created");

        assert!(loaded.created);
        assert_eq!(loaded.path, config_path);
        assert_eq!(loaded.config, AppConfig::default());
        assert!(loaded.path.exists());
    }

    /// Проверяет понятную ошибку validation для некорректной громкости.
    #[test]
    fn invalid_volume_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 1

[audio]
volume = 1.5
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid volume rejected");

        assert!(error.to_string().contains("audio.volume"));
    }

    /// Проверяет отказ от неподдержанной версии схемы.
    #[test]
    fn unsupported_schema_version_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(&config_path, "schema_version = 999\n").expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("schema version rejected");

        assert!(error.to_string().contains("schema_version"));
    }

    /// Проверяет, что неизвестные поля не игнорируются молча.
    #[test]
    fn unknown_field_is_parse_error() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 1
unexpected = true
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("unknown field rejected");

        assert!(error.to_string().contains("TOML-схеме"));
    }
}
