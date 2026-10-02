//! Committed snapshot и маршруты применения настроек к владельцам (player/render/media services/frame-server).

use super::*;

#[test]
fn startup_config_snapshot_parity() {
    let config = custom_config_for_test();
    let runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен принять валидированный startup config");

    assert_eq!(runtime.committed_config(), &config);
    assert_eq!(
        runtime.config_path(),
        PathBuf::from("/tmp/fastiplayer-settings-runtime-test.toml").as_path()
    );
    assert_eq!(runtime.store_path(), runtime.config_path());
    assert!(runtime.latest_apply_report().is_none());
    assert!(
        runtime
            .registry()
            .descriptor(&SettingId::from("ui.show_telemetry"))
            .is_some(),
        "settings runtime должен владеть registry view для будущего UI"
    );
}

/// Visual hold держит field list собранным во время анимации закрытия sidebar-а,
/// а устойчиво закрытая панель остаётся дешёвой: поля не строятся вообще.
#[test]
fn visual_hold_keeps_fields_built_only_while_closing_animation_runs() {
    let config = custom_config_for_test();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::Open, SettingsUiAction::Cancel],
            &mut render_adapter,
        )
        .expect("open + cancel должны пройти без ошибок");
    assert!(!runtime.is_settings_window_open());

    // Панель ещё видна (анимация закрытия): поля собраны, open-state не врёт.
    runtime.set_visual_hold(true);
    let held_model = runtime.ui_model();
    assert!(!held_model.is_open);
    assert!(
        !held_model.fields.is_empty(),
        "во время visual hold уезжающая панель должна показывать поля"
    );

    // Анимация закончилась: закрытая панель снова не строит field list.
    runtime.set_visual_hold(false);
    let closed_model = runtime.ui_model();
    assert!(!closed_model.is_open);
    assert!(
        closed_model.fields.is_empty(),
        "устойчиво закрытая панель не должна строить поля (perf-инвариант)"
    );
}

/// Snapshot отдаёт длительность анимации sidebar в секундах из committed config.
#[test]
fn committed_snapshot_maps_sidebar_slide_duration_to_seconds() {
    let mut config = custom_config_for_test();
    config.ui.animations.reduced_motion = false;
    config.ui.animations.sidebar_slide_duration_ms = 250;
    let snapshot = CommittedConfigSnapshot::from_config(&config);

    assert!((snapshot.sidebar_slide_duration_seconds() - 0.25).abs() < f32::EPSILON);

    config.ui.animations.sidebar_slide_duration_ms = 0;
    let snapshot = CommittedConfigSnapshot::from_config(&config);
    assert_eq!(snapshot.sidebar_slide_duration_seconds(), 0.0);
}

/// Snapshot отдаёт persisted ширину sidebar без доступа AppState к mutable config.
#[test]
fn committed_snapshot_exposes_sidebar_width_points() {
    let mut config = custom_config_for_test();
    config.ui.sidebar.width_points = 515;

    let snapshot = CommittedConfigSnapshot::from_config(&config);

    assert_eq!(snapshot.sidebar_width_points(), 515);
}

/// Snapshot отдаёт высоту titlebar в egui points из committed config.
#[test]
fn committed_snapshot_maps_titlebar_height_to_points() {
    let mut config = custom_config_for_test();
    let default_snapshot = CommittedConfigSnapshot::from_config(&config);
    assert_eq!(default_snapshot.titlebar_height_points(), 40.0);

    config.ui.window.titlebar_height_px = 64;
    let custom_snapshot = CommittedConfigSnapshot::from_config(&config);
    assert_eq!(custom_snapshot.titlebar_height_points(), 64.0);
}

/// Snapshot активирует только committed радиус, переданный после успешного Apply/OK.
#[test]
fn committed_snapshot_maps_window_corner_radius_to_points() {
    let mut config = custom_config_for_test();
    assert_eq!(
        CommittedConfigSnapshot::from_config(&config).window_corner_radius_points(),
        12.0
    );

    config.ui.window.corner_radius_px = 24;
    assert_eq!(
        CommittedConfigSnapshot::from_config(&config).window_corner_radius_points(),
        24.0
    );
}

