//! ASS-настройки `[subtitles]` (схема v18): defaults, миграция v17, диапазоны, режим позиции.

use super::*;
use crate::{
    AssPositionMode, MAX_MAX_ASS_EVENTS_ON_SCREEN, MAX_MAX_ASS_HEADER_KB,
    MAX_MAX_ASS_OVERRIDE_TAGS_PER_EVENT, MAX_MAX_ASS_STYLES, MIN_MAX_ASS_EVENTS_ON_SCREEN,
    MIN_MAX_ASS_HEADER_KB, MIN_MAX_ASS_OVERRIDE_TAGS_PER_EVENT, MIN_MAX_ASS_STYLES,
};

/// Документ v17: стиль субтитров настроен, ASS-ключей ещё нет.
const SCHEMA_V17_WITHOUT_ASS_KEYS: &str = r#"
schema_version = 17

[subtitles]
enabled = true
text_size_percent = 130
ass_force_user_style = true
"#;

/// Старый файл поднимается до v18 с default-ами ASS, значения пользователя целы,
/// после сохранения ключи на месте, повторная загрузка ничего не меняет.
#[test]
fn schema_v17_without_ass_keys_loads_defaults_and_keeps_user_values() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V17_WITHOUT_ASS_KEYS).expect("v17 fixture written");

    let migrated = load_from_path(&config_path).expect("v17 config loads");
    assert_eq!(migrated.config.schema_version, 21);
    let subtitles = &migrated.config.subtitles;
    assert_eq!(
        subtitles.ass_position_mode,
        AssPositionMode::MainLinesAvoidControls
    );
    assert_eq!(subtitles.max_ass_header_kb, 1_024);
    assert_eq!(subtitles.max_ass_styles, 1_000);
    assert_eq!(subtitles.max_ass_override_tags_per_event, 512);
    assert_eq!(subtitles.max_ass_events_on_screen, 256);
    assert!(subtitles.enabled);
    assert_eq!(subtitles.text_size_percent, 130);
    assert!(subtitles.ass_force_user_style);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("schema_version = 21"));
    assert!(saved_text.contains("ass_position_mode = \"main_lines_avoid_controls\""));
    assert!(saved_text.contains("max_ass_header_kb = 1024"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// Режим `whole_layer` и границы лимитов переживают запись и чтение файла.
#[test]
fn ass_settings_roundtrip_through_file() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let mut config = AppConfig::default();
    config.subtitles.ass_position_mode = AssPositionMode::WholeLayer;
    config.subtitles.max_ass_header_kb = MAX_MAX_ASS_HEADER_KB;
    config.subtitles.max_ass_styles = MIN_MAX_ASS_STYLES;
    config.subtitles.max_ass_override_tags_per_event = MAX_MAX_ASS_OVERRIDE_TAGS_PER_EVENT;
    config.subtitles.max_ass_events_on_screen = MAX_MAX_ASS_EVENTS_ON_SCREEN;
    save_validated_atomic_at(&config_path, &config).expect("config saves");

    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("ass_position_mode = \"whole_layer\""));
    let reloaded = load_from_path(&config_path).expect("config reloads");
    assert_eq!(reloaded.config.subtitles, config.subtitles);
}

/// Правка одного поля конфига для таблицы отклонений.
type ConfigMutation = fn(&mut AppConfig);

/// Значения вне диапазона отклоняются с полным путём поля; неизвестный режим — ошибка разбора.
#[test]
fn ass_limits_out_of_range_are_rejected_and_unknown_mode_fails_to_parse() {
    let cases: [(&str, ConfigMutation); 8] = [
        ("subtitles.max_ass_header_kb", |c| {
            c.subtitles.max_ass_header_kb = MIN_MAX_ASS_HEADER_KB - 1;
        }),
        ("subtitles.max_ass_header_kb", |c| {
            c.subtitles.max_ass_header_kb = MAX_MAX_ASS_HEADER_KB + 1;
        }),
        ("subtitles.max_ass_styles", |c| {
            c.subtitles.max_ass_styles = MIN_MAX_ASS_STYLES - 1;
        }),
        ("subtitles.max_ass_styles", |c| {
            c.subtitles.max_ass_styles = MAX_MAX_ASS_STYLES + 1;
        }),
        ("subtitles.max_ass_override_tags_per_event", |c| {
            c.subtitles.max_ass_override_tags_per_event = MIN_MAX_ASS_OVERRIDE_TAGS_PER_EVENT - 1;
        }),
        ("subtitles.max_ass_override_tags_per_event", |c| {
            c.subtitles.max_ass_override_tags_per_event = MAX_MAX_ASS_OVERRIDE_TAGS_PER_EVENT + 1;
        }),
        ("subtitles.max_ass_events_on_screen", |c| {
            c.subtitles.max_ass_events_on_screen = MIN_MAX_ASS_EVENTS_ON_SCREEN - 1;
        }),
        ("subtitles.max_ass_events_on_screen", |c| {
            c.subtitles.max_ass_events_on_screen = MAX_MAX_ASS_EVENTS_ON_SCREEN + 1;
        }),
    ];
    for (field, mutate) in cases {
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

    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        "schema_version = 21\n\n[subtitles]\nass_position_mode = \"somewhere\"\n",
    )
    .expect("fixture written");
    assert!(load_from_path(&config_path).is_err());
}

/// Все три значения режима читаются из файла: прежние `author`/`whole_layer` не сломались,
/// новый по умолчанию `main_lines_avoid_controls` тоже явно задаётся.
#[test]
fn every_ass_position_mode_value_parses_from_file() {
    let expected = [
        (
            "main_lines_avoid_controls",
            AssPositionMode::MainLinesAvoidControls,
        ),
        ("author", AssPositionMode::Author),
        ("whole_layer", AssPositionMode::WholeLayer),
    ];
    for (serialized, mode) in expected {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!("schema_version = 21\n\n[subtitles]\nass_position_mode = \"{serialized}\"\n"),
        )
        .expect("fixture written");
        let loaded = load_from_path(&config_path).expect("known mode value loads");
        assert_eq!(
            loaded.config.subtitles.ass_position_mode, mode,
            "{serialized}"
        );
    }
}
