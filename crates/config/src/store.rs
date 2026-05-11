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
    use crate::{HdrToSdrOperatorConfig, ToneMappingMode};

    /// Проверяет, что default schema остаётся самосогласованной.
    #[test]
    fn default_config_is_valid() {
        AppConfig::default()
            .validate()
            .expect("default config valid");
    }

    /// Проверяет, что render.color_adjustment defaults являются identity.
    #[test]
    fn render_color_adjustment_defaults_are_identity() {
        let config = AppConfig::default();

        assert!(config.render.color_adjustment.is_identity());
        assert_eq!(config.render.color_adjustment.brightness, 0.0);
        assert_eq!(config.render.color_adjustment.contrast, 1.0);
        assert_eq!(config.render.color_adjustment.saturation, 1.0);
        assert_eq!(config.render.color_adjustment.exposure, 0.0);
        assert_eq!(config.render.color_adjustment.rgb_gain, [1.0, 1.0, 1.0]);
        assert_eq!(config.render.color_adjustment.rgb_offset, [0.0, 0.0, 0.0]);
    }

    /// Проверяет documented HDR-to-SDR defaults для Phase 10.
    #[test]
    fn render_hdr_to_sdr_defaults_are_valid_phase10_baseline() {
        let config = AppConfig::default();

        assert!(config.render.hdr_to_sdr.enabled);
        assert_eq!(
            config.render.hdr_to_sdr.operator,
            HdrToSdrOperatorConfig::Bt2446C
        );
        assert_eq!(config.render.hdr_to_sdr.sdr_reference_white_nits, 100.0);
        assert_eq!(config.render.hdr_to_sdr.hdr_reference_peak_nits, 1_000.0);
        assert_eq!(config.render.tone_mapping, ToneMappingMode::Disabled);
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
        assert!(loaded.config.render.color_adjustment.is_identity());

        let created_toml = fs::read_to_string(&loaded.path).expect("created config readable");
        assert!(created_toml.contains("[render.hdr_to_sdr]"));
        assert!(created_toml.contains("operator = \"bt2446_c\""));
    }

    /// Проверяет, что старый config без color_adjustment получает identity defaults.
    #[test]
    fn existing_config_without_color_adjustment_gets_identity_defaults() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 1

[render]
profile = "vulkan"
"#,
        )
        .expect("legacy config written");

        let loaded = load_from_path(&config_path).expect("legacy config accepted");

        assert!(loaded.config.render.color_adjustment.is_identity());
        assert!(loaded.config.render.hdr_to_sdr.enabled);
    }

    /// Проверяет compatibility-путь для старого scalar placeholder `render.hdr_to_sdr`.
    #[test]
    fn legacy_scalar_hdr_to_sdr_defaults_to_phase10_table_config() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 1

[render]
profile = "vulkan"
hdr_to_sdr = false
"#,
        )
        .expect("legacy config written");

        let loaded = load_from_path(&config_path).expect("legacy scalar accepted");

        assert_eq!(loaded.config.render.hdr_to_sdr, Default::default());
    }

    /// Проверяет, что alternative tone mapping operator не проходит TOML-схему.
    #[test]
    fn invalid_hdr_to_sdr_operator_is_rejected() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 1

[render.hdr_to_sdr]
operator = "reinhard"
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid operator rejected");

        assert!(error.to_string().contains("TOML-схеме"));
    }

    /// Проверяет validation для нулевого SDR reference white.
    #[test]
    fn invalid_hdr_to_sdr_sdr_white_nits_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 1

[render.hdr_to_sdr]
sdr_reference_white_nits = 0.0
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid SDR white rejected");

        assert!(
            error
                .to_string()
                .contains("render.hdr_to_sdr.sdr_reference_white_nits")
        );
    }

    /// Проверяет validation для HDR peak, который не выше SDR reference white.
    #[test]
    fn invalid_hdr_to_sdr_peak_nits_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 1

[render.hdr_to_sdr]
sdr_reference_white_nits = 100.0
hdr_reference_peak_nits = 100.0
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid HDR peak rejected");

        assert!(
            error
                .to_string()
                .contains("render.hdr_to_sdr.hdr_reference_peak_nits")
        );
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

    /// Проверяет validation error для RGB-массива неверной длины.
    #[test]
    fn invalid_rgb_gain_array_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 1

[render.color_adjustment]
rgb_gain = [1.0, 1.0]
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid rgb_gain rejected");

        assert!(
            error
                .to_string()
                .contains("render.color_adjustment.rgb_gain")
        );
    }

    /// Проверяет validation error для RGB offset неверной длины.
    #[test]
    fn invalid_rgb_offset_array_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 1

[render.color_adjustment]
rgb_offset = [0.0, 0.0, 0.0, 0.0]
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid rgb_offset rejected");

        assert!(
            error
                .to_string()
                .contains("render.color_adjustment.rgb_offset")
        );
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
