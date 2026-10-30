//! Окно буфера браузерного режима `[browser_session]` (схема v28): defaults, миграция v27,
//! частичная таблица, границы, диапазоны и политика восстановления при значении вне диапазона.

use std::time::{Duration, UNIX_EPOCH};

use super::*;
use crate::{
    BrokenConfigProblem, BrowserSessionConfig, ConfigLoadOrigin, DEFAULT_BROWSER_BUFFER_AHEAD_S,
    DEFAULT_BROWSER_BUFFER_BEHIND_S, MAX_BROWSER_BUFFER_AHEAD_S, MAX_BROWSER_BUFFER_BEHIND_S,
    MIN_BROWSER_BUFFER_AHEAD_S, MIN_BROWSER_BUFFER_BEHIND_S, load_or_recover_at,
};

/// Документ v27: свои значения в других секциях и нет таблицы `[browser_session]`.
const SCHEMA_V27_WITHOUT_BROWSER_SESSION: &str = r#"
schema_version = 27

[audio]
volume = 0.4

[subtitles]
text_size_percent = 130
"#;

/// Временный каталог с `config.toml` заданного содержимого.
fn config_file_with(config_text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, config_text).expect("config fixture written");
    (temp_dir, config_path)
}

/// Defaults решения владельца: 120 с вперёд и 30 с позади, в типизированном окне.
#[test]
fn default_buffer_window_is_120_seconds_ahead_and_30_behind() {
    let window = AppConfig::default().browser_session.buffer_window();

    assert_eq!(window.ahead(), Duration::from_secs(120));
    assert_eq!(window.behind(), Duration::from_secs(30));
    assert_eq!(DEFAULT_BROWSER_BUFFER_AHEAD_S, 120);
    assert_eq!(DEFAULT_BROWSER_BUFFER_BEHIND_S, 30);
}

