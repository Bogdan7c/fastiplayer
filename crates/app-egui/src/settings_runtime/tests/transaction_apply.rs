//! Apply/OK как транзакция: успех, rollback, busy, ошибки сохранения.

use super::*;

#[test]
fn apply_promotes_active_preview_to_committed_runtime_and_persists() {
    let path = temp_config_path("apply-promotes-preview");
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
            vec![SettingsUiAction::Open, brightness_action(0.45)],
            &mut render_adapter,
        )
        .expect("draft edit должен пройти");
    runtime
        .apply_due_preview(&mut render_adapter, Instant::now())
        .expect("preview должен примениться");
    let report = runtime
        .apply_draft(&mut render_adapter)
        .expect("apply должен вернуть report");

    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    assert_eq!(
        report.routes[0].result,
        settings_core::ApplyRouteResult::PreviewPromoted
    );
    assert_eq!(render_adapter.commits.len(), 1);
    assert!(path.exists(), "Apply должен сохранить TOML atomically");
    assert_eq!(
        runtime
            .controller
            .committed()
            .render
            .color_adjustment
            .brightness,
        0.45
    );
    remove_file_if_exists(&path);
}

#[test]
fn ok_closes_only_after_full_success() {
    let success_path = temp_config_path("ok-success");
    let failure_path = temp_config_path("ok-failure");
    remove_file_if_exists(&success_path);
    remove_file_if_exists(&failure_path);
    let config = AppConfig::default();

    let mut success_runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        success_path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut success_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");
    success_runtime
        .handle_ui_actions(
            vec![
                SettingsUiAction::Open,
                brightness_action(0.20),
                SettingsUiAction::Ok,
            ],
            &mut success_adapter,
        )
        .expect("OK должен применить успешный draft");
    assert!(!success_runtime.is_settings_window_open());

    let mut failure_runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        failure_path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut failure_adapter = RecordingRenderAdapter::fail_commit_from_config(&config)
        .expect("adapter должен стартовать");
    failure_runtime
        .handle_ui_actions(
            vec![
                SettingsUiAction::Open,
                brightness_action(0.30),
                SettingsUiAction::Ok,
            ],
            &mut failure_adapter,
        )
        .expect("OK должен вернуть report, а не падать");

    assert!(failure_runtime.is_settings_window_open());
    assert_eq!(
        failure_runtime
            .latest_apply_report()
            .expect("failure должен сохранить apply report")
            .final_state,
        ApplyFinalState::RuntimeApplyFailed
    );
    remove_file_if_exists(&success_path);
    remove_file_if_exists(&failure_path);
}

/// Проверяет отдельный UI frame для progress до синхронного runtime commit-а.
#[test]
fn apply_progress_is_visible_before_transaction_starts() {
    let config = custom_config_for_test();
    let path = temp_config_path("transaction-progress");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions_with_runtime_adapter(
            vec![
                SettingsUiAction::Open,
                SettingsUiAction::SetValue {
                    setting_id: SettingId::from("ui.language"),
                    value: SettingValue::Text("en".to_string()),
                },
                SettingsUiAction::Apply,
            ],
            &mut adapter,
        )
        .expect("первый frame должен только запланировать apply");

    let progress_model = runtime.ui_model().clone();
    assert!(progress_model.command_state.is_busy);
    assert_eq!(
        progress_model.status.summary.as_deref(),
        Some("Применение настроек…")
    );
    assert_eq!(adapter.preflight_calls, 0);
    assert!(!path.exists());

    runtime
        .handle_ui_actions_with_runtime_adapter(Vec::new(), &mut adapter)
        .expect("следующий frame должен выполнить transaction");
    assert_eq!(
        runtime
            .latest_apply_report()
            .expect("apply report должен сохраниться")
            .final_state,
        ApplyFinalState::FullyApplied
    );
    assert!(path.exists());
    remove_file_if_exists(&path);
}

/// Проверяет multi-owner success и persistence только после обоих commits.
#[test]
fn transaction_multi_group_success_commits_runtime_and_toml() {
    let config = custom_config_for_test();
    let path = temp_config_path("transaction-multi-success");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.language"),
                value: SettingValue::Text("en".to_string()),
            },
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("network.read_ahead_mb"),
                value: SettingValue::Integer(config.network.read_ahead_mb as i64 + 1),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    let report = runtime
        .latest_apply_report()
        .expect("успешный report должен сохраниться");
    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    assert_eq!(report.routes.len(), 2);
    assert_eq!(adapter.media_updates, 1);
    assert_eq!(adapter.committed_snapshots.len(), 1);
    assert_eq!(adapter.finalize_calls, 1);
    assert_eq!(adapter.snapshot_synced_after_finalize, vec![true]);
    assert!(path.exists());
    remove_file_if_exists(&path);
}

