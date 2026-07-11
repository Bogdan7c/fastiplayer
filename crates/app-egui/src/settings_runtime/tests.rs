use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use player_core::{
    PlayerRuntimeApplyResult, PlayerRuntimeSettingId, PlayerRuntimeSettingsUpdate,
    PlayerWorkerConfig,
};
use render_core::{
    RenderLiveApplyPhase, RenderLiveApplyReport, RenderLiveSettingId, RenderLiveSettings,
    RenderLiveSettingsAdapter, RenderLiveSettingsError, RenderLiveSettingsUpdate,
};
use rustiplayer_config::{AppConfig, LoadedConfig};
use rustiplayer_settings::{
    AppRouteApplyResult, AppRuntimeRoute, AppRuntimeRouteApplier, AppRuntimeRouteGroup,
    AppRuntimeRouteGroupUpdate, FrameServerRuntimeSettingsUpdate,
    MediaServiceRuntimeSettingsUpdate, PlayerCommittedSettingsUpdate, RuntimeCommittedRoute,
    RuntimeCommittedUpdate, render_live_settings_from_config,
};
use settings_core::{
    ApplyFinalState, ApplyMechanism, OptionProviderId, SettingId, SettingOption,
    SettingOptionCurrentValue, SettingOptionId, SettingOptions, SettingOptionsError,
    SettingOptionsRequest, SettingOptionsStatus, SettingRouteId, SettingText, SettingValue,
    SettingsResult, SettingsSurfaceId,
};

use super::{
    CommittedConfigSnapshot, SettingsRuntime, SettingsRuntimeReconfigureHost,
    SettingsRuntimeRouteAppliers, current_option_value,
};
use crate::render_settings::{
    color_pipeline_settings_from_config, hdr_to_sdr_settings_from_config,
};
use crate::settings_ui::SettingsUiAction;

fn loaded_config_for_test(config: AppConfig) -> LoadedConfig {
    LoadedConfig {
        config,
        path: PathBuf::from("/tmp/rustiplayer-settings-runtime-test.toml"),
        created: false,
    }
}

fn loaded_config_for_test_at(config: AppConfig, path: PathBuf) -> LoadedConfig {
    LoadedConfig {
        config,
        path,
        created: false,
    }
}

fn temp_config_path(test_name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "rustiplayer-settings-runtime-{test_name}-{}.toml",
        std::process::id()
    ))
}

fn remove_file_if_exists(path: &PathBuf) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => panic!("test config file must be removable: {error}"),
    }
}

fn brightness_action(value: f64) -> SettingsUiAction {
    SettingsUiAction::SetValue {
        setting_id: SettingId::from("render.color_adjustment.brightness"),
        value: SettingValue::Float(value),
    }
}

fn audio_output_provider_id() -> OptionProviderId {
    OptionProviderId::from("audio.output_device")
}

fn audio_output_field(runtime: &mut SettingsRuntime) -> crate::settings_ui::SettingsUiField {
    runtime
        .ui_model()
        .fields
        .iter()
        .find(|field| field.descriptor.id == SettingId::from("audio.output_device"))
        .cloned()
        .expect("audio.output_device field должен быть в visual model")
}

fn setting_text(text: &str) -> SettingText {
    SettingText::new("settings.test.option", text)
}

fn ready_audio_options(
    current_value: Option<SettingValue>,
    extra_options: Vec<SettingOption>,
) -> SettingOptions {
    let mut options = vec![SettingOption::new(
        audio::DEFAULT_AUDIO_OUTPUT_DEVICE_ID,
        setting_text("Системное устройство"),
    )];
    options.extend(extra_options);

    SettingOptions::ready(
        audio_output_provider_id(),
        options.clone(),
        current_option_value(current_value, &options),
    )
}

struct ScriptedOptionProvider {
    provider_id: OptionProviderId,
    responses: Arc<Mutex<Vec<Result<SettingOptions, SettingOptionsError>>>>,
}

impl ScriptedOptionProvider {
    fn new(responses: Vec<Result<SettingOptions, SettingOptionsError>>) -> Self {
        Self {
            provider_id: audio_output_provider_id(),
            responses: Arc::new(Mutex::new(responses)),
        }
    }
}

impl settings_core::SettingOptionProvider for ScriptedOptionProvider {
    fn provider_id(&self) -> OptionProviderId {
        self.provider_id.clone()
    }

    fn options(
        &self,
        request: SettingOptionsRequest,
    ) -> Result<SettingOptions, SettingOptionsError> {
        let mut responses = self
            .responses
            .lock()
            .expect("scripted provider responses mutex не должен ломаться");
        let response = if responses.len() > 1 {
            responses.remove(0)
        } else {
            responses
                .first()
                .cloned()
                .expect("scripted provider должен иметь хотя бы один response")
        };

        response.map(|mut options| {
            options.current = current_option_value(request.current_value, &options.options);
            options
        })
    }
}

