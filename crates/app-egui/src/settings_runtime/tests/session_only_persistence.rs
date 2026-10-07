//! Режим «без записи» (UX edge cases, сессия 05): config от новой версии плеера.
//!
//! Сквозной путь: настоящий файл на диске → `load_or_recover_at` → `SettingsRuntime` →
//! Apply из UI. Проверяем то, что видит пользователь и что лежит на диске.

use std::time::SystemTime;

use fastiplayer_config::load_or_recover_at;

use super::*;

/// Каталог теста с config-ом новой версии схемы.
fn newer_schema_config_on_disk(test_name: &str) -> (PathBuf, PathBuf, String) {
    let directory = std::env::temp_dir().join(format!(
        "fastiplayer-settings-session-only-{test_name}-{}",
        std::process::id()
    ));
    match fs::remove_dir_all(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => panic!("test directory must be removable: {error}"),
    }
    fs::create_dir_all(&directory).expect("test directory создаётся");
    let config_path = directory.join("config.toml");
    let newer_text = AppConfig::default()
        .to_pretty_toml()
        .expect("defaults сериализуются")
        .replace("schema_version = 12", "schema_version = 99");
    fs::write(&config_path, &newer_text).expect("config новой версии записан");
    (directory, config_path, newer_text)
}

/// Apply применяет настройку, сообщает «не сохранится» и не трогает файл новой версии.
#[test]
fn apply_with_newer_config_keeps_change_and_reports_session_only() {
    let (directory, config_path, newer_text) = newer_schema_config_on_disk("apply");
    let loaded =
        load_or_recover_at(&config_path, SystemTime::now()).expect("старт не должен падать");
    let mut runtime =
        SettingsRuntime::from_loaded_config(loaded).expect("settings runtime должен построиться");
    let mut adapter = RecordingRuntimeAdapter::from_config(&AppConfig::default())
        .expect("adapter должен стартовать");

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            brightness_action(0.45),
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    let report = runtime
        .latest_apply_report()
        .expect("apply должен оставить report");
    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    // Значение действует до выхода (не откатилось из-за отказа записи).
    assert_eq!(
        runtime
            .controller
            .committed()
            .render
            .color_adjustment
            .brightness,
        0.45
    );
    assert_eq!(adapter.finalize_calls, 1);
    // Пользователь получил toast и честную строку статуса в окне настроек.
    assert_eq!(adapter.session_only_reports, 1);
    assert_eq!(
        runtime.status.summary.as_deref(),
        Some(crate::config_startup_notice::SETTINGS_KEPT_FOR_SESSION_ONLY_MESSAGE)
    );
    // Файл новой версии не изменился ни на байт.
    assert_eq!(
        fs::read_to_string(&config_path).expect("config на месте"),
        newer_text
    );
    fs::remove_dir_all(&directory).expect("test directory удаляется");
}

/// Обычный config: hook «не сохранится» не вызывается, статус прежний.
#[test]
fn apply_with_writable_config_does_not_report_session_only() {
    let path = temp_config_path("session-only-writable");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        AppConfig::default(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut adapter = RecordingRuntimeAdapter::from_config(&AppConfig::default())
        .expect("adapter должен стартовать");

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            brightness_action(0.45),
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    assert_eq!(adapter.session_only_reports, 0);
    assert_eq!(
        runtime.status.summary.as_deref(),
        Some("Настройки сохранены и применены")
    );
    assert!(path.exists(), "обычный Apply пишет TOML");
    remove_file_if_exists(&path);
}
