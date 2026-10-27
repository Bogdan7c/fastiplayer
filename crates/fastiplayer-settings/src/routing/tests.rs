//! Тесты маршрутов настроек (вынесены из `routing.rs` без изменений, сессия 12a: legacy-файл
//! не должен расти).
/// Маршруты от умолчаний к `current` с автоматическим пулом-заглушкой: число пула эти
/// тесты не проверяют (его проверяет `surface_pool_automatic`).
fn test_route_plan(
    registry: &SettingsRegistry<AppConfig>,
    current: &AppConfig,
    diff: &SettingsDiff,
) -> SettingsResult<RuntimeRoutePlan> {
    super::runtime_route_plan_from_diff(
        registry,
        &AppConfig::default(),
        current,
        diff,
        TEST_AUTOMATIC_SURFACE_POOL,
    )
}

use fastiplayer_config::RenderProfile;
use std::cell::RefCell;
use std::fs;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use render_core::{ColorPipelineSettings, HdrToSdrSettings, RenderLiveSettingId, ToneMappingMode};
use settings_core::{
    ApplyFinalState, ApplyRouteResult, PersistRequest, SettingValue, SettingsController,
};

use super::*;
mod subtitles_route;
mod surface_pool_automatic;
mod vod_endpoint_recovery;
#[test]
fn validation_failure_blocks_persist_and_runtime_apply() {
    let registry = app_config_registry().expect("registry builds");
    let mut controller = SettingsController::new(AppConfig::default(), registry);
    controller
        .set_value(
            SettingId::from("render.hdr_to_sdr.hdr_reference_peak_nits"),
            SettingValue::Float(90.0),
        )
        .expect("descriptor-level value is valid");

    let state = Rc::new(RefCell::new(CallState::default()));
    let mut delegate = RecordingDelegate::new(state.clone(), temp_config_path());

    let report = controller
        .apply(&mut delegate)
        .expect("apply returns report");

    assert_eq!(report.final_state, ApplyFinalState::ValidationFailed);
    assert_eq!(state.borrow().calls, vec!["validate"]);
    assert!(!delegate.path().exists());
}

#[test]
fn controller_persists_only_after_runtime_commit() {
    let registry = app_config_registry().expect("registry builds");
    let mut controller = SettingsController::new(AppConfig::default(), registry);
    controller
        .set_value(
            SettingId::from("ui.language"),
            SettingValue::Text("en".into()),
        )
        .expect("draft edit succeeds");

    let state = Rc::new(RefCell::new(CallState::default()));
    let path = temp_config_path();
    let mut delegate = RecordingDelegate::new(state.clone(), path.clone());

    let report = controller.apply(&mut delegate).expect("apply succeeds");

    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    assert_eq!(
        state.borrow().calls,
        vec!["validate", "preflight", "apply", "persist", "finalize"]
    );
    assert!(!state.borrow().persisted_file_seen_by_apply);

    let saved = fs::read_to_string(&path).expect("saved config exists");
    assert!(saved.contains("language = \"en\""));
    remove_file_if_exists(&path);
}

#[test]
fn render_live_mapping_matches_existing_app_egui_behavior() {
    let mut config = AppConfig::default();
    config.render.color_adjustment.brightness = 0.1;
    config.render.color_adjustment.contrast = 1.2;
    config.render.color_adjustment.saturation = 0.9;
    config.render.color_adjustment.exposure = -0.25;
    config.render.color_adjustment.rgb_gain = vec![1.0, 0.95, 1.05];
    config.render.color_adjustment.rgb_offset = vec![0.01, -0.02, 0.03];
    config.render.hdr_to_sdr.enabled = true;
    config.render.hdr_to_sdr.sdr_reference_white_nits = 120.0;
    config.render.hdr_to_sdr.hdr_reference_peak_nits = 1_200.0;

    let mapped = render_live_settings_from_config(&config).expect("render mapping succeeds");

    assert_eq!(
        mapped.color_pipeline,
        ColorPipelineSettings {
            adjustment: ColorAdjustment {
                brightness: 0.1,
                contrast: 1.2,
                saturation: 0.9,
                exposure: -0.25,
                rgb_gain: [1.0, 0.95, 1.05],
                rgb_offset: [0.01, -0.02, 0.03],
            },
            tone_mapping: ToneMappingMode::Off,
            swapchain_transfer: SwapchainTransferMode::PreserveCurrentUnorm,
        }
    );
    assert_eq!(
        mapped.hdr_to_sdr,
        HdrToSdrSettings {
            enabled: true,
            operator: HdrToneMappingOperator::Bt2446C,
            output_mode: HdrOutputMode::SdrBt709Only,
            sdr_reference_white_nits: 120.0,
            hdr_reference_peak_nits: 1_200.0,
        }
    );
}

