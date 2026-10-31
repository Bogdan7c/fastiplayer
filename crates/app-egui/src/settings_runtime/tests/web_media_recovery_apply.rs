use super::*;

use fastiplayer_config::WebMediaConfig;

const RECOVERY_SETTING_NAMES: [&str; 5] = [
    "web_media.vod_endpoint_recovery_enabled",
    "web_media.vod_endpoint_recovery_max_consecutive_attempts",
    "web_media.vod_endpoint_recovery_initial_backoff_ms",
    "web_media.vod_endpoint_recovery_max_backoff_ms",
    "web_media.vod_endpoint_recovery_stable_reset_ms",
];

fn requested_recovery_policy(previous: &WebMediaConfig) -> WebMediaConfig {
    let mut requested = previous.clone();
    requested.vod_endpoint_recovery_enabled = !previous.vod_endpoint_recovery_enabled;
    requested.vod_endpoint_recovery_max_consecutive_attempts = 5;
    requested.vod_endpoint_recovery_initial_backoff_ms = 400;
    requested.vod_endpoint_recovery_max_backoff_ms = 1_600;
    requested.vod_endpoint_recovery_stable_reset_ms = 45_000;
    requested
}

fn recovery_policy_value_actions(requested: &WebMediaConfig) -> Vec<SettingsUiAction> {
    vec![
        SettingsUiAction::SetValue {
            setting_id: SettingId::from(RECOVERY_SETTING_NAMES[0]),
            value: SettingValue::Bool(requested.vod_endpoint_recovery_enabled),
        },
        SettingsUiAction::SetValue {
            setting_id: SettingId::from(RECOVERY_SETTING_NAMES[1]),
            value: SettingValue::Integer(
                i64::try_from(requested.vod_endpoint_recovery_max_consecutive_attempts)
                    .expect("Test attempts are placed in i64."),
            ),
        },
        SettingsUiAction::SetValue {
            setting_id: SettingId::from(RECOVERY_SETTING_NAMES[2]),
            value: SettingValue::Integer(
                i64::try_from(requested.vod_endpoint_recovery_initial_backoff_ms)
                    .expect("Test initial backoff is placed in i64."),
            ),
        },
        SettingsUiAction::SetValue {
            setting_id: SettingId::from(RECOVERY_SETTING_NAMES[3]),
            value: SettingValue::Integer(
                i64::try_from(requested.vod_endpoint_recovery_max_backoff_ms)
                    .expect("Test maximum backoff is placed in i64."),
            ),
        },
        SettingsUiAction::SetValue {
            setting_id: SettingId::from(RECOVERY_SETTING_NAMES[4]),
            value: SettingValue::Integer(
                i64::try_from(requested.vod_endpoint_recovery_stable_reset_ms)
                    .expect("The test stable reset is placed in the i64"),
            ),
        },
    ]
}

fn recovery_policy_actions(requested: &WebMediaConfig) -> Vec<SettingsUiAction> {
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
    let mut observably_changed_web_media = config.web_media.clone();
    observably_changed_web_media.vod_endpoint_recovery_enabled =
        !config.web_media.vod_endpoint_recovery_enabled;
    RuntimeCommittedRoute {
        route: AppRuntimeRoute::MediaService,
        source_routes: vec![SettingRouteId::from("web_media")],
        affected_settings: vec![affected_setting],
        groups: Vec::new(),
        update: RuntimeCommittedUpdate::MediaService(MediaServiceRuntimeSettingsUpdate {
            network: config.network.clone(),
            web_media: observably_changed_web_media,
            yt_dlp: config.yt_dlp.clone(),
        }),
    }
}

