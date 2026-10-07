//! Настройки drag & drop (`playlist.dropped_*`, схема v12): defaults, миграция и валидация.
//!
//! Решение владельца (UX edge cases, сессия 12): лимиты папки и поведение файла
//! плейлиста живут в `config.toml`; старый файл без этих полей обязан грузиться с
//! defaults, а значения вне диапазона — отклоняться по обычной политике config.

use super::*;
use crate::{
    DEFAULT_DROPPED_FOLDER_MAX_DEPTH, DEFAULT_DROPPED_FOLDER_MAX_FILES, DroppedPlaylistFileAction,
    MAX_DROPPED_FOLDER_MAX_DEPTH, MAX_DROPPED_FOLDER_MAX_FILES, MIN_DROPPED_FOLDER_MAX_FILES,
};

/// Документ v11: пользовательские значения playlist есть, полей drag & drop ещё нет.
const SCHEMA_V11_WITHOUT_DROP_FIELDS: &str = r#"
schema_version = 11

[playlist]
load_siblings = false
state_save_debounce_ms = 3000
"#;

/// Старый файл грузится с defaults, чужие значения сохраняются, после сохранения
/// файл содержит v12 и все три поля, а повторная загрузка ничего не меняет.
#[test]
fn schema_v11_without_drop_fields_loads_defaults_and_roundtrips() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V11_WITHOUT_DROP_FIELDS).expect("v11 fixture written");

    let migrated = load_from_path(&config_path).expect("v11 config loads");
    assert_eq!(migrated.config.schema_version, 12);
    assert_eq!(
        migrated.config.playlist.dropped_folder_max_files,
        DEFAULT_DROPPED_FOLDER_MAX_FILES
    );
    assert_eq!(
        migrated.config.playlist.dropped_folder_max_depth,
        DEFAULT_DROPPED_FOLDER_MAX_DEPTH
    );
    assert_eq!(
        migrated.config.playlist.dropped_playlist_file_action,
        DroppedPlaylistFileAction::ByDropTarget
    );
    assert!(!migrated.config.playlist.load_siblings);
    assert_eq!(migrated.config.playlist.state_save_debounce_ms, 3_000);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("schema_version = 12"));
    assert!(saved_text.contains("dropped_folder_max_files = 2000"));
    assert!(saved_text.contains("dropped_folder_max_depth = 8"));
    assert!(saved_text.contains("dropped_playlist_file_action = \"by_drop_target\""));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// Пользовательские значения всех трёх полей переживают сохранение и загрузку.
#[test]
fn custom_drop_settings_roundtrip_through_file() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let mut config = AppConfig::default();
    config.playlist.dropped_folder_max_files = 321;
    config.playlist.dropped_folder_max_depth = 0;
    config.playlist.dropped_playlist_file_action = DroppedPlaylistFileAction::NewPlaylist;

    save_validated_atomic_at(&config_path, &config).expect("custom config saves");
    let loaded = load_from_path(&config_path).expect("custom config loads");

    assert_eq!(loaded.config.playlist.dropped_folder_max_files, 321);
    assert_eq!(loaded.config.playlist.dropped_folder_max_depth, 0);
    assert_eq!(
        loaded.config.playlist.dropped_playlist_file_action,
        DroppedPlaylistFileAction::NewPlaylist
    );
}

/// Значения вне диапазона отклоняются и называют проблемное поле.
#[test]
fn out_of_range_drop_limits_are_rejected_with_field_name() {
    let mut zero_files = AppConfig::default();
    zero_files.playlist.dropped_folder_max_files = MIN_DROPPED_FOLDER_MAX_FILES - 1;
    let error = zero_files.validate().expect_err("0 файлов недопустимо");
    assert!(
        error
            .to_string()
            .contains("playlist.dropped_folder_max_files")
    );

    let mut too_many_files = AppConfig::default();
    too_many_files.playlist.dropped_folder_max_files = MAX_DROPPED_FOLDER_MAX_FILES + 1;
    assert!(too_many_files.validate().is_err());

    let mut too_deep = AppConfig::default();
    too_deep.playlist.dropped_folder_max_depth = MAX_DROPPED_FOLDER_MAX_DEPTH + 1;
    let error = too_deep.validate().expect_err("слишком глубоко");
    assert!(
        error
            .to_string()
            .contains("playlist.dropped_folder_max_depth")
    );
}

/// Неизвестное значение поведения плейлиста — ошибка strict parser-а, а не молчаливый default.
#[test]
fn unknown_dropped_playlist_action_is_a_parse_error() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        "schema_version = 12\n\n[playlist]\ndropped_playlist_file_action = \"explode\"\n",
    )
    .expect("fixture written");

    assert!(load_from_path(&config_path).is_err());
}
