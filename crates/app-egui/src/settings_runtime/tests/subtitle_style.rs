//! Запись стиля субтитров («Готово» в режиме правки на видео) одной транзакцией.
//!
//! «Перезапуск» настоящий: config пишется на диск production-хранилищем и читается заново.

use fastiplayer_config::{SubtitleDecorationPreference, load_from_path};
use settings_core::{RuntimeSettingCommitRequest, SettingOptionId, SettingValue};

use super::super::{RuntimeCommitOutcome, SubtitleStyleCommitOutcome};
use super::*;

fn runtime_at(path: &std::path::Path) -> SettingsRuntime {
    SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        AppConfig::default(),
        path.to_path_buf(),
    ))
    .expect("settings runtime should build")
}

fn adapter() -> RecordingRuntimeAdapter {
    RecordingRuntimeAdapter::from_config(&AppConfig::default()).expect("adapter builds")
}

/// Полный набор значений стиля, отличных от умолчаний.
fn non_default_style_requests() -> Vec<RuntimeSettingCommitRequest> {
    vec![
        RuntimeSettingCommitRequest::new(
            "subtitles.position_horizontal_percent",
            SettingValue::Integer(30),
        ),
        RuntimeSettingCommitRequest::new(
            "subtitles.position_vertical_percent",
            SettingValue::Integer(-10),
        ),
        RuntimeSettingCommitRequest::new("subtitles.text_size_percent", SettingValue::Integer(180)),
        RuntimeSettingCommitRequest::new(
            "subtitles.text_color",
            SettingValue::Text("#FFFF00".to_owned()),
        ),
        RuntimeSettingCommitRequest::new(
            "subtitles.background_color",
            SettingValue::Text("#102030".to_owned()),
        ),
        RuntimeSettingCommitRequest::new(
            "subtitles.background_opacity_percent",
            SettingValue::Integer(40),
        ),
        RuntimeSettingCommitRequest::new(
            "subtitles.decoration",
            SettingValue::Select(SettingOptionId::from("outline")),
        ),
        RuntimeSettingCommitRequest::new(
            "subtitles.ass_force_user_style",
            SettingValue::Bool(true),
        ),
    ]
}

#[test]
fn whole_style_is_written_in_one_transaction_and_survives_a_restart() {
    let path = temp_config_path("subtitle-style-commit");
    remove_file_if_exists(&path);
    let mut runtime = runtime_at(&path);
    let mut adapter = adapter();

    let outcome = runtime.commit_subtitle_style(non_default_style_requests(), &mut adapter);

    assert_eq!(outcome, SubtitleStyleCommitOutcome::Saved);
    let reloaded = load_from_path(&path)
        .expect("persisted config reloads")
        .config
        .subtitles;
    assert_eq!(reloaded.position_horizontal_percent, 30);
    assert_eq!(reloaded.position_vertical_percent, -10);
    assert_eq!(reloaded.text_size_percent, 180);
    assert_eq!(reloaded.text_color, "#FFFF00");
    assert_eq!(reloaded.background_color, "#102030");
    assert_eq!(reloaded.background_opacity_percent, 40);
    assert_eq!(reloaded.decoration, SubtitleDecorationPreference::Outline);
    assert!(reloaded.ass_force_user_style);
    // Один коммит - один снимок для AppState (и значит ровно одно применение стиля).
    assert_eq!(adapter.committed_snapshots.len(), 1);
    assert_eq!(
        adapter.committed_snapshots[0].as_config().subtitles,
        reloaded
    );
    remove_file_if_exists(&path);
}

#[test]
fn rejected_style_leaves_config_and_committed_values_untouched() {
    let path = temp_config_path("subtitle-style-rejected");
    remove_file_if_exists(&path);
    let mut runtime = runtime_at(&path);
    let mut adapter = adapter();
    let before = runtime.committed_snapshot().as_config().subtitles.clone();

    // Размер вне диапазона конфига: транзакция не должна применить НИ ОДНО из полей.
    let mut requests = non_default_style_requests();
    requests[2] = RuntimeSettingCommitRequest::new(
        "subtitles.text_size_percent",
        SettingValue::Integer(9_999),
    );
    let outcome = runtime.commit_subtitle_style(requests, &mut adapter);

    assert_eq!(outcome, SubtitleStyleCommitOutcome::Failed);
    assert_eq!(runtime.committed_snapshot().as_config().subtitles, before);
    assert!(!path.exists(), "a rejected style writes nothing");
    assert!(adapter.committed_snapshots.is_empty());
    remove_file_if_exists(&path);
}

#[test]
fn unknown_setting_in_the_batch_fails_without_changing_neighbours() {
    let path = temp_config_path("subtitle-style-unknown");
    remove_file_if_exists(&path);
    let mut runtime = runtime_at(&path);
    let mut adapter = adapter();
    let before = runtime.committed_snapshot().as_config().subtitles.clone();

    let mut requests = non_default_style_requests();
    requests.push(RuntimeSettingCommitRequest::new(
        "subtitles.no_such_field",
        SettingValue::Bool(true),
    ));
    let outcome = runtime.commit_subtitle_style(requests, &mut adapter);

    assert_eq!(outcome, SubtitleStyleCommitOutcome::Failed);
    assert_eq!(runtime.committed_snapshot().as_config().subtitles, before);
    assert!(!path.exists());
    remove_file_if_exists(&path);
}

#[test]
fn style_equal_to_the_config_is_a_saved_noop_without_a_write() {
    let path = temp_config_path("subtitle-style-noop");
    remove_file_if_exists(&path);
    let mut runtime = runtime_at(&path);
    let mut adapter = adapter();

    let defaults = vec![RuntimeSettingCommitRequest::new(
        "subtitles.text_size_percent",
        SettingValue::Integer(i64::from(AppConfig::default().subtitles.text_size_percent)),
    )];
    assert_eq!(
        runtime.commit_subtitle_style(defaults, &mut adapter),
        SubtitleStyleCommitOutcome::Saved
    );
    assert!(!path.exists(), "unchanged values must not touch the file");
    assert!(adapter.committed_snapshots.is_empty());

    // Пустой запрос - явный no-op уровня controller-а.
    let outcome = runtime
        .commit_runtime_settings_with_runtime_adapter(Vec::new(), &mut adapter)
        .expect("empty batch is not an error");
    assert_eq!(outcome, RuntimeCommitOutcome::NothingRequested);
}