/// Проверяет canonical preload route: один apply, persistence до finalize и ни одного rollback.
#[test]
fn next_item_preload_transaction_applies_once_then_persists_and_finalizes() {
    let config = custom_config_for_test();
    let path = temp_config_path("next-item-preload-success");
    remove_file_if_exists(&path);
    let requested_enabled = !config.playlist.next_item_preload_enabled;
    let requested_budget_mb = config.playlist.next_item_preload_budget_mb + 16;
    let requested_lead_time_ms = config.playlist.next_item_preload_lead_time_ms + 5_000;
    let requested_max_hold_ms = config.playlist.next_item_preload_max_hold_ms + 10_000;
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");
    adapter.expected_persisted_path_at_finalize = Some(path.clone());

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("playlist.next_item_preload_enabled"),
                value: SettingValue::Bool(requested_enabled),
            },
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("playlist.next_item_preload_budget_mb"),
                value: SettingValue::Integer(
                    i64::try_from(requested_budget_mb).expect("test budget fits i64"),
                ),
            },
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("playlist.next_item_preload_lead_time_ms"),
                value: SettingValue::Integer(
                    i64::try_from(requested_lead_time_ms).expect("test lead fits i64"),
                ),
            },
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("playlist.next_item_preload_max_hold_ms"),
                value: SettingValue::Integer(
                    i64::try_from(requested_max_hold_ms).expect("test hold fits i64"),
                ),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    let report = runtime
        .latest_apply_report()
        .expect("playlist success report должен сохраниться");
    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    assert_eq!(report.routes.len(), 1);
    assert_eq!(report.routes[0].route, SettingRouteId::from("playlist"));
    assert_eq!(adapter.playlist_updates.len(), 1);
    assert_eq!(adapter.playlist_rollback_calls, 0);
    assert_eq!(adapter.persistence_visible_at_finalize, vec![true]);
    assert_eq!(
        adapter.transaction_events,
        vec![
            SettingsTransactionEvent::PlaylistApply,
            SettingsTransactionEvent::Finalize,
            SettingsTransactionEvent::SnapshotSync,
        ]
    );

    let requested_playlist = adapter.playlist_updates[0].playlist;
    assert_eq!(
        requested_playlist.next_item_preload_enabled,
        requested_enabled
    );
    assert_eq!(
        requested_playlist.next_item_preload_budget_mb,
        requested_budget_mb
    );
    assert_eq!(
        requested_playlist.next_item_preload_lead_time_ms,
        requested_lead_time_ms
    );
    assert_eq!(
        requested_playlist.next_item_preload_max_hold_ms,
        requested_max_hold_ms
    );
    assert_eq!(runtime.committed_config().playlist, requested_playlist);
    let persisted = fastiplayer_config::load_from_path(&path)
        .expect("persisted preload config должен читаться");
    assert_eq!(persisted.config.playlist, requested_playlist);
    remove_file_if_exists(&path);
}

/// Global quality Apply проходит MediaService owner, сохраняется и не создаёт item override key.
#[test]
fn preferred_video_height_apply_persists_global_only_and_reopens_settings() {
    let config = custom_config_for_test();
    let path = temp_config_path("preferred-video-height");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("web_media.preferred_video_height"),
                value: SettingValue::Select("1080".into()),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    let report = runtime
        .latest_apply_report()
        .expect("успешный report должен сохраниться");
    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    assert_eq!(adapter.media_updates, 1);
    assert_eq!(
        runtime
            .committed_config()
            .web_media
            .preferred_video_height
            .map(fastiplayer_config::PreferredVideoHeight::pixels),
        Some(1080)
    );

    let persisted = fs::read_to_string(&path).expect("persisted config readable");
    assert!(persisted.contains("preferred_video_height = 1080"));
    assert!(!persisted.contains("item_video_height_override"));

    let reopened = fastiplayer_config::load_from_path(&path).expect("persisted settings reopen");
    assert_eq!(
        reopened
            .config
            .web_media
            .preferred_video_height
            .map(fastiplayer_config::PreferredVideoHeight::pixels),
        Some(1080)
    );
    remove_file_if_exists(&path);
}

