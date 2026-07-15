//! Runtime owner пользовательских настроек `app-egui`.
//!
//! Этот модуль не рисует UI. Его задача - держать единственный authoritative
//! `AppConfig` runtime state на уровне shell и выдавать остальным слоям только
//! read-only snapshots или intent-level apply boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use player_core::{
    PlayerRuntimeAcceptedChange, PlayerRuntimeApplyError, PlayerRuntimeApplyGroup,
    PlayerRuntimeApplyGroupReport, PlayerRuntimeApplyOutcome, PlayerRuntimeApplyReport,
    PlayerRuntimeApplyResult, PlayerRuntimeBoundaryActivity, PlayerRuntimeSettingsUpdate,
};
use render_core::{
    ColorPipelineSettings, HdrToSdrSettings, RenderLiveApplyOutcome, RenderLiveSettings,
    RenderLiveSettingsAdapter, RenderLiveSettingsErrorKind, RenderLiveSettingsUpdate,
};
#[cfg(test)]
use render_core::{RenderLiveApplyReport, RenderLiveSettingsError};
use rustiplayer_config::{
    AppConfig, FrameServerLiveScrubDecodeModeConfig, LoadedConfig, NetworkConfig,
    PlayerDemuxConfig, RenderProfile, ToneMappingMode, UiConfig, VideoBackendPreference,
    VulkanConfig, YoutubeConfig,
};
use rustiplayer_settings::{
    AppConfigStore, AppConfigValidator, AppRouteApplyReport, AppRouteApplyResult,
    AppRouteGroupReport, AppRuntimeRouteApplier, AppRuntimeRouteGroup, AppRuntimeRouteGroupUpdate,
    FrameServerRuntimeSettingsUpdate, MediaServiceRuntimeSettingsUpdate,
    PlayerCommittedSettingsUpdate, PlaylistRuntimeSettingsUpdate, RENDER_PREVIEW_ROUTE_ID,
    RenderCommittedSettingsUpdate, RuntimeCommittedRoute, RuntimeCommittedUpdate,
    SettingsBoundaryActivity, UiRuntimeSettingsUpdate, app_config_registry,
    committed_routes_for_updates, render_live_settings_from_config,
};
use settings_core::{
    ApplyFinalState, ApplyMechanism, ApplyReport, ApplyRouteReport, ApplyRouteResult, CancelReport,
    CommittedApplyRequest, CommittedFinalizeRequest, CommittedRollbackRequest,
    CommittedSettingsApplier, OptionProviderId, PersistOutcome, PersistReport, PersistRequest,
    PreviewApplyReport, PreviewApplyRequest, PreviewApplyResult, PreviewRollbackRequest,
    PreviewRollbacker, PreviewSettingsApplier, ResetReport, RollbackReport, RollbackResult,
    SelectDescriptor, SettingEditor, SettingGroupId, SettingId, SettingOption,
    SettingOptionCurrentValue, SettingOptionId, SettingOptionProvider, SettingOptions,
    SettingOptionsError, SettingOptionsRequest, SettingOptionsStatus, SettingRouteId, SettingText,
    SettingValue, SettingsController, SettingsPersister, SettingsRegistry, SettingsResult,
    SettingsSurfaceId, SettingsValidator, ValidationReport, ValidationRequest,
};

#[cfg(test)]
use crate::app_wake::AppWakeOwner;
use crate::app_wake::AppWakePort;
use crate::render_settings::{
    color_pipeline_settings_from_config, hdr_to_sdr_settings_from_config,
};
use crate::settings_ui::{SettingsUiAction, SettingsUiField, SettingsUiModel, SettingsUiStatus};

/// Явно запланированный apply между двумя UI frames, чтобы progress успел отрисоваться.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingSettingsApply {
    /// Закрыть settings sidebar только после полного success/no-op.
    close_on_success: bool,
}

mod committed_snapshot;
mod dynamic_options;
mod preview;
mod route_apply;
mod status_text;
#[cfg(test)]
mod tests;
mod transaction;

