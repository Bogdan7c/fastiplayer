//! Live preview: сброс, coalescing, pacing, валидация и отмена.

use super::*;

#[test]
fn preview_does_not_persist_toml_on_field_change() {
    let path = temp_config_path("preview-does-not-persist");
    remove_file_if_exists(&path);
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::Open, brightness_action(0.25)],
            &mut render_adapter,
        )
        .expect("draft edit не должен писать TOML");
    runtime
        .apply_due_preview(&mut render_adapter, Instant::now())
        .expect("preview должен примениться без persist");

    assert_eq!(render_adapter.preview_updates.len(), 1);
    assert!(
        !path.exists(),
        "slider/input movement и preview не должны создавать TOML"
    );
    remove_file_if_exists(&path);
}

#[test]
fn reset_surface_resets_settings_from_all_sections_because_surface_is_global() {
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");
    let default_volume = AppConfig::default().audio.volume;

    runtime
        .handle_ui_actions(
            vec![
                SettingsUiAction::Open,
                SettingsUiAction::SetValue {
                    setting_id: SettingId::from("audio.volume"),
                    value: SettingValue::Float(default_volume / 2.0 + 0.01),
                },
                brightness_action(0.40),
                SettingsUiAction::ResetSurface {
                    surface: SettingsSurfaceId::from("main-settings-window"),
                },
            ],
            &mut render_adapter,
        )
        .expect("actions должны пройти");

    let audio_value = runtime
        .registry()
        .get_value(runtime.controller.draft(), &SettingId::from("audio.volume"))
        .expect("audio.volume должен читаться");
    let brightness_value = runtime
        .registry()
        .get_value(
            runtime.controller.draft(),
            &SettingId::from("render.color_adjustment.brightness"),
        )
        .expect("brightness должен читаться");
    assert_eq!(audio_value, SettingValue::Float(default_volume));
    assert_eq!(brightness_value, SettingValue::Float(0.0));
}

#[test]
fn reset_group_profile_hits_both_visual_profile_groups() {
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions(
            vec![
                SettingsUiAction::Open,
                SettingsUiAction::SetValue {
                    setting_id: SettingId::from("render.profile"),
                    value: SettingValue::Select(SettingOptionId::from("opengles")),
                },
                SettingsUiAction::SetValue {
                    setting_id: SettingId::from("render.tone_mapping"),
                    value: SettingValue::Select(SettingOptionId::from("auto")),
                },
                SettingsUiAction::ResetGroup {
                    section: settings_core::SettingSectionId::from("render"),
                    group: settings_core::SettingGroupId::from("profile"),
                },
            ],
            &mut render_adapter,
        )
        .expect("actions должны пройти");

    let profile_value = runtime
        .registry()
        .get_value(
            runtime.controller.draft(),
            &SettingId::from("render.profile"),
        )
        .expect("render.profile должен читаться");
    let tone_mapping_value = runtime
        .registry()
        .get_value(
            runtime.controller.draft(),
            &SettingId::from("render.tone_mapping"),
        )
        .expect("render.tone_mapping должен читаться");
    assert_eq!(
        profile_value,
        SettingValue::Select(SettingOptionId::from("vulkan"))
    );
    assert_eq!(
        tone_mapping_value,
        SettingValue::Select(SettingOptionId::from("disabled"))
    );
}

#[test]
fn reset_live_field_previews_default_value() {
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");
    let first_tick = Instant::now();

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::Open, brightness_action(0.40)],
            &mut render_adapter,
        )
        .expect("draft change должен пройти");
    runtime
        .apply_due_preview(&mut render_adapter, first_tick)
        .expect("первый preview должен примениться");
    assert_eq!(
        render_adapter
            .preview_updates
            .last()
            .map(|update| { update.settings.color_pipeline.adjustment.brightness }),
        Some(0.40)
    );

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::ResetField {
                setting_id: SettingId::from("render.color_adjustment.brightness"),
            }],
            &mut render_adapter,
        )
        .expect("reset field должен пройти");
    runtime
        .apply_due_preview(&mut render_adapter, first_tick + Duration::from_secs(1))
        .expect("preview после reset должен примениться");

    let default_brightness = AppConfig::default().render.color_adjustment.brightness;
    assert_eq!(
        render_adapter
            .preview_updates
            .last()
            .map(|update| { update.settings.color_pipeline.adjustment.brightness }),
        Some(default_brightness),
        "reset live поля должен отправить default в renderer preview"
    );
}

#[test]
fn multiple_color_changes_coalesce_to_last_preview_value() {
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions(
            vec![
                SettingsUiAction::Open,
                brightness_action(0.10),
                brightness_action(0.40),
            ],
            &mut render_adapter,
        )
        .expect("draft changes должны coalesce pending preview");
    runtime
        .apply_due_preview(&mut render_adapter, Instant::now())
        .expect("preview должен примениться");

    assert_eq!(render_adapter.preview_updates.len(), 1);
    assert_eq!(
        render_adapter.preview_updates[0]
            .settings
            .color_pipeline
            .adjustment
            .brightness,
        0.40
    );
}

