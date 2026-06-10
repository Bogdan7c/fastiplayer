//! Runtime owner пользовательских настроек `app-egui`.
//!
//! Этот модуль не рисует UI. Его задача - держать единственный authoritative
//! `AppConfig` runtime state на уровне shell и выдавать остальным слоям только
//! read-only snapshots или intent-level apply boundary.

use std::path::{Path, PathBuf};

use render_core::{ColorPipelineSettings, HdrToSdrSettings, RenderLiveSettings};
use rustiplayer_config::{
    AppConfig, LoadedConfig, NetworkConfig, PlayerDemuxConfig, RenderProfile, ToneMappingMode,
    UiConfig, VulkanConfig, YoutubeConfig,
};
use rustiplayer_settings::{
    AppConfigStore, AppConfigValidator, AppRouteApplyReport, AppRouteApplyResult,
    AppRouteGroupReport, AppRuntimeRouteApplier, AppRuntimeRouteGroup, AppRuntimeRouteGroupUpdate,
    MediaServiceRuntimeSettingsUpdate, PlayerCommittedSettingsUpdate,
    RenderCommittedSettingsUpdate, RuntimeCommittedRoute, RuntimeCommittedUpdate,
    UiRuntimeSettingsUpdate, app_config_registry, committed_routes_from_update,
    render_live_settings_from_config,
};
use settings_core::{
    ApplyMechanism, ApplyReport, ApplyRouteReport, CommittedApplyRequest, CommittedSettingsApplier,
    PersistReport, PersistRequest, SettingId, SettingsController, SettingsPersister,
    SettingsRegistry, SettingsResult, SettingsValidator, ValidationReport, ValidationRequest,
};

use crate::render_settings::{
    color_pipeline_settings_from_config, hdr_to_sdr_settings_from_config,
};

/// Read-only snapshot committed config-а для слоёв, которые не должны владеть `AppConfig`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CommittedConfigSnapshot {
    /// Полный clone нужен как snapshot, но поле закрыто, чтобы не стать вторым owner-ом.
    config: AppConfig,
}

impl CommittedConfigSnapshot {
    /// Создаёт snapshot из authoritative committed config-а.
    #[must_use]
    pub(crate) fn from_config(config: &AppConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }

    /// Возвращает immutable config view для legacy boundaries, которые пока ждут `&AppConfig`.
    #[must_use]
    pub(crate) fn as_config(&self) -> &AppConfig {
        &self.config
    }

    /// Autoplay policy для нового media open-а.
    #[must_use]
    pub(crate) fn autoplay_for_new_media(&self) -> bool {
        !self.config.player.start_paused
    }

    /// Demux settings, которые local open job должен захватить в момент запуска.
    #[must_use]
    pub(crate) fn demux_config_for_open(&self) -> PlayerDemuxConfig {
        self.config.player.demux
    }

    /// Default volume policy для startup/new media и mute-toggle restore.
    #[must_use]
    pub(crate) fn default_volume_for_new_media(&self) -> f32 {
        self.config.audio.volume as f32
    }

    /// Stable skin id из последнего committed config-а.
    #[must_use]
    pub(crate) fn ui_skin(&self) -> &str {
        &self.config.ui.skin
    }

    /// Visibility flag для telemetry panel из последнего committed config-а.
    #[must_use]
    pub(crate) fn show_telemetry(&self) -> bool {
        self.config.ui.show_telemetry
    }
}

/// Renderer settings, которые применяются один раз при создании runtime renderer-а.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InitialRenderSettings {
    /// Color pipeline snapshot из committed config-а.
    pub(crate) color_pipeline: ColorPipelineSettings,

    /// HDR-to-SDR snapshot из committed config-а.
    pub(crate) hdr_to_sdr: HdrToSdrSettings,
}

/// Единый owner runtime settings state для `app-egui`.
pub(crate) struct SettingsRuntime {
    /// Controller владеет committed/draft/default documents и рабочим registry.
    controller: SettingsController<AppConfig>,

    /// Явный путь нужен shell/status слоям без выковыривания его из store internals.
    config_path: PathBuf,

    /// Отдельный registry view для будущих UI/read-only surfaces.
    registry: SettingsRegistry<AppConfig>,

    /// Atomic TOML store, используемый apply delegate-ом.
    store: AppConfigStore,

    /// Concrete route appliers/status snapshots, принадлежащие app composition layer.
    route_appliers: SettingsRuntimeRouteAppliers,

    /// Последний apply report остаётся в runtime, чтобы будущий UI мог показать статус.
    latest_apply_report: Option<ApplyReport>,
}