pub(crate) use committed_snapshot::CommittedConfigSnapshot;
#[allow(unused_imports)]
pub(crate) use committed_snapshot::InitialRenderSettings;
#[cfg(test)]
use dynamic_options::current_option_value;
use dynamic_options::default_option_providers;
pub(crate) use preview::SettingsPreviewTick;
#[cfg(test)]
use route_apply::RenderOnlySettingsRuntimeAdapter;
pub(crate) use route_apply::SettingsRuntimePreflightFailure;
pub(crate) use route_apply::SettingsRuntimeReconfigureHost;
use route_apply::SettingsRuntimeRouteAppliers;
#[cfg(test)]
use route_apply::{MediaServiceRuntimeSnapshot, simulated_player_runtime_report};
use status_text::{
    group_reports, status_from_apply_report, status_from_cancel, status_from_preview_reports,
    status_from_reset,
};

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

    /// Apply, который стартует на следующем frame после показа progress state.
    pending_apply: Option<PendingSettingsApply>,

    /// Открыто ли visual settings window; draft transaction начинается при `Open`.
    settings_window_open: bool,

    /// Держит field list собранным, пока закрытая панель ещё видна (анимация закрытия).
    /// Это view-level hint от shell; источником open-state не является.
    visual_hold: bool,

    /// Field-level errors от lightweight metadata validation до full document apply.
    field_validation_errors: BTreeMap<SettingId, String>,

    /// Последний status snapshot, который visual UI только отображает.
    status: SettingsUiStatus,

    /// Runtime-owned dynamic option providers; visual UI никогда не вызывает их напрямую.
    option_providers: BTreeMap<OptionProviderId, Arc<dyn SettingOptionProvider>>,

    /// Cached provider snapshots, которые можно безопасно читать во время egui rendering.
    option_cache: BTreeMap<OptionProviderId, SettingOptions>,

    /// Wake port process shell-а для каждого нового options refresh mailbox-а.
    dynamic_options_wake_port: AppWakePort,

    /// Последний запущенный refresh, результат которого ещё может обновить cache.
    active_options_refresh: Option<dynamic_options::DynamicOptionsRefreshJob>,

    /// Один cooperative-cancelled replacement сохраняет JoinHandle до reap.
    retired_options_refresh: Option<dynamic_options::DynamicOptionsRefreshJob>,

    /// Capacity-one latest request не создаёт третий параллельный thread.
    pending_latest_options_refresh: Option<dynamic_options::DynamicOptionsRefreshWork>,

    /// Terminal shutdown запрещает admission новых refresh requests.
    dynamic_options_shutdown_started: bool,

    /// Повторный terminal вызов различается с первым успешным завершением.
    dynamic_options_shutdown_completed: bool,

    /// Последняя попытка отправить preview update; pacing берётся из committed config.
    last_preview_sent_at: Option<Instant>,

    /// Memoized visual model; сбрасывается при любой мутации settings state.
    ui_model_cache: Option<SettingsUiModel>,
}

impl SettingsRuntime {
    /// Создаёт runtime owner из startup config-а и забирает ownership у bootstrap-а.
    #[cfg(test)]
    pub(crate) fn from_loaded_config(loaded_config: LoadedConfig) -> SettingsResult<Self> {
        Self::from_loaded_config_with_wake_port(
            loaded_config,
            AppWakePort::disconnected(AppWakeOwner::SettingsDynamicOptions),
        )
    }