fn replace_audio_option_provider(runtime: &mut SettingsRuntime, provider: ScriptedOptionProvider) {
    runtime
        .option_providers
        .insert(audio_output_provider_id(), Arc::new(provider));
}

#[derive(Debug)]
struct RecordingRenderAdapter {
    active: RenderLiveSettings,
    preview_updates: Vec<RenderLiveSettingsUpdate>,
    commits: Vec<RenderLiveSettings>,
    rollbacks: Vec<RenderLiveSettings>,
    fail_commit: bool,
    backpressured_preview_attempts: usize,
}

impl RecordingRenderAdapter {
    fn from_config(config: &AppConfig) -> SettingsResult<Self> {
        Ok(Self {
            active: render_live_settings_from_config(config)?,
            preview_updates: Vec::new(),
            commits: Vec::new(),
            rollbacks: Vec::new(),
            fail_commit: false,
            backpressured_preview_attempts: 0,
        })
    }

    fn fail_commit_from_config(config: &AppConfig) -> SettingsResult<Self> {
        let mut adapter = Self::from_config(config)?;
        adapter.fail_commit = true;
        Ok(adapter)
    }

    fn backpressured_once_from_config(config: &AppConfig) -> SettingsResult<Self> {
        let mut adapter = Self::from_config(config)?;
        adapter.backpressured_preview_attempts = 1;
        Ok(adapter)
    }
}

impl RenderLiveSettingsAdapter for RecordingRenderAdapter {
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        if self.backpressured_preview_attempts > 0 {
            self.backpressured_preview_attempts -= 1;
            return Err(RenderLiveSettingsError::absent_resource(
                RenderLiveApplyPhase::Preview,
                "test renderer busy",
            ));
        }
        self.preview_updates.push(update.clone());
        self.active = update.settings.clone();
        Ok(RenderLiveApplyReport::applied(
            RenderLiveApplyPhase::Preview,
            update.changed_fields.clone(),
        ))
    }

    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        if self.fail_commit {
            return Err(RenderLiveSettingsError::fatal(
                RenderLiveApplyPhase::Commit,
                "test commit failure",
            ));
        }
        let changed_fields = self.active.changed_fields_from(settings);
        self.commits.push(settings.clone());
        self.active = settings.clone();
        Ok(RenderLiveApplyReport::applied(
            RenderLiveApplyPhase::Commit,
            changed_fields,
        ))
    }

    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        let changed_fields: Vec<RenderLiveSettingId> = self.active.changed_fields_from(baseline);
        self.rollbacks.push(baseline.clone());
        self.active = baseline.clone();
        Ok(RenderLiveApplyReport::applied(
            RenderLiveApplyPhase::Rollback,
            changed_fields,
        ))
    }
}

struct RecordingRuntimeAdapter {
    render: RecordingRenderAdapter,
    player_updates: Vec<PlayerRuntimeSettingsUpdate>,
    media_updates: usize,
    fail_player: bool,
    fail_media: bool,
}

impl RecordingRuntimeAdapter {
    fn from_config(config: &AppConfig) -> SettingsResult<Self> {
        Ok(Self {
            render: RecordingRenderAdapter::from_config(config)?,
            player_updates: Vec::new(),
            media_updates: 0,
            fail_player: false,
            fail_media: false,
        })
    }
}

impl RenderLiveSettingsAdapter for RecordingRuntimeAdapter {
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.render.preview_live_settings(update)
    }

    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.render.commit_live_settings(settings)
    }

    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.render.rollback_live_settings(baseline)
    }
}

impl SettingsRuntimeReconfigureHost for RecordingRuntimeAdapter {
    fn apply_player_runtime_settings(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
    ) -> PlayerRuntimeApplyResult {
        self.player_updates.push(update.player_core.clone());
        if self.fail_player {
            return Err(player_core::PlayerRuntimeApplyError::Backpressure);
        }
        Ok(super::simulated_player_runtime_report(
            update.player_core.clone(),
        ))
    }

    fn apply_media_service_runtime_settings(
        &mut self,
        _update: &MediaServiceRuntimeSettingsUpdate,
        _affected_settings: &[SettingId],
    ) -> AppRouteApplyResult {
        self.media_updates += 1;
        if self.fail_media {
            return AppRouteApplyResult::Failed {
                message: "media owner failed".to_string(),
            };
        }
        AppRouteApplyResult::Applied
    }
}

