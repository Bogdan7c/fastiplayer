//! Окно буфера браузерного режима `[browser_session]` (схема v28, сейчас v29): defaults, миграция v27,
//! частичная таблица, границы, диапазоны и политика восстановления при значении вне диапазона.

use std::time::{Duration, UNIX_EPOCH};

use super::*;
use crate::{
    BrokenConfigProblem, BrowserSessionConfig, BrowserWindowConfig, ConfigLoadOrigin,
    DEFAULT_BROWSER_BUFFER_AHEAD_S, DEFAULT_BROWSER_MAX_HELD_MB, MAX_BROWSER_BUFFER_AHEAD_S,
    MAX_BROWSER_MAX_HELD_MB, MAX_BROWSER_WINDOW_HEIGHT_POINTS, MAX_BROWSER_WINDOW_WIDTH_POINTS,
    MIN_BROWSER_BUFFER_AHEAD_S, MIN_BROWSER_MAX_HELD_MB, MIN_BROWSER_WINDOW_HEIGHT_POINTS,
    MIN_BROWSER_WINDOW_WIDTH_POINTS, load_or_recover_at,
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
fn default_buffer_window_is_120_seconds_ahead_and_320_mib_ceiling() {
    let window = AppConfig::default().browser_session.buffer_window();

    assert_eq!(window.ahead(), Duration::from_secs(120));
    assert_eq!(window.max_held_bytes(), 320 * 1024 * 1024);
    assert_eq!(DEFAULT_BROWSER_BUFFER_AHEAD_S, 120);
    assert_eq!(DEFAULT_BROWSER_MAX_HELD_MB, 320);
}

/// Файл v27 поднимается до текущей схемы без переписывания значений: новая таблица получает defaults,
/// значения пользователя целы, после сохранения таблица записана и повторная загрузка
/// ничего не меняет.
#[test]
fn schema_v27_without_browser_session_loads_defaults_and_keeps_user_values() {
    let (_temp_dir, config_path) = config_file_with(SCHEMA_V27_WITHOUT_BROWSER_SESSION);

    let migrated = load_from_path(&config_path).expect("v27 config loads");

    assert_eq!(migrated.config.schema_version, 30);
    assert_eq!(
        migrated.config.browser_session,
        BrowserSessionConfig::default()
    );
    // Миграция не переписывает значения пользователя.
    assert!((migrated.config.audio.volume - 0.4).abs() < f64::EPSILON);
    assert_eq!(migrated.config.subtitles.text_size_percent, 130);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("schema_version = 30"));
    assert!(saved_text.contains("[browser_session]"));
    assert!(saved_text.contains("buffer_ahead_s = 120"));
    assert!(saved_text.contains("max_held_mb = 320"));
    assert!(!saved_text.contains("buffer_behind_s"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// Файл текущей схемы без таблицы (пользователь удалил её руками) читается с defaults.
#[test]
fn current_schema_without_browser_session_table_uses_defaults() {
    let (_temp_dir, config_path) = config_file_with("schema_version = 30\n");

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
        config_file_with("schema_version = 30\n\n[browser_session]\nbuffer_ahead_s = 45\n");

    let loaded = load_from_path(&config_path).expect("partial table loads");
    let window = loaded.config.browser_session.buffer_window();

    assert_eq!(window.ahead(), Duration::from_secs(45));
    assert_eq!(
        window.max_held_bytes(),
        usize::from(DEFAULT_BROWSER_MAX_HELD_MB) * 1024 * 1024
    );
}

/// Неизвестный ключ в таблице — ошибка разбора (как во всех строгих секциях).
#[test]
fn unknown_browser_session_key_is_a_parse_error() {
    let (_temp_dir, config_path) =
        config_file_with("schema_version = 30\n\n[browser_session]\nbuffer_ahead_ms = 1\n");

    assert!(load_from_path(&config_path).is_err());
}

/// Границы диапазонов переживают запись и чтение, окно отражает записанные числа.
#[test]
fn browser_session_boundary_values_roundtrip_through_file() {
    let low = BrowserSessionConfig {
        buffer_ahead_s: MIN_BROWSER_BUFFER_AHEAD_S,
        max_held_mb: MIN_BROWSER_MAX_HELD_MB,
        window: BrowserWindowConfig {
            inner_width_points: MIN_BROWSER_WINDOW_WIDTH_POINTS,
            inner_height_points: MIN_BROWSER_WINDOW_HEIGHT_POINTS,
            fullscreen: false,
        },
    };
    let high = BrowserSessionConfig {
        buffer_ahead_s: MAX_BROWSER_BUFFER_AHEAD_S,
        max_held_mb: MAX_BROWSER_MAX_HELD_MB,
        window: BrowserWindowConfig {
            inner_width_points: MAX_BROWSER_WINDOW_WIDTH_POINTS,
            inner_height_points: MAX_BROWSER_WINDOW_HEIGHT_POINTS,
            fullscreen: true,
        },
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
            window.max_held_bytes(),
            usize::from(browser_session.max_held_mb) * 1024 * 1024
        );
    }
}

/// Значения вне диапазона отклоняются, ошибка называет полный путь поля.
#[test]
fn browser_session_values_out_of_range_are_rejected_with_the_field_path() {
    type Mutation = fn(&mut BrowserSessionConfig);
    let cases: [(&str, Mutation); 8] = [
        ("browser_session.buffer_ahead_s", |section| {
            section.buffer_ahead_s = MIN_BROWSER_BUFFER_AHEAD_S - 1;
        }),
        ("browser_session.buffer_ahead_s", |section| {
            section.buffer_ahead_s = MAX_BROWSER_BUFFER_AHEAD_S + 1;
        }),
        ("browser_session.max_held_mb", |section| {
            section.max_held_mb = MIN_BROWSER_MAX_HELD_MB - 1;
        }),
        ("browser_session.max_held_mb", |section| {
            section.max_held_mb = MAX_BROWSER_MAX_HELD_MB + 1;
        }),
        ("browser_session.window.inner_width_points", |section| {
            section.window.inner_width_points = MIN_BROWSER_WINDOW_WIDTH_POINTS - 1;
        }),
        ("browser_session.window.inner_width_points", |section| {
            section.window.inner_width_points = MAX_BROWSER_WINDOW_WIDTH_POINTS + 1;
        }),
        ("browser_session.window.inner_height_points", |section| {
            section.window.inner_height_points = MIN_BROWSER_WINDOW_HEIGHT_POINTS - 1;
        }),
        ("browser_session.window.inner_height_points", |section| {
            section.window.inner_height_points = MAX_BROWSER_WINDOW_HEIGHT_POINTS + 1;
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
    let broken_text = "schema_version = 30\n\n[browser_session]\nbuffer_ahead_s = 5\n";
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

/// Документ v28 без таблицы окна: v29 даёт окну defaults (1280×720, не во весь экран),
/// значения пользователя целы, после сохранения таблица записана.
#[test]
fn schema_v28_without_window_table_gets_default_window() {
    let (_temp_dir, config_path) = config_file_with(
        "schema_version = 28\n\n[browser_session]\nbuffer_ahead_s = 45\n\n[audio]\nvolume = 0.4\n",
    );

    let migrated = load_from_path(&config_path).expect("v28 config loads");

    assert_eq!(migrated.config.schema_version, 30);
    assert_eq!(migrated.config.browser_session.buffer_ahead_s, 45);
    assert_eq!(
        migrated.config.browser_session.window,
        BrowserWindowConfig {
            inner_width_points: 1280,
            inner_height_points: 720,
            fullscreen: false,
        }
    );
    assert!((migrated.config.audio.volume - 0.4).abs() < f64::EPSILON);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("[browser_session.window]"));
    assert!(saved_text.contains("inner_width_points = 1280"));
    assert!(saved_text.contains("fullscreen = false"));
    assert_eq!(
        load_from_path(&config_path)
            .expect("saved config reloads")
            .config,
        migrated.config
    );
}

/// Неизвестный ключ в таблице окна — битый файл (строгий разбор), а не молчаливый пропуск.
#[test]
fn unknown_key_in_window_table_is_rejected() {
    let (_temp_dir, config_path) =
        config_file_with("schema_version = 30\n\n[browser_session.window]\nmaximized = true\n");

    let error = load_from_path(&config_path).expect_err("unknown window key rejected");

    assert!(error.to_string().contains("maximized"));
}

/// Документ v29 со старым окном «назад»: свои значения буфера и окна у пользователя.
const SCHEMA_V29_WITH_BEHIND: &str = r#"
schema_version = 29

[audio]
volume = 0.4

[browser_session]
buffer_ahead_s = 45
buffer_behind_s = 15

[browser_session.window]
inner_width_points = 900
inner_height_points = 500
fullscreen = true
"#;

/// Миграция v29 -> v30: `buffer_behind_s` исчезает (и в памяти, и в сохранённом файле),
/// `max_held_mb` получает 320, остальное (в том числе чужие секции) не тронуто.
#[test]
fn schema_v29_drops_buffer_behind_adds_max_held_and_keeps_everything_else() {
    let (_temp_dir, config_path) = config_file_with(SCHEMA_V29_WITH_BEHIND);

    let migrated = load_from_path(&config_path).expect("v29 config loads");

    assert_eq!(migrated.config.schema_version, 30);
    let section = migrated.config.browser_session;
    assert_eq!(section.max_held_mb, 320);
    assert_eq!(section.buffer_ahead_s, 45);
    assert_eq!(section.window.inner_width_points, 900);
    assert_eq!(section.window.inner_height_points, 500);
    assert!(section.window.fullscreen);
    assert!((migrated.config.audio.volume - 0.4).abs() < f64::EPSILON);

    // Сохранённый файл уже не содержит старый ключ и снова читается строгим разбором.
    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved text");
    assert!(!saved_text.contains("buffer_behind_s"));
    assert!(saved_text.contains("max_held_mb = 320"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config.browser_session, section);
}

/// Пользовательский `max_held_mb` в файле v30 переживает чтение как есть (не затирается
/// default-ом), а лишний `buffer_behind_s` в текущей схеме отвергается строгим разбором.
#[test]
fn current_schema_keeps_user_max_held_and_rejects_the_removed_key() {
    let (_temp_dir, config_path) =
        config_file_with("schema_version = 30\n\n[browser_session]\nmax_held_mb = 512\n");
    let loaded = load_from_path(&config_path).expect("v30 config loads");
    assert_eq!(loaded.config.browser_session.max_held_mb, 512);
    assert_eq!(
        loaded
            .config
            .browser_session
            .buffer_window()
            .max_held_bytes(),
        512 * 1024 * 1024
    );

    let (_temp_dir, config_path) =
        config_file_with("schema_version = 30\n\n[browser_session]\nbuffer_behind_s = 15\n");
    assert!(load_from_path(&config_path).is_err());
}
