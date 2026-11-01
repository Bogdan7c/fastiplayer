//! Настройки субтитров (`[subtitles]`, схема v15): defaults, миграция v12 и validation.
//!
//! Решение владельца (субтитры, сессия 04): лимиты, язык и стиль живут в `config.toml`;
//! старый файл без секции обязан грузиться с defaults, равными прежним константам
//! приложения, а неверные значения отклоняться с полным путём поля в ошибке.

use super::*;
use crate::{
    MAX_EMBEDDED_CUE_MAX_OPEN_DURATION_MS, MAX_EMBEDDED_FEED_CAPACITY, MAX_MAX_ATTACHED_FONT_MB,
    MAX_MAX_ATTACHED_FONTS_TOTAL_MB, MAX_MAX_FILE_SIZE_MB, MAX_PREFERRED_LANGUAGE_COUNT,
    MIN_EMBEDDED_CUE_MAX_OPEN_DURATION_MS, MIN_EMBEDDED_FEED_CAPACITY, MIN_MAX_CUES_ON_SCREEN,
    SubtitleDecorationPreference, SubtitleStreamConfig, SubtitlesConfig,
};

/// Документ v12: пользовательские значения других секций есть, `[subtitles]` ещё нет.
const SCHEMA_V12_WITHOUT_SUBTITLES: &str = r#"
schema_version = 12

[playlist]
load_siblings = false

[audio]
volume = 0.4
"#;

/// Документ v13: стиль субтитров уже настроен, новых ключей позиции и ASS-флага ещё нет.
const SCHEMA_V13_WITHOUT_POSITION: &str = r#"
schema_version = 13

[subtitles]
enabled = true
text_size_percent = 120
"#;

/// Документ v14: стиль настроен, лимитов встроенных дорожек (сессия 06) ещё нет.
const SCHEMA_V14_WITHOUT_EMBEDDED_LIMITS: &str = r#"
schema_version = 14

[subtitles]
enabled = true
position_vertical_percent = 10
max_file_size_mb = 16
"#;

/// Проверяет, что `config` отклонён и ошибка называет `field`.
fn assert_rejected_naming(config: &AppConfig, field: &str) {
    let error = config
        .validate()
        .expect_err("недопустимое значение должно быть отклонено");
    assert!(
        error.to_string().contains(field),
        "ошибка `{error}` должна называть поле `{field}`"
    );
}

/// Старый файл грузится с defaults субтитров, чужие значения целы, после сохранения
/// файл содержит текущую версию схемы и секцию, а повторная загрузка ничего не меняет.
#[test]
fn schema_v12_without_subtitles_loads_defaults_and_roundtrips() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V12_WITHOUT_SUBTITLES).expect("v12 fixture written");

    let migrated = load_from_path(&config_path).expect("v12 config loads");
    assert_eq!(migrated.config.schema_version, 30);
    assert_eq!(migrated.config.subtitles, SubtitlesConfig::default());
    assert!(!migrated.config.playlist.load_siblings);
    assert!((migrated.config.audio.volume - 0.4).abs() < f64::EPSILON);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("schema_version = 30"));
    assert!(saved_text.contains("[subtitles]"));
    assert!(saved_text.contains("max_file_size_mb = 8"));
    assert!(saved_text.contains("decoration = \"box\""));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// v13 без новых ключей: позиция и ASS-флаг получают defaults, чужие значения целы,