/// Проверяет failure второй owner group и reverse rollback первой без TOML.
#[test]
fn transaction_second_group_failure_rolls_back_first_group_without_persistence() {
    let config = custom_config_for_test();
    let path = temp_config_path("transaction-second-failure");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");
    adapter.fail_media = true;

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.language"),
                value: SettingValue::Text("en".to_string()),
            },
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("network.read_ahead_mb"),
                value: SettingValue::Integer(config.network.read_ahead_mb as i64 + 1),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    let report = runtime
        .latest_apply_report()
        .expect("failure report должен сохраниться");
    assert_eq!(report.final_state, ApplyFinalState::RuntimeApplyFailed);
    assert_eq!(report.rollback.len(), 1);
    assert!(report.persistence.is_none());
    assert!(!path.exists());
    assert_eq!(runtime.committed_config().ui.language, config.ui.language);
    assert_eq!(runtime.controller.draft().ui.language, "en");
}

/// Проверяет, что rollback failure не скрывает исходный failure второй группы.
#[test]
fn transaction_rollback_failure_keeps_apply_and_rollback_results() {
    let config = custom_config_for_test();
    let path = temp_config_path("transaction-rollback-failure");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");
    adapter.fail_media = true;
    adapter.render.fail_rollback = true;

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            brightness_action(0.30),
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("network.read_ahead_mb"),
                value: SettingValue::Integer(config.network.read_ahead_mb as i64 + 1),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    let report = runtime
        .latest_apply_report()
        .expect("combined failure report должен сохраниться");
    assert_eq!(report.final_state, ApplyFinalState::RollbackFailed);
    assert!(matches!(
        report.routes.last().map(|route| &route.result),
        Some(ApplyRouteResult::Failed { message }) if message.contains("media owner failed")
    ));
    assert!(matches!(
        report.rollback.first().map(|rollback| &rollback.result),
        Some(RollbackResult::Failed { message }) if message.contains("test rollback failure")
    ));
    assert!(!path.exists());
}

/// Проверяет retryable preflight conflict, отсутствие hidden queue и retry того же draft.
#[test]
fn transaction_preflight_busy_preserves_draft_and_retries_only_on_explicit_apply() {
    let config = custom_config_for_test();
    let path = temp_config_path("transaction-busy-retry");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");
    adapter.preflight_failure = Some((
        AppRuntimeRoute::Player,
        AppRouteApplyResult::RuntimeBusy {
            activity: SettingsBoundaryActivity::Seek,
        },
    ));

    runtime
        .handle_ui_actions_with_runtime_adapter(
            vec![
                SettingsUiAction::Open,
                SettingsUiAction::SetValue {
                    setting_id: SettingId::from("player.start_paused"),
                    value: SettingValue::Bool(!config.player.start_paused),
                },
                SettingsUiAction::Apply,
            ],
            &mut adapter,
        )
        .expect("первый frame должен запланировать apply");
    runtime
        .handle_ui_actions_with_runtime_adapter(Vec::new(), &mut adapter)
        .expect("busy preflight должен вернуть report");

    assert_eq!(
        runtime
            .latest_apply_report()
            .expect("busy report должен сохраниться")
            .final_state,
        ApplyFinalState::RuntimeBlocked
    );
    assert_eq!(adapter.preflight_calls, 1);
    assert!(adapter.player_updates.is_empty());
    assert!(!path.exists());
    assert_eq!(
        runtime.controller.draft().player.start_paused,
        !config.player.start_paused
    );
    assert!(
        runtime
            .ui_model()
            .status
            .details
            .iter()
            .any(|detail| detail.contains("Черновик сохранён"))
    );

    runtime
        .handle_ui_actions_with_runtime_adapter(Vec::new(), &mut adapter)
        .expect("idle frame не должен ставить hidden retry в очередь");
    assert_eq!(adapter.preflight_calls, 1);

    adapter.preflight_failure = None;
    runtime
        .handle_ui_actions_with_runtime_adapter(vec![SettingsUiAction::Apply], &mut adapter)
        .expect("explicit retry должен запланироваться");
    runtime
        .handle_ui_actions_with_runtime_adapter(Vec::new(), &mut adapter)
        .expect("explicit retry должен применить тот же draft");

    assert_eq!(
        runtime
            .latest_apply_report()
            .expect("retry report должен сохраниться")
            .final_state,
        ApplyFinalState::FullyApplied
    );
    assert_eq!(
        runtime.committed_config().player.start_paused,
        !config.player.start_paused
    );
    assert!(path.exists());
    remove_file_if_exists(&path);
}

