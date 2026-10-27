//! Настройка удерживаемой памяти размытия субтитров `[subtitles]` (схема v20): defaults,
//! миграция v19, диапазон и граничные значения.

use super::*;
use crate::{
    DEFAULT_BLUR_SCRATCH_RETAINED_MB, MAX_BLUR_SCRATCH_RETAINED_MB, MIN_BLUR_SCRATCH_RETAINED_MB,
};

/// Документ v19: свои значения субтитров и нет нового ключа.
const SCHEMA_V19_WITHOUT_BLUR_KEY: &str = r#"
schema_version = 19

[subtitles]
enabled = true
text_size_percent = 130
prerender_frames = 9
prepared_overlays = 12
render_cache_mb = 64
"#;

/// Файл v19 поднимается до v20: значения пользователя целы, новое поле равно прежней
/// константе растеризатора (32 МБ), после сохранения ключ на месте и повторная загрузка
/// ничего не меняет.
#[test]
fn schema_v19_without_blur_key_loads_default_and_keeps_user_values() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V19_WITHOUT_BLUR_KEY).expect("v19 fixture written");

    let migrated = load_from_path(&config_path).expect("v19 config loads");
    assert_eq!(migrated.config.schema_version, 27);
    let subtitles = &migrated.config.subtitles;
    assert_eq!(subtitles.blur_scratch_retained_mb, 32);
    assert_eq!(
        subtitles.blur_scratch_retained_mb,
        DEFAULT_BLUR_SCRATCH_RETAINED_MB
    );
    // Миграция не переписывает значения пользователя.
    assert!(subtitles.enabled);
    assert_eq!(subtitles.text_size_percent, 130);
    assert_eq!(subtitles.prerender_frames, 9);
    assert_eq!(subtitles.prepared_overlays, 12);
    assert_eq!(subtitles.render_cache_mb, 64);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("schema_version = 27"));
    assert!(saved_text.contains("blur_scratch_retained_mb = 32"));
    assert!(saved_text.contains("prerender_frames = 9"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// Границы диапазона переживают запись и чтение файла.
#[test]
fn blur_scratch_boundary_values_roundtrip_through_file() {
    for megabytes in [MIN_BLUR_SCRATCH_RETAINED_MB, MAX_BLUR_SCRATCH_RETAINED_MB] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        let mut config = AppConfig::default();
        config.subtitles.blur_scratch_retained_mb = megabytes;
        save_validated_atomic_at(&config_path, &config).expect("boundary config saves");
        let reloaded = load_from_path(&config_path).expect("boundary config reloads");
        assert_eq!(
            reloaded.config.subtitles.blur_scratch_retained_mb,
            megabytes
        );
    }
}

/// Значения вне диапазона отклоняются с полным путём поля.
#[test]
fn blur_scratch_out_of_range_is_rejected() {
    for megabytes in [
        MIN_BLUR_SCRATCH_RETAINED_MB - 1,
        MAX_BLUR_SCRATCH_RETAINED_MB + 1,
    ] {
        let mut config = AppConfig::default();
        config.subtitles.blur_scratch_retained_mb = megabytes;
        let error = config
            .validate()
            .expect_err("значение вне диапазона отклонено");
        assert!(
            error
                .to_string()
                .contains("subtitles.blur_scratch_retained_mb"),
            "ошибка `{error}` без имени поля"
        );
    }
}