#[test]
fn route_diff_groups_affected_settings_by_typed_owner_routes() {
    let registry = app_config_registry().expect("registry builds");
    let mut current = AppConfig::default();
    current.ui.language = "en".into();
    current.render.color_adjustment.brightness = 0.2;
    current.render.profile = RenderProfile::OpenGles;
    current.video.scheduler.demux_packets_per_tick += 1;
    current.network.read_ahead_mb += 1;
    current.yt_dlp.enabled = !current.yt_dlp.enabled;
    current.playlist.next_item_preload_enabled = false;
    current.playlist.next_item_preload_budget_mb = 80;
    current.playlist.next_item_preload_lead_time_ms = 35_000;
    current.playlist.next_item_preload_max_hold_ms = 130_000;

    let diff = registry
        .diff(&AppConfig::default(), &current)
        .expect("diff succeeds");
    let plan = test_route_plan(&registry, &current, &diff).expect("route plan builds");

    assert_eq!(plan.preview_routes.len(), 1);
    assert_eq!(plan.preview_routes[0].route, AppRuntimeRoute::RenderPreview);
    assert_eq!(
        plan.preview_routes[0].affected_settings,
        vec![SettingId::from("render.color_adjustment.brightness")]
    );

    let committed_routes: Vec<AppRuntimeRoute> = plan
        .committed_routes
        .iter()
        .map(|route| route.route)
        .collect();
    assert_eq!(
        committed_routes,
        vec![
            AppRuntimeRoute::Ui,
            AppRuntimeRoute::RenderPreview,
            AppRuntimeRoute::RenderCommitted,
            AppRuntimeRoute::Player,
            AppRuntimeRoute::MediaService,
            AppRuntimeRoute::Playlist,
        ]
    );
    let route = |expected| {
        plan.committed_routes
            .iter()
            .find(|route| route.route == expected)
            .expect("typed route exists")
    };

    let player_route = route(AppRuntimeRoute::Player);
    assert_eq!(
        player_route.groups,
        vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::PlayerTickConfig,
            affected_settings: vec![SettingId::from("video.scheduler.demux_packets_per_tick")],
        }]
    );

    let media_route = route(AppRuntimeRoute::MediaService);
    assert_eq!(
        media_route.groups,
        vec![
            AppRuntimeRouteGroupUpdate {
                group: AppRuntimeRouteGroup::MediaNetwork,
                affected_settings: vec![SettingId::from("network.read_ahead_mb")],
            },
            AppRuntimeRouteGroupUpdate {
                group: AppRuntimeRouteGroup::MediaYtDlp,
                affected_settings: vec![SettingId::from("yt_dlp.enabled")],
            },
        ]
    );

    let playlist_route = route(AppRuntimeRoute::Playlist);
    // Четыре preload knobs обязаны идти одним payload к process-lifetime owner-у.
    // Так persistence и rollback не могут разделить атомарную speculative policy.
    assert_eq!(
        playlist_route.groups,
        vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::PlaylistPolicy,
            affected_settings: vec![
                SettingId::from("playlist.next_item_preload_enabled"),
                SettingId::from("playlist.next_item_preload_budget_mb"),
                SettingId::from("playlist.next_item_preload_lead_time_ms"),
                SettingId::from("playlist.next_item_preload_max_hold_ms"),
            ],
        }]
    );
    assert!(matches!(
        &playlist_route.update,
        RuntimeCommittedUpdate::Playlist(PlaylistRuntimeSettingsUpdate { playlist })
            if *playlist == current.playlist
    ));
}

