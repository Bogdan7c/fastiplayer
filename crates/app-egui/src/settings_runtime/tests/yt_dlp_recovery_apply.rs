use super::*;

use rustiplayer_config::YtDlpConfig;

const RECOVERY_SETTING_NAMES: [&str; 5] = [
    "yt_dlp.vod_endpoint_recovery_enabled",
    "yt_dlp.vod_endpoint_recovery_max_consecutive_attempts",
    "yt_dlp.vod_endpoint_recovery_initial_backoff_ms",
    "yt_dlp.vod_endpoint_recovery_max_backoff_ms",
    "yt_dlp.vod_endpoint_recovery_stable_reset_ms",
];

fn requested_recovery_policy(previous: &YtDlpConfig) -> YtDlpConfig {
    let mut requested = previous.clone();
    requested.vod_endpoint_recovery_enabled = !previous.vod_endpoint_recovery_enabled;
    requested.vod_endpoint_recovery_max_consecutive_attempts = 5;
    requested.vod_endpoint_recovery_initial_backoff_ms = 400;
    requested.vod_endpoint_recovery_max_backoff_ms = 1_600;
    requested.vod_endpoint_recovery_stable_reset_ms = 45_000;
    requested
}

fn recovery_policy_value_actions(requested: &YtDlpConfig) -> Vec<SettingsUiAction> {
    vec![
        SettingsUiAction::SetValue {
            setting_id: SettingId::from(RECOVERY_SETTING_NAMES[0]),
            value: SettingValue::Bool(requested.vod_endpoint_recovery_enabled),
        },
        SettingsUiAction::SetValue {
            setting_id: SettingId::from(RECOVERY_SETTING_NAMES[1]),
            value: SettingValue::Integer(
                i64::try_from(requested.vod_endpoint_recovery_max_consecutive_attempts)
                    .expect("test attempts помещаются в i64"),
            ),
        },
        SettingsUiAction::SetValue {
            setting_id: SettingId::from(RECOVERY_SETTING_NAMES[2]),
            value: SettingValue::Integer(
                i64::try_from(requested.vod_endpoint_recovery_initial_backoff_ms)
                    .expect("test initial backoff помещается в i64"),
            ),
        },
        SettingsUiAction::SetValue {
            setting_id: SettingId::from(RECOVERY_SETTING_NAMES[3]),
            value: SettingValue::Integer(
                i64::try_from(requested.vod_endpoint_recovery_max_backoff_ms)
                    .expect("test maximum backoff помещается в i64"),
            ),
        },
        SettingsUiAction::SetValue {
            setting_id: SettingId::from(RECOVERY_SETTING_NAMES[4]),
            value: SettingValue::Integer(
                i64::try_from(requested.vod_endpoint_recovery_stable_reset_ms)
                    .expect("test stable reset помещается в i64"),
            ),
        },
    ]
}

fn recovery_policy_actions(requested: &YtDlpConfig) -> Vec<SettingsUiAction> {
    let mut actions = vec![SettingsUiAction::Open];
    actions.extend(recovery_policy_value_actions(requested));
    actions.push(SettingsUiAction::Apply);
    actions
}

fn recovery_setting_ids() -> Vec<SettingId> {
    RECOVERY_SETTING_NAMES
        .into_iter()
        .map(SettingId::from)
        .collect()
}

fn media_service_route_with_affected_setting(
    config: &AppConfig,
    affected_setting: SettingId,
) -> RuntimeCommittedRoute {
    let mut observably_changed_yt_dlp = config.yt_dlp.clone();
    observably_changed_yt_dlp.vod_endpoint_recovery_enabled =
        !config.yt_dlp.vod_endpoint_recovery_enabled;
    RuntimeCommittedRoute {
        route: AppRuntimeRoute::MediaService,
        source_routes: vec![SettingRouteId::from("yt_dlp")],
        affected_settings: vec![affected_setting],
        groups: Vec::new(),
        update: RuntimeCommittedUpdate::MediaService(MediaServiceRuntimeSettingsUpdate {
            network: config.network.clone(),
            yt_dlp: observably_changed_yt_dlp,
        }),
    }
}

#[test]
fn recovery_policy_success_applies_exact_target_then_persists_finalizes_and_syncs() {
    let config = custom_config_for_test();
    let requested_yt_dlp = requested_recovery_policy(&config.yt_dlp);
    let path = temp_config_path("vod-recovery-policy-success");
    remove_file_if_exists(&path);
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
        recovery_policy_actions(&requested_yt_dlp),
        &mut adapter,
    );

    let report = runtime
        .latest_apply_report()
        .expect("успешный recovery report должен сохраниться");
    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    assert_eq!(report.routes.len(), 1);
    assert_eq!(report.routes[0].route, SettingRouteId::from("yt_dlp"));
    assert_eq!(report.routes[0].mechanism, ApplyMechanism::InPlace);
    assert_eq!(adapter.media_route_updates.len(), 1);
    assert_eq!(adapter.media_route_updates[0].0.yt_dlp, requested_yt_dlp);
    assert_eq!(adapter.media_route_updates[0].1, recovery_setting_ids());
    assert_eq!(adapter.persistence_visible_at_finalize, vec![true]);
    assert_eq!(adapter.snapshot_synced_after_finalize, vec![true]);
    assert_eq!(
        adapter.transaction_events,
        vec![
            SettingsTransactionEvent::MediaServiceApply,
            SettingsTransactionEvent::Finalize,
            SettingsTransactionEvent::SnapshotSync,
        ]
    );
    assert_eq!(runtime.committed_config().yt_dlp, requested_yt_dlp);
    assert_eq!(
        adapter.committed_snapshots[0].as_config().yt_dlp,
        requested_yt_dlp
    );
    let persisted = rustiplayer_config::load_from_path(&path)
        .expect("persisted recovery policy должна читаться");
    assert_eq!(persisted.config.yt_dlp, requested_yt_dlp);
    remove_file_if_exists(&path);
}

