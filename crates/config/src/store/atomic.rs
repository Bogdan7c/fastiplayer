use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::{AppConfig, ConfigError, ConfigResult};

/// Сколько разных имён временного файла пробуем до явной ошибки коллизии.
const MAX_TEMP_CONFIG_CREATE_ATTEMPTS: u32 = 32;

/// Валидирует конфигурацию и атомарно заменяет целевой TOML-файл.
pub(super) fn save_validated(path: &Path, config: &AppConfig) -> ConfigResult<()> {
    let toml_text = prepare_validated_toml_for_save(path, config)?;
    create_parent_dir_if_needed(path)?;

    let (temp_path, mut temp_file) = create_config_temp_file(path)?;
    let write_result = write_and_sync_temp_config(&temp_path, &mut temp_file, &toml_text);
    // На Windows открытый handle может мешать rename, поэтому закрываем его явно.
    drop(temp_file);

    if let Err(error) = write_result {
        remove_temp_config_after_error(&temp_path);
        return Err(error);
    }
    if let Err(error) = rename_temp_config(&temp_path, path) {
        remove_temp_config_after_error(&temp_path);
        return Err(error);
    }

    sync_parent_directory_best_effort(path);
    info!(path = %path.display(), "Сохранён config rustiplayer через atomic rename");
    Ok(())
}

/// Создаёт parent directory для нового или заменяемого config-файла.
pub(super) fn create_parent_dir_if_needed(path: &Path) -> ConfigResult<()> {
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

/// Создаёт новый config без риска перезаписать появившийся параллельно файл.
pub(super) fn write_new_config_file(path: &Path, toml_text: &str) -> ConfigResult<()> {
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

fn prepare_validated_toml_for_save(path: &Path, config: &AppConfig) -> ConfigResult<String> {
    config
        .validate()
        .map_err(|source| ConfigError::ValidateConfigFile {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
    let toml_text = config.to_pretty_toml()?;
    let reparsed = toml::from_str::<AppConfig>(&toml_text).map_err(|source| {
        ConfigError::ParseSerializedConfig {
            path: path.to_path_buf(),
            source,
        }
    })?;
    if reparsed != *config {
        return Err(ConfigError::SerializedConfigRoundtripMismatch {
            path: path.to_path_buf(),
        });
    }
    Ok(toml_text)
}

fn create_config_temp_file(path: &Path) -> ConfigResult<(PathBuf, fs::File)> {
    for attempt in 0..MAX_TEMP_CONFIG_CREATE_ATTEMPTS {
        let temp_path = config_temp_path(path, attempt);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(ConfigError::CreateConfigTempFile {
                    path: temp_path,
                    source,
                });
            }
        }
    }
    Err(ConfigError::CreateConfigTempFile {
        path: config_temp_path(path, MAX_TEMP_CONFIG_CREATE_ATTEMPTS),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "все candidate имена временного config-файла уже существуют",
        ),
    })
}

fn config_temp_path(path: &Path, attempt: u32) -> PathBuf {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("config.toml"));
    let mut temp_file_name = OsString::from(".");
    temp_file_name.push(file_name);
    temp_file_name.push(format!(".{}.{}.tmp", std::process::id(), attempt));
    parent.join(temp_file_name)
}

fn write_and_sync_temp_config(
    temp_path: &Path,
    temp_file: &mut fs::File,
    toml_text: &str,
) -> ConfigResult<()> {
    temp_file
        .write_all(toml_text.as_bytes())
        .map_err(|source| ConfigError::WriteConfigTempFile {
            path: temp_path.to_path_buf(),
            source,
        })?;
    temp_file
        .flush()
        .map_err(|source| ConfigError::FlushConfigTempFile {
            path: temp_path.to_path_buf(),
            source,
        })?;
    temp_file
        .sync_all()
        .map_err(|source| ConfigError::SyncConfigTempFile {
            path: temp_path.to_path_buf(),
            source,
        })
}

fn rename_temp_config(temp_path: &Path, target_path: &Path) -> ConfigResult<()> {
    fs::rename(temp_path, target_path).map_err(|source| ConfigError::RenameConfigFile {
        source_path: temp_path.to_path_buf(),
        target_path: target_path.to_path_buf(),
        source,
    })
}

fn remove_temp_config_after_error(temp_path: &Path) {
    match fs::remove_file(temp_path) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            warn!(path = %temp_path.display(), error = %source, "Не удалось удалить временный config после ошибки save")
        }
    }
}

fn sync_parent_directory_best_effort(path: &Path) {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return;
    };
    match OpenOptions::new().read(true).open(parent) {
        Ok(parent_directory) => {
            if let Err(source) = parent_directory.sync_all() {
                warn!(path = %parent.display(), error = %source, "Не удалось sync директорию config после atomic rename");
            }
        }
        Err(source) => {
            warn!(path = %parent.display(), error = %source, "Не удалось открыть директорию config для best-effort sync")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_schema_toml_roundtrip_is_textually_stable() {
        let expected_current_toml = include_str!("../../tests/fixtures/current_schema_v5.toml");
        let generated_current_toml = AppConfig::default()
            .to_pretty_toml()
            .expect("serialize current defaults");
        let parsed: AppConfig =
            toml::from_str(expected_current_toml).expect("parse golden current schema");
        let roundtripped_toml = parsed
            .to_pretty_toml()
            .expect("serialize parsed current schema");

        assert_eq!(generated_current_toml, expected_current_toml);
        assert_eq!(roundtripped_toml, expected_current_toml);
        assert_eq!(parsed, AppConfig::default());
    }
}
