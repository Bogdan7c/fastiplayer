//! Создание и атомарное сохранение конфига, дефолты новых секций UI.

use super::*;

/// Проверяет первый запуск без существующего config-файла.
#[test]
fn missing_config_is_created_with_defaults() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("fastiplayer").join("config.toml");

    let loaded = load_or_create_at(&config_path).expect("default config created");

    assert_eq!(loaded.origin, ConfigLoadOrigin::CreatedDefault);
    assert_eq!(loaded.path, config_path);
    assert_eq!(loaded.config, AppConfig::default());
    assert!(loaded.path.exists());
    assert!(loaded.config.render.color_adjustment.is_identity());

    let created_toml = fs::read_to_string(&loaded.path).expect("created config readable");
    assert!(created_toml.contains("schema_version = 10"));
    assert!(created_toml.contains("[web_media]"));
    assert!(created_toml.contains("[player.seek]"));
    assert!(created_toml.contains("# Настройки seek commit"));
    assert!(created_toml.contains("commit_timeout_ms = 10000"));
    assert!(created_toml.contains("resume_audio_gate_timeout_ms = 250"));
    assert!(created_toml.contains("resume_video_min_ready_frames = 3"));
    assert!(created_toml.contains("fast_preroll_budget_ms = 48"));
    assert!(created_toml.contains("fast_preroll_video_packet_burst = 512"));
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
    assert!(created_toml.contains("read_ahead_mb = 256"));
    assert!(created_toml.contains("prefetch_initial_chunk_kb = 64"));
    assert!(created_toml.contains("# Размер ПЕРВОГО prefetch-чтения"));
    assert!(created_toml.contains("prefetch_chunk_mb = 8"));
    assert!(created_toml.contains("# Timeout подготовки metadata через системный yt-dlp"));
    assert!(created_toml.contains("resolve_timeout_ms = 30000"));
    assert!(!created_toml.contains("preferred_video_height"));
    assert!(!created_toml.contains("index_fingerprint_sample_kb"));
    assert!(created_toml.contains("# UI skin id"));
    assert!(created_toml.contains("skin = \"minimal\""));
    assert!(created_toml.contains("[ui.window]"));
    assert!(created_toml.contains("titlebar_height_px = 40"));
    assert!(created_toml.contains("corner_radius_px = 12"));
    assert!(created_toml.contains("[ui.settings]"));
    assert!(created_toml.contains("live_preview_max_hz = 60"));
    assert!(created_toml.contains("[render.hdr_to_sdr]"));
    assert!(created_toml.contains("operator = \"bt2446_c\""));
    assert!(!created_toml.contains(REMOVED_HARDWARE_DECODE_ONLY_KEY));

    let reparsed = toml::from_str::<AppConfig>(&created_toml)
        .expect("documented default config remains valid TOML");
    assert_eq!(reparsed, AppConfig::default());
}

/// Проверяет atomic save happy path: файл заменяется generated TOML и потом читается обратно.
#[test]
fn save_validated_atomic_at_writes_roundtrippable_generated_toml() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        "# Пользовательский комментарий, который save не обязан сохранять.\nschema_version = 2\n",
    )
    .expect("old config written");

    let mut config = AppConfig::default();
    config.ui.settings.live_preview_max_hz = 144;

    save_validated_atomic_at(&config_path, &config).expect("valid config saved atomically");

    let saved_toml = fs::read_to_string(&config_path).expect("saved config readable");
    assert!(!saved_toml.contains("Пользовательский комментарий"));
    assert!(saved_toml.contains("[ui.settings]"));
    assert!(saved_toml.contains("live_preview_max_hz = 144"));
    assert_no_save_temp_files(temp_dir.path());

    let loaded = load_from_path(&config_path).expect("saved config loads");
    assert_eq!(loaded.config, config);
}

/// Проверяет, что invalid config отбрасывается до любых операций записи.
#[test]
fn save_validated_atomic_at_does_not_touch_file_when_config_is_invalid() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let original_toml = "это неважное старое содержимое, которое нельзя перетереть\n";
    fs::write(&config_path, original_toml).expect("old config written");

    let mut config = AppConfig::default();
    config.ui.settings.live_preview_max_hz = 0;

    let error = save_validated_atomic_at(&config_path, &config)
        .expect_err("invalid config rejected before write");

    assert!(
        error
            .to_string()
            .contains("ui.settings.live_preview_max_hz")
    );
    assert_eq!(
        fs::read_to_string(&config_path).expect("old config still readable"),
        original_toml
    );
    assert_no_save_temp_files(temp_dir.path());
}

/// Проверяет compatibility: старый `[ui]` без `[ui.settings]` получает defaults.
#[test]
fn existing_ui_config_without_settings_gets_live_preview_defaults() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[ui]
show_telemetry = false
language = "ru"
skin = "minimal"
"#,
    )
    .expect("legacy ui config written");

    let loaded = load_from_path(&config_path).expect("legacy ui config accepted");

    assert!(!loaded.config.ui.show_telemetry);
    assert_eq!(loaded.config.ui.window.titlebar_height_px, 40);
    assert_eq!(loaded.config.ui.window.corner_radius_px, 12);
    assert_eq!(
        loaded.config.ui.sidebar.width_points,
        crate::DEFAULT_SIDEBAR_WIDTH_POINTS
    );
    assert_eq!(loaded.config.ui.settings.live_preview_max_hz, 60);
    assert!(loaded.config.ui.animations.reduced_motion);
    assert_eq!(loaded.config.ui.animations.sidebar_slide_duration_ms, 500);
}

/// Проверяет additive/defaulted reduced-motion поле на legacy schema v6 документе.
#[test]
fn schema_v6_without_reduced_motion_loads_safe_default_and_roundtrips_field() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 6

[ui]
show_telemetry = true
language = "ru"
skin = "minimal"

[ui.animations]
sidebar_slide_duration_ms = 500
"#,
    )
    .expect("current config without reduced-motion written");

    let loaded = load_from_path(&config_path).expect("missing additive field accepted");
    assert!(loaded.config.ui.animations.reduced_motion);

    save_validated_atomic_at(&config_path, &loaded.config).expect("defaulted config saved");
    let saved = fs::read_to_string(&config_path).expect("saved config read");
    assert!(saved.contains("reduced_motion = true"));
}

/// Проверяет backward compatibility legacy schema v6 без нового sidebar-поля
/// и появление поля после следующего успешного атомарного сохранения.
#[test]
fn schema_v6_without_sidebar_width_loads_default_and_roundtrips_new_field() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 6

[ui]
show_telemetry = true
language = "ru"
skin = "minimal"
"#,
    )
    .expect("schema v6 config without sidebar width written");

    let loaded = load_from_path(&config_path).expect("old schema v6 shape accepted");
    assert_eq!(
        loaded.config.ui.sidebar.width_points,
        crate::DEFAULT_SIDEBAR_WIDTH_POINTS
    );

    save_validated_atomic_at(&config_path, &loaded.config)
        .expect("compatible config saved with current defaults");
    let persisted = fs::read_to_string(&config_path).expect("saved config readable");
    assert!(persisted.contains("[ui.sidebar]"));
    assert!(persisted.contains("width_points = 420"));

    let reloaded = load_from_path(&config_path).expect("saved sidebar width roundtrips");
    assert_eq!(reloaded.config, loaded.config);
}
