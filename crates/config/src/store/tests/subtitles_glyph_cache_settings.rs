//! Настройки кэшей глифов субтитров `[subtitles]` (схема v21): defaults, миграция v20,
//! диапазоны и граничные значения.

use super::*;
use crate::{
    DEFAULT_GLYPH_CACHE_MB, DEFAULT_GLYPH_OUTLINE_CACHE_ITEMS, DEFAULT_SHAPING_CACHE_MB,
    MAX_GLYPH_CACHE_MB, MAX_GLYPH_OUTLINE_CACHE_ITEMS, MAX_SHAPING_CACHE_MB, MIN_GLYPH_CACHE_MB,
    MIN_GLYPH_OUTLINE_CACHE_ITEMS, MIN_SHAPING_CACHE_MB,
};

/// Документ v20: свои значения субтитров и нет новых ключей.
const SCHEMA_V20_WITHOUT_GLYPH_CACHE_KEYS: &str = r#"
schema_version = 20

[subtitles]
enabled = true
text_size_percent = 130
render_cache_mb = 48
blur_scratch_retained_mb = 16
"#;

/// Файл v20 поднимается до v21: значения пользователя целы, новые поля равны решению
/// владельца (64 МБ, 10000 записей), после сохранения ключи на месте и повторная загрузка
/// ничего не меняет.
#[test]
fn schema_v20_without_glyph_cache_keys_loads_defaults_and_keeps_user_values() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V20_WITHOUT_GLYPH_CACHE_KEYS).expect("v20 fixture written");

    let migrated = load_from_path(&config_path).expect("v20 config loads");
    assert_eq!(migrated.config.schema_version, 23);
    let subtitles = &migrated.config.subtitles;
    assert_eq!(subtitles.glyph_cache_mb, 64);
    assert_eq!(subtitles.glyph_cache_mb, DEFAULT_GLYPH_CACHE_MB);
    assert_eq!(subtitles.glyph_outline_cache_items, 10_000);
    assert_eq!(subtitles.shaping_cache_mb, 4);
    assert_eq!(subtitles.shaping_cache_mb, DEFAULT_SHAPING_CACHE_MB);
    assert_eq!(
        subtitles.glyph_outline_cache_items,
        DEFAULT_GLYPH_OUTLINE_CACHE_ITEMS
    );
    // Миграция не переписывает значения пользователя.
    assert!(subtitles.enabled);
    assert_eq!(subtitles.text_size_percent, 130);
    assert_eq!(subtitles.render_cache_mb, 48);
    assert_eq!(subtitles.blur_scratch_retained_mb, 16);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("schema_version = 23"));
    assert!(saved_text.contains("glyph_cache_mb = 64"));
    assert!(saved_text.contains("glyph_outline_cache_items = 10000"));
    assert!(saved_text.contains("shaping_cache_mb = 4"));
    assert!(saved_text.contains("blur_scratch_retained_mb = 16"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// Границы диапазонов переживают запись и чтение файла.
#[test]
fn glyph_cache_boundary_values_roundtrip_through_file() {
    for (megabytes, items, shaping_megabytes) in [
        (
            MIN_GLYPH_CACHE_MB,
            MIN_GLYPH_OUTLINE_CACHE_ITEMS,
            MIN_SHAPING_CACHE_MB,
        ),
        (
            MAX_GLYPH_CACHE_MB,
            MAX_GLYPH_OUTLINE_CACHE_ITEMS,
            MAX_SHAPING_CACHE_MB,
        ),
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        let mut config = AppConfig::default();
        config.subtitles.glyph_cache_mb = megabytes;
        config.subtitles.glyph_outline_cache_items = items;
        config.subtitles.shaping_cache_mb = shaping_megabytes;
        save_validated_atomic_at(&config_path, &config).expect("boundary config saves");
        let reloaded = load_from_path(&config_path).expect("boundary config reloads");
        assert_eq!(reloaded.config.subtitles.glyph_cache_mb, megabytes);
        assert_eq!(reloaded.config.subtitles.glyph_outline_cache_items, items);
        assert_eq!(
            reloaded.config.subtitles.shaping_cache_mb,
            shaping_megabytes
        );
    }
}

/// Значения вне диапазона отклоняются с полным путём поля.
#[test]
fn glyph_cache_out_of_range_is_rejected() {
    for megabytes in [MIN_GLYPH_CACHE_MB - 1, MAX_GLYPH_CACHE_MB + 1] {
        let mut config = AppConfig::default();
        config.subtitles.glyph_cache_mb = megabytes;
        let error = config
            .validate()
            .expect_err("значение вне диапазона отклонено");
        assert!(
            error.to_string().contains("subtitles.glyph_cache_mb"),
            "ошибка `{error}` без имени поля"
        );
    }
    for items in [
        MIN_GLYPH_OUTLINE_CACHE_ITEMS - 1,
        MAX_GLYPH_OUTLINE_CACHE_ITEMS + 1,
    ] {
        let mut config = AppConfig::default();
        config.subtitles.glyph_outline_cache_items = items;
        let error = config
            .validate()
            .expect_err("значение вне диапазона отклонено");
        assert!(
            error
                .to_string()
                .contains("subtitles.glyph_outline_cache_items"),
            "ошибка `{error}` без имени поля"
        );
    }
}

/// Кэш шейпинга: границы диапазона отклоняются с полным путём поля.
#[test]
fn shaping_cache_out_of_range_is_rejected() {
    for megabytes in [MIN_SHAPING_CACHE_MB - 1, MAX_SHAPING_CACHE_MB + 1] {
        let mut config = AppConfig::default();
        config.subtitles.shaping_cache_mb = megabytes;
        let error = config
            .validate()
            .expect_err("значение вне диапазона отклонено");
        assert!(
            error.to_string().contains("subtitles.shaping_cache_mb"),
            "ошибка `{error}` без имени поля"
        );
    }
}