/// после сохранения файл содержит v14 и новые ключи, повторная загрузка ничего не меняет.
#[test]
fn schema_v13_without_position_keys_loads_defaults_and_roundtrips() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V13_WITHOUT_POSITION).expect("v13 fixture written");

    let migrated = load_from_path(&config_path).expect("v13 config loads");
    assert_eq!(migrated.config.schema_version, 30);
    assert_eq!(migrated.config.subtitles.position_horizontal_percent, 50);
    assert_eq!(migrated.config.subtitles.position_vertical_percent, 6);
    assert!(!migrated.config.subtitles.ass_force_user_style);
    assert!(migrated.config.subtitles.enabled);
    assert_eq!(migrated.config.subtitles.text_size_percent, 120);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("schema_version = 30"));
    assert!(saved_text.contains("position_horizontal_percent = 50"));
    assert!(saved_text.contains("position_vertical_percent = 6"));
    assert!(saved_text.contains("ass_force_user_style = false"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// v14 без лимитов встроенных дорожек: новые ключи получают defaults, остальное цело,
/// после сохранения файл содержит v17 и новые ключи, повторная загрузка ничего не меняет.
#[test]
fn schema_v14_without_embedded_limits_loads_defaults_and_roundtrips() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V14_WITHOUT_EMBEDDED_LIMITS).expect("v14 fixture written");

    let migrated = load_from_path(&config_path).expect("v14 config loads");
    assert_eq!(migrated.config.schema_version, 30);
    let subtitles = &migrated.config.subtitles;
    assert_eq!(subtitles.max_attached_font_mb, 20);
    assert_eq!(subtitles.max_attached_fonts_total_mb, 100);
    assert_eq!(subtitles.embedded_feed_capacity, 4096);
    assert_eq!(subtitles.embedded_cue_max_open_duration_ms, 5000);
    assert!(subtitles.enabled);
    assert_eq!(subtitles.position_vertical_percent, 10);
    assert_eq!(subtitles.max_file_size_mb, 16);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("schema_version = 30"));
    assert!(saved_text.contains("max_attached_font_mb = 20"));
    assert!(saved_text.contains("max_attached_fonts_total_mb = 100"));
    assert!(saved_text.contains("embedded_feed_capacity = 4096"));
    assert!(saved_text.contains("embedded_cue_max_open_duration_ms = 5000"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// Граничные значения лимитов встроенных дорожек переживают файл.
#[test]
fn embedded_limit_bounds_roundtrip_through_file() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let mut config = AppConfig::default();
    config.subtitles.max_attached_font_mb = MAX_MAX_ATTACHED_FONT_MB;
    config.subtitles.max_attached_fonts_total_mb = MAX_MAX_ATTACHED_FONTS_TOTAL_MB;
    config.subtitles.embedded_feed_capacity = MIN_EMBEDDED_FEED_CAPACITY;
    config.subtitles.embedded_cue_max_open_duration_ms = MAX_EMBEDDED_CUE_MAX_OPEN_DURATION_MS;
    save_validated_atomic_at(&config_path, &config).expect("bounds config saves");
    let loaded = load_from_path(&config_path).expect("bounds config loads");
    assert_eq!(loaded.config.subtitles, config.subtitles);
}

/// Значения вне диапазона и общий лимит меньше лимита одного вложения отклоняются с полным путём поля.
#[test]
fn embedded_limits_out_of_range_or_inconsistent_are_rejected() {
    let mut zero_font = AppConfig::default();
    zero_font.subtitles.max_attached_font_mb = 0;
    assert_rejected_naming(&zero_font, "subtitles.max_attached_font_mb");

    let mut huge_font = AppConfig::default();
    huge_font.subtitles.max_attached_font_mb = MAX_MAX_ATTACHED_FONT_MB + 1;
    assert_rejected_naming(&huge_font, "subtitles.max_attached_font_mb");

    let mut huge_total = AppConfig::default();
    huge_total.subtitles.max_attached_fonts_total_mb = MAX_MAX_ATTACHED_FONTS_TOTAL_MB + 1;
    assert_rejected_naming(&huge_total, "subtitles.max_attached_fonts_total_mb");

    let mut total_below_single = AppConfig::default();
    total_below_single.subtitles.max_attached_font_mb = 50;
    total_below_single.subtitles.max_attached_fonts_total_mb = 49;
    assert_rejected_naming(&total_below_single, "subtitles.max_attached_fonts_total_mb");

    let mut small_feed = AppConfig::default();
    small_feed.subtitles.embedded_feed_capacity = MIN_EMBEDDED_FEED_CAPACITY - 1;
    assert_rejected_naming(&small_feed, "subtitles.embedded_feed_capacity");

    let mut huge_feed = AppConfig::default();
    huge_feed.subtitles.embedded_feed_capacity = MAX_EMBEDDED_FEED_CAPACITY + 1;
    assert_rejected_naming(&huge_feed, "subtitles.embedded_feed_capacity");

    let mut short_duration = AppConfig::default();
    short_duration.subtitles.embedded_cue_max_open_duration_ms =
        MIN_EMBEDDED_CUE_MAX_OPEN_DURATION_MS - 1;
    assert_rejected_naming(
        &short_duration,
        "subtitles.embedded_cue_max_open_duration_ms",
    );

    let mut long_duration = AppConfig::default();
    long_duration.subtitles.embedded_cue_max_open_duration_ms =
        MAX_EMBEDDED_CUE_MAX_OPEN_DURATION_MS + 1;
    assert_rejected_naming(
        &long_duration,
        "subtitles.embedded_cue_max_open_duration_ms",
    );
}

/// Негативная и граничная позиция переживают файл; значения вне диапазона отклоняются.
#[test]
fn subtitle_position_bounds_roundtrip_and_out_of_range_is_rejected() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let mut config = AppConfig::default();
    config.subtitles.position_horizontal_percent = 100;
    config.subtitles.position_vertical_percent = -40;
    config.subtitles.ass_force_user_style = true;
    save_validated_atomic_at(&config_path, &config).expect("bounds config saves");
    let loaded = load_from_path(&config_path).expect("bounds config loads");
    assert_eq!(loaded.config.subtitles, config.subtitles);

    let mut too_high = AppConfig::default();
    too_high.subtitles.position_vertical_percent = 96;
    assert_rejected_naming(&too_high, "subtitles.position_vertical_percent");
    let mut too_low = AppConfig::default();
    too_low.subtitles.position_vertical_percent = -41;
    assert_rejected_naming(&too_low, "subtitles.position_vertical_percent");
    let mut too_right = AppConfig::default();
    too_right.subtitles.position_horizontal_percent = 101;
    assert_rejected_naming(&too_right, "subtitles.position_horizontal_percent");
}

