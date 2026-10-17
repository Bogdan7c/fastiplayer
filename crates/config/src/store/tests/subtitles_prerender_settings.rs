//! Настройки упреждающей отрисовки субтитров `[subtitles]` (схема v19): defaults, миграция v18,
//! диапазоны и граничные значения.

use super::*;
use crate::{
    DEFAULT_PREPARED_OVERLAYS, DEFAULT_PRERENDER_FRAMES, MAX_PREPARED_OVERLAYS,
    MAX_PRERENDER_FRAMES, MIN_PREPARED_OVERLAYS, MIN_PRERENDER_FRAMES,
};

/// Документ v18: свои значения субтитров (включая ASS-лимиты) и нет новых ключей.
const SCHEMA_V18_WITHOUT_PRERENDER_KEYS: &str = r#"
schema_version = 18

[subtitles]
enabled = true
text_size_percent = 130
max_ass_events_on_screen = 32
render_cache_mb = 64
"#;

/// Файл v18 поднимается до v19: значения пользователя целы, новые поля равны defaults,
/// после сохранения ключи на месте, повторная загрузка ничего не меняет.
#[test]
fn schema_v18_without_prerender_keys_loads_defaults_and_keeps_user_values() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V18_WITHOUT_PRERENDER_KEYS).expect("v18 fixture written");

    let migrated = load_from_path(&config_path).expect("v18 config loads");
    assert_eq!(migrated.config.schema_version, 20);
    let subtitles = &migrated.config.subtitles;
    assert_eq!(subtitles.prerender_frames, 4);
    assert_eq!(subtitles.prepared_overlays, 8);
    assert_eq!(subtitles.prerender_frames, DEFAULT_PRERENDER_FRAMES);
    assert_eq!(subtitles.prepared_overlays, DEFAULT_PREPARED_OVERLAYS);
    // Миграция не переписывает значения пользователя.
    assert!(subtitles.enabled);
    assert_eq!(subtitles.text_size_percent, 130);
    assert_eq!(subtitles.max_ass_events_on_screen, 32);
    assert_eq!(subtitles.render_cache_mb, 64);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("schema_version = 20"));
    assert!(saved_text.contains("prerender_frames = 4"));
    assert!(saved_text.contains("prepared_overlays = 8"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// Границы диапазонов переживают запись и чтение файла (включая 0 упреждения).
#[test]
fn prerender_boundary_values_roundtrip_through_file() {
    for (frames, overlays) in [
        (MIN_PRERENDER_FRAMES, MIN_PREPARED_OVERLAYS),
        (MAX_PRERENDER_FRAMES, MAX_PREPARED_OVERLAYS),
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        let mut config = AppConfig::default();
        config.subtitles.prerender_frames = frames;
        config.subtitles.prepared_overlays = overlays;
        save_validated_atomic_at(&config_path, &config).expect("boundary config saves");
        let reloaded = load_from_path(&config_path).expect("boundary config reloads");
        assert_eq!(reloaded.config.subtitles.prerender_frames, frames);
        assert_eq!(reloaded.config.subtitles.prepared_overlays, overlays);
    }
}

/// Значения вне диапазона отклоняются с полным путём поля; `prerender_frames = 0` допустим,
/// а `prepared_overlays = 0` нет.
#[test]
fn prerender_settings_out_of_range_are_rejected() {
    type ConfigMutation = fn(&mut AppConfig);
    let rejected: [(&str, ConfigMutation); 3] = [
        ("subtitles.prerender_frames", |c| {
            c.subtitles.prerender_frames = MAX_PRERENDER_FRAMES + 1;
        }),
        ("subtitles.prepared_overlays", |c| {
            c.subtitles.prepared_overlays = MIN_PREPARED_OVERLAYS - 1;
        }),
        ("subtitles.prepared_overlays", |c| {
            c.subtitles.prepared_overlays = MAX_PREPARED_OVERLAYS + 1;
        }),
    ];
    for (field, mutate) in rejected {
        let mut config = AppConfig::default();
        mutate(&mut config);
        let error = config
            .validate()
            .expect_err("значение вне диапазона отклонено");
        assert!(
            error.to_string().contains(field),
            "ошибка `{error}` без поля `{field}`"
        );
    }

    let mut zero_lookahead = AppConfig::default();
    zero_lookahead.subtitles.prerender_frames = 0;
    zero_lookahead
        .validate()
        .expect("нулевое упреждение допустимо: готовится только текущий кадр");
}
