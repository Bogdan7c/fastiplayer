//! Лимиты потоковых субтитров `[subtitles.stream]` (схема v23): defaults, миграция v22,
//! граничные значения, диапазоны и согласованность окон.

use super::*;
use crate::{
    DEFAULT_STREAM_AHEAD_S, DEFAULT_STREAM_BEHIND_S, DEFAULT_STREAM_FAILURE_THRESHOLD,
    DEFAULT_STREAM_LIVE_RETENTION_S, DEFAULT_STREAM_MAX_PENDING_SEGMENTS,
    DEFAULT_STREAM_MAX_SEGMENT_KB, MAX_STREAM_AHEAD_S, MAX_STREAM_BEHIND_S,
    MAX_STREAM_FAILURE_THRESHOLD, MAX_STREAM_LIVE_RETENTION_S, MAX_STREAM_MAX_PENDING_SEGMENTS,
    MAX_STREAM_MAX_SEGMENT_KB, MIN_STREAM_AHEAD_S, MIN_STREAM_BEHIND_S,
    MIN_STREAM_FAILURE_THRESHOLD, MIN_STREAM_LIVE_RETENTION_S, MIN_STREAM_MAX_PENDING_SEGMENTS,
    MIN_STREAM_MAX_SEGMENT_KB, SubtitleStreamConfig,
};

/// Документ v22: свои значения субтитров и нет таблицы `[subtitles.stream]`.
const SCHEMA_V22_WITHOUT_STREAM_TABLE: &str = r#"
schema_version = 22

[subtitles]
enabled = true
text_size_percent = 130
pgs_background_track_window_s = 90
"#;

/// Файл v22 поднимается до v23 без переписывания значений: новая таблица получает defaults,
/// значения пользователя целы, после сохранения таблица записана и повторная загрузка
/// ничего не меняет.
#[test]
fn schema_v22_without_stream_table_loads_defaults_and_keeps_user_values() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V22_WITHOUT_STREAM_TABLE).expect("v22 fixture written");

    let migrated = load_from_path(&config_path).expect("v22 config loads");
    assert_eq!(migrated.config.schema_version, 26);
    let stream = migrated.config.subtitles.stream;
    assert_eq!(stream, SubtitleStreamConfig::default());
    assert_eq!(stream.ahead_s, DEFAULT_STREAM_AHEAD_S);
    assert_eq!(stream.behind_s, DEFAULT_STREAM_BEHIND_S);
    assert_eq!(stream.max_segment_kb, DEFAULT_STREAM_MAX_SEGMENT_KB);
    assert_eq!(
        stream.max_pending_segments,
        DEFAULT_STREAM_MAX_PENDING_SEGMENTS
    );
    assert_eq!(stream.failure_threshold, DEFAULT_STREAM_FAILURE_THRESHOLD);
    assert_eq!(stream.live_retention_s, DEFAULT_STREAM_LIVE_RETENTION_S);
    // Миграция не переписывает значения пользователя.
    assert!(migrated.config.subtitles.enabled);
    assert_eq!(migrated.config.subtitles.text_size_percent, 130);
    assert_eq!(migrated.config.subtitles.pgs_background_track_window_s, 90);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("schema_version = 26"));
    assert!(saved_text.contains("[subtitles.stream]"));
    assert!(saved_text.contains("ahead_s = 60"));
    assert!(saved_text.contains("live_retention_s = 600"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// Частично заданная таблица: указанные ключи читаются, остальные остаются по умолчанию.
#[test]
fn partial_stream_table_keeps_given_keys_and_defaults_for_the_rest() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        "schema_version = 23\n\n[subtitles.stream]\nahead_s = 120\nfailure_threshold = 7\n",
    )
    .expect("fixture written");
    let loaded = load_from_path(&config_path).expect("partial table loads");
    let stream = loaded.config.subtitles.stream;
    assert_eq!(stream.ahead_s, 120);
    assert_eq!(stream.failure_threshold, 7);
    assert_eq!(stream.behind_s, DEFAULT_STREAM_BEHIND_S);
    assert_eq!(stream.live_retention_s, DEFAULT_STREAM_LIVE_RETENTION_S);
}

