//! Потолок чтения аудио вперёд до первого видеокадра (`audio.startup_read_ahead_cap_ms`,
//! схема v17): defaults, миграция v16 -> v17 без переписывания значений и validation.

use super::*;
use crate::{
    DEFAULT_AUDIO_STARTUP_READ_AHEAD_CAP_MS, MAX_AUDIO_STARTUP_READ_AHEAD_CAP_MS,
    MIN_AUDIO_STARTUP_READ_AHEAD_CAP_MS,
};

/// Документ v16: пользовательские значения audio есть, потолка стартового read-ahead ещё нет.
const SCHEMA_V16_WITHOUT_STARTUP_READ_AHEAD: &str = r#"
schema_version = 16

[audio]
volume = 0.3
buffer_target_ms = 350
"#;

/// Старый файл поднимается до v17 с default-потолком, чужие значения целы, после
/// сохранения файл содержит ключ и повторная загрузка ничего не меняет.
#[test]
fn schema_v16_without_startup_read_ahead_loads_default_and_keeps_user_values() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V16_WITHOUT_STARTUP_READ_AHEAD).expect("v16 fixture written");

    let migrated = load_from_path(&config_path).expect("v16 config loads");

    assert_eq!(migrated.config.schema_version, 24);
    assert_eq!(
        migrated.config.audio.startup_read_ahead_cap_ms,
        DEFAULT_AUDIO_STARTUP_READ_AHEAD_CAP_MS
    );
    assert_eq!(DEFAULT_AUDIO_STARTUP_READ_AHEAD_CAP_MS, 1_500);
    assert_eq!(migrated.config.audio.buffer_target_ms, 350);
    assert!((migrated.config.audio.volume - 0.3).abs() < f64::EPSILON);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved config readable");
    assert!(saved_text.contains("schema_version = 24"));
    assert!(saved_text.contains("startup_read_ahead_cap_ms = 1500"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// Пользовательское значение переживает сохранение и загрузку.
#[test]
fn custom_startup_read_ahead_cap_roundtrips() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let mut config = AppConfig::default();
    config.audio.startup_read_ahead_cap_ms = 900;

    save_validated_atomic_at(&config_path, &config).expect("custom config saves");
    let loaded = load_from_path(&config_path).expect("custom config loads");

    assert_eq!(loaded.config.audio.startup_read_ahead_cap_ms, 900);
}

/// Границы диапазона включительно допустимы, за ними значение отклоняется и ошибка называет поле.
#[test]
fn startup_read_ahead_cap_is_validated_inclusively() {
    for accepted in [
        MIN_AUDIO_STARTUP_READ_AHEAD_CAP_MS,
        MAX_AUDIO_STARTUP_READ_AHEAD_CAP_MS,
    ] {
        let mut config = AppConfig::default();
        config.audio.startup_read_ahead_cap_ms = accepted;
        config.validate().expect("boundary value is valid");
    }

    for rejected in [
        MIN_AUDIO_STARTUP_READ_AHEAD_CAP_MS - 1,
        MAX_AUDIO_STARTUP_READ_AHEAD_CAP_MS + 1,
    ] {
        let mut config = AppConfig::default();
        config.audio.startup_read_ahead_cap_ms = rejected;
        let error = config.validate().expect_err("out of range value rejected");
        assert!(
            error
                .to_string()
                .contains("audio.startup_read_ahead_cap_ms"),
            "error must name the field: {error}"
        );
    }
}
