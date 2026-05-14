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
    use crate::{
        CURRENT_SCHEMA_VERSION, HdrToSdrOperatorConfig, PausedCommitBehavior, ToneMappingMode,
    };

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

    /// Проверяет defaults новой schema version 2.
    #[test]
    fn schema_version_2_defaults_include_seek_network_and_ui_skin() {
        let config = AppConfig::default();

        assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(CURRENT_SCHEMA_VERSION, 2);
        assert_eq!(config.player.seek.live_interval_ms, 100);
        assert_eq!(config.player.seek.live_preview_budget_ms, 100);
        assert_eq!(config.player.seek.commit_timeout_ms, 10_000);
        assert_eq!(config.player.seek.resume_audio_min_buffer_ms, 50);
        assert_eq!(config.player.seek.resume_video_min_ready_frames, 3);
        assert_eq!(
            config.player.seek.paused_commit_behavior,
            PausedCommitBehavior::StayPaused
        );
        assert_eq!(config.player.seek.hotkey_small_step_secs, 5);
        assert_eq!(config.player.seek.hotkey_large_step_secs, 30);
        assert_eq!(config.player.demux.max_consecutive_corrupted_packets, 64);
        assert_eq!(config.video.decoder_packet_channel_frames, 32);
        assert_eq!(config.video.decoder_frame_channel_frames, 8);
        assert_eq!(config.video.decoder_ready_queue_frames, 8);
        assert_eq!(config.video.decoder_surface_pool_frames, 24);
        assert_eq!(config.video.zero_copy_surface_pool_slots, 24);
        assert_eq!(config.video.scheduler.demux_packets_per_tick, 12);
        assert_eq!(config.video.scheduler.video_packets_per_tick, 8);
        assert_eq!(config.video.scheduler.decoded_frames_per_tick, 8);
        assert_eq!(config.video.scheduler.catch_up_budget_ms, 4);
        assert_eq!(config.video.scheduler.present_queue_min_frames, 2);
        assert_eq!(config.video.scheduler.present_queue_target_frames, 4);
        assert_eq!(config.video.scheduler.decode_ahead_target_ms, 250);
        assert_eq!(config.video.scheduler.surface_free_slots_min, 2);
        assert_eq!(config.video.scheduler.surface_free_slots_target, 4);
        assert_eq!(config.network.memory_cache_mb, 128);
        assert_eq!(config.network.read_ahead_mb, 64);
        assert_eq!(config.network.connect_timeout_ms, 15_000);
        assert_eq!(config.network.read_timeout_ms, 15_000);
        assert_eq!(config.youtube.resolve_timeout_ms, 30_000);
        assert_eq!(config.ui.skin, "minimal");
    }

    /// Проверяет, что demux skip-window не может быть нулевым.
    #[test]
    fn invalid_demux_corrupted_packet_limit_fails_validation() {
        let mut config = AppConfig::default();
        config.player.demux.max_consecutive_corrupted_packets = 0;

        let error = config
            .validate()
            .expect_err("zero demux corrupted packet limit rejected");

        assert!(
            error
                .to_string()
                .contains("player.demux.max_consecutive_corrupted_packets")
        );
    }

    /// Проверяет, что decoder queues остаются bounded и не могут быть нулевыми.
    #[test]
    fn invalid_decoder_queue_limit_fails_validation() {
        let mut config = AppConfig::default();
        config.video.decoder_packet_channel_frames = 0;

        let error = config
            .validate()
            .expect_err("zero decoder packet channel rejected");

        assert!(
            error
                .to_string()
                .contains("video.decoder_packet_channel_frames")
        );
    }

    /// Проверяет, что scheduler budget не может быть нулевым.
    #[test]
    fn invalid_scheduler_budget_fails_validation() {
        let mut config = AppConfig::default();
        config.video.scheduler.demux_packets_per_tick = 0;

        let error = config
            .validate()
            .expect_err("zero scheduler demux budget rejected");

        assert!(
            error
                .to_string()
                .contains("video.scheduler.demux_packets_per_tick")
        );
    }

    /// Проверяет cross-field min/target/max для presentation queue.
    #[test]
    fn invalid_scheduler_present_queue_target_fails_validation() {
        let mut config = AppConfig::default();
        config.video.present_queue_frames = 4;
        config.video.scheduler.present_queue_min_frames = 3;
        config.video.scheduler.present_queue_target_frames = 5;

        let error = config
            .validate()
            .expect_err("present queue target above max rejected");

        assert!(
            error
                .to_string()
                .contains("video.scheduler.present_queue_target_frames")
        );
    }

    /// Проверяет, что decode-ahead target не может превышать max.
    #[test]
    fn invalid_scheduler_decode_ahead_target_fails_validation() {
        let mut config = AppConfig::default();
        config.video.max_decode_ahead_ms = 100;
        config.video.scheduler.decode_ahead_target_ms = 200;

        let error = config
            .validate()
            .expect_err("decode ahead target above max rejected");

        assert!(
            error
                .to_string()
                .contains("video.scheduler.decode_ahead_target_ms")
        );
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
        assert!(created_toml.contains("schema_version = 2"));
        assert!(created_toml.contains("[player.seek]"));
        assert!(created_toml.contains("# Настройки live seek"));
        assert!(created_toml.contains("live_interval_ms = 100"));
        assert!(created_toml.contains("resume_video_min_ready_frames = 3"));
        assert!(created_toml.contains("[player.demux]"));
        assert!(created_toml.contains("# Fail-safe настройки demuxer-а."));
        assert!(created_toml.contains("max_consecutive_corrupted_packets = 64"));
        assert!(created_toml.contains("decoder_packet_channel_frames = 32"));
        assert!(created_toml.contains("# Bounded очередь packets"));
        assert!(created_toml.contains("[video.scheduler]"));
        assert!(created_toml.contains("# Настройки worker scheduler-а"));
        assert!(created_toml.contains("demux_packets_per_tick = 12"));
        assert!(created_toml.contains("present_queue_target_frames = 4"));
        assert!(created_toml.contains("decode_ahead_target_ms = 250"));
        assert!(created_toml.contains("surface_free_slots_target = 4"));
        assert!(created_toml.contains("# RAM cache budget"));
        assert!(created_toml.contains("memory_cache_mb = 128"));
        assert!(created_toml.contains("read_ahead_mb = 64"));
        assert!(created_toml.contains("# Timeout подготовки YouTube metadata"));
        assert!(created_toml.contains("resolve_timeout_ms = 30000"));
        assert!(!created_toml.contains("index_fingerprint_sample_kb"));
        assert!(created_toml.contains("# UI skin id"));
        assert!(created_toml.contains("skin = \"minimal\""));
        assert!(created_toml.contains("[render.hdr_to_sdr]"));
        assert!(created_toml.contains("operator = \"bt2446_c\""));

        let reparsed = toml::from_str::<AppConfig>(&created_toml)
            .expect("documented default config remains valid TOML");
        assert_eq!(reparsed, AppConfig::default());
    }

    /// Проверяет, что старые index-only network поля больше не принимаются.
    #[test]
    fn legacy_index_only_network_config_fields_are_rejected() {
        for legacy_field in [
            "index_fingerprint_sample_kb = 512",
            "indexer_io_budget_mb_per_sec = 32",
        ] {
            let temp_dir = tempfile::tempdir().expect("temp dir created");
            let config_path = temp_dir.path().join("config.toml");
            fs::write(
                &config_path,
                format!(
                    r#"
schema_version = 2

[network]
{legacy_field}
"#
                ),
            )
            .expect("legacy config written");

            let error = load_from_path(&config_path).expect_err("legacy config rejected");
            let field_name = legacy_field
                .split_once(" = ")
                .expect("test legacy field format")
                .0;

            assert!(error.to_string().contains(field_name));
        }
    }

    /// Проверяет, что старый config без color_adjustment получает identity defaults.
    #[test]
    fn existing_config_without_color_adjustment_gets_identity_defaults() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 2

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
schema_version = 2

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
schema_version = 2

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
schema_version = 2

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
schema_version = 2

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
schema_version = 2