/// Defaults повторяют прежние константы приложения и пресет `youtube_like()`.
#[test]
fn subtitle_defaults_match_previous_application_constants() {
    let defaults = SubtitlesConfig::default();
    assert!(!defaults.enabled);
    assert!(defaults.preferred_language_codes().is_empty());
    assert_eq!(defaults.decoration, SubtitleDecorationPreference::Box);
    assert_eq!(defaults.text_size_percent, 100);
    assert_eq!(defaults.text_color, "#FFFFFF");
    assert_eq!(defaults.background_color, "#080808");
    assert_eq!(defaults.background_opacity_percent, 75);
    assert_eq!(defaults.position_horizontal_percent, 50);
    assert_eq!(defaults.position_vertical_percent, 6);
    assert!(!defaults.ass_force_user_style);
    assert_eq!(defaults.max_file_size_mb, 8);
    assert_eq!(defaults.max_cues_per_file, 100_000);
    assert_eq!(defaults.sidecar_max_entries_per_folder, 5_000);
    assert_eq!(defaults.sidecar_max_candidates, 32);
    assert_eq!(defaults.render_cache_mb, 32);
    assert_eq!(defaults.font_lookup_cache_chars, 65_536);
    assert_eq!(defaults.max_chars_per_cue, 2_000);
    assert_eq!(defaults.max_cues_on_screen, 16);
    assert!(AppConfig::default().validate().is_ok());
}

/// Пользовательские значения всех полей переживают сохранение и загрузку.
#[test]
fn custom_subtitle_settings_roundtrip_through_file() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let config = AppConfig {
        subtitles: SubtitlesConfig {
            enabled: true,
            preferred_languages: "uk, pt-BR, en".to_string(),
            decoration: SubtitleDecorationPreference::Outline,
            text_size_percent: 140,
            text_color: "#ffcc00".to_string(),
            background_color: "#102030".to_string(),
            background_opacity_percent: 40,
            position_horizontal_percent: 30,
            position_vertical_percent: -15,
            ass_force_user_style: true,
            ass_position_mode: crate::AssPositionMode::WholeLayer,
            max_file_size_mb: 16,
            max_cues_per_file: 250_000,
            sidecar_max_entries_per_folder: 1_000,
            sidecar_max_candidates: 64,
            render_cache_mb: 128,
            font_lookup_cache_chars: 4_096,
            max_chars_per_cue: 500,
            max_cues_on_screen: 4,
            max_attached_font_mb: 8,
            max_attached_fonts_total_mb: 64,
            embedded_feed_capacity: 1_024,
            embedded_cue_max_open_duration_ms: 2_500,
            max_ass_header_kb: 256,
            max_ass_styles: 50,
            max_ass_override_tags_per_event: 64,
            max_ass_events_on_screen: 32,
            prerender_frames: 12,
            prepared_overlays: 16,
            blur_scratch_retained_mb: 64,
            glyph_cache_mb: 96,
            glyph_outline_cache_items: 4_096,
            shaping_cache_mb: 12,
            pgs_displayed_track_cache_mb: 48,
            pgs_background_track_window_s: 90,
            stream: SubtitleStreamConfig {
                ahead_s: 90,
                behind_s: 20,
                max_segment_kb: 512,
                max_pending_segments: 4,
                failure_threshold: 5,
                live_retention_s: 300,
            },
        },
        ..AppConfig::default()
    };

    save_validated_atomic_at(&config_path, &config).expect("custom config saves");
    let loaded = load_from_path(&config_path).expect("custom config loads");

    assert_eq!(loaded.config.subtitles, config.subtitles);
    assert_eq!(
        loaded.config.subtitles.preferred_language_codes(),
        vec!["uk", "pt-BR", "en"]
    );
}