fn custom_config_for_test() -> AppConfig {
    let mut config = AppConfig::default();
    config.player.start_paused = false;
    config.player.demux.max_consecutive_corrupted_packets = 17;
    config.audio.volume = 0.42;
    config.ui.show_telemetry = false;
    config.render.color_adjustment.brightness = 0.1;
    config.render.hdr_to_sdr.sdr_reference_white_nits = 180.0;
    config
}

#[test]
fn startup_config_snapshot_parity() {
    let config = custom_config_for_test();
    let runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен принять валидированный startup config");

    assert_eq!(runtime.committed_config(), &config);
    assert_eq!(
        runtime.config_path(),
        PathBuf::from("/tmp/rustiplayer-settings-runtime-test.toml").as_path()
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
    config.ui.animations.sidebar_slide_duration_ms = 250;
    let snapshot = CommittedConfigSnapshot::from_config(&config);

    assert!((snapshot.sidebar_slide_duration_seconds() - 0.25).abs() < f32::EPSILON);

    config.ui.animations.sidebar_slide_duration_ms = 0;
    let snapshot = CommittedConfigSnapshot::from_config(&config);
    assert_eq!(snapshot.sidebar_slide_duration_seconds(), 0.0);
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
            media_pipeline: None,
            deferred_boundary_settings: Vec::new(),
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
            media_pipeline: None,
            deferred_boundary_settings: Vec::new(),
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
fn player_decoder_route_uses_runtime_host_without_deferred_debt() {
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
            media_pipeline: None,
            deferred_boundary_settings: Vec::new(),
        })),
    };

    let report = appliers
        .apply_committed_route_with_render_adapter(route, &mut runtime_adapter)
        .expect("decoder route должен построить report");

    assert_eq!(runtime_adapter.player_updates.len(), 1);
    assert_eq!(report.result, AppRouteApplyResult::Applied);
    assert_eq!(report.mechanism, ApplyMechanism::PipelineRebuild);
    assert_eq!(report.groups[0].result, AppRouteApplyResult::Applied);
}

#[test]
fn media_service_route_uses_app_owner_without_deferred_debt() {
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
            youtube: config.youtube.clone(),
        }),
    };

    let report = appliers
        .apply_committed_route_with_render_adapter(route, &mut runtime_adapter)
        .expect("media route должен построить report");

    assert_eq!(runtime_adapter.media_updates, 1);
    assert_eq!(report.result, AppRouteApplyResult::Applied);
    assert_eq!(report.mechanism, ApplyMechanism::PipelineRebuild);
    assert_eq!(report.groups[0].result, AppRouteApplyResult::Applied);
}

#[test]
fn media_service_route_keeps_snapshot_when_owner_rebuild_fails() {
    let config = custom_config_for_test();
    let original_snapshot = super::MediaServiceRuntimeSnapshot::from_config(&config);
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
            youtube: config.youtube.clone(),
        }),
    };

    let report = appliers
        .apply_committed_route_with_render_adapter(route, &mut runtime_adapter)
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
        .apply_committed_route_with_render_adapter(route, &mut runtime_adapter)
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
        .apply_committed_route_with_render_adapter(route, &mut runtime_adapter)
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

#[test]
fn dynamic_options_preserve_unavailable_current_value() {
    let mut config = custom_config_for_test();
    config.audio.output_device = "cpal-0.15-name:Missing%20DAC".to_string();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
    replace_audio_option_provider(
        &mut runtime,
        ScriptedOptionProvider::new(vec![Ok(ready_audio_options(None, Vec::new()))]),
    );
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Open], &mut render_adapter)
        .expect("open должен запустить refresh provider-а без hard error");
    runtime.wait_for_options_refresh_for_test();

    let field = audio_output_field(&mut runtime);
    let options = field.options.expect("dynamic options должны быть в model");
    let SettingOptionCurrentValue::UnavailableCurrent { id, .. } = options.current else {
        panic!("saved unavailable current должен сохраниться в snapshot");
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
        .expect("settings runtime должен построиться");
    replace_audio_option_provider(
        &mut runtime,
        ScriptedOptionProvider::new(vec![Err(SettingOptionsError::Failed {
            provider_id: audio_output_provider_id(),
            message: "test provider failed".to_string(),
        })]),
    );
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Open], &mut render_adapter)
        .expect("provider error должен стать cached status, а не hard error");
    runtime.wait_for_options_refresh_for_test();

    let model = runtime.ui_model();
    let field = model
        .fields
        .iter()
        .find(|field| field.descriptor.id == SettingId::from("audio.output_device"))
        .cloned()
        .expect("settings window должен продолжать строить fields");
    let options = field.options.expect("error snapshot должен быть в model");

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
        .expect("settings runtime должен построиться");
    runtime.option_providers.remove(&audio_output_provider_id());
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Open], &mut render_adapter)
        .expect("missing provider должен стать cached status");
    runtime.wait_for_options_refresh_for_test();

    let field = audio_output_field(&mut runtime);
    let options = field
        .options
        .expect("missing provider snapshot должен быть");

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
        .expect("settings runtime должен построиться");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    // Закрыто -> toggle открывает (fresh draft transaction).
    runtime
        .handle_ui_actions(vec![SettingsUiAction::ToggleOpen], &mut render_adapter)
        .expect("toggle на закрытой панели должен открыть настройки");
    assert!(runtime.is_settings_window_open());

    // Меняем draft-значение, чтобы проверить discard при toggle-закрытии.
    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.show_telemetry"),
                value: SettingValue::Bool(true),
            }],
            &mut render_adapter,
        )
        .expect("draft change должен пройти");
    assert_ne!(runtime.controller.draft(), runtime.committed_config());

    // Открыто -> toggle закрывает и отбрасывает draft, как `Отмена`.
    runtime
        .handle_ui_actions(vec![SettingsUiAction::ToggleOpen], &mut render_adapter)
        .expect("toggle на открытой панели должен закрыть настройки");
    assert!(!runtime.is_settings_window_open());
    assert_eq!(runtime.controller.draft(), runtime.committed_config());
}