[audio]
volume = 1.5
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid volume rejected");

        assert!(error.to_string().contains("audio.volume"));
    }

    /// Проверяет, что RAM cache можно явно отключить нулём.
    #[test]
    fn network_memory_cache_zero_is_valid() {
        let mut config = AppConfig::default();
        config.network.memory_cache_mb = 0;

        config
            .validate()
            .expect("zero memory cache disables RAM cache");
    }

    /// Проверяет верхнюю границу RAM cache.
    #[test]
    fn invalid_network_memory_cache_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 2

[network]
memory_cache_mb = 4097
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid memory cache rejected");

        assert!(error.to_string().contains("network.memory_cache_mb"));
    }

    /// Проверяет положительность network timeout-ов.
    #[test]
    fn invalid_network_timeout_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 2

[network]
connect_timeout_ms = 0
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid timeout rejected");

        assert!(error.to_string().contains("network.connect_timeout_ms"));
    }

    /// Проверяет положительность timeout-а подготовки YouTube metadata.
    #[test]
    fn invalid_youtube_resolve_timeout_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 2

[youtube]
resolve_timeout_ms = 0
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid timeout rejected");

        assert!(error.to_string().contains("youtube.resolve_timeout_ms"));
    }

    /// Проверяет положительность seek interval/budget.
    #[test]
    fn invalid_seek_interval_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 2

[player.seek]
live_interval_ms = 0
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid seek interval rejected");

        assert!(error.to_string().contains("player.seek.live_interval_ms"));
    }

    /// Проверяет положительность seek commit timeout-а.
    #[test]
    fn invalid_seek_commit_timeout_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 2

[player.seek]
commit_timeout_ms = 0
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid seek timeout rejected");

        assert!(error.to_string().contains("player.seek.commit_timeout_ms"));
    }

    /// Проверяет положительность video preroll перед seek resume.
    #[test]
    fn invalid_seek_resume_video_ready_frames_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 2

[player.seek]
resume_video_min_ready_frames = 0
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid video preroll rejected");

        assert!(
            error
                .to_string()
                .contains("player.seek.resume_video_min_ready_frames")
        );
    }

    /// Проверяет положительность hotkey step-ов.
    #[test]
    fn invalid_seek_hotkey_step_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 2

[player.seek]
hotkey_small_step_secs = 0
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid hotkey step rejected");

        assert!(
            error
                .to_string()
                .contains("player.seek.hotkey_small_step_secs")
        );
    }

    /// Проверяет, что неизвестный skin не мапится молча на default.
    #[test]
    fn invalid_ui_skin_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 2

[ui]
skin = "dense"
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid skin rejected");

        assert!(error.to_string().contains("ui.skin"));
    }

    /// Проверяет validation error для RGB-массива неверной длины.
    #[test]
    fn invalid_rgb_gain_array_fails_validation() {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
schema_version = 2

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
schema_version = 2

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
schema_version = 2
unexpected = true
"#,
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("unknown field rejected");

        assert!(error.to_string().contains("TOML-схеме"));
    }
}