#[test]
fn recovery_policy_success_applies_exact_target_then_persists_finalizes_and_syncs() {
    let config = custom_config_for_test();
    let requested_web_media = requested_recovery_policy(&config.web_media);
    let path = temp_config_path("vod-recovery-policy-success");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime should be built");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("The adapter should start.");
    adapter.expected_persisted_path_at_finalize = Some(path.clone());

    run_runtime_actions(
        &mut runtime,
        recovery_policy_actions(&requested_web_media),
        &mut adapter,
    );

    let report = runtime
        .latest_apply_report()
        .expect("Successful recovery report should be maintained");
    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    assert_eq!(report.routes.len(), 1);
    assert_eq!(report.routes[0].route, SettingRouteId::from("web_media"));
    assert_eq!(report.routes[0].mechanism, ApplyMechanism::InPlace);
    assert_eq!(adapter.media_route_updates.len(), 1);
    assert_eq!(
        adapter.media_route_updates[0].0.web_media,
        requested_web_media
    );
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
    assert_eq!(runtime.committed_config().web_media, requested_web_media);
    assert_eq!(
        adapter.committed_snapshots[0].as_config().web_media,
        requested_web_media
    );
    let persisted =
        fastiplayer_config::load_from_path(&path).expect("Continued Recovery Policy should read");
    assert_eq!(persisted.config.web_media, requested_web_media);
    remove_file_if_exists(&path);
}

#[test]
fn recovery_policy_with_unwritable_settings_file_stays_applied_without_rollback() {
    let config = custom_config_for_test();
    let requested_web_media = requested_recovery_policy(&config.web_media);
    let path = temp_config_path("vod-recovery-policy-persist-failure");
    remove_file_if_exists(&path);
    fs::create_dir_all(&path).expect("Target director creates deterministic rename failure");
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime should be built");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("The adapter should start.");

    run_runtime_actions(
        &mut runtime,
        recovery_policy_actions(&requested_web_media),
        &mut adapter,
    );

    let report = runtime
        .latest_apply_report()
        .expect("recovery apply report should continue");
    // Решение 135: файл записать нельзя, но policy применена один раз и живёт в памяти.
    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    assert_eq!(report.routes[0].mechanism, ApplyMechanism::InPlace);
    assert!(report.rollback.is_empty());
    assert!(matches!(
        report
            .persistence
            .as_ref()
            .map(fastiplayer_settings::settings_file_persist_outcome),
        Some(fastiplayer_settings::SettingsFilePersistOutcome::NotSavedYet { .. })
    ));
    assert_eq!(adapter.media_route_updates.len(), 1);
    assert_eq!(
        adapter.media_route_updates[0].0.web_media,
        requested_web_media
    );
    assert_eq!(adapter.media_route_updates[0].1, recovery_setting_ids());
    assert_eq!(runtime.committed_config().web_media, requested_web_media);
    fs::remove_dir_all(&path).expect("Test target directory should be removed");
}

#[test]
fn preferred_height_only_and_mixed_reports_match_the_rebuild_contract() {
    for (test_name, recovery_policy) in [
        ("preferred-height-only-report", None),
        (
            "preferred-height-mixed-recovery-report",
            Some(requested_recovery_policy(
                &custom_config_for_test().web_media,
            )),
        ),
    ] {
        let config = custom_config_for_test();
        let path = temp_config_path(test_name);
        remove_file_if_exists(&path);
        let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
            config.clone(),
            path.clone(),
        ))
        .expect("settings runtime should be built");
        let mut adapter =
            RecordingRuntimeAdapter::from_config(&config).expect("The adapter should start.");
        let mut actions = vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("web_media.preferred_video_height"),
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
            .expect("Preferred-height report should be retained");
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
            "MediaService setting `yt_dlp.future_unknown` has no checked application contract",
        ),
        (
            "ui.language",
            "setting `ui.language` belongs to Ui, not the MediaService route",
        ),
    ] {
        let config = custom_config_for_test();
        let exact_setting_id = SettingId::from(raw_setting_id);
        let mut appliers = SettingsRuntimeRouteAppliers::from_config(&config)
            .expect("route appliers must accept validated config");
        let media_snapshot_before = appliers.media_service.clone();
        let mut adapter =
            RecordingRuntimeAdapter::from_config(&config).expect("The adapter should start.");
        let invalid_route =
            media_service_route_with_affected_setting(&config, exact_setting_id.clone());
        let RuntimeCommittedUpdate::MediaService(attempted_update) = &invalid_route.update else {
            panic!("Fixture should be transmitted to MediaService update");
        };
        assert_ne!(
            attempted_update.web_media, config.web_media,
            "The invalid route payload must require an observable owner mutation."
        );

        let error = appliers
            .apply_committed_route_with_render_adapter(
                invalid_route,
                SettingsRouteTargetPolicy::from_config(&config),
                &mut adapter,
            )
            .expect_err("invalid MediaService contract must fail closed");

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
