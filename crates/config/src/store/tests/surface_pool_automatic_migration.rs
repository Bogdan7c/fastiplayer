//! Миграция v26 → v27: явное 24 в пулах поверхностей decoder-а становится 0 («авто»).
//!
//! Решение владельца (gles2-renderer, решение 66): размер пула выбирает активный рендерер
//! (16 на OpenGL ES 2.0, 24 на Vulkan). Явные значения, отличные от 24, сохраняются.

use super::*;

/// Загружает TOML-документ через file boundary и возвращает конфиг после миграции.
fn load_document(document_text: &str) -> crate::AppConfig {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, document_text).expect("fixture written");
    load_from_path(&config_path).expect("fixture loads").config
}

/// Оба ключа со старым default 24 становятся 0, схема поднимается до текущей.
#[test]
fn schema_v26_explicit_24_pools_migrate_to_automatic() {
    let config = load_document(
        "schema_version = 26\n\n[video]\ndecoder_surface_pool_frames = 24\nzero_copy_surface_pool_slots = 24\n",
    );

    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(config.video.decoder_surface_pool_frames, 0);
    assert_eq!(config.video.zero_copy_surface_pool_slots, 0);
}

/// Любое другое явное число остаётся выбором пользователя; каждый ключ мигрирует независимо.
#[test]
fn schema_v26_non_default_pool_values_are_kept() {
    let config = load_document(
        "schema_version = 26\n\n[video]\ndecoder_surface_pool_frames = 12\nzero_copy_surface_pool_slots = 24\n",
    );

    assert_eq!(config.video.decoder_surface_pool_frames, 12);
    assert_eq!(config.video.zero_copy_surface_pool_slots, 0);
}

/// Отсутствующие ключи и отсутствующая секция `[video]` — no-op: действуют defaults (авто).
#[test]
fn schema_v26_without_pool_keys_loads_automatic_defaults() {
    let config = load_document("schema_version = 26\n");

    assert_eq!(config.video.decoder_surface_pool_frames, 0);
    assert_eq!(config.video.zero_copy_surface_pool_slots, 0);
}

/// В актуальной схеме 24 — обычное явное значение и больше не переписывается.
#[test]
fn schema_v27_explicit_24_is_kept() {
    let config = load_document(
        "schema_version = 27\n\n[video]\ndecoder_surface_pool_frames = 24\nzero_copy_surface_pool_slots = 24\n",
    );

    assert_eq!(config.video.decoder_surface_pool_frames, 24);
    assert_eq!(config.video.zero_copy_surface_pool_slots, 24);
}

/// Default-документ пишет 0, читается обратно без потерь и проходит валидацию watermarks.
#[test]
fn default_document_round_trips_automatic_pools() {
    let default_text = crate::AppConfig::default()
        .to_pretty_toml()
        .expect("defaults are serialized");
    assert!(default_text.contains("decoder_surface_pool_frames = 0"));
    assert!(default_text.contains("zero_copy_surface_pool_slots = 0"));

    let config = load_document(&default_text);

    assert_eq!(config.video.decoder_surface_pool_frames, 0);
    assert_eq!(config.video.zero_copy_surface_pool_slots, 0);
}

/// При слотах = 0 watermark проверяется по минимальному автоматическому пулу (16):
/// target 16 проходит, 17 отклоняется.
#[test]
fn automatic_slots_validate_watermarks_against_smallest_automatic_pool() {
    let mut config = crate::AppConfig::default();
    config.video.scheduler.surface_free_slots_target = crate::MIN_AUTOMATIC_SURFACE_POOL_FRAMES;
    validation::validate_app_config(&config)
        .expect("target equal to smallest automatic pool is valid");

    config.video.scheduler.surface_free_slots_target = crate::MIN_AUTOMATIC_SURFACE_POOL_FRAMES + 1;
    let error = validation::validate_app_config(&config)
        .expect_err("target above smallest automatic pool is rejected");
    assert!(
        error
            .to_string()
            .contains("video.scheduler.surface_free_slots_target")
    );
}