/// Draft и Cancel не меняют активный контур, а успешный Apply синхронизирует snapshot.
#[test]
fn window_corner_radius_activates_only_after_successful_apply() {
    let config = custom_config_for_test();
    let path = temp_config_path("window-corner-radius-apply");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime should build");
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter should build");

    runtime
        .handle_ui_actions_with_runtime_adapter(
            vec![
                SettingsUiAction::Open,
                SettingsUiAction::SetValue {
                    setting_id: SettingId::from("ui.window.corner_radius_px"),
                    value: SettingValue::Integer(24),
                },
            ],
            &mut adapter,
        )
        .expect("corner radius draft accepted");
    assert_eq!(
        runtime.committed_snapshot().window_corner_radius_points(),
        12.0
    );

    runtime
        .handle_ui_actions_with_runtime_adapter(vec![SettingsUiAction::Cancel], &mut adapter)
        .expect("Cancel discards corner radius draft");
    assert_eq!(
        runtime.committed_snapshot().window_corner_radius_points(),
        12.0
    );

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.window.corner_radius_px"),
                value: SettingValue::Integer(24),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );
    assert_eq!(
        runtime.committed_snapshot().window_corner_radius_points(),
        24.0
    );
    assert_eq!(
        adapter
            .committed_snapshots
            .last()
            .expect("successful Apply syncs app snapshot")
            .window_corner_radius_points(),
        24.0
    );

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.window.corner_radius_px"),
                value: SettingValue::Integer(0),
            },
            SettingsUiAction::Ok,
        ],
        &mut adapter,
    );
    assert_eq!(
        runtime.committed_snapshot().window_corner_radius_points(),
        0.0
    );
    remove_file_if_exists(&path);
}

#[test]
fn committed_snapshot_updates_hotkey_seek_policy_without_synthetic_event() {
    let mut config = AppConfig::default();
    config.player.seek.hotkey_small_step_secs = 7;
    config.player.seek.hotkey_large_step_secs = 45;

    let snapshot = CommittedConfigSnapshot::from_config(&config);

    assert_eq!(snapshot.hotkey_small_seek_step(), Duration::from_secs(7));
    assert_eq!(snapshot.hotkey_large_seek_step(), Duration::from_secs(45));
}

#[test]
fn local_open_snapshot_uses_current_committed_config() {
    let config = custom_config_for_test();
    let snapshot = CommittedConfigSnapshot::from_config(&config);

    assert!(snapshot.autoplay_for_new_media());
    assert_eq!(
        snapshot
            .demux_config_for_open()
            .max_consecutive_corrupted_packets,
        17
    );
}

#[test]
fn render_initial_settings_are_unchanged() {
    let config = custom_config_for_test();
    let runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");

    let initial_settings = runtime
        .initial_render_settings()
        .expect("render settings должны мапиться как раньше");

    assert_eq!(
        initial_settings.color_pipeline,
        color_pipeline_settings_from_config(&config).expect("old render mapping должен пройти")
    );
    assert_eq!(
        initial_settings.hdr_to_sdr,
        hdr_to_sdr_settings_from_config(&config)
    );
}

