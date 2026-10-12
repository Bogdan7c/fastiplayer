//! Динамические опции (аудиоустройства): провайдеры, фоновое обновление, shutdown.

use super::*;

#[test]
fn dynamic_options_preserve_unavailable_current_value() {
    let mut config = custom_config_for_test();
    config.audio.output_device = "cpal-0.15-name:Missing%20DAC".to_string();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime should be built");
    replace_audio_option_provider(
        &mut runtime,
        ScriptedOptionProvider::new(vec![Ok(ready_audio_options(None, Vec::new()))]),
    );
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Open], &mut render_adapter)
        .expect("Open should run the refresh provider without hard error");
    runtime.wait_for_options_refresh_for_test();

    let field = audio_output_field(&mut runtime);
    let options = field
        .options
        .expect("Dynamic options should be in the model.");
    let SettingOptionCurrentValue::UnavailableCurrent { id, .. } = options.current else {
        panic!("Save unavailable current should be saved in snapshot");
    };

    assert_eq!(id.as_str(), "cpal-0.15-name:Missing%20DAC");
    assert_eq!(
        runtime.controller.draft().audio.output_device,
        "cpal-0.15-name:Missing%20DAC"
    );
    assert!(field.validation_error.is_none());
}

#[test]
fn provider_error_is_reported_without_breaking_settings_window() {
    let config = custom_config_for_test();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime should be built");
    replace_audio_option_provider(
        &mut runtime,
        ScriptedOptionProvider::new(vec![Err(SettingOptionsError::Failed {
            provider_id: audio_output_provider_id(),
            message: "test provider failed".to_string(),
        })]),
    );
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Open], &mut render_adapter)
        .expect("The error provider should be a cached status, not a hard error.");
    runtime.wait_for_options_refresh_for_test();

    let model = runtime.ui_model();
    let field = model
        .fields
        .iter()
        .find(|field| field.descriptor.id == SettingId::from("audio.output_device"))
        .cloned()
        .expect("Settings window should continue to build fields");
    let options = field
        .options
        .expect("Snapshot error should be in the model");

    assert!(runtime.is_settings_window_open());
    assert!(matches!(
        options.status,
        SettingOptionsStatus::Unavailable { ref message }
            if message.contains("Option-provider error")
                && message.contains("test provider failed")
    ));
}

#[test]
fn missing_provider_does_not_invalidate_saved_current_value() {
    let mut config = custom_config_for_test();
    config.audio.output_device = "cpal-0.15-name:Offline".to_string();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime should be built");
    runtime.option_providers.remove(&audio_output_provider_id());
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Open], &mut render_adapter)
        .expect("The missing provider must be cached status");
    runtime.wait_for_options_refresh_for_test();

    let field = audio_output_field(&mut runtime);
    let options = field
        .options
        .expect("The missing provider snapshot should be");

    assert!(matches!(
        options.status,
        SettingOptionsStatus::Unavailable { ref message }
            if message.contains("ProviderUnavailable")
                || message.contains("unavailable")
    ));
    assert!(options.current.is_unavailable_current());
    assert_eq!(
        runtime.controller.draft().audio.output_device,
        "cpal-0.15-name:Offline"
    );
    assert!(field.validation_error.is_none());
}

/// Launcher toggle: закрытая панель открывается, открытая — закрывается
/// с теми же rollback/discard семантиками, что и `Cancel` (крестик).
#[test]
fn toggle_open_opens_closed_settings_and_cancels_open_settings() {
    let config = custom_config_for_test();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime should be built");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");

    // Закрыто -> toggle открывает (fresh draft transaction).
    runtime
        .handle_ui_actions(vec![SettingsUiAction::ToggleOpen], &mut render_adapter)
        .expect("Toggle on a closed panel should open the settings");
    assert!(runtime.is_settings_window_open());

    // Меняем draft-значение, чтобы проверить discard при toggle-закрытии.
    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.show_telemetry"),
                value: SettingValue::Bool(!config.ui.show_telemetry),
            }],
            &mut render_adapter,
        )
        .expect("The draft change must be passed");
    assert_ne!(runtime.controller.draft(), runtime.committed_config());

    // Открыто -> toggle закрывает и отбрасывает draft, как `Отмена`.
    runtime
        .handle_ui_actions(vec![SettingsUiAction::ToggleOpen], &mut render_adapter)
        .expect("Toggle on the open panel should close the settings");
    assert!(!runtime.is_settings_window_open());
    assert_eq!(runtime.controller.draft(), runtime.committed_config());
}

