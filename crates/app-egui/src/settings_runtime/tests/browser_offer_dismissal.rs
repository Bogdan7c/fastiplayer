//! Ответ на предложение «подключить браузеры»: `offer_on_startup = false` одиночным коммитом.
//!
//! «Перезапуск» настоящий: config пишется через production store и читается заново.

use fastiplayer_config::load_from_path;

use super::super::browser_offer_dismissal::BrowserOfferDismissalOutcome;
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

/// Ответ пишет `false` на диск; соседнее незавершённое значение draft-а не попадает в файл
/// и остаётся в draft-е окна настроек; второй ответ ничего не пишет.
#[test]
fn dismissal_persists_false_without_touching_draft_values() {
    let path = temp_config_path("browser-offer-dismissal");
    remove_file_if_exists(&path);
    let mut runtime = runtime_at(&path);
    let mut adapter = adapter();
    runtime.open_settings().expect("settings open");
    // Незавершённая правка пользователя в окне настроек (ещё не применена).
    let draft_edit_id = "browser_session.buffer_ahead_s";
    let default_buffer = AppConfig::default().browser_session.buffer_ahead_s;
    run_runtime_actions(
        &mut runtime,
        vec![SettingsUiAction::SetValue {
            setting_id: SettingId::from(draft_edit_id),
            value: SettingValue::Integer(i64::from(default_buffer) + 30),
        }],
        &mut adapter,
    );

    let outcome = runtime
        .dismiss_browser_connection_offer(&mut adapter)
        .expect("dismissal commits");

    assert_eq!(outcome, BrowserOfferDismissalOutcome::Recorded);
    assert!(!runtime.committed_config().browser_bridge.offer_on_startup);
    let reloaded = load_from_path(&path).expect("persisted config reloads");
    assert!(
        !reloaded.config.browser_bridge.offer_on_startup,
        "the restarted app must not show the offer again"
    );
    assert_eq!(
        reloaded.config.browser_session.buffer_ahead_s, default_buffer,
        "the unapplied draft value must not reach the file"
    );
    let dirty_ids: Vec<_> = runtime
        .ui_model()
        .fields
        .iter()
        .filter(|field| field.is_dirty)
        .map(|field| field.descriptor.id.as_str().to_owned())
        .collect();
    assert_eq!(
        dirty_ids,
        vec![draft_edit_id.to_owned()],
        "the user's draft edit survives; the offer key itself is not dirty"
    );

    assert_eq!(
        runtime
            .dismiss_browser_connection_offer(&mut adapter)
            .expect("repeat is a no-op"),
        BrowserOfferDismissalOutcome::AlreadyDismissed
    );
    remove_file_if_exists(&path);
}
