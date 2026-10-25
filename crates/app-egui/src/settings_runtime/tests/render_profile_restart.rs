//! Смена профиля рендера: «Applies after restart», кнопка «Restart now» и запрос перезапуска
//! — через настоящий Apply в production-пути (запись TOML, отчёт, модель UI).

use fastiplayer_config::{RenderProfile, load_from_path};

use super::*;
use crate::app_instance::RelaunchRequest;
use crate::settings_runtime::render_profile_restart::APPLIES_AFTER_RESTART_STATUS;
use crate::settings_ui::RestartOffer;

const PROFILE_FIELD_ID: &str = "render.profile";

fn set_profile_action(option_id: &str) -> SettingsUiAction {
    SettingsUiAction::SetValue {
        setting_id: SettingId::from(PROFILE_FIELD_ID),
        value: SettingValue::Select(SettingOptionId::from(option_id)),
    }
}

fn profile_field(runtime: &mut SettingsRuntime) -> crate::settings_ui::SettingsUiField {
    runtime
        .ui_model()
        .fields
        .iter()
        .find(|field| field.descriptor.id == SettingId::from(PROFILE_FIELD_ID))
        .cloned()
        .expect("render.profile should be in the visual model.")
}

fn runtime_at(test_name: &str) -> (SettingsRuntime, PathBuf, RecordingRuntimeAdapter) {
    let path = temp_config_path(test_name);
    remove_file_if_exists(&path);
    let config = AppConfig::default();
    let runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime should be built");
    let adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter starts");
    (runtime, path, adapter)
}

/// Полный путь: правка -> статус без кнопки -> Apply -> запись на диск, живой рендер не
/// тронут, статус с кнопкой -> `RestartNow` -> запрос перезапуска.
#[test]
fn profile_change_applies_after_restart_and_offers_restart_now() {
    let (mut runtime, path, mut adapter) = runtime_at("profile-restart-flow");
    assert_eq!(
        runtime.committed_config().render.profile,
        RenderProfile::Auto
    );
    run_runtime_actions(&mut runtime, vec![SettingsUiAction::Open], &mut adapter);
    assert_eq!(profile_field(&mut runtime).status, None);

    run_runtime_actions(
        &mut runtime,
        vec![set_profile_action("vulkan")],
        &mut adapter,
    );
    let edited = profile_field(&mut runtime);
    assert_eq!(edited.status.as_deref(), Some(APPLIES_AFTER_RESTART_STATUS));
    assert_eq!(
        edited.restart_offer,
        RestartOffer::None,
        "unapplied draft: no button yet"
    );

    run_runtime_actions(&mut runtime, vec![SettingsUiAction::Apply], &mut adapter);

    let saved = load_from_path(&path).expect("applied config is on disk");
    assert_eq!(saved.config.render.profile, RenderProfile::Vulkan);
    assert!(
        adapter.renderer_recreation_updates.is_empty(),
        "a profile change must not recreate the live renderer"
    );
    let status_lines = runtime.ui_model().status.details.join("\n");
    assert!(
        status_lines.contains("saved; applies after restart"),
        "status was: {status_lines}"
    );
    let applied = profile_field(&mut runtime);
    assert_eq!(
        applied.status.as_deref(),
        Some(APPLIES_AFTER_RESTART_STATUS)
    );
    assert_eq!(applied.restart_offer, RestartOffer::Available);
    assert_eq!(runtime.relaunch_request(), RelaunchRequest::NotRequested);

    run_runtime_actions(
        &mut runtime,
        vec![SettingsUiAction::RestartNow],
        &mut adapter,
    );

    assert_eq!(runtime.relaunch_request(), RelaunchRequest::AfterExit);
    remove_file_if_exists(&path);
}

/// Возврат к профилю запуска убирает статус и кнопку, `RestartNow` без причины игнорируется.
#[test]
fn restart_now_without_pending_profile_change_is_ignored() {
    let (mut runtime, path, mut adapter) = runtime_at("profile-restart-ignored");

    run_runtime_actions(
        &mut runtime,
        vec![SettingsUiAction::Open, SettingsUiAction::RestartNow],
        &mut adapter,
    );
    assert_eq!(runtime.relaunch_request(), RelaunchRequest::NotRequested);

    run_runtime_actions(
        &mut runtime,
        vec![set_profile_action("vulkan")],
        &mut adapter,
    );
    run_runtime_actions(&mut runtime, vec![set_profile_action("auto")], &mut adapter);
    let reverted = profile_field(&mut runtime);
    assert_eq!(
        reverted.status, None,
        "draft equals the startup profile again"
    );
    assert_eq!(reverted.restart_offer, RestartOffer::None);
    remove_file_if_exists(&path);
}

/// Неприменённые правки других полей не теряются молча: перезапуск не принимается.
#[test]
fn restart_now_is_refused_while_other_edits_are_unapplied() {
    let (mut runtime, path, mut adapter) = runtime_at("profile-restart-dirty");
    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            set_profile_action("vulkan"),
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );
    run_runtime_actions(&mut runtime, vec![brightness_action(0.4)], &mut adapter);
    assert_eq!(
        profile_field(&mut runtime).restart_offer,
        RestartOffer::None
    );

    run_runtime_actions(
        &mut runtime,
        vec![SettingsUiAction::RestartNow],
        &mut adapter,
    );

    assert_eq!(runtime.relaunch_request(), RelaunchRequest::NotRequested);
    remove_file_if_exists(&path);
}

/// Профиль запуска запоминается один раз: смена и Apply его не двигают, поэтому статус
/// остаётся, пока процесс не перезапущен.
#[test]
fn startup_profile_is_not_moved_by_apply() {
    let (mut runtime, path, mut adapter) = runtime_at("profile-restart-startup-fixed");
    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            set_profile_action("opengles"),
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );
    run_runtime_actions(
        &mut runtime,
        vec![brightness_action(0.3), SettingsUiAction::Apply],
        &mut adapter,
    );

    let field = profile_field(&mut runtime);
    assert_eq!(field.status.as_deref(), Some(APPLIES_AFTER_RESTART_STATUS));
    assert_eq!(field.restart_offer, RestartOffer::Available);
    remove_file_if_exists(&path);
}