#[test]
fn frame_server_metadata_uses_committed_route_with_player_policy_payload() {
    let registry = app_config_registry().expect("registry builds");
    let descriptor = registry
        .descriptor(&settings_core::SettingId::from(
            "frame_server.live_scrub_enabled",
        ))
        .expect("frame_server descriptor exists");

    assert_eq!(descriptor.access, settings_core::SettingAccess::ReadWrite);
    assert_eq!(descriptor.route.as_str(), "frame_server.apply");

    let mut current = AppConfig::default();
    current.frame_server.live_scrub_enabled = false;
    let diff = registry
        .diff(&AppConfig::default(), &current)
        .expect("frame_server diff should be readable");
    let plan = test_route_plan(&registry, &current, &diff)
        .expect("frame_server S30B route should be planned");

    assert!(plan.preview_routes.is_empty());
    assert_eq!(plan.committed_routes.len(), 1);
    let route = &plan.committed_routes[0];
    assert_eq!(route.route, AppRuntimeRoute::FrameServer);
    assert_eq!(
        route.source_routes,
        vec![SettingRouteId::from("frame_server.apply")]
    );
    assert_eq!(
        route.groups,
        vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::FrameServerSettings,
            affected_settings: vec![SettingId::from("frame_server.live_scrub_enabled")],
        }]
    );
    let RuntimeCommittedUpdate::FrameServer(update) = &route.update else {
        panic!("frame_server route must carry FrameServerRuntimeSettingsUpdate");
    };
    assert!(!update.frame_server.live_scrub_enabled);
    assert!(update.player_core.frame_server_policy.is_some());
    assert_eq!(
        update
            .player_core
            .frame_server_policy
            .as_ref()
            .expect("frame server route must carry player policy")
            .affected_settings,
        vec![PlayerRuntimeSettingId::FrameServerLiveScrubEnabled]
    );
}

#[test]
fn audio_output_device_route_uses_live_owner_payload() {
    let registry = app_config_registry().expect("registry builds");
    let mut current = AppConfig::default();
    current.audio.output_device = "cpal-0.15-name:USB%20DAC".to_string();

    let diff = registry
        .diff(&AppConfig::default(), &current)
        .expect("diff succeeds");
    let plan = test_route_plan(&registry, &current, &diff).expect("route plan builds");

    let player_route = plan
        .committed_routes
        .iter()
        .find(|route| route.route == AppRuntimeRoute::Player)
        .expect("player route exists");
    let RuntimeCommittedUpdate::Player(update) = &player_route.update else {
        panic!("audio.output_device should be included in the player route update");
    };

    assert_eq!(
        player_route.groups,
        vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::PlayerAudioOutputDevice,
            affected_settings: vec![SettingId::from("audio.output_device")],
        }]
    );
    assert_eq!(
        update.audio_output_device_id.as_deref(),
        Some("cpal-0.15-name:USB%20DAC")
    );
}

#[test]
fn preferred_backend_route_requests_live_pipeline_rebuild() {
    let registry = app_config_registry().expect("registry builds");
    let mut current = AppConfig::default();
    current.video.preferred_backend = fastiplayer_config::VideoBackendPreference::Hardware;

    let diff = registry
        .diff(&AppConfig::default(), &current)
        .expect("diff succeeds");
    let plan = test_route_plan(&registry, &current, &diff).expect("route plan builds");

    let player_route = plan
        .committed_routes
        .iter()
        .find(|route| route.route == AppRuntimeRoute::Player)
        .expect("player route exists");
    let RuntimeCommittedUpdate::Player(update) = &player_route.update else {
        panic!("video.preferred_backend should be included in the player route update");
    };

    assert_eq!(
        player_route.groups,
        vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::PlayerVideoBackend,
            affected_settings: vec![SettingId::from("video.preferred_backend")],
        }]
    );
    assert_eq!(
        update
            .player_core
            .video_backend
            .as_ref()
            .expect("video backend update expected")
            .affected_settings,
        vec![PlayerRuntimeSettingId::VideoPreferredBackend]
    );
}

#[test]
fn session_08b_player_policy_and_media_pipeline_settings_have_live_payloads() {
    let mut config = AppConfig::default();
    config.player.start_paused = !config.player.start_paused;
    config.player.demux.max_consecutive_corrupted_packets += 1;
    config.player.preferred_video_codec_order.swap(0, 1);
    let affected_settings = vec![
        SettingId::from("player.start_paused"),
        SettingId::from("player.demux.max_consecutive_corrupted_packets"),
        SettingId::from("player.preferred_video_codec_order"),
    ];

    let update =
        player_update_from_settings(&config, &affected_settings, TEST_AUTOMATIC_SURFACE_POOL)
            .expect("checked settings must have live player routes");

    assert_eq!(
        update.event_policy_settings,
        vec![SettingId::from("player.start_paused")]
    );
    let media_update = update
        .media_pipeline
        .expect("demux/codec policy must request controlled active-media rebuild");
    assert_eq!(media_update.demux, config.player.demux);
    assert_eq!(
        media_update.preferred_video_codec_order,
        config.player.preferred_video_codec_order
    );
    assert_eq!(media_update.affected_settings.len(), 2);
}

