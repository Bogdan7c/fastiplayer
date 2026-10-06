use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use fastiplayer_config::{
    AppConfig, ConfigLoadOrigin, ConfigSavePolicy, LoadedConfig, VideoBackendPreference,
};
use fastiplayer_settings::{
    AppRouteApplyResult, AppRuntimeRoute, AppRuntimeRouteApplier, AppRuntimeRouteGroup,
    AppRuntimeRouteGroupUpdate, FrameServerRuntimeSettingsUpdate,
    MediaServiceRuntimeSettingsUpdate, PlayerCommittedSettingsUpdate,
    PlaylistRuntimeSettingsUpdate, RenderCommittedSettingsUpdate, RendererRecreationApplyError,
    RendererRecreationApplyErrorKind, RuntimeCommittedRoute, RuntimeCommittedUpdate,
    SettingStateOwner, SettingsApplyFailure, SettingsBoundaryActivity,
    render_live_settings_from_config,
};
use player_core::{
    PlayerRuntimeApplyResult, PlayerRuntimeSettingId, PlayerRuntimeSettingsUpdate,
    PlayerWorkerConfig,
};
use render_core::{
    RenderLiveApplyPhase, RenderLiveApplyReport, RenderLiveSettingId, RenderLiveSettings,
    RenderLiveSettingsAdapter, RenderLiveSettingsError, RenderLiveSettingsUpdate,
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

mod dynamic_options;
mod preview;
mod session_only_persistence;
mod sidebar_resize;
mod snapshot_routes;
mod transaction_apply;
mod user_audio_level;
mod web_media_recovery_apply;

fn loaded_config_for_test(config: AppConfig) -> LoadedConfig {
    LoadedConfig {
        config,
        path: PathBuf::from("/tmp/fastiplayer-settings-runtime-test.toml"),
        origin: ConfigLoadOrigin::LoadedExisting,
        save_policy: ConfigSavePolicy::WriteToFile,
    }
}

fn loaded_config_for_test_at(config: AppConfig, path: PathBuf) -> LoadedConfig {
    LoadedConfig {
        config,
        path,
        origin: ConfigLoadOrigin::LoadedExisting,
        save_policy: ConfigSavePolicy::WriteToFile,
    }
}

fn temp_config_path(test_name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "fastiplayer-settings-runtime-{test_name}-{}.toml",
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
    preflight_failure: Option<(fastiplayer_settings::AppRuntimeRoute, AppRouteApplyResult)>,
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
    /// Сколько раз runtime сообщил «применено, но не записано» (сессия 05).
    session_only_reports: usize,
    /// Уровни громкости, отправленные в текущее воспроизведение (UX-11).
    delivered_audio_levels: Vec<crate::user_audio_level::UserAudioLevel>,
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
            session_only_reports: 0,
            delivered_audio_levels: Vec::new(),
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

    fn report_settings_kept_for_session_only(&mut self) {
        self.session_only_reports += 1;
    }

    fn apply_user_audio_level_to_playback(
        &mut self,
        level: crate::user_audio_level::UserAudioLevel,
    ) -> super::PlaybackAudioLevelDelivery {
        self.delivered_audio_levels.push(level);
        super::PlaybackAudioLevelDelivery::Sent
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
