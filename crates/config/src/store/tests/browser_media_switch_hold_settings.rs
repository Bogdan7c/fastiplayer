//! Удержание кадра на смене медиа `[browser_session] media_switch_frame_hold_ms` (схема v33,
//! решение 380): миграция v32, свои значения, «0 — выключено» и границы.

use std::time::Duration;

use super::*;
use crate::{
    BrowserMediaSwitchFrameHold, DEFAULT_BROWSER_MEDIA_SWITCH_FRAME_HOLD_MS,
    MAX_BROWSER_MEDIA_SWITCH_FRAME_HOLD_MS,
};

/// Документ v32: свои значения браузерного режима, ключа удержания ещё нет.
const SCHEMA_V32_WITHOUT_FRAME_HOLD: &str = r#"
schema_version = 32

[audio]
volume = 0.4

[browser_session]
buffer_ahead_s = 45
variant_return_window_s = 300
"#;

/// Временный каталог с `config.toml` заданного содержимого.
fn config_file_with(config_text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, config_text).expect("config fixture written");
    (temp_dir, config_path)
}

/// Миграция v32 -> v33: удержание получает 3 с, значения пользователя целы; после сохранения
/// ключ записан и повторная загрузка ничего не меняет.
#[test]
fn schema_v32_gets_the_default_frame_hold_and_keeps_user_values() {
    let (_temp_dir, config_path) = config_file_with(SCHEMA_V32_WITHOUT_FRAME_HOLD);

    let migrated = load_from_path(&config_path).expect("v32 config loads");

    assert_eq!(migrated.config.schema_version, CURRENT_SCHEMA_VERSION);
    let section = migrated.config.browser_session;
    assert_eq!(
        section.media_switch_frame_hold_ms,
        DEFAULT_BROWSER_MEDIA_SWITCH_FRAME_HOLD_MS
    );
    assert_eq!(
        section.media_switch_frame_hold(),
        BrowserMediaSwitchFrameHold::UpTo(Duration::from_secs(3))
    );
    assert_eq!(section.buffer_ahead_s, 45);
    assert_eq!(section.variant_return_window_s, 300);
    assert!((migrated.config.audio.volume - 0.4).abs() < f64::EPSILON);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved text");
    assert!(saved_text.contains("media_switch_frame_hold_ms = 3000"));
    assert!(saved_text.contains("schema_version = 33"));
    let reloaded = load_from_path(&config_path).expect("saved config reloads");
    assert_eq!(reloaded.config.browser_session, section);
}

/// 0 — удержание выключено (пустое поле между роликами, как до 16b3c); своё число — как есть.
#[test]
fn zero_disables_the_frame_hold_and_other_values_are_kept() {
    let (_temp_dir, config_path) = config_file_with(
        "schema_version = 33\n\n[browser_session]\nmedia_switch_frame_hold_ms = 0\n",
    );
    let disabled = load_from_path(&config_path).expect("current config loads");
    assert_eq!(
        disabled.config.browser_session.media_switch_frame_hold(),
        BrowserMediaSwitchFrameHold::Disabled
    );

    let (_temp_dir, config_path) = config_file_with(
        "schema_version = 33\n\n[browser_session]\nmedia_switch_frame_hold_ms = 1500\n",
    );
    let custom = load_from_path(&config_path).expect("current config loads");
    assert_eq!(
        custom.config.browser_session.media_switch_frame_hold(),
        BrowserMediaSwitchFrameHold::UpTo(Duration::from_millis(1500))
    );
}

/// Больше 10 с — отказ с путём поля; ровно 10 с принимается.
#[test]
fn frame_hold_above_the_maximum_is_rejected_with_the_field_path() {
    let mut config = AppConfig::default();
    config.browser_session.media_switch_frame_hold_ms = MAX_BROWSER_MEDIA_SWITCH_FRAME_HOLD_MS;
    config.validate().expect("the maximum is accepted");

    config.browser_session.media_switch_frame_hold_ms = MAX_BROWSER_MEDIA_SWITCH_FRAME_HOLD_MS + 1;
    let error = config
        .validate()
        .expect_err("above the maximum is rejected");
    assert!(
        error
            .to_string()
            .contains("browser_session.media_switch_frame_hold_ms"),
        "{error}"
    );
}