/// Файл v27 поднимается до v28 без переписывания значений: новая таблица получает defaults,
/// значения пользователя целы, после сохранения таблица записана и повторная загрузка
/// ничего не меняет.
#[test]
fn schema_v27_without_browser_session_loads_defaults_and_keeps_user_values() {
    let (_temp_dir, config_path) = config_file_with(SCHEMA_V27_WITHOUT_BROWSER_SESSION);

    let migrated = load_from_path(&config_path).expect("v27 config loads");

    assert_eq!(migrated.config.schema_version, 28);
    assert_eq!(
        migrated.config.browser_session,
        BrowserSessionConfig::default()
    );
    // Миграция не переписывает значения пользователя.
    assert!((migrated.config.audio.volume - 0.4).abs() < f64::EPSILON);
    assert_eq!(migrated.config.subtitles.text_size_percent, 130);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("schema_version = 28"));
    assert!(saved_text.contains("[browser_session]"));
    assert!(saved_text.contains("buffer_ahead_s = 120"));
    assert!(saved_text.contains("buffer_behind_s = 30"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// Файл текущей схемы без таблицы (пользователь удалил её руками) читается с defaults.
#[test]
fn current_schema_without_browser_session_table_uses_defaults() {
    let (_temp_dir, config_path) = config_file_with("schema_version = 28\n");

    let loaded = load_from_path(&config_path).expect("config without table loads");

    assert_eq!(
        loaded.config.browser_session,
        BrowserSessionConfig::default()
    );
}

/// Частично заданная таблица: указанный ключ читается и попадает в окно, второй — default.
#[test]
fn partial_browser_session_table_keeps_given_key_and_defaults_for_the_rest() {
    let (_temp_dir, config_path) =
        config_file_with("schema_version = 28\n\n[browser_session]\nbuffer_ahead_s = 45\n");

    let loaded = load_from_path(&config_path).expect("partial table loads");
    let window = loaded.config.browser_session.buffer_window();

    assert_eq!(window.ahead(), Duration::from_secs(45));
    assert_eq!(
        window.behind(),
        Duration::from_secs(u64::from(DEFAULT_BROWSER_BUFFER_BEHIND_S))
    );
}

/// Неизвестный ключ в таблице — ошибка разбора (как во всех строгих секциях).
#[test]
fn unknown_browser_session_key_is_a_parse_error() {
    let (_temp_dir, config_path) =
        config_file_with("schema_version = 28\n\n[browser_session]\nbuffer_ahead_ms = 1\n");

    assert!(load_from_path(&config_path).is_err());
}

/// Границы диапазонов переживают запись и чтение, окно отражает записанные числа.
#[test]
fn browser_session_boundary_values_roundtrip_through_file() {
    let low = BrowserSessionConfig {
        buffer_ahead_s: MIN_BROWSER_BUFFER_AHEAD_S,
        buffer_behind_s: MIN_BROWSER_BUFFER_BEHIND_S,
    };
    let high = BrowserSessionConfig {
        buffer_ahead_s: MAX_BROWSER_BUFFER_AHEAD_S,
        buffer_behind_s: MAX_BROWSER_BUFFER_BEHIND_S,
    };
    for browser_session in [low, high] {
        let (_temp_dir, config_path) = config_file_with("");
        let config = AppConfig {
            browser_session,
            ..AppConfig::default()
        };

        save_validated_atomic_at(&config_path, &config).expect("boundary config saves");
        let reloaded = load_from_path(&config_path).expect("boundary config reloads");

        assert_eq!(reloaded.config.browser_session, browser_session);
        let window = reloaded.config.browser_session.buffer_window();
        assert_eq!(
            window.ahead(),
            Duration::from_secs(u64::from(browser_session.buffer_ahead_s))
        );
        assert_eq!(
            window.behind(),
            Duration::from_secs(u64::from(browser_session.buffer_behind_s))
        );
    }
}

/// Значения вне диапазона отклоняются, ошибка называет полный путь поля.
#[test]
fn browser_session_values_out_of_range_are_rejected_with_the_field_path() {
    type Mutation = fn(&mut BrowserSessionConfig);
    let cases: [(&str, Mutation); 3] = [
        ("browser_session.buffer_ahead_s", |section| {
            section.buffer_ahead_s = MIN_BROWSER_BUFFER_AHEAD_S - 1;
        }),
        ("browser_session.buffer_ahead_s", |section| {
            section.buffer_ahead_s = MAX_BROWSER_BUFFER_AHEAD_S + 1;
        }),
        ("browser_session.buffer_behind_s", |section| {
            section.buffer_behind_s = MAX_BROWSER_BUFFER_BEHIND_S + 1;
        }),
    ];
    for (field, mutate) in cases {
        let mut config = AppConfig::default();
        mutate(&mut config.browser_session);

        let error = config.validate().expect_err("value out of range rejected");

        assert!(
            error.to_string().contains(field),
            "error `{error}` does not name `{field}`"
        );
    }
}

/// Значение вне диапазона при запуске идёт общей политикой восстановления: файл
/// переименован в `.bak`, приложение работает на defaults, причина называет поле.
#[test]
fn out_of_range_buffer_at_startup_recovers_through_backup_policy() {
    let broken_text = "schema_version = 28\n\n[browser_session]\nbuffer_ahead_s = 5\n";
    let (temp_dir, config_path) = config_file_with(broken_text);
    let moment = UNIX_EPOCH + Duration::from_secs(1_791_210_612);

    let loaded = load_or_recover_at(&config_path, moment).expect("launch does not fail");

    let ConfigLoadOrigin::RecoveredFromBroken(recovery) = loaded.origin else {
        panic!(
            "expected recovery from broken config, got {:?}",
            loaded.origin
        );
    };
    assert_eq!(
        recovery.problem,
        BrokenConfigProblem::InvalidValue {
            field: "browser_session.buffer_ahead_s"
        }
    );
    assert_eq!(loaded.config, AppConfig::default());
    let backup_text = fs::read_to_string(temp_dir.path().join(recovery.backup_file_name))
        .expect("backup readable");
    assert_eq!(backup_text, broken_text);
}