    /// Production constructor связывает completion refresh-а с typed winit bridge.
    pub(crate) fn from_loaded_config_with_wake_port(
        loaded_config: LoadedConfig,
        dynamic_options_wake_port: AppWakePort,
    ) -> SettingsResult<Self> {
        let LoadedConfig { config, path, .. } = loaded_config;
        let controller_registry = app_config_registry()?;
        let registry = app_config_registry()?;
        let controller = SettingsController::new(config, controller_registry);
        let route_appliers = SettingsRuntimeRouteAppliers::from_config(controller.committed())?;
        let option_providers =
            default_option_providers(route_appliers.audio_output_device_controller.clone());
        let store = AppConfigStore::new(path.clone());

        let runtime = Self {
            controller,
            config_path: path,
            registry,
            store,
            route_appliers,
            latest_apply_report: None,
            pending_apply: None,
            settings_window_open: false,
            field_validation_errors: BTreeMap::new(),
            status: SettingsUiStatus::default(),
            option_providers,
            option_cache: BTreeMap::new(),
            dynamic_options_wake_port,
            active_options_refresh: None,
            retired_options_refresh: None,
            pending_latest_options_refresh: None,
            dynamic_options_shutdown_started: false,
            dynamic_options_shutdown_completed: false,
            last_preview_sent_at: None,
            ui_model_cache: None,
            visual_hold: false,
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

    /// Возвращает true, когда settings window должно быть показано visual слоем.
    #[must_use]
    pub(crate) const fn is_settings_window_open(&self) -> bool {
        self.settings_window_open
    }

    /// Возвращает shared audio device selection owner для playback factory.
    #[must_use]
    pub(crate) fn audio_output_device_controller(&self) -> audio::AudioOutputDeviceController {
        self.route_appliers.audio_output_device_controller.clone()
    }

    /// Возвращает visual model, пересобирая её только после мутаций settings state.
    #[must_use]
    pub(crate) fn ui_model(&mut self) -> &SettingsUiModel {
        // Hot path: вызывается каждый кадр playback. Model зависит только от
        // внутреннего state runtime-а, поэтому memoization безопасна: все
        // мутирующие методы вызывают invalidate_ui_model().
        if self.ui_model_cache.is_none() {
            let model = self.build_ui_model();
            self.ui_model_cache = Some(model);
        }
        // get_or_insert_with здесь не подходит: build_ui_model берёт &self,
        // а замыкание держало бы &mut self.ui_model_cache (borrow conflict, E0502).
        // Поэтому invariant выражен через is_none()-заполнение выше + expect.
        self.ui_model_cache
            .as_ref()
            .expect("ui_model_cache заполнен выше")
    }

    /// Сбрасывает memoized visual model; обязан вызываться после любой мутации.
    fn invalidate_ui_model(&mut self) {
        self.ui_model_cache = None;
    }

    /// View-level hint от shell: панель закрыта, но ещё видна (анимация закрытия),
    /// поэтому field list должен оставаться собранным до конца анимации.
    /// Hint не является вторым open-state: runtime open-state остаётся authoritative.
    pub(crate) fn set_visual_hold(&mut self, visual_hold: bool) {
        if self.visual_hold != visual_hold {
            self.visual_hold = visual_hold;
            self.invalidate_ui_model();
        }
    }

    /// Собирает visual model из draft document-а без передачи `AppConfig` в UI modules.
    fn build_ui_model(&self) -> SettingsUiModel {
        // При закрытой панели field list никем не читается,
        // поэтому diff/клоны descriptors не выполняем. Исключение — visual hold:
        // панель ещё видна во время анимации закрытия и должна показывать поля.
        if !self.is_settings_window_open() && !self.visual_hold {
            return SettingsUiModel::new(false, Vec::new(), self.pending_apply.is_some())
                .with_status(self.status.clone());
        }
        let is_open = self.is_settings_window_open();
        match self.ui_fields() {
            Ok(fields) => SettingsUiModel::new(is_open, fields, self.pending_apply.is_some())
                .with_status(self.status.clone()),
            Err(error) => SettingsUiModel::new(is_open, Vec::new(), self.pending_apply.is_some())
                .with_status(SettingsUiStatus {
                    summary: Some("Не удалось собрать модель настроек".to_string()),
                    details: vec![error.to_string()],
                }),
        }
    }

    /// Обрабатывает visual actions и возвращает true, если shell должен запросить redraw.
    #[cfg(test)]
    pub(crate) fn handle_ui_actions<A>(
        &mut self,
        actions: Vec<SettingsUiAction>,
        render_adapter: &mut A,
    ) -> SettingsResult<bool>
    where
        A: RenderLiveSettingsAdapter,
    {
        let mut runtime_adapter = RenderOnlySettingsRuntimeAdapter { render_adapter };
        let mut needs_redraw =
            self.handle_ui_actions_with_runtime_adapter(actions, &mut runtime_adapter)?;
        if self.pending_apply.is_some() {
            needs_redraw |=
                self.handle_ui_actions_with_runtime_adapter(Vec::new(), &mut runtime_adapter)?;
        }
        Ok(needs_redraw)
    }

    /// Обрабатывает visual actions с доступом к runtime owners для committed apply.
    pub(crate) fn handle_ui_actions_with_runtime_adapter<A>(
        &mut self,
        actions: Vec<SettingsUiAction>,
        runtime_adapter: &mut A,
    ) -> SettingsResult<bool>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        let mut needs_redraw = false;
        if let Some(pending_apply) = self.pending_apply.take() {
            self.invalidate_ui_model();
            let report = self.apply_draft_with_runtime_adapter(runtime_adapter)?;
            if pending_apply.close_on_success
                && matches!(
                    report.final_state,
                    ApplyFinalState::FullyApplied | ApplyFinalState::NoChanges
                )
            {
                self.settings_window_open = false;
            }
            needs_redraw = true;
        }
        if !actions.is_empty() {
            self.invalidate_ui_model();
        }
        for action in actions {
            if self.pending_apply.is_some() {
                break;
            }
            needs_redraw |= self.handle_ui_action(action, runtime_adapter)?;
        }
        Ok(needs_redraw)
    }

    /// Сохраняет runtime error в status snapshot без изменения draft document-а.
    pub(crate) fn report_runtime_error(
        &mut self,
        summary: impl Into<String>,
        error: impl ToString,
    ) {
        self.invalidate_ui_model();
        self.status = SettingsUiStatus {
            summary: Some(summary.into()),
            details: vec![error.to_string()],
        };
    }

    /// Собирает field list в registry order, отмечая dirty и field validation state.
    fn ui_fields(&self) -> SettingsResult<Vec<SettingsUiField>> {
        let dirty_ids = self
            .controller
            .diff()?
            .changes()
            .iter()
            .map(|change| change.id.clone())
            .collect::<BTreeSet<_>>();
        self.registry()
            .descriptors()
            .map(|descriptor| {
                let draft_value = self
                    .registry()
                    .get_value(self.controller.draft(), &descriptor.id)?;
                let mut field = SettingsUiField::new(descriptor.clone(), draft_value)
                    .dirty(dirty_ids.contains(&descriptor.id));
                if let Some(error) = self.field_validation_errors.get(&descriptor.id) {
                    field = field.with_validation_error(error.clone());
                }
                if let Some(options) = self.cached_options_for_descriptor(descriptor, &field) {
                    field = field.with_options(options);
                }
                Ok(field)
            })
            .collect()
    }

    /// Обрабатывает одно visual action на уровне authoritative runtime owner-а.
    fn handle_ui_action<A>(
        &mut self,
        action: SettingsUiAction,
        runtime_adapter: &mut A,
    ) -> SettingsResult<bool>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        tracing::debug!(target: "rustiplayer::settings_actions", ?action, "settings UI action");
        match action {
            SettingsUiAction::Open => self.open_settings(),
            SettingsUiAction::ToggleOpen => {
                if self.settings_window_open {
                    // Закрытие launcher-ом — тот же intent, что крестик/`Отмена`:
                    // rollback live preview и отброс draft.
                    self.cancel_edit(runtime_adapter)
                } else {
                    self.open_settings()
                }
            }
            SettingsUiAction::Cancel => self.cancel_edit(runtime_adapter),
            SettingsUiAction::SetValue { setting_id, value } => {
                self.set_draft_value(setting_id, value)
            }
            SettingsUiAction::ResetField { setting_id } => self.reset_field(setting_id),
            SettingsUiAction::ResetGroup { section, group } => self.reset_group(section, group),
            SettingsUiAction::ResetSurface { surface } => self.reset_surface(surface),
            SettingsUiAction::ResetAll => self.reset_all(),
            SettingsUiAction::RefreshOptions { provider_id } => {
                self.refresh_dynamic_options(provider_id)?;
                Ok(true)
            }
            SettingsUiAction::Apply => {
                if self.block_apply_when_field_errors_exist() {
                    return Ok(true);
                }
                self.schedule_apply(false);
                Ok(true)
            }
            SettingsUiAction::Ok => {
                if self.block_apply_when_field_errors_exist() {
                    return Ok(true);
                }
                self.schedule_apply(true);
                Ok(true)
            }
        }
    }