#[test]
fn player_default_volume_route_updates_policy_snapshot_only() {
    let config = custom_config_for_test();
    let mut appliers = SettingsRuntimeRouteAppliers::from_config(&config)
        .expect("route appliers должны принять валидированный config");
    let route = RuntimeCommittedRoute {
        route: AppRuntimeRoute::Player,
        source_routes: vec![SettingRouteId::from("audio")],
        affected_settings: vec![SettingId::from("audio.volume")],
        groups: vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::PlayerDefaultVolume,
            affected_settings: vec![SettingId::from("audio.volume")],
        }],
        update: RuntimeCommittedUpdate::Player(Box::new(PlayerCommittedSettingsUpdate {
            player_core: PlayerRuntimeSettingsUpdate::empty()
                .with_default_volume(0.25, [PlayerRuntimeSettingId::AudioDefaultVolume]),
            audio_output_device_id: None,
            event_policy_settings: Vec::new(),
            resume_last_position: None,
            media_pipeline: None,
        })),
    };

    let report = appliers
        .apply_committed_route(route)
        .expect("default volume route должен построить report");

    assert_eq!(appliers.player.default_volume, 0.25);
    assert_eq!(report.result, AppRouteApplyResult::Applied);
    assert_eq!(report.groups[0].result, AppRouteApplyResult::Applied);
}

#[test]
fn selected_available_audio_device_is_passed_to_audio_owner() {
    let config = custom_config_for_test();
    let mut appliers = SettingsRuntimeRouteAppliers::from_config(&config)
        .expect("route appliers должны принять валидированный config");
    let selected_device_id = "cpal-0.15-name:USB%20DAC".to_string();
    let route = RuntimeCommittedRoute {
        route: AppRuntimeRoute::Player,
        source_routes: vec![SettingRouteId::from("audio")],
        affected_settings: vec![SettingId::from("audio.output_device")],
        groups: vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::PlayerAudioOutputDevice,
            affected_settings: vec![SettingId::from("audio.output_device")],
        }],
        update: RuntimeCommittedUpdate::Player(Box::new(PlayerCommittedSettingsUpdate {
            player_core: PlayerRuntimeSettingsUpdate::empty(),
            audio_output_device_id: Some(selected_device_id.clone()),
            event_policy_settings: Vec::new(),
            resume_last_position: None,
            media_pipeline: None,
        })),
    };

    let report = appliers
        .apply_committed_route(route)
        .expect("audio device route должен построить report");

    assert_eq!(
        appliers
            .audio_output_device_controller
            .selected_device_id()
            .expect("audio owner должен вернуть selected id"),
        selected_device_id
    );
    assert_eq!(report.result, AppRouteApplyResult::Applied);
    assert_eq!(report.groups[0].result, AppRouteApplyResult::Applied);
}

#[test]
fn player_decoder_route_uses_live_pipeline_rebuild() {
    let config = custom_config_for_test();
    let mut appliers = SettingsRuntimeRouteAppliers::from_config(&config)
        .expect("route appliers должны принять валидированный config");
    let mut runtime_adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");
    let requested_decoder_config = player_core::PlayerVideoDecoderThreadConfig {
        packet_channel_frames: 64,
        ..player_core::PlayerVideoDecoderThreadConfig::default()
    };
    let route = RuntimeCommittedRoute {
        route: AppRuntimeRoute::Player,
        source_routes: vec![SettingRouteId::from("video")],
        affected_settings: vec![SettingId::from("video.decoder_packet_channel_frames")],
        groups: vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::PlayerDecoderThreadConfig,
            affected_settings: vec![SettingId::from("video.decoder_packet_channel_frames")],
        }],
        update: RuntimeCommittedUpdate::Player(Box::new(PlayerCommittedSettingsUpdate {
            player_core: PlayerRuntimeSettingsUpdate::empty().with_decoder_thread_config(
                requested_decoder_config,
                [PlayerRuntimeSettingId::VideoDecoderPacketChannelFrames],
            ),
            audio_output_device_id: None,
            event_policy_settings: Vec::new(),
            resume_last_position: None,
            media_pipeline: None,
        })),
    };

    let report = appliers
        .apply_committed_route_with_render_adapter(
            route,
            SettingsRouteTargetPolicy::from_config(&config),
            &mut runtime_adapter,
        )
        .expect("decoder route должен построить report");

    assert_eq!(runtime_adapter.player_updates.len(), 1);
    assert_eq!(report.result, AppRouteApplyResult::Applied);
    assert_eq!(report.mechanism, ApplyMechanism::PipelineRebuild);
    assert_eq!(report.groups[0].result, AppRouteApplyResult::Applied);
}

