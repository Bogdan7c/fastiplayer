use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use player_core::{
    PlayerRuntimeApplyResult, PlayerRuntimeSettingId, PlayerRuntimeSettingsUpdate,
    PlayerWorkerConfig,
};
use render_core::{
    RenderLiveApplyPhase, RenderLiveApplyReport, RenderLiveSettingId, RenderLiveSettings,
    RenderLiveSettingsAdapter, RenderLiveSettingsError, RenderLiveSettingsUpdate,
};
use rustiplayer_config::{AppConfig, LoadedConfig, VideoBackendPreference};
use rustiplayer_settings::{
    AppRouteApplyResult, AppRuntimeRoute, AppRuntimeRouteApplier, AppRuntimeRouteGroup,
    AppRuntimeRouteGroupUpdate, FrameServerRuntimeSettingsUpdate,
    MediaServiceRuntimeSettingsUpdate, PlayerCommittedSettingsUpdate,
    PlaylistRuntimeSettingsUpdate, RenderCommittedSettingsUpdate, RendererRecreationApplyError,
    RendererRecreationApplyErrorKind, RuntimeCommittedRoute, RuntimeCommittedUpdate,
    SettingStateOwner, SettingsApplyFailure, SettingsBoundaryActivity,
    render_live_settings_from_config,
};
use settings_core::{
    ApplyFinalState, ApplyMechanism, ApplyRouteResult, OptionProviderId, RollbackResult, SettingId,
    SettingOption, SettingOptionCurrentValue, SettingOptionId, SettingOptions, SettingOptionsError,
    SettingOptionsRequest, SettingOptionsStatus, SettingRouteId, SettingText, SettingValue,
    SettingsError, SettingsResult, SettingsSurfaceId,
};

use super::{
    CommittedConfigSnapshot, SettingsRouteTargetPolicy, SettingsRuntime,
    SettingsRuntimePreflightFailure, SettingsRuntimeReconfigureHost, SettingsRuntimeRouteAppliers,
    current_option_value,
};
use crate::render_settings::{
    color_pipeline_settings_from_config, hdr_to_sdr_settings_from_config,
};
use crate::settings_ui::SettingsUiAction;
use crate::ui::sidebar::{SidebarWidthChange, SidebarWidthPoints};

mod yt_dlp_recovery_apply;

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

