//! Настройки памяти встроенных дорожек PGS `[subtitles]` (схема v22): defaults, миграция v21,
//! диапазоны и граничные значения.

use super::*;
use crate::{
    DEFAULT_PGS_BACKGROUND_TRACK_WINDOW_S, DEFAULT_PGS_DISPLAYED_TRACK_CACHE_MB,
    MAX_PGS_BACKGROUND_TRACK_WINDOW_S, MAX_PGS_DISPLAYED_TRACK_CACHE_MB,
    MIN_PGS_BACKGROUND_TRACK_WINDOW_S, MIN_PGS_DISPLAYED_TRACK_CACHE_MB,
};

/// Документ v21: свои значения субтитров и нет новых ключей.
const SCHEMA_V21_WITHOUT_PGS_KEYS: &str = r#"
schema_version = 21

[subtitles]
enabled = true
text_size_percent = 130
glyph_cache_mb = 96
"#;

/// Файл v21 поднимается до v22: значения пользователя целы, новые поля равны решению владельца
/// (32 МБ, 60 с), после сохранения ключи на месте и повторная загрузка ничего не меняет.
#[test]
fn schema_v21_without_pgs_keys_loads_defaults_and_keeps_user_values() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V21_WITHOUT_PGS_KEYS).expect("v21 fixture written");

    let migrated = load_from_path(&config_path).expect("v21 config loads");
    assert_eq!(migrated.config.schema_version, 28);
    let subtitles = &migrated.config.subtitles;
    assert_eq!(subtitles.pgs_displayed_track_cache_mb, 32);
    assert_eq!(
        subtitles.pgs_displayed_track_cache_mb,
        DEFAULT_PGS_DISPLAYED_TRACK_CACHE_MB
    );
    assert_eq!(subtitles.pgs_background_track_window_s, 60);
    assert_eq!(
        subtitles.pgs_background_track_window_s,
        DEFAULT_PGS_BACKGROUND_TRACK_WINDOW_S
    );
    // Миграция не переписывает значения пользователя.
    assert!(subtitles.enabled);
    assert_eq!(subtitles.text_size_percent, 130);
    assert_eq!(subtitles.glyph_cache_mb, 96);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("schema_version = 28"));
    assert!(saved_text.contains("pgs_displayed_track_cache_mb = 32"));
    assert!(saved_text.contains("pgs_background_track_window_s = 60"));
    assert!(saved_text.contains("glyph_cache_mb = 96"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// Пользовательские значения новых ключей и границы диапазонов переживают запись и чтение.
#[test]
fn pgs_boundary_values_roundtrip_through_file() {
    for (megabytes, window_seconds) in [
        (
            MIN_PGS_DISPLAYED_TRACK_CACHE_MB,
            MIN_PGS_BACKGROUND_TRACK_WINDOW_S,
        ),
        (
            MAX_PGS_DISPLAYED_TRACK_CACHE_MB,
            MAX_PGS_BACKGROUND_TRACK_WINDOW_S,
        ),
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        let mut config = AppConfig::default();
        config.subtitles.pgs_displayed_track_cache_mb = megabytes;
        config.subtitles.pgs_background_track_window_s = window_seconds;
        save_validated_atomic_at(&config_path, &config).expect("boundary config saves");
        let reloaded = load_from_path(&config_path).expect("boundary config reloads");
        assert_eq!(
            reloaded.config.subtitles.pgs_displayed_track_cache_mb,
            megabytes
        );
        assert_eq!(
            reloaded.config.subtitles.pgs_background_track_window_s,
            window_seconds
        );
    }
}

/// Значения вне диапазона отклоняются с полным путём поля.
#[test]
fn pgs_settings_out_of_range_are_rejected() {
    for megabytes in [
        MIN_PGS_DISPLAYED_TRACK_CACHE_MB - 1,
        MAX_PGS_DISPLAYED_TRACK_CACHE_MB + 1,
    ] {
        let mut config = AppConfig::default();
        config.subtitles.pgs_displayed_track_cache_mb = megabytes;
        let error = config
            .validate()
            .expect_err("значение вне диапазона отклонено");
        assert!(
            error
                .to_string()
                .contains("subtitles.pgs_displayed_track_cache_mb"),
            "ошибка `{error}` без имени поля"
        );
    }
    // Нижняя граница окна равна 0 (валидна), поэтому вне диапазона только верх.
    let mut config = AppConfig::default();
    config.subtitles.pgs_background_track_window_s = MAX_PGS_BACKGROUND_TRACK_WINDOW_S + 1;
    let error = config
        .validate()
        .expect_err("значение вне диапазона отклонено");
    assert!(
        error
            .to_string()
            .contains("subtitles.pgs_background_track_window_s"),
        "ошибка `{error}` без имени поля"
    );
}