#[test]
fn decoder_ready_queue_route_updates_decoder_and_tick_capacity() {
    let registry = app_config_registry().expect("registry builds");
    let mut current = AppConfig::default();
    current.video.decoder_ready_queue_frames = 3;

    let diff = registry
        .diff(&AppConfig::default(), &current)
        .expect("diff succeeds");
    let plan = test_route_plan(&registry, &current, &diff).expect("route plan builds");

    let player_route = plan
        .committed_routes
        .iter()
        .find(|route| route.route == AppRuntimeRoute::Player)
        .expect("player route exists");
    let RuntimeCommittedUpdate::Player(update) = &player_route.update else {
        panic!("Decoder ready queue must be included in the player route update");
    };

    assert_eq!(
        player_route.groups,
        vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::PlayerDecoderThreadConfig,
            affected_settings: vec![SettingId::from("video.decoder_ready_queue_frames")],
        }]
    );
    assert_eq!(
        update
            .player_core
            .decoder_thread_config
            .as_ref()
            .expect("decoder-thread update expected")
            .decoder_thread_config
            .decoder_ready_queue_frames,
        3
    );
    assert_eq!(
        update
            .player_core
            .tick_config
            .as_ref()
            .expect("tick update expected")
            .tick_config
            .decoder_ready_queue_frames,
        3
    );
}

#[test]
fn software_surface_pool_route_updates_decoder_thread_config() {
    let registry = app_config_registry().expect("registry builds");
    let mut current = AppConfig::default();
    // Меняем только software-пул: ожидаем controlled rebuild decoder-thread
    // группы и корректное значение software_frame_pool_frames в update-е.
    current.video.sw_decoder_surface_pool_frames = 6;

    let diff = registry
        .diff(&AppConfig::default(), &current)
        .expect("diff succeeds");
    let plan = test_route_plan(&registry, &current, &diff).expect("route plan builds");

    let player_route = plan
        .committed_routes
        .iter()
        .find(|route| route.route == AppRuntimeRoute::Player)
        .expect("player route exists");
    let RuntimeCommittedUpdate::Player(update) = &player_route.update else {
        panic!("The software surface pool must be included in the player route update");
    };

    // Software-пул применяется через ту же controlled rebuild группу, что и
    // hardware pool/channel настройки, а не через tick config.
    assert_eq!(
        player_route.groups,
        vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::PlayerDecoderThreadConfig,
            affected_settings: vec![SettingId::from("video.sw_decoder_surface_pool_frames")],
        }]
    );
    assert_eq!(
        update
            .player_core
            .decoder_thread_config
            .as_ref()
            .expect("decoder-thread update expected")
            .decoder_thread_config
            .software_frame_pool_frames,
        6
    );
    assert!(update.player_core.tick_config.is_none());
}

#[test]
fn route_reports_preserve_retryable_busy_without_string_collapse() {
    let report = AppRouteApplyReport {
        route: AppRuntimeRoute::MediaService,
        source_routes: vec![SettingRouteId::from(NETWORK_ROUTE_ID)],
        result: AppRouteApplyResult::RuntimeBusy {
            activity: SettingsBoundaryActivity::PipelineLifecycle,
        },
        mechanism: ApplyMechanism::PipelineRebuild,
        affected_settings: vec![SettingId::from("network.read_ahead_mb")],
        groups: vec![AppRouteGroupReport {
            group: AppRuntimeRouteGroup::MediaNetwork,
            result: AppRouteApplyResult::RuntimeBusy {
                activity: SettingsBoundaryActivity::PipelineLifecycle,
            },
            affected_settings: vec![SettingId::from("network.read_ahead_mb")],
        }],
    };

    assert!(!report.result.is_success());
    assert_eq!(
        report.groups[0].affected_settings,
        vec![SettingId::from("network.read_ahead_mb")]
    );

    let core_report = report.into_core_report();
    assert_eq!(core_report.route, SettingRouteId::from(NETWORK_ROUTE_ID));
    assert_eq!(core_report.mechanism, ApplyMechanism::PipelineRebuild);
    assert!(matches!(
        core_report.result,
        ApplyRouteResult::RuntimeBusy { .. }
    ));
    assert_eq!(
        core_report.affected_settings,
        vec![SettingId::from("network.read_ahead_mb")]
    );
}