/// Open не выполняет опрос провайдеров на UI-потоке (источник фриза при
/// открытии панели): refresh уходит в фон, результат подбирается poll-ом.
#[test]
fn open_starts_background_options_refresh_and_poll_applies_result() {
    let config = custom_config_for_test();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
    replace_audio_option_provider(
        &mut runtime,
        ScriptedOptionProvider::new(vec![Ok(ready_audio_options(None, Vec::new()))]),
    );
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Open], &mut render_adapter)
        .expect("open должен пройти мгновенно");
    assert!(
        runtime.has_pending_options_refresh(),
        "после Open refresh должен быть фоновым pending job-ом, а не синхронным вызовом"
    );

    // Фоновый поток быстрый, но не мгновенный: poll-им с дедлайном.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !runtime.poll_dynamic_options_refresh() {
        assert!(
            Instant::now() < deadline,
            "фоновый refresh должен завершиться в разумное время"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    assert!(!runtime.has_pending_options_refresh());
    let field = audio_output_field(&mut runtime);
    assert!(
        field.options.is_some(),
        "после poll-а options snapshot должен попасть в model"
    );
}

#[test]
fn manual_refresh_updates_cached_dynamic_options() {
    let config = custom_config_for_test();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
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
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Open], &mut render_adapter)
        .expect("open должен запустить initial refresh");
    runtime.wait_for_options_refresh_for_test();
    assert_eq!(
        audio_output_field(&mut runtime)
            .options
            .expect("initial options должны быть")
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
        .expect("manual refresh должен обновить cache");
    runtime.wait_for_options_refresh_for_test();

    let options = audio_output_field(&mut runtime)
        .options
        .expect("refreshed options должны быть");
    assert!(
        options
            .options
            .iter()
            .any(|option| option.id.as_str() == "cpal-0.15-name:USB%20DAC")
    );
}

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
                // «Сбросить экран» из заголовка ЛЮБОЙ секции шлёт общий surface.
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
    // Документируем текущее поведение: один surface на всю панель, поэтому
    // section header reset сбрасывает настройки ВСЕХ секций, не только своей.
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
                // UI кнопка «Сбросить группу» у ЛЮБОГО из двух визуальных
                // заголовков "profile" шлёт один и тот же group id.
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
    // Документируем текущее поведение: render.profile и render.tone_mapping
    // делят group id "profile", поэтому один reset бьёт по обоим визуальным
    // группам сразу. Дефолты: profile=vulkan, tone_mapping=disabled.
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
        ApplyFinalState::PersistedRuntimeDiverged
    );
    remove_file_if_exists(&success_path);
    remove_file_if_exists(&failure_path);
}

#[test]
fn app_shell_and_app_state_do_not_keep_duplicate_mutable_app_config_owner() {
    let app_shell_source = include_str!("../app_shell/mod.rs");
    let app_state_source = include_str!("../state.rs");

    assert!(
        !app_shell_source.contains("app_config: AppConfig"),
        "AppShell должен владеть SettingsRuntime, а не отдельным AppConfig"
    );
    assert!(
        !app_state_source.contains("app_config: AppConfig"),
        "AppState должен хранить controlled snapshot, а не второй AppConfig owner"
    );
    assert!(
        !app_state_source.contains("pub app_config"),
        "AppState не должен открывать mutable config storage наружу"
    );
}