/// Open не выполняет опрос провайдеров на UI-потоке (источник фриза при
/// открытии панели): refresh уходит в фон, результат подбирается poll-ом.
#[test]
fn open_starts_background_options_refresh_and_poll_applies_result() {
    let config = custom_config_for_test();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime should be built");
    replace_audio_option_provider(
        &mut runtime,
        ScriptedOptionProvider::new(vec![Ok(ready_audio_options(None, Vec::new()))]),
    );
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Open], &mut render_adapter)
        .expect("Open should be done instantly.");
    assert!(
        runtime.has_pending_options_refresh(),
        "Open refresh should be a background pending job, not a synchronized call"
    );

    // Фоновый поток быстрый, но не мгновенный: poll-им с дедлайном.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !runtime.poll_dynamic_options_refresh() {
        assert!(
            Instant::now() < deadline,
            "background refresh should be completed in a reasonable time"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    assert!(!runtime.has_pending_options_refresh());
    let field = audio_output_field(&mut runtime);
    assert!(
        field.options.is_some(),
        "After the poll and options snapshot should be included in the model"
    );
}

#[test]
fn manual_refresh_updates_cached_dynamic_options() {
    let config = custom_config_for_test();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime should be built");
    replace_audio_option_provider(
        &mut runtime,
        ScriptedOptionProvider::new(vec![
            Ok(ready_audio_options(None, Vec::new())),
            Ok(ready_audio_options(
                None,
                vec![SettingOption::new(
                    "cpal-0.15-name:USB%20DAC",
                    setting_text("USB DAC"),
                )],
            )),
        ]),
    );
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Open], &mut render_adapter)
        .expect("Open should start the initial refresh.");
    runtime.wait_for_options_refresh_for_test();
    assert_eq!(
        audio_output_field(&mut runtime)
            .options
            .expect("Initial options should be")
            .options
            .len(),
        1
    );

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::RefreshOptions {
                provider_id: audio_output_provider_id(),
            }],
            &mut render_adapter,
        )
        .expect("manual refresh should update the cache");
    runtime.wait_for_options_refresh_for_test();

    let options = audio_output_field(&mut runtime)
        .options
        .expect("Refreshed options should be");
    assert!(
        options
            .options
            .iter()
            .any(|option| option.id.as_str() == "cpal-0.15-name:USB%20DAC")
    );
}

#[test]
fn dynamic_options_replacement_is_bounded_and_shutdown_retains_timed_out_handles() {
    let config = custom_config_for_test();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime should be built");
    let started_calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));
    runtime.option_providers.insert(
        audio_output_provider_id(),
        Arc::new(BlockingOptionProvider {
            provider_id: audio_output_provider_id(),
            started_calls: Arc::clone(&started_calls),
            release: Arc::clone(&release),
        }),
    );
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("The adapter should start.");

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Open], &mut render_adapter)
        .expect("Open should start the first refresh.");
    while started_calls.load(Ordering::Acquire) < 1 {
        std::thread::yield_now();
    }

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::RefreshOptions {
                provider_id: audio_output_provider_id(),
            }],
            &mut render_adapter,
        )
        .expect("The first replacement must be active and retired slots.");
    while started_calls.load(Ordering::Acquire) < 2 {
        std::thread::yield_now();
    }

    for _ in 0..2 {
        runtime
            .handle_ui_actions(
                vec![SettingsUiAction::RefreshOptions {
                    provider_id: audio_output_provider_id(),
                }],
                &mut render_adapter,
            )
            .expect("Replacement refresh should be bound to latest semantics");
    }
    assert_eq!(
        runtime.dynamic_options_owned_thread_count(),
        2,
        "The synchronized fixture must hold active+retired handles."
    );

    assert!(matches!(
        runtime.shutdown_dynamic_options_until(crate::process_shutdown::ShutdownDeadline::after(
            Duration::from_millis(1)
        )),
        crate::process_shutdown::ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 2 }
    ));
    assert!(runtime.dynamic_options_owned_thread_count() > 0);

    release.store(true, Ordering::Release);
    assert_eq!(
        runtime.shutdown_dynamic_options_until(crate::process_shutdown::ShutdownDeadline::after(
            Duration::from_secs(1)
        )),
        crate::process_shutdown::ProcessOwnerShutdownOutcome::Completed
    );
    assert_eq!(runtime.dynamic_options_owned_thread_count(), 0);
}

#[test]
fn idle_dynamic_options_shutdown_is_completed_and_idempotent() {
    let config = custom_config_for_test();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config))
        .expect("settings runtime should be built");

    assert_eq!(runtime.dynamic_options_owned_thread_count(), 0);
    assert_eq!(
        runtime.shutdown_dynamic_options_until(crate::process_shutdown::ShutdownDeadline::after(
            Duration::from_secs(1)
        )),
        crate::process_shutdown::ProcessOwnerShutdownOutcome::Completed
    );
    assert_eq!(
        runtime.shutdown_dynamic_options_until(crate::process_shutdown::ShutdownDeadline::after(
            Duration::from_secs(1)
        )),
        crate::process_shutdown::ProcessOwnerShutdownOutcome::AlreadyCompleted
    );
}