#[test]
fn media_service_route_groups_without_service_crate_dependencies() {
    let registry = app_config_registry().expect("registry builds");
    let mut current = AppConfig::default();
    current.network.prefetch_chunk_mb += 1;
    current.yt_dlp.resolve_timeout_ms += 100;

    let diff = registry
        .diff(&AppConfig::default(), &current)
        .expect("diff succeeds");
    let plan = test_route_plan(&registry, &current, &diff).expect("route plan builds");
    let media_route = plan
        .committed_routes
        .iter()
        .find(|route| route.route == AppRuntimeRoute::MediaService)
        .expect("media route exists");

    assert_eq!(
        media_route.source_routes,
        vec![
            SettingRouteId::from(NETWORK_ROUTE_ID),
            SettingRouteId::from(YT_DLP_ROUTE_ID),
        ]
    );
    assert!(matches!(
        media_route.update,
        RuntimeCommittedUpdate::MediaService(_)
    ));
}

#[test]
fn render_preview_route_uses_render_core_field_diff() {
    let registry = app_config_registry().expect("registry builds");
    let mut current = AppConfig::default();
    current.render.color_adjustment.brightness = 0.15;
    current.render.hdr_to_sdr.sdr_reference_white_nits = 110.0;

    let diff = registry
        .diff(&AppConfig::default(), &current)
        .expect("diff succeeds");
    let plan = test_route_plan(&registry, &current, &diff).expect("route plan builds");

    let RuntimePreviewUpdate::Render(update) = &plan.preview_routes[0].update;
    assert_eq!(
        update.live_settings.changed_fields,
        vec![
            RenderLiveSettingId::ColorAdjustmentBrightness,
            RenderLiveSettingId::HdrToSdrSdrReferenceWhiteNits,
        ]
    );
}

#[derive(Debug, Default)]
struct CallState {
    calls: Vec<&'static str>,
    persisted_file_seen_by_apply: bool,
}

struct RecordingDelegate {
    state: Rc<RefCell<CallState>>,
    store: AppConfigStore,
}

impl RecordingDelegate {
    fn new(state: Rc<RefCell<CallState>>, path: PathBuf) -> Self {
        Self {
            state,
            store: AppConfigStore::new(path),
        }
    }

    fn path(&self) -> &Path {
        self.store.path()
    }
}

impl SettingsValidator<AppConfig> for RecordingDelegate {
    fn validate(
        &mut self,
        request: ValidationRequest<'_, AppConfig>,
    ) -> SettingsResult<ValidationReport> {
        self.state.borrow_mut().calls.push("validate");
        AppConfigValidator.validate(request)
    }
}

impl SettingsPersister<AppConfig> for RecordingDelegate {
    fn persist(&mut self, request: PersistRequest<'_, AppConfig>) -> SettingsResult<PersistReport> {
        self.state.borrow_mut().calls.push("persist");
        self.store.persist(request)
    }
}

impl CommittedSettingsApplier<AppConfig> for RecordingDelegate {
    fn preflight_committed(
        &mut self,
        _request: CommittedApplyRequest<'_, AppConfig>,
    ) -> SettingsResult<Vec<ApplyRouteReport>> {
        self.state.borrow_mut().calls.push("preflight");
        Ok(Vec::new())
    }

    fn apply_committed(
        &mut self,
        request: CommittedApplyRequest<'_, AppConfig>,
    ) -> SettingsResult<Vec<ApplyRouteReport>> {
        self.state.borrow_mut().calls.push("apply");
        self.state.borrow_mut().persisted_file_seen_by_apply = self.path().exists();

        Ok(request
            .route_updates
            .iter()
            .map(|update| {
                ApplyRouteReport::applied(update.route.clone(), update.affected_settings.clone())
            })
            .collect())
    }

    fn rollback_committed(
        &mut self,
        request: CommittedRollbackRequest<'_, AppConfig>,
    ) -> SettingsResult<Vec<RollbackReport>> {
        self.state.borrow_mut().calls.push("rollback");
        Ok(request
            .route_updates
            .iter()
            .rev()
            .map(|update| {
                RollbackReport::rolled_back(update.route.clone(), update.affected_settings.clone())
            })
            .collect())
    }

    fn finalize_committed(&mut self, _request: CommittedFinalizeRequest<'_, AppConfig>) {
        self.state.borrow_mut().calls.push("finalize");
    }
}

fn temp_config_path() -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "fastiplayer-settings-test-{}-{timestamp}.toml",
        std::process::id()
    ))
}

fn remove_file_if_exists(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) | Err(_) => {}
    }
}