impl SettingsRuntime {
    /// Создаёт runtime owner из startup config-а и забирает ownership у bootstrap-а.
    pub(crate) fn from_loaded_config(loaded_config: LoadedConfig) -> SettingsResult<Self> {
        let LoadedConfig { config, path, .. } = loaded_config;
        let controller_registry = app_config_registry()?;
        let registry = app_config_registry()?;
        let controller = SettingsController::new(config, controller_registry);
        let route_appliers = SettingsRuntimeRouteAppliers::from_config(controller.committed())?;
        let store = AppConfigStore::new(path.clone());

        let runtime = Self {
            controller,
            config_path: path,
            registry,
            store,
            route_appliers,
            latest_apply_report: None,
        };

        debug_assert_eq!(runtime.config_path(), runtime.store_path());
        debug_assert!(runtime.registry().descriptors().len() > 0);
        debug_assert!(runtime.latest_apply_report().is_none());

        Ok(runtime)
    }

    /// Возвращает authoritative committed config; вызывающий не получает mutable доступ.
    #[must_use]
    pub(crate) fn committed_config(&self) -> &AppConfig {
        self.controller.committed()
    }

    /// Создаёт controlled snapshot для runtime-слоя, который живёт короче owner-а.
    #[must_use]
    pub(crate) fn committed_snapshot(&self) -> CommittedConfigSnapshot {
        CommittedConfigSnapshot::from_config(self.controller.committed())
    }

    /// Путь TOML config-а, загруженного при startup.
    #[must_use]
    pub(crate) fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Registry view для будущего visual settings UI.
    #[must_use]
    pub(crate) fn registry(&self) -> &SettingsRegistry<AppConfig> {
        &self.registry
    }

    /// Путь, которым владеет concrete store.
    #[must_use]
    pub(crate) fn store_path(&self) -> &Path {
        self.store.path()
    }

    /// Последний report validate/persist/apply pipeline-а.
    #[must_use]
    pub(crate) fn latest_apply_report(&self) -> Option<&ApplyReport> {
        self.latest_apply_report.as_ref()
    }

    /// Возвращает initial render settings без передачи `AppConfig` renderer-у.
    pub(crate) fn initial_render_settings(&self) -> SettingsResult<InitialRenderSettings> {
        Ok(InitialRenderSettings {
            color_pipeline: color_pipeline_settings_from_config(self.committed_config())
                .map_err(|error| settings_core::SettingsError::access_failed(error.to_string()))?,
            hdr_to_sdr: hdr_to_sdr_settings_from_config(self.committed_config()),
        })
    }

    /// Apply path для будущего settings UI: validate -> persist -> route apply.
    #[allow(dead_code)]
    pub(crate) fn apply_draft(&mut self) -> SettingsResult<ApplyReport> {
        let mut delegate = SettingsRuntimeApplyDelegate {
            validator: AppConfigValidator,
            store: &mut self.store,
            route_appliers: &mut self.route_appliers,
        };
        let report = self.controller.apply(&mut delegate)?;
        self.latest_apply_report = Some(report.clone());

        Ok(report)
    }
}

/// Delegate, который позволяет controller-у использовать store/appliers без передачи ownership.
struct SettingsRuntimeApplyDelegate<'runtime> {
    /// Authoritative AppConfig validator из settings binding crate.
    validator: AppConfigValidator,

    /// Store остаётся owned by `SettingsRuntime`.
    store: &'runtime mut AppConfigStore,

    /// Route appliers остаются owned by `SettingsRuntime`.
    route_appliers: &'runtime mut SettingsRuntimeRouteAppliers,
}

impl SettingsValidator<AppConfig> for SettingsRuntimeApplyDelegate<'_> {
    fn validate(
        &mut self,
        request: ValidationRequest<'_, AppConfig>,
    ) -> SettingsResult<ValidationReport> {
        self.validator.validate(request)
    }
}

impl SettingsPersister<AppConfig> for SettingsRuntimeApplyDelegate<'_> {
    fn persist(&mut self, request: PersistRequest<'_, AppConfig>) -> SettingsResult<PersistReport> {
        self.store.persist(request)
    }
}

impl CommittedSettingsApplier<AppConfig> for SettingsRuntimeApplyDelegate<'_> {
    fn apply_committed(
        &mut self,
        request: CommittedApplyRequest<'_, AppConfig>,
    ) -> SettingsResult<Vec<ApplyRouteReport>> {
        let mut reports = Vec::with_capacity(request.route_updates.len());

        for update in request.route_updates {
            let routes = committed_routes_from_update(
                request.previous_committed,
                request.persisted,
                update,
            )?;
            for route in routes {
                reports.push(
                    self.route_appliers
                        .apply_committed_route(route)?
                        .into_core_report(),
                );
            }
        }

        Ok(reports)
    }
}