/// Значения вне диапазона отклоняются и называют полный путь поля.
#[test]
fn out_of_range_subtitle_values_are_rejected_with_full_path() {
    let mut too_large_file = AppConfig::default();
    too_large_file.subtitles.max_file_size_mb = MAX_MAX_FILE_SIZE_MB + 1;
    assert_rejected_naming(&too_large_file, "subtitles.max_file_size_mb");

    let mut no_cues_on_screen = AppConfig::default();
    no_cues_on_screen.subtitles.max_cues_on_screen = MIN_MAX_CUES_ON_SCREEN - 1;
    assert_rejected_naming(&no_cues_on_screen, "subtitles.max_cues_on_screen");

    let mut tiny_text = AppConfig::default();
    tiny_text.subtitles.text_size_percent = 49;
    assert_rejected_naming(&tiny_text, "subtitles.text_size_percent");

    let mut over_opaque = AppConfig::default();
    over_opaque.subtitles.background_opacity_percent = 101;
    assert_rejected_naming(&over_opaque, "subtitles.background_opacity_percent");

    let mut tiny_cache = AppConfig::default();
    tiny_cache.subtitles.font_lookup_cache_chars = 10;
    assert_rejected_naming(&tiny_cache, "subtitles.font_lookup_cache_chars");

    // Граничные значения допустимы.
    let mut boundary = AppConfig::default();
    boundary.subtitles.max_file_size_mb = MAX_MAX_FILE_SIZE_MB;
    boundary.subtitles.max_cues_on_screen = MIN_MAX_CUES_ON_SCREEN;
    assert!(boundary.validate().is_ok());
}

/// Неверные списки языков отклоняются, корректные принимаются.
#[test]
fn invalid_preferred_languages_are_rejected() {
    let invalid_lists = [
        "u",                                 // слишком короткий тег
        "uk,,en",                            // пустой элемент
        "uk, en,",                           // пустой хвост
        "uk, UK",                            // дубликат без учёта регистра
        "русский",                           // не ASCII
        "uk_UA",                             // недопустимый символ
        "u1",                                // первый подтег не из букв
        "ukrainian-",                        // пустая часть после дефиса
        "toolonglanguage-code",              // первый подтег длиннее 8 букв
        "a-b-c-d-e-f-g-h-i-j-k-l-m-n-o-p-q", // тег длиннее 35 символов
        "aa,bb,cc,dd,ee,ff,gg,hh,ii",        // больше восьми языков
    ];
    for invalid_list in invalid_lists {
        let mut config = AppConfig::default();
        config.subtitles.preferred_languages = invalid_list.to_string();
        assert_rejected_naming(&config, "subtitles.preferred_languages");
    }
    assert_eq!(MAX_PREFERRED_LANGUAGE_COUNT, 8);

    for valid_list in ["", "  ", "uk", "uk, ru, en", "pt-BR,zh-Hans-CN", "fil, yue"] {
        let mut config = AppConfig::default();
        config.subtitles.preferred_languages = valid_list.to_string();
        assert!(
            config.validate().is_ok(),
            "`{valid_list}` должен приниматься"
        );
    }
}

/// Цвета только `#RRGGBB`; ошибка называет нужное поле.
#[test]
fn invalid_subtitle_colors_are_rejected() {
    for invalid_color in [
        "FFFFFF",
        "#FFF",
        "#GGGGGG",
        "#FFFFFFFF",
        "",
        "red",
        "#12 456",
    ] {
        let mut bad_text = AppConfig::default();
        bad_text.subtitles.text_color = invalid_color.to_string();
        assert_rejected_naming(&bad_text, "subtitles.text_color");

        let mut bad_box = AppConfig::default();
        bad_box.subtitles.background_color = invalid_color.to_string();
        assert_rejected_naming(&bad_box, "subtitles.background_color");
    }

    let mut lowercase = AppConfig::default();
    lowercase.subtitles.text_color = "#aabbcc".to_string();
    lowercase.subtitles.background_color = "#AaBbCc".to_string();
    assert!(lowercase.validate().is_ok());
}

/// Неизвестное поле секции и неизвестный вариант оформления — ошибка strict parser-а.
#[test]
fn unknown_subtitle_key_or_decoration_is_a_parse_error() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");

    fs::write(
        &config_path,
        "schema_version = 23\n\n[subtitles]\ndecoration = \"glow\"\n",
    )
    .expect("fixture written");
    assert!(load_from_path(&config_path).is_err());

    fs::write(
        &config_path,
        "schema_version = 23\n\n[subtitles]\nmystery = 1\n",
    )
    .expect("fixture written");
    assert!(load_from_path(&config_path).is_err());
}