/// Неизвестный ключ в таблице — ошибка разбора (как во всех строгих секциях).
#[test]
fn unknown_stream_key_is_a_parse_error() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        "schema_version = 23\n\n[subtitles.stream]\nmystery = 1\n",
    )
    .expect("fixture written");
    assert!(load_from_path(&config_path).is_err());
}

/// Границы диапазонов переживают запись и чтение.
#[test]
fn stream_boundary_values_roundtrip_through_file() {
    let low = SubtitleStreamConfig {
        ahead_s: MIN_STREAM_AHEAD_S,
        behind_s: MIN_STREAM_BEHIND_S,
        max_segment_kb: MIN_STREAM_MAX_SEGMENT_KB,
        max_pending_segments: MIN_STREAM_MAX_PENDING_SEGMENTS,
        failure_threshold: MIN_STREAM_FAILURE_THRESHOLD,
        live_retention_s: MIN_STREAM_LIVE_RETENTION_S,
    };
    let high = SubtitleStreamConfig {
        ahead_s: MAX_STREAM_AHEAD_S,
        behind_s: MAX_STREAM_BEHIND_S,
        max_segment_kb: MAX_STREAM_MAX_SEGMENT_KB,
        max_pending_segments: MAX_STREAM_MAX_PENDING_SEGMENTS,
        failure_threshold: MAX_STREAM_FAILURE_THRESHOLD,
        live_retention_s: MAX_STREAM_LIVE_RETENTION_S,
    };
    for stream in [low, high] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        let mut config = AppConfig::default();
        config.subtitles.stream = stream;
        save_validated_atomic_at(&config_path, &config).expect("boundary config saves");
        let reloaded = load_from_path(&config_path).expect("boundary config reloads");
        assert_eq!(reloaded.config.subtitles.stream, stream);
    }
}

/// Значения вне диапазона отклоняются, ошибка называет полный путь поля.
#[test]
fn stream_values_out_of_range_are_rejected_with_the_field_path() {
    type Mutation = fn(&mut SubtitleStreamConfig);
    let cases: [(&str, Mutation); 8] = [
        ("subtitles.stream.ahead_s", |s| {
            s.ahead_s = MIN_STREAM_AHEAD_S - 1;
        }),
        ("subtitles.stream.ahead_s", |s| {
            s.ahead_s = MAX_STREAM_AHEAD_S + 1;
        }),
        ("subtitles.stream.behind_s", |s| {
            s.behind_s = MAX_STREAM_BEHIND_S + 1;
        }),
        ("subtitles.stream.max_segment_kb", |s| {
            s.max_segment_kb = MIN_STREAM_MAX_SEGMENT_KB - 1;
        }),
        ("subtitles.stream.max_pending_segments", |s| {
            s.max_pending_segments = 0;
        }),
        ("subtitles.stream.failure_threshold", |s| {
            s.failure_threshold = MAX_STREAM_FAILURE_THRESHOLD + 1;
        }),
        ("subtitles.stream.live_retention_s", |s| {
            s.live_retention_s = MIN_STREAM_LIVE_RETENTION_S - 1;
        }),
        ("subtitles.stream.live_retention_s", |s| {
            s.live_retention_s = MAX_STREAM_LIVE_RETENTION_S + 1;
        }),
    ];
    for (field, mutate) in cases {
        let mut config = AppConfig::default();
        mutate(&mut config.subtitles.stream);
        let error = config.validate().expect_err("value out of range rejected");
        assert!(
            error.to_string().contains(field),
            "error `{error}` does not name `{field}`"
        );
    }
}

/// История live короче окна «назад» отклоняется: докачанные назад реплики сразу вытеснялись бы.
#[test]
fn live_retention_shorter_than_look_behind_is_rejected() {
    let mut config = AppConfig::default();
    config.subtitles.stream.behind_s = 300;
    config.subtitles.stream.live_retention_s = 120;
    let error = config
        .validate()
        .expect_err("inconsistent windows rejected");
    assert!(
        error
            .to_string()
            .contains("subtitles.stream.live_retention_s")
    );

    config.subtitles.stream.live_retention_s = 300;
    config.validate().expect("equal windows are accepted");
}