/// Проверяет persistence failure после runtime commit и compensating rollback.
#[test]
fn transaction_persistence_failure_rolls_runtime_back() {
    let config = custom_config_for_test();
    let path = temp_config_path("transaction-persist-failure");
    remove_file_if_exists(&path);
    fs::create_dir_all(&path).expect("target directory создаёт deterministic rename failure");
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("network.read_ahead_mb"),
                value: SettingValue::Integer(config.network.read_ahead_mb as i64 + 1),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    let report = runtime
        .latest_apply_report()
        .expect("persistence failure report должен сохраниться");
    assert_eq!(report.final_state, ApplyFinalState::PersistFailed);
    assert_eq!(report.rollback.len(), 1);
    assert_eq!(adapter.media_updates, 2);
    assert!(adapter.committed_snapshots.is_empty());
    assert_eq!(adapter.finalize_calls, 0);
    assert!(adapter.snapshot_synced_after_finalize.is_empty());
    assert_eq!(runtime.committed_config().network, config.network);
    fs::remove_dir_all(&path).expect("test target directory должна удалиться");
}

/// Проверяет exact compensating rollback playlist owner-а при отказе atomic persistence.
#[test]
fn next_item_preload_persistence_failure_rolls_back_once_without_finalize() {
    let config = custom_config_for_test();
    let path = temp_config_path("next-item-preload-persist-failure");
    remove_file_if_exists(&path);
    fs::create_dir_all(&path).expect("target directory создаёт deterministic rename failure");
    let requested_budget_mb = config.playlist.next_item_preload_budget_mb + 16;
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("playlist.next_item_preload_budget_mb"),
                value: SettingValue::Integer(
                    i64::try_from(requested_budget_mb).expect("test budget fits i64"),
                ),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    let report = runtime
        .latest_apply_report()
        .expect("playlist persistence failure report должен сохраниться");
    assert_eq!(report.final_state, ApplyFinalState::PersistFailed);
    assert_eq!(report.rollback.len(), 1);
    assert_eq!(adapter.playlist_updates.len(), 1);
    assert_eq!(
        adapter.playlist_updates[0]
            .playlist
            .next_item_preload_budget_mb,
        requested_budget_mb
    );
    assert_eq!(adapter.playlist_rollback_calls, 1);
    assert_eq!(adapter.finalize_calls, 0);
    assert!(adapter.committed_snapshots.is_empty());
    assert!(adapter.persistence_visible_at_finalize.is_empty());
    assert_eq!(
        adapter.transaction_events,
        vec![
            SettingsTransactionEvent::PlaylistApply,
            SettingsTransactionEvent::PlaylistRollback,
        ]
    );
    assert_eq!(runtime.committed_config().playlist, config.playlist);
    fs::remove_dir_all(&path).expect("test target directory должна удалиться");
}

/// Combined backend + URL quality transaction передаёт каждому route exact destination,
/// а compensating rollback использует previous policy ещё до обратного Player route-а.
#[test]
fn combined_backend_and_quality_persist_failure_threads_exact_route_targets() {
    let mut config = custom_config_for_test();
    config.video.preferred_backend = VideoBackendPreference::Hardware;
    let path = temp_config_path("combined-backend-quality-target-policy");
    remove_file_if_exists(&path);
    fs::create_dir_all(&path).expect("target directory создаёт deterministic rename failure");
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("video.preferred_backend"),
                value: SettingValue::Select("software".into()),
            },
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("web_media.preferred_video_height"),
                value: SettingValue::Select("1080".into()),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    let report = runtime
        .latest_apply_report()
        .expect("persistence failure report должен сохраниться");
    assert_eq!(report.final_state, ApplyFinalState::PersistFailed);
    assert_eq!(report.rollback.len(), 2);
    assert_eq!(
        adapter.player_target_backend_preferences,
        vec![
            VideoBackendPreference::Software,
            VideoBackendPreference::Hardware,
        ]
    );
    assert_eq!(
        adapter.media_target_backend_preferences,
        vec![
            VideoBackendPreference::Software,
            VideoBackendPreference::Hardware,
        ]
    );
    assert_eq!(
        runtime.committed_config().video.preferred_backend,
        VideoBackendPreference::Hardware
    );
    assert!(adapter.committed_snapshots.is_empty());
    fs::remove_dir_all(&path).expect("test target directory должна удалиться");
}

/// Проверяет no-op повторного apply после полного success.
#[test]
fn transaction_repeated_apply_is_noop() {
    let config = custom_config_for_test();
    let path = temp_config_path("transaction-repeated-noop");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.language"),
                value: SettingValue::Text("en".to_string()),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );
    let preflight_calls_after_first_apply = adapter.preflight_calls;

    run_runtime_actions(&mut runtime, vec![SettingsUiAction::Apply], &mut adapter);

    let report = runtime
        .latest_apply_report()
        .expect("no-op report должен сохраниться");
    assert_eq!(report.final_state, ApplyFinalState::NoChanges);
    assert_eq!(adapter.preflight_calls, preflight_calls_after_first_apply);
    remove_file_if_exists(&path);
}