    /// Публикует progress status и переносит runtime transaction на следующий frame.
    fn schedule_apply(&mut self, close_on_success: bool) {
        self.pending_apply = Some(PendingSettingsApply { close_on_success });
        self.latest_apply_report = None;
        self.status = SettingsUiStatus {
            summary: Some("Применение настроек…".to_string()),
            details: vec![
                "Проверяем runtime owners; TOML будет записан только после полного commit-а."
                    .to_string(),
            ],
        };
        self.invalidate_ui_model();
    }

    /// Блокирует программный Apply/OK, если field-level validation уже нашла ошибки.
    fn block_apply_when_field_errors_exist(&mut self) -> bool {
        if self.field_validation_errors.is_empty() {
            return false;
        }

        self.status = SettingsUiStatus {
            summary: Some("Сначала исправьте ошибки в полях".to_string()),
            details: self
                .field_validation_errors
                .iter()
                .map(|(setting_id, error)| format!("{setting_id}: {error}"))
                .collect(),
        };
        true
    }

    /// Начинает fresh draft transaction от текущего committed document-а.
    fn begin_edit(&mut self) {
        self.controller.begin_edit();
        self.field_validation_errors.clear();
        self.settings_window_open = true;
        self.status = SettingsUiStatus::default();
        self.last_preview_sent_at = None;
    }