/// Concrete route appliers/status snapshots for settings runtime.
struct SettingsRuntimeRouteAppliers {
    /// Последний UI runtime snapshot.
    ui: UiConfig,

    /// Последний live render snapshot, известный settings runtime-у.
    render_live: RenderLiveSettings,

    /// Последний committed renderer lifecycle snapshot.
    render_committed: RenderCommittedRuntimeSnapshot,

    /// Последний player settings snapshot, не смешанный с current playback controls.
    player: PlayerRuntimeSnapshot,

    /// Последний media/service policy snapshot.
    media_service: MediaServiceRuntimeSnapshot,
}

impl SettingsRuntimeRouteAppliers {
    /// Инициализирует route snapshots из startup committed config-а.
    fn from_config(config: &AppConfig) -> SettingsResult<Self> {
        Ok(Self {
            ui: config.ui.clone(),
            render_live: render_live_settings_from_config(config)?,
            render_committed: RenderCommittedRuntimeSnapshot::from_config(config),
            player: PlayerRuntimeSnapshot::from_config(config),
            media_service: MediaServiceRuntimeSnapshot::from_config(config),
        })
    }

    /// Единый helper для route report-а с одинаковым результатом по всем groups.
    fn route_report(
        route: RuntimeCommittedRoute,
        result: AppRouteApplyResult,
        mechanism: ApplyMechanism,
    ) -> AppRouteApplyReport {
        AppRouteApplyReport {
            route: route.route,
            source_routes: route.source_routes,
            result: result.clone(),
            mechanism,
            affected_settings: route.affected_settings,
            groups: group_reports(route.groups, result),
        }
    }
}

impl AppRuntimeRouteApplier for SettingsRuntimeRouteAppliers {
    fn apply_committed_route(
        &mut self,
        route: RuntimeCommittedRoute,
    ) -> SettingsResult<AppRouteApplyReport> {
        match route.update.clone() {
            RuntimeCommittedUpdate::Ui(update) => {
                let result = self.apply_ui_update(&update);
                Ok(Self::route_report(route, result, ApplyMechanism::InPlace))
            }
            RuntimeCommittedUpdate::RenderPreview(update) => {
                self.render_live = update.live_settings.settings.clone();
                Ok(Self::route_report(
                    route,
                    AppRouteApplyResult::PreviewPromoted,
                    ApplyMechanism::PreviewPromoted,
                ))
            }
            RuntimeCommittedUpdate::RenderCommitted(update) => {
                let result = self.apply_render_committed_update(&update);
                Ok(Self::route_report(
                    route,
                    result,
                    ApplyMechanism::DeferredTechnicalDebt,
                ))
            }
            RuntimeCommittedUpdate::Player(update) => Ok(self.apply_player_route(route, &update)),
            RuntimeCommittedUpdate::MediaService(update) => {
                let result = self.apply_media_service_update(&update);
                Ok(Self::route_report(route, result, ApplyMechanism::InPlace))
            }
        }
    }
}

impl SettingsRuntimeRouteAppliers {
    /// Применяет UI shell/settings-runtime snapshot.
    fn apply_ui_update(&mut self, update: &UiRuntimeSettingsUpdate) -> AppRouteApplyResult {
        if self.ui == update.ui {
            return AppRouteApplyResult::Noop;
        }

        self.ui = update.ui.clone();
        AppRouteApplyResult::Applied
    }

    /// Фиксирует committed render lifecycle snapshot без пересоздания renderer-а в S09.
    fn apply_render_committed_update(
        &mut self,
        update: &RenderCommittedSettingsUpdate,
    ) -> AppRouteApplyResult {
        let next_snapshot = RenderCommittedRuntimeSnapshot::from_update(update);
        if self.render_committed == next_snapshot {
            return AppRouteApplyResult::Noop;
        }

        self.render_committed = next_snapshot;
        AppRouteApplyResult::DeferredTechnicalDebt {
            message: "Render lifecycle settings сохранены, но controlled renderer rebuild будет добавлен отдельной фазой"
                .to_string(),
        }
    }

