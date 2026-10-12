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
    .expect("settings runtime should be built");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::Open, brightness_action(0.25)],
            &mut render_adapter,
        )
        .expect("Draft Edit Shouldn't Write TOML");
    runtime
        .apply_due_preview(&mut render_adapter, Instant::now())
        .expect("Preview should be applied without persisting.");

    assert_eq!(render_adapter.preview_updates.len(), 1);
    assert!(
        !path.exists(),
        "slider/input movement and preview do not create TOML"
    );
    remove_file_if_exists(&path);
}

#[test]
fn reset_surface_resets_settings_from_all_sections_because_surface_is_global() {
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime should be built");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");
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
        .expect("Actions must pass through");

    let audio_value = runtime
        .registry()
        .get_value(runtime.controller.draft(), &SettingId::from("audio.volume"))
        .expect("audio.volume should read");
    let brightness_value = runtime
        .registry()
        .get_value(
            runtime.controller.draft(),
            &SettingId::from("render.color_adjustment.brightness"),
        )
        .expect("brightness should be read");
    assert_eq!(audio_value, SettingValue::Float(default_volume));
    assert_eq!(brightness_value, SettingValue::Float(0.0));
}

#[test]
fn reset_group_profile_hits_both_visual_profile_groups() {
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime should be built");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");

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
        .expect("Actions must pass through");

    let profile_value = runtime
        .registry()
        .get_value(
            runtime.controller.draft(),
            &SettingId::from("render.profile"),
        )
        .expect("render.profile should read");
    let tone_mapping_value = runtime
        .registry()
        .get_value(
            runtime.controller.draft(),
            &SettingId::from("render.tone_mapping"),
        )
        .expect("render.tone_mapping should read");
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
        .expect("settings runtime should be built");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");
    let first_tick = Instant::now();

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::Open, brightness_action(0.40)],
            &mut render_adapter,
        )
        .expect("The draft change must be passed");
    runtime
        .apply_due_preview(&mut render_adapter, first_tick)
        .expect("The first preview should apply.");
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
        .expect("The reset field must pass through.");
    runtime
        .apply_due_preview(&mut render_adapter, first_tick + Duration::from_secs(1))
        .expect("preview after reset should apply");

    let default_brightness = AppConfig::default().render.color_adjustment.brightness;
    assert_eq!(
        render_adapter
            .preview_updates
            .last()
            .map(|update| { update.settings.color_pipeline.adjustment.brightness }),
        Some(default_brightness),
        "reset live fields must send default to renderer preview"
    );
}

#[test]
fn multiple_color_changes_coalesce_to_last_preview_value() {
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime should be built");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");

    runtime
        .handle_ui_actions(
            vec![
                SettingsUiAction::Open,
                brightness_action(0.10),
                brightness_action(0.40),
            ],
            &mut render_adapter,
        )
        .expect("Draft changes to coalesce pending preview");
    runtime
        .apply_due_preview(&mut render_adapter, Instant::now())
        .expect("The preview should apply");

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
        .expect("settings runtime should be built");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");
    let first_tick = Instant::now();

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::Open, brightness_action(0.10)],
            &mut render_adapter,
        )
        .expect("The first draft change must be completed.");
    runtime
        .apply_due_preview(&mut render_adapter, first_tick)
        .expect("The first preview should apply immediately.");
    runtime
        .handle_ui_actions(vec![brightness_action(0.20)], &mut render_adapter)
        .expect("The second draft change should be completed.");

    let early_tick = runtime
        .apply_due_preview(&mut render_adapter, first_tick + Duration::from_millis(100))
        .expect("Early preview tick should only schedule retry");

    assert_eq!(
        render_adapter.preview_updates.len(),
        1,
        "runtime don't send preview more often committed max_hz"
    );
    assert_eq!(early_tick.repaint_after, Some(Duration::from_millis(400)));

    runtime
        .apply_due_preview(&mut render_adapter, first_tick + Duration::from_millis(500))
        .expect("Preview must be applied after the config-paced interval");
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
        .expect("settings runtime should be built");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::Open, brightness_action(99.0)],
            &mut render_adapter,
        )
        .expect("Field validation error should be a UI state, not a hard error.");

    let brightness_field = runtime
        .ui_model()
        .fields
        .iter()
        .find(|field| field.descriptor.id == SettingId::from("render.color_adjustment.brightness"))
        .cloned()
        .expect("The brightness field should be in the visual model.");
    assert!(brightness_field.validation_error.is_some());
    assert!(
        runtime
            .controller
            .diff()
            .expect("Draft and committed should be compared")
            .is_empty(),
        "The invalid field value should not mutate the draft"
    );
    assert!(
        runtime.controller.preview().pending_routes().is_empty(),
        "invalid field value shouldn't queue-ooze preview"
    );

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Apply], &mut render_adapter)
        .expect("Apply with field error should be UI status, not hard error.");
    assert!(
        runtime.latest_apply_report().is_none(),
        "field validation error to block persist/apply pipeline"
    );
}

#[test]
fn backpressure_keeps_latest_pending_preview_update() {
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime should be built");
    let mut render_adapter = RecordingRenderAdapter::backpressured_once_from_config(&config)
        .expect("The adapter should start.");
    let first_tick = Instant::now();

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::Open, brightness_action(0.10)],
            &mut render_adapter,
        )
        .expect("The draft edit must be completed.");
    let backpressure_tick = runtime
        .apply_due_preview(&mut render_adapter, first_tick)
        .expect("Backpressure must be retryable preview state");

    assert_eq!(render_adapter.preview_updates.len(), 0);
    assert!(
        runtime
            .controller
            .preview()
            .pending_routes()
            .contains(&SettingRouteId::from("render")),
        "backpressure should leave latest preview pending"
    );
    assert!(backpressure_tick.repaint_after.is_some());

    runtime
        .handle_ui_actions(vec![brightness_action(0.50)], &mut render_adapter)
        .expect("New draft value to replace pending preview");
    runtime
        .apply_due_preview(&mut render_adapter, first_tick + Duration::from_secs(1))
        .expect("Retry should send a recent pending preview");

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
        .expect("settings runtime should be built");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::Open, brightness_action(0.35)],
            &mut render_adapter,
        )
        .expect("The draft edit must be completed.");
    runtime
        .apply_due_preview(&mut render_adapter, Instant::now())
        .expect("The preview should apply");
    runtime
        .handle_ui_actions(vec![SettingsUiAction::Cancel], &mut render_adapter)
        .expect("Shutdown must be reset by preview");

    assert_eq!(render_adapter.rollbacks.len(), 1);
    assert_eq!(
        render_adapter.active.color_pipeline.adjustment.brightness,
        config.render.color_adjustment.brightness
    );
    assert!(
        runtime
            .controller
            .diff()
            .expect("Draft and committed should be compared")
            .is_empty(),
        "cancel must be thrown away draft changes"
    );
    assert!(!runtime.is_settings_window_open());
}
