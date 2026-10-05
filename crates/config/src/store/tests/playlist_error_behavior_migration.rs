//! Миграция v10 → v11: `playlist.error_behavior = "stop"` становится `"skip"`.
//!
//! Решение владельца (UX edge cases, сессия 07): битый файл в очереди по умолчанию
//! пропускается. Default-документ v10 записывал `stop` явно, поэтому миграция меняет
//! значение у всех; остальные поля и повторная загрузка не должны меняться.

use super::*;
use crate::{PlaylistErrorBehavior, PlaylistPlaybackBehavior};

/// v10-документ с явным `stop` и нестандартными соседними полями плейлиста.
const SCHEMA_V10_WITH_STOP: &str = r#"
schema_version = 10

[playlist]
load_siblings = false
playback_behavior = "repeat_queue"
error_behavior = "stop"
state_save_debounce_ms = 3000
previous_restart_threshold_ms = 7000
"#;

/// Полный file boundary: загрузка мигрирует `stop`, сохранение пишет v11 `skip`,
/// повторная загрузка ничего больше не меняет, соседние поля сохраняются.
#[test]
fn schema_v10_stop_migrates_to_skip_and_roundtrips_idempotently() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V10_WITH_STOP).expect("schema v10 fixture written");

    let migrated = load_from_path(&config_path).expect("schema v10 config migrates");
    assert_eq!(migrated.config.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(migrated.config.schema_version, 11);
    assert_eq!(
        migrated.config.playlist.error_behavior,
        PlaylistErrorBehavior::Skip
    );
    // Соседние значения пользователя остаются как были.
    assert!(!migrated.config.playlist.load_siblings);
    assert_eq!(
        migrated.config.playlist.playback_behavior,
        PlaylistPlaybackBehavior::RepeatQueue
    );
    assert_eq!(migrated.config.playlist.state_save_debounce_ms, 3_000);
    assert_eq!(
        migrated.config.playlist.previous_restart_threshold_ms,
        7_000
    );

    save_validated_atomic_at(&config_path, &migrated.config)
        .expect("migrated v11 config saves atomically");
    let saved_text = fs::read_to_string(&config_path).expect("saved v11 TOML readable");
    assert!(saved_text.contains("schema_version = 11"));
    assert!(saved_text.contains("error_behavior = \"skip\""));
    assert!(!saved_text.contains("error_behavior = \"stop\""));

    // Повторная загрузка уже сохранённого v11 идемпотентна.
    let reloaded = load_from_path(&config_path).expect("saved v11 config reloads");
    assert_eq!(reloaded.config, migrated.config);
}

/// Сознательный выбор `stop`, сохранённый уже в v11, миграция больше не трогает.
#[test]
fn schema_v11_stop_is_kept_as_explicit_user_choice() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let current_text = "schema_version = 11\n\n[playlist]\nerror_behavior = \"stop\"\n";
    fs::write(&config_path, current_text).expect("schema v11 fixture written");

    let loaded = load_from_path(&config_path).expect("schema v11 config loads");

    assert_eq!(
        loaded.config.playlist.error_behavior,
        PlaylistErrorBehavior::Stop
    );
    assert_eq!(
        fs::read_to_string(&config_path).expect("current file remains readable"),
        current_text
    );
}

/// v10 с уже выбранным `skip` или без секции плейлиста получает `skip`.
#[test]
fn schema_v10_without_stop_value_loads_skip() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    for (file_name, legacy_text) in [
        (
            "explicit-skip.toml",
            "schema_version = 10\n\n[playlist]\nerror_behavior = \"skip\"\n",
        ),
        ("no-playlist.toml", "schema_version = 10\n"),
    ] {
        let config_path = temp_dir.path().join(file_name);
        fs::write(&config_path, legacy_text).expect("schema v10 fixture written");

        let loaded = load_from_path(&config_path).expect("schema v10 config loads");

        assert_eq!(
            loaded.config.playlist.error_behavior,
            PlaylistErrorBehavior::Skip,
            "{file_name}"
        );
    }
}

/// Неизвестное значение миграция не «чинит»: strict parser отклоняет его как раньше.
#[test]
fn schema_v10_unknown_error_behavior_is_still_rejected() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        "schema_version = 10\n\n[playlist]\nerror_behavior = \"pause\"\n",
    )
    .expect("schema v10 fixture written");

    assert!(load_from_path(&config_path).is_err());
}