/// Выполняет visual action frame и следующий explicit transaction frame в production adapter path.
fn run_runtime_actions(
    runtime: &mut SettingsRuntime,
    actions: Vec<SettingsUiAction>,
    adapter: &mut RecordingRuntimeAdapter,
) {
    runtime
        .handle_ui_actions_with_runtime_adapter(actions, adapter)
        .expect("visual action frame должен обработаться");
    if runtime.pending_apply.is_some() {
        runtime
            .handle_ui_actions_with_runtime_adapter(Vec::new(), adapter)
            .expect("transaction frame должен обработаться");
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

/// Provider, которым focused shutdown tests удерживают refresh threads активными.
struct BlockingOptionProvider {
    provider_id: OptionProviderId,
    started_calls: Arc<AtomicUsize>,
    release: Arc<AtomicBool>,
}

impl settings_core::SettingOptionProvider for BlockingOptionProvider {
    fn provider_id(&self) -> OptionProviderId {
        self.provider_id.clone()
    }

    fn options(
        &self,
        request: SettingOptionsRequest,
    ) -> Result<SettingOptions, SettingOptionsError> {
        self.started_calls.fetch_add(1, Ordering::AcqRel);
        while !self.release.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        Ok(ready_audio_options(request.current_value, Vec::new()))
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
    fail_rollback: bool,
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
            fail_rollback: false,
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
        if self.fail_rollback {
            return Err(RenderLiveSettingsError::fatal(
                RenderLiveApplyPhase::Rollback,
                "test rollback failure",
            ));
        }
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
    player_target_backend_preferences: Vec<VideoBackendPreference>,
    media_updates: usize,
    media_route_updates: Vec<(MediaServiceRuntimeSettingsUpdate, Vec<SettingId>)>,
    media_target_backend_preferences: Vec<VideoBackendPreference>,
    fail_player: bool,
    fail_media: bool,
    renderer_recreation_result: AppRouteApplyResult,
    renderer_recreation_updates:
        Vec<(RenderCommittedSettingsUpdate, RenderCommittedSettingsUpdate)>,
    preflight_failure: Option<(rustiplayer_settings::AppRuntimeRoute, AppRouteApplyResult)>,
    preflight_calls: usize,
    committed_snapshots: Vec<CommittedConfigSnapshot>,
    restored_sidebar_widths: Vec<SidebarWidthPoints>,
    finalize_calls: usize,
    snapshot_synced_after_finalize: Vec<bool>,
    playlist_updates: Vec<PlaylistRuntimeSettingsUpdate>,
    playlist_rollback_calls: usize,
    transaction_events: Vec<SettingsTransactionEvent>,
    expected_persisted_path_at_finalize: Option<PathBuf>,
    persistence_visible_at_finalize: Vec<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTransactionEvent {
    MediaServiceApply,
    PlaylistApply,
    PlaylistRollback,
    Finalize,
    SnapshotSync,
}

impl RecordingRuntimeAdapter {
    fn from_config(config: &AppConfig) -> SettingsResult<Self> {
        Ok(Self {
            render: RecordingRenderAdapter::from_config(config)?,
            player_updates: Vec::new(),
            player_target_backend_preferences: Vec::new(),
            media_updates: 0,
            media_route_updates: Vec::new(),
            media_target_backend_preferences: Vec::new(),
            fail_player: false,
            fail_media: false,
            renderer_recreation_result: AppRouteApplyResult::Applied,
            renderer_recreation_updates: Vec::new(),
            preflight_failure: None,
            preflight_calls: 0,
            committed_snapshots: Vec::new(),
            restored_sidebar_widths: Vec::new(),
            finalize_calls: 0,
            snapshot_synced_after_finalize: Vec::new(),
            playlist_updates: Vec::new(),
            playlist_rollback_calls: 0,
            transaction_events: Vec::new(),
            expected_persisted_path_at_finalize: None,
            persistence_visible_at_finalize: Vec::new(),
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
    fn preflight_settings_transaction(
        &mut self,
        _routes: &[RuntimeCommittedRoute],
    ) -> Result<(), SettingsRuntimePreflightFailure> {
        self.preflight_calls += 1;
        match self.preflight_failure.clone() {
            Some((route, result)) => Err(SettingsRuntimePreflightFailure { route, result }),
            None => Ok(()),
        }
    }

    fn sync_committed_config_snapshot(&mut self, snapshot: CommittedConfigSnapshot) {
        self.transaction_events
            .push(SettingsTransactionEvent::SnapshotSync);
        self.snapshot_synced_after_finalize
            .push(self.finalize_calls > 0);
        self.committed_snapshots.push(snapshot);
    }

    fn restore_sidebar_width(&mut self, width_points: SidebarWidthPoints) {
        self.restored_sidebar_widths.push(width_points);
    }

    fn finalize_settings_transaction(&mut self) {
        self.transaction_events
            .push(SettingsTransactionEvent::Finalize);
        if let Some(path) = &self.expected_persisted_path_at_finalize {
            self.persistence_visible_at_finalize.push(path.is_file());
        }
        self.finalize_calls += 1;
    }

    fn apply_playlist_runtime_settings(
        &mut self,
        update: &PlaylistRuntimeSettingsUpdate,
    ) -> AppRouteApplyResult {
        self.transaction_events
            .push(SettingsTransactionEvent::PlaylistApply);
        self.playlist_updates.push(*update);
        AppRouteApplyResult::Applied
    }

    fn rollback_playlist_runtime_settings(&mut self) -> AppRouteApplyResult {
        self.transaction_events
            .push(SettingsTransactionEvent::PlaylistRollback);
        self.playlist_rollback_calls += 1;
        AppRouteApplyResult::Applied
    }

    fn recreate_renderer(
        &mut self,
        previous: &RenderCommittedSettingsUpdate,
        next: &RenderCommittedSettingsUpdate,
    ) -> AppRouteApplyResult {
        self.renderer_recreation_updates
            .push((previous.clone(), next.clone()));
        self.renderer_recreation_result.clone()
    }

    fn apply_player_runtime_settings(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
        target_policy: SettingsRouteTargetPolicy,
    ) -> PlayerRuntimeApplyResult {
        let target_backend_preference =
            target_policy.video_backend_preference().ok_or_else(|| {
                player_core::PlayerRuntimeApplyError::Fatal(
                    "recording player owner requires exact target policy".to_owned(),
                )
            })?;
        self.player_target_backend_preferences
            .push(target_backend_preference);
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
        update: &MediaServiceRuntimeSettingsUpdate,
        affected_settings: &[SettingId],
        target_policy: SettingsRouteTargetPolicy,
    ) -> AppRouteApplyResult {
        let Some(target_backend_preference) = target_policy.video_backend_preference() else {
            return AppRouteApplyResult::Failed {
                message: "recording media owner requires exact target policy".to_owned(),
            };
        };
        self.media_target_backend_preferences
            .push(target_backend_preference);
        self.transaction_events
            .push(SettingsTransactionEvent::MediaServiceApply);
        self.media_route_updates
            .push((update.clone(), affected_settings.to_vec()));
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
fn dynamic_options_replacement_is_bounded_and_shutdown_retains_timed_out_handles() {
    let config = custom_config_for_test();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
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
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions(vec![SettingsUiAction::Open], &mut render_adapter)
        .expect("open должен запустить первый refresh");
    while started_calls.load(Ordering::Acquire) < 1 {
        std::thread::yield_now();
    }

    for _ in 0..3 {
        runtime
            .handle_ui_actions(
                vec![SettingsUiAction::RefreshOptions {
                    provider_id: audio_output_provider_id(),
                }],
                &mut render_adapter,
            )
            .expect("replacement refresh должен сохранять bounded latest semantics");
    }
    assert!(
        runtime.dynamic_options_owned_thread_count() <= 2,
        "owner хранит не более active+retired handles"
    );

    assert!(matches!(
        runtime.shutdown_dynamic_options_until(crate::process_shutdown::ShutdownDeadline::after(
            Duration::from_millis(1)
        )),
        crate::process_shutdown::ProcessOwnerShutdownOutcome::TimedOut {
            pending_threads: 1 | 2
        }
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
    let persisted = rustiplayer_config::load_from_path(&path)
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
                setting_id: SettingId::from("yt_dlp.preferred_video_height"),
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
            .yt_dlp
            .preferred_video_height
            .map(rustiplayer_config::PreferredVideoHeight::pixels),
        Some(1080)
    );

    let persisted = fs::read_to_string(&path).expect("persisted config readable");
    assert!(persisted.contains("preferred_video_height = 1080"));
    assert!(!persisted.contains("item_video_height_override"));

    let reopened = rustiplayer_config::load_from_path(&path).expect("persisted settings reopen");
    assert_eq!(
        reopened
            .config
            .yt_dlp
            .preferred_video_height
            .map(rustiplayer_config::PreferredVideoHeight::pixels),
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
                setting_id: SettingId::from("yt_dlp.preferred_video_height"),
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

/// Latest-only debounce заменяет старое значение, не переносит deadline для того же
/// округлённого width и будит idle loop ровно к новому quiet-period.
#[test]
fn sidebar_resize_debounce_coalesces_and_persists_latest_width() {
    let config = custom_config_for_test();
    let path = temp_config_path("sidebar-resize-debounce");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime should build");
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter should build");
    let started_at = Instant::now();

    assert!(runtime.record_sidebar_width_change(sidebar_change(450), started_at));
    let first_deadline = runtime
        .next_sidebar_resize_deadline()
        .expect("first resize should schedule deadline");
    assert!(
        !runtime.record_sidebar_width_change(
            sidebar_change(450),
            started_at + Duration::from_millis(100)
        )
    );
    assert_eq!(runtime.next_sidebar_resize_deadline(), Some(first_deadline));

    let replacement_at = started_at + Duration::from_millis(200);
    assert!(runtime.record_sidebar_width_change(sidebar_change(480), replacement_at));
    let replacement_deadline = replacement_at + Duration::from_millis(500);
    assert_eq!(
        runtime.next_sidebar_resize_deadline(),
        Some(replacement_deadline)
    );
    assert_eq!(
        runtime
            .flush_due_sidebar_resize(
                replacement_deadline - Duration::from_millis(1),
                &mut adapter
            )
            .expect("early tick should stay pending"),
        super::SidebarResizeFlushOutcome::NoPending
    );
    assert_eq!(runtime.committed_config().ui.sidebar.width_points, 420);

    assert_eq!(
        runtime
            .flush_due_sidebar_resize(replacement_deadline, &mut adapter)
            .expect("deadline tick should commit"),
        super::SidebarResizeFlushOutcome::Succeeded
    );
    assert_eq!(runtime.next_sidebar_resize_deadline(), None);
    assert_eq!(runtime.committed_config().ui.sidebar.width_points, 480);
    assert_eq!(
        adapter
            .committed_snapshots
            .last()
            .expect("successful resize should sync snapshot")
            .sidebar_width_points(),
        480
    );
    let persisted =
        rustiplayer_config::load_from_path(&path).expect("persisted sidebar config should reload");
    assert_eq!(persisted.config.ui.sidebar.width_points, 480);
    remove_file_if_exists(&path);
}

/// Drag commit сохраняет соседний draft, Apply применяет его без conflict, а более позднее
/// ручное редактирование width выигрывает у pending drag.
#[test]
fn sidebar_resize_order_preserves_draft_and_last_direct_action_wins() {
    let config = custom_config_for_test();
    let path = temp_config_path("sidebar-resize-order");
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
                    setting_id: SettingId::from("ui.language"),
                    value: SettingValue::Text("en".to_owned()),
                },
            ],
            &mut adapter,
        )
        .expect("neighbor UI draft should open");
    runtime.record_sidebar_width_change(sidebar_change(460), Instant::now());
    run_runtime_actions(&mut runtime, vec![SettingsUiAction::Apply], &mut adapter);
    assert_eq!(runtime.committed_config().ui.sidebar.width_points, 460);
    assert_eq!(runtime.committed_config().ui.language, "en");

    runtime.record_sidebar_width_change(sidebar_change(480), Instant::now());
    runtime
        .handle_ui_actions_with_runtime_adapter(
            vec![SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.sidebar.width_points"),
                value: SettingValue::Integer(520),
            }],
            &mut adapter,
        )
        .expect("manual width edit should first flush pending drag");
    run_runtime_actions(&mut runtime, vec![SettingsUiAction::Apply], &mut adapter);
    assert_eq!(runtime.committed_config().ui.sidebar.width_points, 520);

    runtime
        .handle_ui_actions_with_runtime_adapter(
            vec![SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.language"),
                value: SettingValue::Text("ru".to_owned()),
            }],
            &mut adapter,
        )
        .expect("second neighbor draft should be accepted");
    runtime.record_sidebar_width_change(sidebar_change(490), Instant::now());
    assert_eq!(
        runtime
            .flush_pending_sidebar_resize(&mut adapter)
            .expect("lifecycle-style force flush should ignore future deadline"),
        super::SidebarResizeFlushOutcome::Succeeded
    );
    runtime
        .handle_ui_actions_with_runtime_adapter(vec![SettingsUiAction::Cancel], &mut adapter)
        .expect("Cancel should discard only remaining draft");
    assert_eq!(runtime.committed_config().ui.sidebar.width_points, 490);
    assert_eq!(runtime.committed_config().ui.language, "en");

    remove_file_if_exists(&path);
}

/// Persistence failure сохраняет committed/draft, компенсирует runtime route и явно
/// возвращает live host к последней сохранённой ширине.
#[test]
fn sidebar_resize_persistence_failure_rolls_back_live_width_and_preserves_draft() {
    let config = custom_config_for_test();
    let path = temp_config_path("sidebar-resize-persist-failure");
    remove_file_if_exists(&path);
    fs::create_dir_all(&path).expect("target directory creates deterministic rename failure");
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
                    setting_id: SettingId::from("ui.language"),
                    value: SettingValue::Text("en".to_owned()),
                },
            ],
            &mut adapter,
        )
        .expect("neighbor draft should be accepted");
    runtime.record_sidebar_width_change(sidebar_change(500), Instant::now());

    assert_eq!(
        runtime
            .flush_pending_sidebar_resize(&mut adapter)
            .expect("typed persistence failure should stay in apply report"),
        super::SidebarResizeFlushOutcome::Failed
    );
    assert_eq!(
        runtime
            .latest_apply_report()
            .expect("failure report should be retained")
            .final_state,
        ApplyFinalState::PersistFailed
    );
    assert_eq!(runtime.committed_config().ui.sidebar.width_points, 420);
    assert_eq!(runtime.controller.draft().ui.sidebar.width_points, 420);
    assert_eq!(runtime.controller.draft().ui.language, "en");
    assert!(adapter.committed_snapshots.is_empty());
    assert_eq!(
        adapter.restored_sidebar_widths,
        vec![SidebarWidthPoints::from_committed(420)]
    );
    assert!(
        runtime
            .ui_model()
            .status
            .summary
            .as_deref()
            .is_some_and(|summary| summary.contains("сохран"))
    );
    fs::remove_dir_all(&path).expect("test target directory should be removed");
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

fn sidebar_change(width_points: u16) -> SidebarWidthChange {
    SidebarWidthChange {
        width_points: SidebarWidthPoints::from_committed(width_points),
    }
}