fn render_recreation_route(next: RenderCommittedSettingsUpdate) -> RuntimeCommittedRoute {
    RuntimeCommittedRoute {
        route: AppRuntimeRoute::RenderCommitted,
        source_routes: vec![SettingRouteId::from("render")],
        affected_settings: vec![SettingId::from("render.vulkan.max_frame_latency")],
        groups: vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::RenderBackendLifecycle,
            affected_settings: vec![SettingId::from("render.vulkan.max_frame_latency")],
        }],
        update: RuntimeCommittedUpdate::RenderCommitted(RenderCommittedSettingsUpdate {
            profile: next.profile,
            tone_mapping: next.tone_mapping,
            vulkan: next.vulkan,
            opengles: next.opengles,
        }),
    }
}

fn render_update_from_config(config: &AppConfig) -> RenderCommittedSettingsUpdate {
    RenderCommittedSettingsUpdate {
        profile: config.render.profile,
        tone_mapping: config.render.tone_mapping,
        vulkan: config.render.vulkan.clone(),
        opengles: config.render.opengles.clone(),
    }
}

#[test]
fn render_recreation_commits_snapshot_only_after_owner_success() {
    let config = custom_config_for_test();
    let mut next = render_update_from_config(&config);
    next.vulkan.max_frame_latency += 1;
    let route = render_recreation_route(next.clone());
    let mut appliers = SettingsRuntimeRouteAppliers::from_config(&config)
        .expect("route appliers должны принять валидированный config");
    let mut runtime_adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");

    let first_report = appliers
        .apply_committed_route_with_render_adapter(
            route.clone(),
            SettingsRouteTargetPolicy::from_config(&config),
            &mut runtime_adapter,
        )
        .expect("renderer route должен примениться");
    let second_report = appliers
        .apply_committed_route_with_render_adapter(
            route,
            SettingsRouteTargetPolicy::from_config(&config),
            &mut runtime_adapter,
        )
        .expect("повторный renderer route должен быть noop");

    assert_eq!(first_report.result, AppRouteApplyResult::Applied);
    assert_eq!(first_report.mechanism, ApplyMechanism::RendererRecreate);
    assert_eq!(second_report.result, AppRouteApplyResult::Noop);
    assert_eq!(runtime_adapter.renderer_recreation_updates.len(), 1);
    assert_eq!(runtime_adapter.renderer_recreation_updates[0].1, next);
}

#[test]
fn render_recreation_failure_keeps_old_snapshot_for_same_draft_retry() {
    let config = custom_config_for_test();
    let original = render_update_from_config(&config);
    let mut next = original.clone();
    next.vulkan.max_frame_latency += 1;
    let route = render_recreation_route(next.clone());
    let mut appliers = SettingsRuntimeRouteAppliers::from_config(&config)
        .expect("route appliers должны принять валидированный config");
    let mut runtime_adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");
    runtime_adapter.renderer_recreation_result = AppRouteApplyResult::RendererRecreationFailed {
        failure: SettingsApplyFailure::ApplyFailed {
            owner: SettingStateOwner::RendererLifecycle,
            error: RendererRecreationApplyError {
                kind: RendererRecreationApplyErrorKind::CandidateCreation,
                message: "fake renderer creation failure".into(),
            },
        },
    };

    let failed_report = appliers
        .apply_committed_route_with_render_adapter(
            route.clone(),
            SettingsRouteTargetPolicy::from_config(&config),
            &mut runtime_adapter,
        )
        .expect("renderer failure должен остаться typed report-ом");
    runtime_adapter.renderer_recreation_result = AppRouteApplyResult::Applied;
    let retry_report = appliers
        .apply_committed_route_with_render_adapter(
            route,
            SettingsRouteTargetPolicy::from_config(&config),
            &mut runtime_adapter,
        )
        .expect("тот же renderer draft должен повторно примениться");

    assert!(matches!(
        failed_report.result,
        AppRouteApplyResult::RendererRecreationFailed { .. }
    ));
    assert_eq!(retry_report.result, AppRouteApplyResult::Applied);
    assert_eq!(runtime_adapter.renderer_recreation_updates.len(), 2);
    assert_eq!(runtime_adapter.renderer_recreation_updates[1].0, original);
    assert_eq!(runtime_adapter.renderer_recreation_updates[1].1, next);
}