    /// Открывает settings window: fresh draft transaction + фоновый refresh options.
    fn open_settings(&mut self) -> SettingsResult<bool> {
        if !self.settings_window_open {
            self.begin_edit();
        }
        self.refresh_all_dynamic_options()?;
        Ok(true)
    }

    /// Валидирует field через metadata и пишет значение только в draft document.
    fn set_draft_value(
        &mut self,
        setting_id: SettingId,
        value: SettingValue,
    ) -> SettingsResult<bool> {
        let Some(descriptor) = self.registry().descriptor(&setting_id) else {
            self.field_validation_errors.insert(
                setting_id.clone(),
                format!("Неизвестная настройка `{setting_id}`"),
            );
            return Ok(true);
        };

        if let Err(error) = descriptor.validate_value(&value) {
            self.field_validation_errors
                .insert(setting_id.clone(), error.to_string());
            self.status = SettingsUiStatus {
                summary: Some("Поле содержит недопустимое значение".to_string()),
                details: vec![format!("{setting_id}: {error}")],
            };
            return Ok(true);
        }

        self.field_validation_errors.remove(&setting_id);
        let report = self.controller.set_value(setting_id, value)?;
        if report.draft_changed {
            self.status = SettingsUiStatus::default();
        }
        Ok(report.draft_changed || report.preview_queued)
    }

    /// Сбрасывает одно поле к default document-у и оставляет изменение только в draft.
    fn reset_field(&mut self, setting_id: SettingId) -> SettingsResult<bool> {
        self.field_validation_errors.remove(&setting_id);
        let report = self.controller.reset_value(setting_id)?;
        self.status = status_from_reset("Поле сброшено к значению по умолчанию", &report);
        Ok(true)
    }

    /// Сбрасывает группу настроек к default document-у.
    fn reset_group(
        &mut self,
        section: settings_core::SettingSectionId,
        group: SettingGroupId,
    ) -> SettingsResult<bool> {
        let report = self.controller.reset_group(section, group)?;
        self.remove_validation_errors_for(&report.affected_settings);
        self.status = status_from_reset("Группа сброшена к значениям по умолчанию", &report);
        Ok(true)
    }

    /// Сбрасывает настройки одного visual surface к default document-у.
    fn reset_surface(&mut self, surface: SettingsSurfaceId) -> SettingsResult<bool> {
        let report = self.controller.reset_surface(surface)?;
        self.remove_validation_errors_for(&report.affected_settings);
        self.status = status_from_reset("Раздел сброшен к значениям по умолчанию", &report);
        Ok(true)
    }

    /// Сбрасывает весь draft document к default document-у.
    fn reset_all(&mut self) -> SettingsResult<bool> {
        let report = self.controller.reset_all()?;
        self.field_validation_errors.clear();
        self.status = status_from_reset("Все настройки сброшены к значениям по умолчанию", &report);
        Ok(true)
    }

    /// Удаляет field validation errors для settings, которые reset уже переписал.
    fn remove_validation_errors_for(&mut self, setting_ids: &[SettingId]) {
        for setting_id in setting_ids {
            self.field_validation_errors.remove(setting_id);
        }
    }

    /// Возвращает интервал preview pacing из committed config-а, а не из draft.
    fn live_preview_interval(&self) -> Duration {
        let max_hz = self
            .controller
            .committed()
            .ui
            .settings
            .live_preview_max_hz
            .max(1);
        Duration::from_secs_f64(1.0 / f64::from(max_hz))
    }
}
