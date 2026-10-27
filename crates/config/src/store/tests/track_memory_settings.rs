//! Лимит памяти дорожек (`playlist.track_memory_max_entries`, схема v16): defaults,
//! миграция v15 -> v16 без переписывания значений и validation.

use super::*;
use crate::{
    DEFAULT_TRACK_MEMORY_MAX_ENTRIES, MAX_TRACK_MEMORY_MAX_ENTRIES, MIN_TRACK_MEMORY_MAX_ENTRIES,
};

/// Документ v15: пользовательские значения playlist есть, лимита памяти дорожек ещё нет.
const SCHEMA_V15_WITHOUT_TRACK_MEMORY: &str = r#"
schema_version = 15

[playlist]
load_siblings = false
state_save_debounce_ms = 3000
"#;

/// Старый файл поднимается до v17 с default-лимитом, чужие значения целы, а после
/// сохранения файл содержит ключ и повторная загрузка ничего не меняет.
#[test]
fn schema_v15_without_track_memory_loads_default_and_keeps_user_values() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V15_WITHOUT_TRACK_MEMORY).expect("v15 fixture written");

    let migrated = load_from_path(&config_path).expect("v15 config loads");

    assert_eq!(migrated.config.schema_version, 26);
    assert_eq!(
        migrated.config.playlist.track_memory_max_entries,
        DEFAULT_TRACK_MEMORY_MAX_ENTRIES
    );
    assert_eq!(DEFAULT_TRACK_MEMORY_MAX_ENTRIES, 1_000);
    assert!(!migrated.config.playlist.load_siblings);
    assert_eq!(migrated.config.playlist.state_save_debounce_ms, 3_000);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved config readable");
    assert!(saved_text.contains("schema_version = 26"));
    assert!(saved_text.contains("track_memory_max_entries = 1000"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// Пользовательское значение переживает сохранение и загрузку.
#[test]
fn custom_track_memory_limit_roundtrips() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let mut config = AppConfig::default();
    config.playlist.track_memory_max_entries = 250;

    save_validated_atomic_at(&config_path, &config).expect("custom config saves");
    let loaded = load_from_path(&config_path).expect("custom config loads");

    assert_eq!(loaded.config.playlist.track_memory_max_entries, 250);
}

/// Границы диапазона включительно допустимы, за ними значение отклоняется и ошибка называет поле.
#[test]
fn track_memory_limit_is_validated_inclusively() {
    for accepted in [MIN_TRACK_MEMORY_MAX_ENTRIES, MAX_TRACK_MEMORY_MAX_ENTRIES] {
        let mut config = AppConfig::default();
        config.playlist.track_memory_max_entries = accepted;
        config.validate().expect("inclusive bound accepted");
    }
    for rejected in [
        MIN_TRACK_MEMORY_MAX_ENTRIES - 1,
        MAX_TRACK_MEMORY_MAX_ENTRIES + 1,
        0,
    ] {
        let mut config = AppConfig::default();
        config.playlist.track_memory_max_entries = rejected;
        let error = config.validate().expect_err("out of range rejected");
        assert!(
            error
                .to_string()
                .contains("playlist.track_memory_max_entries"),
            "ошибка `{error}` должна называть поле"
        );
    }
}