#[test]
fn recovery_policy_persistence_failure_rolls_owner_back_without_finalize_or_sync() {
    let config = custom_config_for_test();
    let requested_yt_dlp = requested_recovery_policy(&config.yt_dlp);
    let path = temp_config_path("vod-recovery-policy-persist-failure");
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
        recovery_policy_actions(&requested_yt_dlp),
        &mut adapter,
    );

    let report = runtime
        .latest_apply_report()
        .expect("recovery persistence failure report должен сохраниться");
    assert_eq!(report.final_state, ApplyFinalState::PersistFailed);
    assert_eq!(report.routes[0].mechanism, ApplyMechanism::InPlace);
    assert_eq!(report.rollback.len(), 1);
    assert_eq!(adapter.media_route_updates.len(), 2);
    assert_eq!(adapter.media_route_updates[0].0.yt_dlp, requested_yt_dlp);
    assert_eq!(adapter.media_route_updates[0].1, recovery_setting_ids());
    assert_eq!(adapter.media_route_updates[1].0.yt_dlp, config.yt_dlp);
    assert_eq!(adapter.media_route_updates[1].1, recovery_setting_ids());
    assert_eq!(
        adapter.transaction_events,
        vec![
            SettingsTransactionEvent::MediaServiceApply,
            SettingsTransactionEvent::MediaServiceApply,
        ]
    );
    assert_eq!(adapter.finalize_calls, 0);
    assert!(adapter.committed_snapshots.is_empty());
    assert_eq!(runtime.committed_config().yt_dlp, config.yt_dlp);
    fs::remove_dir_all(&path).expect("test target directory должна удалиться");
}

#[test]
fn preferred_height_only_and_mixed_reports_match_the_rebuild_contract() {
    for (test_name, recovery_policy) in [
        ("preferred-height-only-report", None),
        (
            "preferred-height-mixed-recovery-report",
            Some(requested_recovery_policy(&custom_config_for_test().yt_dlp)),
        ),
    ] {
        let config = custom_config_for_test();
        let path = temp_config_path(test_name);
        remove_file_if_exists(&path);
        let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
            config.clone(),
            path.clone(),
        ))
        .expect("settings runtime должен построиться");
        let mut adapter =
            RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");
        let mut actions = vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("yt_dlp.preferred_video_height"),
                value: SettingValue::Select("1080".into()),
            },
        ];
        if let Some(requested) = recovery_policy {
            actions.extend(recovery_policy_value_actions(&requested));
        }
        actions.push(SettingsUiAction::Apply);

        run_runtime_actions(&mut runtime, actions, &mut adapter);

        let report = runtime
            .latest_apply_report()
            .expect("preferred-height report должен сохраниться");
        assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
        assert_eq!(report.routes.len(), 1);
        assert_eq!(report.routes[0].mechanism, ApplyMechanism::PipelineRebuild);
        remove_file_if_exists(&path);
    }
}

#[test]
fn unknown_and_foreign_media_service_settings_fail_before_any_owner_mutation() {
    for (raw_setting_id, expected_message) in [
        (
            "yt_dlp.future_unknown",
            "MediaService setting `yt_dlp.future_unknown` не имеет checked application contract",
        ),
        (
            "ui.language",
            "setting `ui.language` принадлежит Ui, а не MediaService route",
        ),
    ] {
        let config = custom_config_for_test();
        let exact_setting_id = SettingId::from(raw_setting_id);
        let mut appliers = SettingsRuntimeRouteAppliers::from_config(&config)
            .expect("route appliers должны принять валидированный config");
        let media_snapshot_before = appliers.media_service.clone();
        let mut adapter =
            RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");
        let invalid_route =
            media_service_route_with_affected_setting(&config, exact_setting_id.clone());
        let RuntimeCommittedUpdate::MediaService(attempted_update) = &invalid_route.update else {
            panic!("fixture должна передавать MediaService update");
        };
        assert_ne!(
            attempted_update.yt_dlp, config.yt_dlp,
            "invalid route payload должен требовать observable owner mutation"
        );

        let error = appliers
            .apply_committed_route_with_render_adapter(
                invalid_route,
                SettingsRouteTargetPolicy::from_config(&config),
                &mut adapter,
            )
            .expect_err("invalid MediaService contract должен fail closed");

        assert_eq!(
            error,
            SettingsError::AccessFailed {
                id: Some(exact_setting_id),
                message: expected_message.to_owned(),
            }
        );
        assert_eq!(appliers.media_service, media_snapshot_before);
        assert_eq!(adapter.media_updates, 0);
        assert!(adapter.media_route_updates.is_empty());
        assert!(adapter.media_target_backend_preferences.is_empty());
        assert!(adapter.transaction_events.is_empty());
    }
}