    /// Строит player route report, не смешивая default volume policy и current volume.
    fn apply_player_route(
        &mut self,
        route: RuntimeCommittedRoute,
        update: &PlayerCommittedSettingsUpdate,
    ) -> AppRouteApplyReport {
        let default_volume_result = self.apply_player_default_volume(update);
        let deferred_message = player_deferred_message(update);
        let route_result =
            player_route_result(default_volume_result.clone(), deferred_message.as_deref());
        let mechanism = if route_result.is_success() {
            ApplyMechanism::InPlace
        } else {
            ApplyMechanism::DeferredTechnicalDebt
        };
        let groups = route
            .groups
            .into_iter()
            .map(|group| AppRouteGroupReport {
                result: player_group_result(&group.group, &default_volume_result, &route_result),
                group: group.group,
                affected_settings: group.affected_settings,
            })
            .collect();

        AppRouteApplyReport {
            route: route.route,
            source_routes: route.source_routes,
            result: route_result,
            mechanism,
            affected_settings: route.affected_settings,
            groups,
        }
    }

    /// Применяет default-volume policy snapshot без команды `SetVolume` в текущий playback.
    fn apply_player_default_volume(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
    ) -> AppRouteApplyResult {
        let Some(default_volume_update) = &update.player_core.default_volume else {
            return AppRouteApplyResult::Noop;
        };
        let next_default_volume = default_volume_update.default_volume;
        if (self.player.default_volume - next_default_volume).abs() <= f32::EPSILON {
            return AppRouteApplyResult::Noop;
        }

        self.player.default_volume = next_default_volume;
        AppRouteApplyResult::Applied
    }

    /// Применяет media/service policy snapshot на уровне app settings runtime.
    fn apply_media_service_update(
        &mut self,
        update: &MediaServiceRuntimeSettingsUpdate,
    ) -> AppRouteApplyResult {
        let next_snapshot = MediaServiceRuntimeSnapshot::from_update(update);
        if self.media_service == next_snapshot {
            return AppRouteApplyResult::Noop;
        }

        self.media_service = next_snapshot;
        AppRouteApplyResult::Applied
    }
}