#[test]
fn preview_pacing_uses_committed_live_preview_max_hz() {
    let mut config = AppConfig::default();
    config.ui.settings.live_preview_max_hz = 2;
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");
    let first_tick = Instant::now();

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::Open, brightness_action(0.10)],
            &mut render_adapter,
        )
        .expect("первое draft change должно пройти");
    runtime
        .apply_due_preview(&mut render_adapter, first_tick)
        .expect("первый preview должен примениться сразу");
    runtime
        .handle_ui_actions(vec![brightness_action(0.20)], &mut render_adapter)
        .expect("второе draft change должно пройти");

    let early_tick = runtime
        .apply_due_preview(&mut render_adapter, first_tick + Duration::from_millis(100))
        .expect("ранний preview tick должен только запланировать retry");

    assert_eq!(
        render_adapter.preview_updates.len(),
        1,
        "runtime не должен отправлять preview чаще committed max_hz"
    );
    assert_eq!(early_tick.repaint_after, Some(Duration::from_millis(400)));

    runtime
        .apply_due_preview(&mut render_adapter, first_tick + Duration::from_millis(500))
        .expect("preview должен примениться после config-paced интервала");
    assert_eq!(render_adapter.preview_updates.len(), 2);
    assert_eq!(
        render_adapter.preview_updates[1]
            .settings
            .color_pipeline
            .adjustment
            .brightness,
        0.20
    );
}

#[test]
fn field_validation_error_does_not_mutate_draft_or_queue_preview() {
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::Open, brightness_action(99.0)],
            &mut render_adapter,
        )
        .expect("field validation error должен стать UI state, а не hard error");

    let brightness_field = runtime
        .ui_model()
        .fields
        .iter()
        .find(|field| field.descriptor.id == SettingId::from("render.color_adjustment.brightness"))
        .cloned()
        .expect("brightness field должен быть в visual model");
    assert!(brightness_field.validation_error.is_some());
    assert!(
        runtime
            .controller
            .diff()
            .expect("draft и committed должны сравниваться")
            .is_empty(),
        "invalid field value не должен мутировать draft"
    );
    assert!(
        runtime.controller.preview().pending_routes().is_empty(),
        "invalid field value не должен queue-ить preview"
    );

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Apply], &mut render_adapter)
        .expect("Apply с field error должен стать UI status, а не hard error");
    assert!(
        runtime.latest_apply_report().is_none(),
        "field validation error должен блокировать persist/apply pipeline"
    );
}

#[test]
fn backpressure_keeps_latest_pending_preview_update() {
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
    let mut render_adapter = RecordingRenderAdapter::backpressured_once_from_config(&config)
        .expect("adapter должен стартовать");
    let first_tick = Instant::now();

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::Open, brightness_action(0.10)],
            &mut render_adapter,
        )
        .expect("draft edit должен пройти");
    let backpressure_tick = runtime
        .apply_due_preview(&mut render_adapter, first_tick)
        .expect("backpressure должен быть retryable preview state");

    assert_eq!(render_adapter.preview_updates.len(), 0);
    assert!(
        runtime
            .controller
            .preview()
            .pending_routes()
            .contains(&SettingRouteId::from("render")),
        "backpressure должен оставить latest preview pending"
    );
    assert!(backpressure_tick.repaint_after.is_some());

    runtime
        .handle_ui_actions(vec![brightness_action(0.50)], &mut render_adapter)
        .expect("новое draft значение должно заменить pending preview");
    runtime
        .apply_due_preview(&mut render_adapter, first_tick + Duration::from_secs(1))
        .expect("retry должен отправить latest pending preview");

    assert_eq!(render_adapter.preview_updates.len(), 1);
    assert_eq!(
        render_adapter.preview_updates[0]
            .settings
            .color_pipeline
            .adjustment
            .brightness,
        0.50
    );
}

#[test]
fn cancel_rolls_back_lazy_preview_baseline_and_discards_draft() {
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::Open, brightness_action(0.35)],
            &mut render_adapter,
        )
        .expect("draft edit должен пройти");
    runtime
        .apply_due_preview(&mut render_adapter, Instant::now())
        .expect("preview должен примениться");
    runtime
        .handle_ui_actions(vec![SettingsUiAction::Cancel], &mut render_adapter)
        .expect("cancel должен откатить preview");

    assert_eq!(render_adapter.rollbacks.len(), 1);
    assert_eq!(
        render_adapter.active.color_pipeline.adjustment.brightness,
        config.render.color_adjustment.brightness
    );
    assert!(
        runtime
            .controller
            .diff()
            .expect("draft и committed должны сравниваться")
            .is_empty(),
        "cancel должен отбросить draft changes"
    );
    assert!(!runtime.is_settings_window_open());
}