#[test]
fn media_service_route_uses_live_app_owner() {
    let config = custom_config_for_test();
    let mut appliers = SettingsRuntimeRouteAppliers::from_config(&config)
        .expect("route appliers должны принять валидированный config");
    let mut runtime_adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");
    let mut next_network = config.network.clone();
    next_network.read_ahead_mb += 1;
    let route = RuntimeCommittedRoute {
        route: AppRuntimeRoute::MediaService,
        source_routes: vec![SettingRouteId::from("network")],
        affected_settings: vec![SettingId::from("network.read_ahead_mb")],
        groups: vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::MediaNetwork,
            affected_settings: vec![SettingId::from("network.read_ahead_mb")],
        }],
        update: RuntimeCommittedUpdate::MediaService(MediaServiceRuntimeSettingsUpdate {
            network: next_network,
            web_media: config.web_media.clone(),
            yt_dlp: config.yt_dlp.clone(),
        }),
    };

    let report = appliers
        .apply_committed_route_with_render_adapter(
            route,
            SettingsRouteTargetPolicy::from_config(&config),
            &mut runtime_adapter,
        )
        .expect("media route должен построить report");

    assert_eq!(runtime_adapter.media_updates, 1);
    assert_eq!(report.result, AppRouteApplyResult::Applied);
    assert_eq!(report.mechanism, ApplyMechanism::PipelineRebuild);
    assert_eq!(report.groups[0].result, AppRouteApplyResult::Applied);
}

#[test]
fn media_service_route_keeps_snapshot_when_owner_rebuild_fails() {
    let config = custom_config_for_test();
    let original_snapshot = super::super::MediaServiceRuntimeSnapshot::from_config(&config);
    let mut appliers = SettingsRuntimeRouteAppliers::from_config(&config)
        .expect("route appliers должны принять валидированный config");
    let mut runtime_adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");
    runtime_adapter.fail_media = true;
    let mut next_network = config.network.clone();
    next_network.read_ahead_mb += 1;
    let route = RuntimeCommittedRoute {
        route: AppRuntimeRoute::MediaService,
        source_routes: vec![SettingRouteId::from("network")],
        affected_settings: vec![SettingId::from("network.read_ahead_mb")],
        groups: vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::MediaNetwork,
            affected_settings: vec![SettingId::from("network.read_ahead_mb")],
        }],
        update: RuntimeCommittedUpdate::MediaService(MediaServiceRuntimeSettingsUpdate {
            network: next_network,
            web_media: config.web_media.clone(),
            yt_dlp: config.yt_dlp.clone(),
        }),
    };

    let report = appliers
        .apply_committed_route_with_render_adapter(
            route,
            SettingsRouteTargetPolicy::from_config(&config),
            &mut runtime_adapter,
        )
        .expect("media route должен построить failure report");

    assert_eq!(runtime_adapter.media_updates, 1);
    assert_eq!(
        report.result,
        AppRouteApplyResult::Failed {
            message: "media owner failed".to_string()
        }
    );
    assert_eq!(report.mechanism, ApplyMechanism::PipelineRebuild);
    assert_eq!(report.groups[0].result, report.result);
    assert_eq!(appliers.media_service, original_snapshot);
}

