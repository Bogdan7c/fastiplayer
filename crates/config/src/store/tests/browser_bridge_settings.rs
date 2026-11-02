//! Таблица `[browser_bridge]` (схема v31): миграция v30 с defaults, круговой обход значения
//! `false`, defaults в созданном документе и отказ на незнакомом ключе.

use super::*;
use crate::BrowserBridgeConfig;

/// Документ v30: свои значения в других секциях и нет таблицы `[browser_bridge]`.
const SCHEMA_V30_WITHOUT_BROWSER_BRIDGE: &str = r#"
schema_version = 30

[audio]
volume = 0.4

[browser_session]
buffer_ahead_s = 45

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

/// v30 без таблицы: предложение включено, версия поднята до 31, значения пользователя целы,
/// после сохранения таблица записана и повторная загрузка ничего не меняет.
#[test]
fn schema_v30_without_browser_bridge_loads_offer_enabled_and_keeps_user_values() {
    let (_temp_dir, config_path) = config_file_with(SCHEMA_V30_WITHOUT_BROWSER_BRIDGE);

    let migrated = load_from_path(&config_path).expect("v30 config loads");

    assert_eq!(migrated.config.schema_version, 31);
    assert!(migrated.config.browser_bridge.offer_on_startup);
    assert_eq!(
        migrated.config.browser_bridge,
        BrowserBridgeConfig::default()
    );
    // Миграция не переписывает значения пользователя.
    assert!((migrated.config.audio.volume - 0.4).abs() < f64::EPSILON);
    assert_eq!(migrated.config.browser_session.buffer_ahead_s, 45);
    assert_eq!(migrated.config.subtitles.text_size_percent, 130);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("schema_version = 31"));
    assert!(saved_text.contains("[browser_bridge]"));
    assert!(saved_text.contains("offer_on_startup = true"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// v31 с `offer_on_startup = false`: значение читается, переживает сохранение и перезагрузку.
#[test]
fn offer_on_startup_false_round_trips_through_save_and_load() {
    let (_temp_dir, config_path) =
        config_file_with("schema_version = 31\n\n[browser_bridge]\noffer_on_startup = false\n");

    let loaded = load_from_path(&config_path).expect("config with false loads");
    assert!(!loaded.config.browser_bridge.offer_on_startup);

    save_validated_atomic_at(&config_path, &loaded.config).expect("config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved TOML readable");
    assert!(saved_text.contains("offer_on_startup = false"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert!(!reloaded.config.browser_bridge.offer_on_startup);
    assert_eq!(reloaded.config, loaded.config);
}

/// Неизвестный ключ в таблице — ошибка разбора (как во всех строгих секциях).
#[test]
fn unknown_browser_bridge_key_is_a_parse_error() {
    let (_temp_dir, config_path) =
        config_file_with("schema_version = 31\n\n[browser_bridge]\noffer_forever = true\n");

    assert!(load_from_path(&config_path).is_err());
}