/// Возвращает причину, по которой player route нельзя полностью применить в S09.
fn player_deferred_message(update: &PlayerCommittedSettingsUpdate) -> Option<String> {
    if !update.deferred_boundary_settings.is_empty() {
        return Some(format!(
            "Player settings требуют будущего controlled rebuild: {}",
            setting_ids_text(&update.deferred_boundary_settings)
        ));
    }
    if update.player_core.tick_config.is_some() {
        return Some(
            "Player tick/runtime update построен, но S09 не владеет active PlayerWorker target"
                .to_string(),
        );
    }
    if update.player_core.decoder_thread_config.is_some() {
        return Some(
            "Player decoder-thread settings требуют controlled PlayerWorker rebuild".to_string(),
        );
    }
    if !update.player_core.unsupported_settings.is_empty() {
        return Some(format!(
            "Player settings пока не поддержаны player-core: {}",
            update
                .player_core
                .unsupported_settings
                .iter()
                .map(|setting| format!("{setting:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    None
}

/// Вычисляет route-level result с честным partial status.
fn player_route_result(
    default_volume_result: AppRouteApplyResult,
    deferred_message: Option<&str>,
) -> AppRouteApplyResult {
    match deferred_message {
        Some(message) if default_volume_result.is_success() => {
            AppRouteApplyResult::PartialFailure {
                message: message.to_string(),
            }
        }
        Some(message) => AppRouteApplyResult::DeferredTechnicalDebt {
            message: message.to_string(),
        },
        None => default_volume_result,
    }
}

/// Возвращает group-level player result без слияния разных owner semantics.
fn player_group_result(
    group: &AppRuntimeRouteGroup,
    default_volume_result: &AppRouteApplyResult,
    route_result: &AppRouteApplyResult,
) -> AppRouteApplyResult {
    match group {
        AppRuntimeRouteGroup::PlayerDefaultVolume => default_volume_result.clone(),
        AppRuntimeRouteGroup::PlayerTickConfig => match route_result {
            AppRouteApplyResult::Applied | AppRouteApplyResult::Noop => AppRouteApplyResult::Noop,
            _ => AppRouteApplyResult::DeferredTechnicalDebt {
                message: "Player tick/runtime update требует active PlayerWorker target после S09"
                    .to_string(),
            },
        },
        AppRuntimeRouteGroup::PlayerDecoderThreadConfig
        | AppRuntimeRouteGroup::PlayerDeferredBoundary => match route_result {
            AppRouteApplyResult::Applied | AppRouteApplyResult::Noop => AppRouteApplyResult::Noop,
            _ => route_result.clone(),
        },
        _ => route_result.clone(),
    }
}

/// Snapshot renderer lifecycle настроек, которые нельзя применить как live preview.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderCommittedRuntimeSnapshot {
    /// Renderer profile selection.
    profile: RenderProfile,

    /// Legacy tone mapping config.
    tone_mapping: ToneMappingMode,

    /// Vulkan committed settings.
    vulkan: VulkanConfig,

    /// OpenGL ES committed settings.
    opengles: rustiplayer_config::OpenGlesConfig,
}

impl RenderCommittedRuntimeSnapshot {
    /// Создаёт snapshot из full config-а.
    fn from_config(config: &AppConfig) -> Self {
        Self {
            profile: config.render.profile,
            tone_mapping: config.render.tone_mapping,
            vulkan: config.render.vulkan.clone(),
            opengles: config.render.opengles.clone(),
        }
    }

    /// Создаёт snapshot из committed route payload-а.
    fn from_update(update: &RenderCommittedSettingsUpdate) -> Self {
        Self {
            profile: update.profile,
            tone_mapping: update.tone_mapping,
            vulkan: update.vulkan.clone(),
            opengles: update.opengles.clone(),
        }
    }
}

/// Snapshot player policy настроек, отделённый от current playback controls.
#[derive(Debug, Clone, PartialEq)]
struct PlayerRuntimeSnapshot {
    /// Default volume policy для будущих media.
    default_volume: f32,
}

impl PlayerRuntimeSnapshot {
    /// Создаёт snapshot из full config-а.
    fn from_config(config: &AppConfig) -> Self {
        Self {
            default_volume: config.audio.volume as f32,
        }
    }
}

/// Snapshot media/service settings без владения конкретными network jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MediaServiceRuntimeSnapshot {
    /// Network/source cache policy.
    network: NetworkConfig,

    /// YouTube service policy.
    youtube: YoutubeConfig,
}

impl MediaServiceRuntimeSnapshot {
    /// Создаёт snapshot из full config-а.
    fn from_config(config: &AppConfig) -> Self {
        Self {
            network: config.network.clone(),
            youtube: config.youtube.clone(),
        }
    }

    /// Создаёт snapshot из committed route payload-а.
    fn from_update(update: &MediaServiceRuntimeSettingsUpdate) -> Self {
        Self {
            network: update.network.clone(),
            youtube: update.youtube.clone(),
        }
    }
}

/// Строит per-group reports, не теряя affected setting ids.
fn group_reports(
    groups: Vec<AppRuntimeRouteGroupUpdate>,
    result: AppRouteApplyResult,
) -> Vec<AppRouteGroupReport> {
    groups
        .into_iter()
        .map(|group| AppRouteGroupReport {
            group: group.group,
            result: group_result(&group.group, &result),
            affected_settings: group.affected_settings,
        })
        .collect()
}

/// Корректирует route-level result для no-op/deferred player groups.
fn group_result(
    group: &AppRuntimeRouteGroup,
    route_result: &AppRouteApplyResult,
) -> AppRouteApplyResult {
    match (group, route_result) {
        (
            AppRuntimeRouteGroup::PlayerDefaultVolume,
            AppRouteApplyResult::DeferredTechnicalDebt { .. },
        ) => AppRouteApplyResult::DeferredTechnicalDebt {
            message:
                "Default volume policy сохранён как committed setting; current volume не меняется"
                    .to_string(),
        },
        _ => route_result.clone(),
    }
}

/// Форматирует setting ids для report-а без потери конкретики.
fn setting_ids_text(setting_ids: &[SettingId]) -> String {
    setting_ids
        .iter()
        .map(SettingId::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use player_core::{PlayerRuntimeSettingId, PlayerRuntimeSettingsUpdate};
    use rustiplayer_config::{AppConfig, LoadedConfig};
    use rustiplayer_settings::{
        AppRouteApplyResult, AppRuntimeRoute, AppRuntimeRouteApplier, AppRuntimeRouteGroup,
        AppRuntimeRouteGroupUpdate, PlayerCommittedSettingsUpdate, RuntimeCommittedRoute,
        RuntimeCommittedUpdate,
    };
    use settings_core::{SettingId, SettingRouteId};

    use super::{CommittedConfigSnapshot, SettingsRuntime, SettingsRuntimeRouteAppliers};
    use crate::render_settings::{
        color_pipeline_settings_from_config, hdr_to_sdr_settings_from_config,
    };

    fn loaded_config_for_test(config: AppConfig) -> LoadedConfig {
        LoadedConfig {
            config,
            path: PathBuf::from("/tmp/rustiplayer-settings-runtime-test.toml"),
            created: false,
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
    fn app_shell_and_app_state_do_not_keep_duplicate_mutable_app_config_owner() {
        let app_shell_source = include_str!("app_shell/mod.rs");
        let app_state_source = include_str!("state.rs");

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
}