#[test]
fn frame_server_route_applies_player_policy_and_commits_snapshot() {
    let config = custom_config_for_test();
    let mut appliers = SettingsRuntimeRouteAppliers::from_config(&config)
        .expect("route appliers должны принять валидированный config");
    let mut runtime_adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");
    let mut next_frame_server = config.frame_server.clone();
    next_frame_server.live_scrub_max_hz = 120;
    let route = RuntimeCommittedRoute {
        route: AppRuntimeRoute::FrameServer,
        source_routes: vec![SettingRouteId::from("frame_server.apply")],
        affected_settings: vec![SettingId::from("frame_server.live_scrub_max_hz")],
        groups: vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::FrameServerSettings,
            affected_settings: vec![SettingId::from("frame_server.live_scrub_max_hz")],
        }],
        update: RuntimeCommittedUpdate::FrameServer(Box::new(FrameServerRuntimeSettingsUpdate {
            frame_server: next_frame_server.clone(),
            player_core: PlayerRuntimeSettingsUpdate::empty().with_frame_server_policy(
                PlayerWorkerConfig::frame_server_config_from_app_config(&AppConfig {
                    frame_server: next_frame_server.clone(),
                    ..config.clone()
                }),
                [PlayerRuntimeSettingId::FrameServerLiveScrubMaxHz],
            ),
        })),
    };

    let report = appliers
        .apply_committed_route_with_render_adapter(
            route,
            SettingsRouteTargetPolicy::from_config(&config),
            &mut runtime_adapter,
        )
        .expect("frame_server route должен построить report");

    assert_eq!(runtime_adapter.player_updates.len(), 1);
    assert!(
        runtime_adapter.player_updates[0]
            .frame_server_policy
            .is_some()
    );
    assert_eq!(appliers.frame_server, next_frame_server);
    assert_eq!(report.result, AppRouteApplyResult::Applied);
    assert_eq!(report.mechanism, ApplyMechanism::WorkerReconfigure);
    assert_eq!(report.groups[0].result, AppRouteApplyResult::Applied);
}

#[test]
fn frame_server_route_keeps_snapshot_when_player_policy_apply_fails() {
    let config = custom_config_for_test();
    let original_frame_server = config.frame_server.clone();
    let mut appliers = SettingsRuntimeRouteAppliers::from_config(&config)
        .expect("route appliers должны принять валидированный config");
    let mut runtime_adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");
    runtime_adapter.fail_player = true;
    let mut next_frame_server = config.frame_server.clone();
    next_frame_server.live_scrub_max_hz = 120;
    let route = RuntimeCommittedRoute {
        route: AppRuntimeRoute::FrameServer,
        source_routes: vec![SettingRouteId::from("frame_server.apply")],
        affected_settings: vec![SettingId::from("frame_server.live_scrub_max_hz")],
        groups: vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::FrameServerSettings,
            affected_settings: vec![SettingId::from("frame_server.live_scrub_max_hz")],
        }],
        update: RuntimeCommittedUpdate::FrameServer(Box::new(FrameServerRuntimeSettingsUpdate {
            frame_server: next_frame_server.clone(),
            player_core: PlayerRuntimeSettingsUpdate::empty().with_frame_server_policy(
                PlayerWorkerConfig::frame_server_config_from_app_config(&AppConfig {
                    frame_server: next_frame_server,
                    ..config.clone()
                }),
                [PlayerRuntimeSettingId::FrameServerLiveScrubMaxHz],
            ),
        })),
    };

    let report = appliers
        .apply_committed_route_with_render_adapter(
            route,
            SettingsRouteTargetPolicy::from_config(&config),
            &mut runtime_adapter,
        )
        .expect("frame_server route должен построить failure report");

    assert_eq!(runtime_adapter.player_updates.len(), 1);
    assert!(matches!(
        report.result,
        AppRouteApplyResult::Failed { ref message }
            if message.contains("player runtime apply failed")
    ));
    assert_eq!(report.mechanism, ApplyMechanism::WorkerReconfigure);
    assert_eq!(report.groups[0].result, report.result);
    assert_eq!(appliers.frame_server, original_frame_server);
}
