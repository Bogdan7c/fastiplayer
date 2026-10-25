//! Значение `video.preferred_backend = "dynamic"` (схема v25): чтение, запись и миграция v24 -> v25.

use super::*;

/// Документ v24: пользовательский режим декодера и соседние значения заданы явно,
/// значения `dynamic` старая схема не знала.
const SCHEMA_V24_WITH_SOFTWARE_BACKEND: &str = r#"
schema_version = 24

[video]
preferred_backend = "software"
max_decode_ahead_ms = 777
"#;

/// `dynamic` читается из файла и переживает сохранение и повторную загрузку.
#[test]
fn dynamic_backend_preference_roundtrips_through_file() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let mut config = AppConfig::default();
    config.video.preferred_backend = VideoBackendPreference::Dynamic;

    save_validated_atomic_at(&config_path, &config).expect("dynamic config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved config readable");
    assert!(saved_text.contains("preferred_backend = \"dynamic\""));
    let loaded = load_from_path(&config_path).expect("dynamic config loads");

    assert_eq!(
        loaded.config.video.preferred_backend,
        VideoBackendPreference::Dynamic
    );
}

/// По умолчанию остаётся `auto`: новый режим включается только осознанным выбором.
#[test]
fn default_backend_preference_stays_auto() {
    assert_eq!(
        AppConfig::default().video.preferred_backend,
        VideoBackendPreference::Auto
    );
}

/// Файл v24 поднимается до текущей схемы, остальные поля остаются как были.
#[test]
fn schema_v24_migrates_to_current_without_changing_other_fields() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V24_WITH_SOFTWARE_BACKEND).expect("v24 fixture written");

    let migrated = load_from_path(&config_path).expect("v24 config loads");

    assert_eq!(migrated.config.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(
        migrated.config.video.preferred_backend,
        VideoBackendPreference::Software
    );
    assert_eq!(migrated.config.video.max_decode_ahead_ms, 777);
}
