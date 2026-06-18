//! Runtime owner пользовательских настроек `app-egui`.
//!
//! Этот модуль не рисует UI. Его задача - держать единственный authoritative
//! `AppConfig` runtime state на уровне shell и выдавать остальным слоям только
//! read-only snapshots или intent-level apply boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use player_core::{
    PlayerRuntimeAcceptedChange, PlayerRuntimeApplyError, PlayerRuntimeApplyGroup,
    PlayerRuntimeApplyGroupReport, PlayerRuntimeApplyOutcome, PlayerRuntimeApplyReport,
    PlayerRuntimeApplyResult, PlayerRuntimeSettingsUpdate,
};
use render_core::{
    ColorPipelineSettings, HdrToSdrSettings, RenderLiveApplyOutcome, RenderLiveSettings,
    RenderLiveSettingsAdapter, RenderLiveSettingsErrorKind, RenderLiveSettingsUpdate,
};
#[cfg(test)]
use render_core::{RenderLiveApplyReport, RenderLiveSettingsError};
use rustiplayer_config::{
    AppConfig, LoadedConfig, NetworkConfig, PlayerDemuxConfig, RenderProfile, ToneMappingMode,
    UiConfig, VideoBackendPreference, VulkanConfig, YoutubeConfig,
};
use rustiplayer_settings::{
    AppConfigStore, AppConfigValidator, AppRouteApplyReport, AppRouteApplyResult,
    AppRouteGroupReport, AppRuntimeRouteApplier, AppRuntimeRouteGroup, AppRuntimeRouteGroupUpdate,
    MediaServiceRuntimeSettingsUpdate, PlayerCommittedSettingsUpdate, RENDER_PREVIEW_ROUTE_ID,
    RenderCommittedSettingsUpdate, RuntimeCommittedRoute, RuntimeCommittedUpdate,
    UiRuntimeSettingsUpdate, app_config_registry, committed_routes_from_update,
    render_live_settings_from_config,
};
use settings_core::{
    ApplyFinalState, ApplyMechanism, ApplyReport, ApplyRouteReport, ApplyRouteResult, CancelReport,
    CommittedApplyRequest, CommittedSettingsApplier, OptionProviderId, PersistOutcome,
    PersistReport, PersistRequest, PreviewApplyReport, PreviewApplyRequest, PreviewApplyResult,
    PreviewRollbackRequest, PreviewRollbacker, PreviewSettingsApplier, ResetReport, RollbackReport,
    RollbackResult, SelectDescriptor, SettingEditor, SettingGroupId, SettingId, SettingOption,
    SettingOptionCurrentValue, SettingOptionId, SettingOptionProvider, SettingOptions,
    SettingOptionsError, SettingOptionsRequest, SettingOptionsStatus, SettingRouteId, SettingText,
    SettingValue, SettingsController, SettingsPersister, SettingsRegistry, SettingsResult,
    SettingsSurfaceId, SettingsValidator, ValidationReport, ValidationRequest,
};

use crate::render_settings::{
    color_pipeline_settings_from_config, hdr_to_sdr_settings_from_config,
};
use crate::settings_ui::{SettingsUiAction, SettingsUiField, SettingsUiModel, SettingsUiStatus};

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

    /// Committed intent выбора video backend-а для app-owned pipeline selector-а.
    #[must_use]
    pub(crate) fn video_backend_preference(&self) -> VideoBackendPreference {
        self.config.video.preferred_backend
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

    /// Длительность анимации выезда settings sidebar в секундах; `0` — без анимации.
    #[must_use]
    pub(crate) fn sidebar_slide_duration_seconds(&self) -> f32 {
        f32::from(self.config.ui.animations.sidebar_slide_duration_ms) / 1000.0
    }

    /// Высота кастомного titlebar в egui points; config хранит те же логические UI px.
    #[must_use]
    pub(crate) fn titlebar_height_points(&self) -> f32 {
        f32::from(self.config.ui.window.titlebar_height_px)
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

    /// Активный фоновый refresh dynamic options. Опрос провайдеров (например,
    /// CPAL/ALSA устройств) занимает сотни мс и НЕ должен блокировать UI-поток
    /// в кадре открытия панели; результат подбирается poll-ом перед сборкой model.
    pending_options_refresh: Option<mpsc::Receiver<Vec<(OptionProviderId, SettingOptions)>>>,

    /// Последняя попытка отправить preview update; pacing берётся из committed config.
    last_preview_sent_at: Option<Instant>,

    /// Memoized visual model; сбрасывается при любой мутации settings state.
    ui_model_cache: Option<SettingsUiModel>,
}

/// Результат preview tick-а, по которому shell решает, нужен ли timed repaint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SettingsPreviewTick {
    /// Следующий wake-up для retry/coalesced update, если pending preview ещё есть.
    pub(crate) repaint_after: Option<Duration>,
}

/// Boundary для runtime owners, которые живут снаружи settings controller-а.
///
/// Visual settings UI не знает про worker, source jobs или concrete backend.
/// Этот trait передаётся только из app composition/frame слоя, где эти owners
/// реально доступны и где можно сохранить порядок команд.
pub(crate) trait SettingsRuntimeReconfigureHost {
    /// Синхронизирует внешний owner с committed config-ом перед owner-level apply.
    fn sync_committed_config_snapshot(&mut self, _snapshot: CommittedConfigSnapshot) {}

    /// Применяет typed player update через owner-level worker boundary.
    fn apply_player_runtime_settings(
        &mut self,
        update: PlayerRuntimeSettingsUpdate,
    ) -> PlayerRuntimeApplyResult;

    /// Применяет media/source policy и при необходимости запускает controlled rebuild.
    fn apply_media_service_runtime_settings(
        &mut self,
        update: &MediaServiceRuntimeSettingsUpdate,
    ) -> AppRouteApplyResult;
}

/// Fallback host для unit tests и UI-only paths без active `AppState`.
struct NoopSettingsRuntimeReconfigureHost;

impl SettingsRuntimeReconfigureHost for NoopSettingsRuntimeReconfigureHost {
    fn apply_player_runtime_settings(
        &mut self,
        update: PlayerRuntimeSettingsUpdate,
    ) -> PlayerRuntimeApplyResult {
        Ok(simulated_player_runtime_report(update))
    }

    fn apply_media_service_runtime_settings(
        &mut self,
        _update: &MediaServiceRuntimeSettingsUpdate,
    ) -> AppRouteApplyResult {
        AppRouteApplyResult::Applied
    }
}

/// Adapter для старых call sites, которым нужен только render live adapter.
#[cfg(test)]
struct RenderOnlySettingsRuntimeAdapter<'adapter, A> {
    /// Реальный renderer-neutral adapter.
    render_adapter: &'adapter mut A,
}

#[cfg(test)]
impl<A> RenderLiveSettingsAdapter for RenderOnlySettingsRuntimeAdapter<'_, A>
where
    A: RenderLiveSettingsAdapter,
{
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.render_adapter.preview_live_settings(update)
    }

    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.render_adapter.commit_live_settings(settings)
    }

    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.render_adapter.rollback_live_settings(baseline)
    }
}

#[cfg(test)]
impl<A> SettingsRuntimeReconfigureHost for RenderOnlySettingsRuntimeAdapter<'_, A>
where
    A: RenderLiveSettingsAdapter,
{
    fn apply_player_runtime_settings(
        &mut self,
        update: PlayerRuntimeSettingsUpdate,
    ) -> PlayerRuntimeApplyResult {
        let mut host = NoopSettingsRuntimeReconfigureHost;
        host.apply_player_runtime_settings(update)
    }

    fn apply_media_service_runtime_settings(
        &mut self,
        update: &MediaServiceRuntimeSettingsUpdate,
    ) -> AppRouteApplyResult {
        let mut host = NoopSettingsRuntimeReconfigureHost;
        host.apply_media_service_runtime_settings(update)
    }
}

/// Строит успешный report для tests, где нет real worker-а.
fn simulated_player_runtime_report(
    update: PlayerRuntimeSettingsUpdate,
) -> PlayerRuntimeApplyReport {
    let mut report = PlayerRuntimeApplyReport::empty();

    if let Some(tick_update) = update.tick_config {
        report.push(simulated_player_group(
            PlayerRuntimeApplyGroup::TickConfig,
            tick_update.affected_settings,
            "player tick config accepted by test host",
        ));
    }
    if let Some(default_volume_update) = update.default_volume {
        report.push(simulated_player_group(
            PlayerRuntimeApplyGroup::DefaultVolume,
            default_volume_update.affected_settings,
            "default volume accepted by test host",
        ));
    }
    if let Some(decoder_thread_update) = update.decoder_thread_config {
        report.push(simulated_player_group(
            PlayerRuntimeApplyGroup::DecoderThreadConfig,
            decoder_thread_update.affected_settings,
            "decoder thread config accepted by test host",
        ));
    }
    if !update.unsupported_settings.is_empty() {
        report.push(PlayerRuntimeApplyGroupReport::unsupported(
            PlayerRuntimeApplyGroup::UnsupportedSettings,
            update.unsupported_settings,
            "test host preserves unsupported player settings as owner failure",
        ));
    }

    if report.groups.is_empty() {
        report.push(PlayerRuntimeApplyGroupReport::accepted(
            PlayerRuntimeApplyGroup::Request,
            std::iter::empty(),
            PlayerRuntimeAcceptedChange::Unchanged,
            "no player-core settings in update",
        ));
    }

    report
}

/// Создаёт accepted group report для fallback/test host-а.
fn simulated_player_group(
    group: PlayerRuntimeApplyGroup,
    affected_settings: Vec<player_core::PlayerRuntimeSettingId>,
    message: &'static str,
) -> PlayerRuntimeApplyGroupReport {
    PlayerRuntimeApplyGroupReport::accepted(
        group,
        affected_settings,
        PlayerRuntimeAcceptedChange::Applied,
        message,
    )
}

impl SettingsRuntime {
    /// Создаёт runtime owner из startup config-а и забирает ownership у bootstrap-а.
    pub(crate) fn from_loaded_config(loaded_config: LoadedConfig) -> SettingsResult<Self> {
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
            settings_window_open: false,
            field_validation_errors: BTreeMap::new(),
            status: SettingsUiStatus::default(),
            option_providers,
            option_cache: BTreeMap::new(),
            pending_options_refresh: None,
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
            return SettingsUiModel::new(false, Vec::new(), false).with_status(self.status.clone());
        }
        let is_open = self.is_settings_window_open();
        match self.ui_fields() {
            Ok(fields) => {
                SettingsUiModel::new(is_open, fields, false).with_status(self.status.clone())
            }
            Err(error) => {
                SettingsUiModel::new(is_open, Vec::new(), false).with_status(SettingsUiStatus {
                    summary: Some("Не удалось собрать модель настроек".to_string()),
                    details: vec![error.to_string()],
                })
            }
        }
    }

    /// Возвращает initial render settings без передачи `AppConfig` renderer-у.
    pub(crate) fn initial_render_settings(&self) -> SettingsResult<InitialRenderSettings> {
        Ok(InitialRenderSettings {
            color_pipeline: color_pipeline_settings_from_config(self.committed_config())
                .map_err(|error| settings_core::SettingsError::access_failed(error.to_string()))?,
            hdr_to_sdr: hdr_to_sdr_settings_from_config(self.committed_config()),
        })
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
        self.handle_ui_actions_with_runtime_adapter(actions, &mut runtime_adapter)
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
        if !actions.is_empty() {
            self.invalidate_ui_model();
        }
        let mut needs_redraw = false;
        for action in actions {
            needs_redraw |= self.handle_ui_action(action, runtime_adapter)?;
        }
        Ok(needs_redraw)
    }

    /// Отправляет pending live preview update, если pacing разрешает runtime apply сейчас.
    pub(crate) fn apply_due_preview<A>(
        &mut self,
        render_adapter: &mut A,
        now: Instant,
    ) -> SettingsResult<SettingsPreviewTick>
    where
        A: RenderLiveSettingsAdapter,
    {
        let pending_routes = self.controller.preview().pending_routes();
        if pending_routes.is_empty() {
            return Ok(SettingsPreviewTick::default());
        }

        let preview_interval = self.live_preview_interval();
        if let Some(last_sent_at) = self.last_preview_sent_at {
            let elapsed = now.saturating_duration_since(last_sent_at);
            if elapsed < preview_interval {
                return Ok(SettingsPreviewTick {
                    repaint_after: Some(preview_interval - elapsed),
                });
            }
        }

        self.invalidate_ui_model();
        let mut delegate = SettingsRuntimePreviewDelegate {
            route_appliers: &mut self.route_appliers,
            render_adapter,
        };
        let mut reports = Vec::new();
        for route in pending_routes {
            if let Some(report) = self
                .controller
                .apply_pending_preview(&route, &mut delegate)?
            {
                reports.push(report);
            }
        }

        if !reports.is_empty() {
            self.last_preview_sent_at = Some(now);
            self.status = status_from_preview_reports(&reports);
        }

        let repaint_after =
            (!self.controller.preview().pending_routes().is_empty()).then_some(preview_interval);
        Ok(SettingsPreviewTick { repaint_after })
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

    /// Apply path для settings UI: validate -> persist -> route apply.
    #[cfg(test)]
    pub(crate) fn apply_draft<A>(&mut self, render_adapter: &mut A) -> SettingsResult<ApplyReport>
    where
        A: RenderLiveSettingsAdapter,
    {
        let mut runtime_adapter = RenderOnlySettingsRuntimeAdapter { render_adapter };
        self.apply_draft_with_runtime_adapter(&mut runtime_adapter)
    }

    /// Apply path для production runtime owners.
    pub(crate) fn apply_draft_with_runtime_adapter<A>(
        &mut self,
        runtime_adapter: &mut A,
    ) -> SettingsResult<ApplyReport>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        self.invalidate_ui_model();
        let mut delegate = SettingsRuntimeApplyDelegate {
            validator: AppConfigValidator,
            store: &mut self.store,
            route_appliers: &mut self.route_appliers,
            runtime_adapter,
        };
        let report = self.controller.apply(&mut delegate)?;
        self.latest_apply_report = Some(report.clone());
        self.status = status_from_apply_report(&report);

        Ok(report)
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

    /// Возвращает cached snapshot или neutral loading snapshot для dynamic select-а.
    fn cached_options_for_descriptor(
        &self,
        descriptor: &settings_core::SettingDescriptor,
        field: &SettingsUiField,
    ) -> Option<SettingOptions> {
        let SettingEditor::Select(SelectDescriptor::Dynamic { provider_id }) = &descriptor.editor
        else {
            return None;
        };

        self.option_cache.get(provider_id).cloned().or_else(|| {
            Some(options_snapshot_from_status(
                provider_id.clone(),
                SettingOptionsStatus::Loading,
                Vec::new(),
                Some(field.draft_value.clone()),
            ))
        })
    }

    /// Обновляет все dynamic option providers, известные registry.
    fn refresh_all_dynamic_options(&mut self) -> SettingsResult<()> {
        let provider_ids = self.dynamic_option_provider_ids();
        self.start_dynamic_options_refresh(provider_ids)
    }

    /// Запускает фоновый refresh одного dynamic option provider-а.
    fn refresh_dynamic_options(&mut self, provider_id: OptionProviderId) -> SettingsResult<()> {
        self.start_dynamic_options_refresh(vec![provider_id])
    }

    /// Стартует фоновый поток опроса providers, не блокируя UI-поток.
    ///
    /// Requests собираются здесь (дёшево, нужен доступ к draft), а сам опрос
    /// устройств уходит в поток. Повторный старт заменяет pending refresh:
    /// старый receiver выбрасывается, результат устаревшего потока игнорируется.
    fn start_dynamic_options_refresh(
        &mut self,
        provider_ids: Vec<OptionProviderId>,
    ) -> SettingsResult<()> {
        if provider_ids.is_empty() {
            return Ok(());
        }

        let mut refresh_jobs = Vec::with_capacity(provider_ids.len());
        for provider_id in provider_ids {
            let request = self.option_request_for_provider(&provider_id)?;
            let provider = self.option_providers.get(&provider_id).cloned();
            refresh_jobs.push((provider_id, request, provider));
        }

        let (result_sender, result_receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("settings-options-refresh".to_string())
            .spawn(move || {
                let snapshots = refresh_jobs
                    .into_iter()
                    .map(|(provider_id, request, provider)| {
                        let snapshot = collect_provider_snapshot(&provider_id, request, provider);
                        (provider_id, snapshot)
                    })
                    .collect::<Vec<_>>();
                // Получатель мог исчезнуть (новый refresh заменил этот) — не ошибка.
                let _ = result_sender.send(snapshots);
            })
            .map_err(|error| {
                settings_core::SettingsError::access_failed(format!(
                    "не удалось запустить фоновый refresh dynamic options: {error}"
                ))
            })?;

        self.pending_options_refresh = Some(result_receiver);
        Ok(())
    }

    /// `true`, пока фоновый refresh dynamic options ещё не доставил результат.
    /// Shell использует это для пробуждения idle loop под background polling.
    #[must_use]
    pub(crate) fn has_pending_options_refresh(&self) -> bool {
        self.pending_options_refresh.is_some()
    }

    /// Подбирает результат фонового refresh-а, если он готов.
    ///
    /// Возвращает `true`, когда cache обновился и model была инвалидирована.
    /// Вызывается на каждом кадре перед сборкой `ui_model`; `try_recv` дешёвый.
    pub(crate) fn poll_dynamic_options_refresh(&mut self) -> bool {
        let Some(result_receiver) = self.pending_options_refresh.as_ref() else {
            return false;
        };
        match result_receiver.try_recv() {
            Ok(snapshots) => {
                self.pending_options_refresh = None;
                self.apply_dynamic_options_snapshots(snapshots);
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                // Поток умер, не отправив результат (panic). Pending снимаем,
                // чтобы не будить idle loop вечно; старый cache остаётся валидным.
                self.pending_options_refresh = None;
                tracing::warn!(
                    "фоновый refresh dynamic options завершился без результата (panic потока?)"
                );
                false
            }
        }
    }

    /// Кладёт готовые snapshots провайдеров в cache и инвалидирует visual model.
    fn apply_dynamic_options_snapshots(
        &mut self,
        snapshots: Vec<(OptionProviderId, SettingOptions)>,
    ) {
        for (provider_id, snapshot) in snapshots {
            self.option_cache.insert(provider_id, snapshot);
        }
        self.invalidate_ui_model();
    }

    /// Блокирующее ожидание фонового refresh-а для детерминированных тестов.
    #[cfg(test)]
    pub(crate) fn wait_for_options_refresh_for_test(&mut self) {
        if let Some(result_receiver) = self.pending_options_refresh.take() {
            let snapshots = result_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("фоновый refresh dynamic options должен завершиться в тесте");
            self.apply_dynamic_options_snapshots(snapshots);
        }
    }

    /// Собирает request для provider-а из текущего draft value.
    fn option_request_for_provider(
        &self,
        provider_id: &OptionProviderId,
    ) -> SettingsResult<SettingOptionsRequest> {
        for descriptor in self.registry().descriptors() {
            let SettingEditor::Select(SelectDescriptor::Dynamic {
                provider_id: descriptor_provider_id,
            }) = &descriptor.editor
            else {
                continue;
            };
            if descriptor_provider_id != provider_id {
                continue;
            }

            let current_value = self
                .registry()
                .get_value(self.controller.draft(), &descriptor.id)?;
            return Ok(SettingOptionsRequest::new(
                descriptor.id.clone(),
                Some(current_value),
            ));
        }

        Ok(SettingOptionsRequest::new(provider_id.as_str(), None))
    }

    /// Возвращает provider ids из registry без дублей.
    fn dynamic_option_provider_ids(&self) -> Vec<OptionProviderId> {
        self.registry()
            .descriptors()
            .filter_map(|descriptor| match &descriptor.editor {
                SettingEditor::Select(SelectDescriptor::Dynamic { provider_id }) => {
                    Some(provider_id.clone())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
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
                self.apply_draft_with_runtime_adapter(runtime_adapter)?;
                Ok(true)
            }
            SettingsUiAction::Ok => {
                if self.block_apply_when_field_errors_exist() {
                    return Ok(true);
                }
                let report = self.apply_draft_with_runtime_adapter(runtime_adapter)?;
                if report.final_state == ApplyFinalState::FullyApplied
                    || report.final_state == ApplyFinalState::NoChanges
                {
                    self.settings_window_open = false;
                }
                Ok(true)
            }
        }
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
        self.begin_edit();
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

    /// Откатывает live preview routes и закрывает окно только после успешного rollback.
    fn cancel_edit<A>(&mut self, render_adapter: &mut A) -> SettingsResult<bool>
    where
        A: RenderLiveSettingsAdapter,
    {
        let mut rollbacker = SettingsRuntimeRollbackDelegate {
            route_appliers: &mut self.route_appliers,
            render_adapter,
        };
        let report = self.controller.cancel(&mut rollbacker)?;
        self.settings_window_open = false;
        self.field_validation_errors.clear();
        self.status = status_from_cancel(&report);
        self.last_preview_sent_at = None;
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

/// Опрашивает один provider; выполняется в фоновом потоке refresh-а.
///
/// Ошибки провайдера и его отсутствие превращаются в error-snapshot —
/// семантика идентична прежнему синхронному refresh-у.
fn collect_provider_snapshot(
    provider_id: &OptionProviderId,
    request: SettingOptionsRequest,
    provider: Option<Arc<dyn SettingOptionProvider>>,
) -> SettingOptions {
    let current_value = request.current_value.clone();
    match provider {
        Some(provider) => match provider.options(request) {
            Ok(snapshot) => snapshot,
            Err(error) => options_snapshot_from_error(provider_id.clone(), current_value, error),
        },
        None => options_snapshot_from_error(
            provider_id.clone(),
            current_value,
            SettingOptionsError::ProviderUnavailable {
                provider_id: provider_id.clone(),
            },
        ),
    }
}

/// Создаёт production dynamic option providers.
#[cfg(not(test))]
fn default_option_providers(
    audio_output_device_controller: audio::AudioOutputDeviceController,
) -> BTreeMap<OptionProviderId, Arc<dyn SettingOptionProvider>> {
    let provider = AudioOutputDeviceOptionProvider::new(audio_output_device_controller);
    let provider_id = provider.provider_id();
    let mut providers: BTreeMap<OptionProviderId, Arc<dyn SettingOptionProvider>> = BTreeMap::new();
    providers.insert(provider_id, Arc::new(provider));
    providers
}

/// В unit tests providers подставляются явно, чтобы unrelated tests не трогали CPAL/ALSA.
#[cfg(test)]
fn default_option_providers(
    _audio_output_device_controller: audio::AudioOutputDeviceController,
) -> BTreeMap<OptionProviderId, Arc<dyn SettingOptionProvider>> {
    BTreeMap::new()
}

/// Dynamic options provider для `audio.output_device`.
#[cfg(not(test))]
struct AudioOutputDeviceOptionProvider {
    /// Audio owner API без CPAL types в settings runtime/UI.
    audio_output_device_controller: audio::AudioOutputDeviceController,
}

#[cfg(not(test))]
impl AudioOutputDeviceOptionProvider {
    /// Создаёт provider поверх shared audio output device controller-а.
    fn new(audio_output_device_controller: audio::AudioOutputDeviceController) -> Self {
        Self {
            audio_output_device_controller,
        }
    }
}

#[cfg(not(test))]
impl SettingOptionProvider for AudioOutputDeviceOptionProvider {
    fn provider_id(&self) -> OptionProviderId {
        OptionProviderId::from("audio.output_device")
    }

    fn options(
        &self,
        request: SettingOptionsRequest,
    ) -> Result<SettingOptions, SettingOptionsError> {
        let provider_id = self.provider_id();
        let mut options = vec![SettingOption::new(
            audio::DEFAULT_AUDIO_OUTPUT_DEVICE_ID,
            SettingText::new(
                "settings.audio.output_device.default",
                "Системное устройство",
            ),
        )];

        let devices = self
            .audio_output_device_controller
            .output_devices()
            .map_err(|error| SettingOptionsError::Failed {
                provider_id: provider_id.clone(),
                message: error.to_string(),
            })?;
        options.extend(devices.into_iter().map(audio_output_device_option));

        Ok(SettingOptions::ready(
            provider_id,
            options.clone(),
            current_option_value(request.current_value, &options),
        ))
    }
}

/// Преобразует neutral audio device snapshot в settings option.
#[cfg(not(test))]
fn audio_output_device_option(device: audio::AudioOutputDeviceInfo) -> SettingOption {
    let label = if device.is_system_default {
        format!("{} (текущее системное)", device.display_name)
    } else {
        device.display_name
    };

    SettingOption::new(
        device.stable_id,
        SettingText::new("settings.audio.output_device.detected", label),
    )
}

/// Создаёт snapshot с заданным status, сохраняя current select id.
fn options_snapshot_from_status(
    provider_id: OptionProviderId,
    status: SettingOptionsStatus,
    options: Vec<SettingOption>,
    current_value: Option<SettingValue>,
) -> SettingOptions {
    let current = current_option_value(current_value, &options);
    SettingOptions {
        provider_id,
        status,
        options,
        current,
    }
}

/// Создаёт error snapshot, который visual UI покажет как option-provider error.
fn options_snapshot_from_error(
    provider_id: OptionProviderId,
    current_value: Option<SettingValue>,
    error: SettingOptionsError,
) -> SettingOptions {
    options_snapshot_from_status(
        provider_id,
        SettingOptionsStatus::Unavailable {
            message: format!("Option-provider error: {error}"),
        },
        Vec::new(),
        current_value,
    )
}

/// Вычисляет current dynamic option state без замены unavailable id на default.
fn current_option_value(
    current_value: Option<SettingValue>,
    options: &[SettingOption],
) -> SettingOptionCurrentValue {
    let Some(SettingValue::Select(current_id)) = current_value else {
        return SettingOptionCurrentValue::None;
    };

    if options.iter().any(|option| option.id == current_id) {
        SettingOptionCurrentValue::Available(current_id)
    } else {
        SettingOptionCurrentValue::UnavailableCurrent {
            label: unavailable_current_label(&current_id),
            id: current_id,
        }
    }
}

/// Формирует label для сохранённого, но сейчас недоступного dynamic option id.
fn unavailable_current_label(current_id: &SettingOptionId) -> SettingText {
    SettingText::new(
        "settings.dynamic_options.unavailable_current",
        format!("{} (сейчас недоступно)", current_id.as_str()),
    )
}

/// Delegate, который позволяет controller-у использовать store/appliers без передачи ownership.
struct SettingsRuntimeApplyDelegate<'runtime, A> {
    /// Authoritative AppConfig validator из settings binding crate.
    validator: AppConfigValidator,

    /// Store остаётся owned by `SettingsRuntime`.
    store: &'runtime mut AppConfigStore,

    /// Route appliers остаются owned by `SettingsRuntime`.
    route_appliers: &'runtime mut SettingsRuntimeRouteAppliers,

    /// Renderer/player/media owners проходят только через narrow runtime traits.
    runtime_adapter: &'runtime mut A,
}

impl<A> SettingsValidator<AppConfig> for SettingsRuntimeApplyDelegate<'_, A> {
    fn validate(
        &mut self,
        request: ValidationRequest<'_, AppConfig>,
    ) -> SettingsResult<ValidationReport> {
        self.validator.validate(request)
    }
}

impl<A> SettingsPersister<AppConfig> for SettingsRuntimeApplyDelegate<'_, A> {
    fn persist(&mut self, request: PersistRequest<'_, AppConfig>) -> SettingsResult<PersistReport> {
        self.store.persist(request)
    }
}

impl<A> CommittedSettingsApplier<AppConfig> for SettingsRuntimeApplyDelegate<'_, A>
where
    A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
{
    fn apply_committed(
        &mut self,
        request: CommittedApplyRequest<'_, AppConfig>,
    ) -> SettingsResult<Vec<ApplyRouteReport>> {
        let mut reports = Vec::with_capacity(request.route_updates.len());
        self.runtime_adapter
            .sync_committed_config_snapshot(CommittedConfigSnapshot::from_config(
                request.persisted,
            ));

        for update in request.route_updates {
            let routes = committed_routes_from_update(
                request.previous_committed,
                request.persisted,
                update,
            )?;
            for route in routes {
                reports.push(
                    self.route_appliers
                        .apply_committed_route_with_render_adapter(route, self.runtime_adapter)?
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

    /// Shared audio output device selection owner из concrete audio crate.
    audio_output_device_controller: audio::AudioOutputDeviceController,

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
            audio_output_device_controller: audio::AudioOutputDeviceController::new(
                config.audio.output_device.clone(),
            ),
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
        if let RuntimeCommittedUpdate::RenderPreview(update) = route.update.clone() {
            self.render_live = update.live_settings.settings.clone();
            return Ok(Self::route_report(
                route,
                AppRouteApplyResult::PreviewPromoted,
                ApplyMechanism::PreviewPromoted,
            ));
        }

        let mut reconfigure_host = NoopSettingsRuntimeReconfigureHost;
        self.apply_committed_route_with_reconfigure_host(route, &mut reconfigure_host)
    }
}

impl SettingsRuntimeRouteAppliers {
    /// Применяет committed route с доступом к renderer-neutral live adapter.
    fn apply_committed_route_with_render_adapter<A>(
        &mut self,
        route: RuntimeCommittedRoute,
        runtime_adapter: &mut A,
    ) -> SettingsResult<AppRouteApplyReport>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        match route.update.clone() {
            RuntimeCommittedUpdate::RenderPreview(update) => {
                let result = self
                    .commit_render_preview_update(&update.live_settings.settings, runtime_adapter);
                Ok(Self::route_report(
                    route,
                    result,
                    ApplyMechanism::PreviewPromoted,
                ))
            }
            _ => self.apply_committed_route_with_reconfigure_host(route, runtime_adapter),
        }
    }

    /// Применяет committed route с доступом к app/player/source owners.
    fn apply_committed_route_with_reconfigure_host(
        &mut self,
        route: RuntimeCommittedRoute,
        reconfigure_host: &mut dyn SettingsRuntimeReconfigureHost,
    ) -> SettingsResult<AppRouteApplyReport> {
        match route.update.clone() {
            RuntimeCommittedUpdate::Ui(update) => {
                let result = self.apply_ui_update(&update);
                Ok(Self::route_report(route, result, ApplyMechanism::InPlace))
            }
            RuntimeCommittedUpdate::RenderCommitted(update) => {
                let result = self.apply_render_committed_update(&update);
                let mechanism =
                    if matches!(result, AppRouteApplyResult::DeferredTechnicalDebt { .. }) {
                        ApplyMechanism::DeferredTechnicalDebt
                    } else {
                        ApplyMechanism::RendererRecreate
                    };
                Ok(Self::route_report(route, result, mechanism))
            }
            RuntimeCommittedUpdate::Player(update) => {
                Ok(self.apply_player_route(route, &update, reconfigure_host))
            }
            RuntimeCommittedUpdate::MediaService(update) => {
                let result = self.apply_media_service_update(&update, reconfigure_host);
                Ok(Self::route_report(
                    route,
                    result,
                    ApplyMechanism::WorkerReconfigure,
                ))
            }
            RuntimeCommittedUpdate::RenderPreview(_update) => unreachable!(
                "render preview committed route requires render adapter promotion path"
            ),
        }
    }

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
        reconfigure_host: &mut dyn SettingsRuntimeReconfigureHost,
    ) -> AppRouteApplyReport {
        let player_core_result = self.apply_player_core_update(update, reconfigure_host);
        if player_core_result.is_success() {
            self.commit_player_default_volume_snapshot(update);
        }
        let audio_output_device_result = self.apply_player_audio_output_device(update);
        let in_place_result = combine_player_in_place_results([
            player_core_result.clone(),
            audio_output_device_result.clone(),
        ]);
        let deferred_message = player_deferred_message(update);
        let route_result = player_route_result(in_place_result, deferred_message.as_deref());
        let mechanism = if route_result.is_success() {
            player_apply_mechanism(update)
        } else {
            ApplyMechanism::WorkerReconfigure
        };
        let groups = route
            .groups
            .into_iter()
            .map(|group| AppRouteGroupReport {
                result: player_group_result(
                    &group.group,
                    &player_core_result,
                    &audio_output_device_result,
                    &route_result,
                ),
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

    /// Применяет player-core update через request/reply worker boundary.
    fn apply_player_core_update(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
        reconfigure_host: &mut dyn SettingsRuntimeReconfigureHost,
    ) -> AppRouteApplyResult {
        if update.player_core.is_empty() {
            return AppRouteApplyResult::Noop;
        };

        match reconfigure_host.apply_player_runtime_settings(update.player_core.clone()) {
            Ok(report) => player_runtime_report_result(&report),
            Err(error) => AppRouteApplyResult::Failed {
                message: player_runtime_error_message(error),
            },
        }
    }

    /// Обновляет локальный policy snapshot только после успешного worker apply.
    fn commit_player_default_volume_snapshot(&mut self, update: &PlayerCommittedSettingsUpdate) {
        let Some(default_volume_update) = &update.player_core.default_volume else {
            return;
        };
        let next_default_volume = default_volume_update.default_volume;
        if (self.player.default_volume - next_default_volume).abs() > f32::EPSILON {
            self.player.default_volume = next_default_volume;
        }
    }

    /// Применяет выбранный audio output device через owner API concrete audio crate.
    fn apply_player_audio_output_device(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
    ) -> AppRouteApplyResult {
        let Some(next_device_id) = &update.audio_output_device_id else {
            return AppRouteApplyResult::Noop;
        };
        if &self.player.audio_output_device_id == next_device_id {
            return AppRouteApplyResult::Noop;
        }

        match self
            .audio_output_device_controller
            .select_output_device(next_device_id.clone())
        {
            Ok(audio::AudioOutputDeviceSelectionChange::Applied) => {
                self.player.audio_output_device_id = next_device_id.clone();
                AppRouteApplyResult::Applied
            }
            Ok(audio::AudioOutputDeviceSelectionChange::Noop) => {
                self.player.audio_output_device_id = next_device_id.clone();
                AppRouteApplyResult::Noop
            }
            Err(error) => AppRouteApplyResult::Failed {
                message: error.to_string(),
            },
        }
    }

    /// Применяет media/service policy snapshot на уровне app settings runtime.
    fn apply_media_service_update(
        &mut self,
        update: &MediaServiceRuntimeSettingsUpdate,
        reconfigure_host: &mut dyn SettingsRuntimeReconfigureHost,
    ) -> AppRouteApplyResult {
        let next_snapshot = MediaServiceRuntimeSnapshot::from_update(update);
        if self.media_service == next_snapshot {
            return AppRouteApplyResult::Noop;
        }

        let result = reconfigure_host.apply_media_service_runtime_settings(update);
        if !result.is_success() {
            return result;
        }

        self.media_service = next_snapshot;
        result
    }

    /// Отправляет live preview update в renderer и фиксирует active preview snapshot.
    fn preview_render_live_settings<A>(
        &mut self,
        document: &AppConfig,
        render_adapter: &mut A,
    ) -> PreviewApplyResult
    where
        A: RenderLiveSettingsAdapter,
    {
        let next_settings = match render_live_settings_from_config(document) {
            Ok(settings) => settings,
            Err(error) => {
                return PreviewApplyResult::Fatal {
                    message: error.to_string(),
                };
            }
        };
        let update = RenderLiveSettingsUpdate::from_baseline(&self.render_live, next_settings);
        match render_adapter.preview_live_settings(&update) {
            Ok(report) => {
                self.render_live = update.settings;
                preview_result_from_render_report(report.outcome)
            }
            Err(error) => preview_result_from_render_error(&error),
        }
    }

    /// Откатывает renderer preview route к captured baseline document-у.
    fn rollback_render_live_settings<A>(
        &mut self,
        baseline_document: &AppConfig,
        render_adapter: &mut A,
    ) -> SettingsResult<RollbackResult>
    where
        A: RenderLiveSettingsAdapter,
    {
        let baseline_settings = render_live_settings_from_config(baseline_document)?;
        let report = render_adapter
            .rollback_live_settings(&baseline_settings)
            .map_err(|error| settings_core::SettingsError::access_failed(error.to_string()))?;
        self.render_live = baseline_settings;
        Ok(rollback_result_from_render_report(report.outcome))
    }

    /// Фиксирует preview-capable render settings как committed renderer state.
    fn commit_render_preview_update<A>(
        &mut self,
        settings: &RenderLiveSettings,
        render_adapter: &mut A,
    ) -> AppRouteApplyResult
    where
        A: RenderLiveSettingsAdapter,
    {
        match render_adapter.commit_live_settings(settings) {
            Ok(_) => {
                self.render_live = settings.clone();
                AppRouteApplyResult::PreviewPromoted
            }
            Err(error) => AppRouteApplyResult::Failed {
                message: error.to_string(),
            },
        }
    }
}

/// Delegate preview-а: controller отвечает за coalescing, runtime - за renderer boundary.
struct SettingsRuntimePreviewDelegate<'runtime, A> {
    /// App-owned route snapshots.
    route_appliers: &'runtime mut SettingsRuntimeRouteAppliers,

    /// Renderer-neutral adapter без WGPU типов в settings/runtime controller.
    render_adapter: &'runtime mut A,
}

impl<A> PreviewSettingsApplier<AppConfig> for SettingsRuntimePreviewDelegate<'_, A>
where
    A: RenderLiveSettingsAdapter,
{
    fn apply_preview(
        &mut self,
        request: PreviewApplyRequest<'_, AppConfig>,
    ) -> SettingsResult<PreviewApplyReport> {
        let result = if request.update.route == SettingRouteId::from(RENDER_PREVIEW_ROUTE_ID) {
            self.route_appliers
                .preview_render_live_settings(&request.update.document, self.render_adapter)
        } else {
            PreviewApplyResult::Unsupported {
                message: format!(
                    "Preview route `{}` не поддержан settings runtime",
                    request.update.route
                ),
            }
        };
        Ok(PreviewApplyReport {
            route: request.update.route.clone(),
            result,
            affected_settings: request.update.affected_settings.clone(),
        })
    }
}

/// Delegate rollback-а: controller хранит baseline, runtime применяет его к renderer.
struct SettingsRuntimeRollbackDelegate<'runtime, A> {
    /// App-owned route snapshots.
    route_appliers: &'runtime mut SettingsRuntimeRouteAppliers,

    /// Renderer-neutral adapter без зависимости UI от renderer backend-а.
    render_adapter: &'runtime mut A,
}

impl<A> PreviewRollbacker<AppConfig> for SettingsRuntimeRollbackDelegate<'_, A>
where
    A: RenderLiveSettingsAdapter,
{
    fn rollback_preview(
        &mut self,
        request: PreviewRollbackRequest<'_, AppConfig>,
    ) -> SettingsResult<RollbackReport> {
        let result = if request.route == &SettingRouteId::from(RENDER_PREVIEW_ROUTE_ID) {
            self.route_appliers
                .rollback_render_live_settings(request.baseline_document, self.render_adapter)?
        } else {
            RollbackResult::Noop
        };
        Ok(RollbackReport {
            route: request.route.clone(),
            result,
            affected_settings: request.affected_settings.to_vec(),
        })
    }
}

/// Возвращает причину для явно будущих player/backend settings.
fn player_deferred_message(update: &PlayerCommittedSettingsUpdate) -> Option<String> {
    if !update.deferred_boundary_settings.is_empty() {
        return Some(format!(
            "Player settings требуют будущего backend boundary: {}",
            setting_ids_text(&update.deferred_boundary_settings)
        ));
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

/// Выбирает mechanism по самому тяжёлому player operation в route.
fn player_apply_mechanism(update: &PlayerCommittedSettingsUpdate) -> ApplyMechanism {
    if update.player_core.decoder_thread_config.is_some()
        || update.player_core.video_backend.is_some()
    {
        ApplyMechanism::PipelineRebuild
    } else if update.player_core.tick_config.is_some()
        || update.player_core.default_volume.is_some()
        || update.audio_output_device_id.is_some()
    {
        ApplyMechanism::WorkerReconfigure
    } else {
        ApplyMechanism::InPlace
    }
}

/// Вычисляет route-level result с честным partial status.
fn player_route_result(
    in_place_result: AppRouteApplyResult,
    deferred_message: Option<&str>,
) -> AppRouteApplyResult {
    match deferred_message {
        Some(message) if in_place_result.is_success() => AppRouteApplyResult::PartialFailure {
            message: message.to_string(),
        },
        Some(message) => AppRouteApplyResult::DeferredTechnicalDebt {
            message: message.to_string(),
        },
        None => in_place_result,
    }
}

/// Преобразует worker report в route-level result без потери failure messages.
fn player_runtime_report_result(report: &PlayerRuntimeApplyReport) -> AppRouteApplyResult {
    combine_player_in_place_results(report.groups.iter().map(player_runtime_group_report_result))
}

/// Преобразует одну player group в app route result.
fn player_runtime_group_report_result(
    report: &PlayerRuntimeApplyGroupReport,
) -> AppRouteApplyResult {
    match report.outcome {
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Applied) => {
            AppRouteApplyResult::Applied
        }
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Unchanged) => {
            AppRouteApplyResult::Noop
        }
        PlayerRuntimeApplyOutcome::RequiresControlledRebuild => AppRouteApplyResult::Failed {
            message: format!(
                "player-core still requires controlled rebuild for {:?}: {}",
                report.group, report.message
            ),
        },
        PlayerRuntimeApplyOutcome::Unsupported
        | PlayerRuntimeApplyOutcome::AbsentResource
        | PlayerRuntimeApplyOutcome::Invalid
        | PlayerRuntimeApplyOutcome::Fatal => AppRouteApplyResult::Failed {
            message: format!("{:?}: {}", report.group, report.message),
        },
    }
}

/// Форматирует request/reply error без silent collapse.
fn player_runtime_error_message(error: PlayerRuntimeApplyError) -> String {
    format!("player runtime apply failed: {error}")
}

/// Собирает результат independent in-place player updates без потери error details.
fn combine_player_in_place_results(
    results: impl IntoIterator<Item = AppRouteApplyResult>,
) -> AppRouteApplyResult {
    let mut applied = false;
    let mut failures = Vec::new();

    for result in results {
        match result {
            AppRouteApplyResult::Applied | AppRouteApplyResult::PreviewPromoted => {
                applied = true;
            }
            AppRouteApplyResult::Noop => {}
            AppRouteApplyResult::Failed { message }
            | AppRouteApplyResult::PartialFailure { message }
            | AppRouteApplyResult::DeferredTechnicalDebt { message } => failures.push(message),
            AppRouteApplyResult::Conflict { baseline, current } => failures.push(format!(
                "conflict: baseline {}, current {}",
                baseline.value(),
                current.value()
            )),
        }
    }

    if failures.is_empty() {
        if applied {
            AppRouteApplyResult::Applied
        } else {
            AppRouteApplyResult::Noop
        }
    } else if applied {
        AppRouteApplyResult::PartialFailure {
            message: failures.join("; "),
        }
    } else {
        AppRouteApplyResult::Failed {
            message: failures.join("; "),
        }
    }
}

/// Возвращает group-level player result без слияния разных owner semantics.
fn player_group_result(
    group: &AppRuntimeRouteGroup,
    player_core_result: &AppRouteApplyResult,
    audio_output_device_result: &AppRouteApplyResult,
    route_result: &AppRouteApplyResult,
) -> AppRouteApplyResult {
    match group {
        AppRuntimeRouteGroup::PlayerDefaultVolume
        | AppRuntimeRouteGroup::PlayerTickConfig
        | AppRuntimeRouteGroup::PlayerDecoderThreadConfig
        | AppRuntimeRouteGroup::PlayerVideoBackend
        | AppRuntimeRouteGroup::PlayerDeferredBoundary => player_core_result.clone(),
        AppRuntimeRouteGroup::PlayerAudioOutputDevice => audio_output_device_result.clone(),
        _ => route_result.clone(),
    }
}

/// Переводит успешный render-core preview report в neutral settings-core result.
fn preview_result_from_render_report(outcome: RenderLiveApplyOutcome) -> PreviewApplyResult {
    match outcome {
        RenderLiveApplyOutcome::Applied => PreviewApplyResult::Applied,
        RenderLiveApplyOutcome::NoOp => PreviewApplyResult::Noop,
    }
}

/// Переводит render-core error taxonomy в retry/non-retry preview status.
fn preview_result_from_render_error(
    error: &render_core::RenderLiveSettingsError,
) -> PreviewApplyResult {
    match error.kind() {
        RenderLiveSettingsErrorKind::AbsentResource => PreviewApplyResult::Backpressured,
        RenderLiveSettingsErrorKind::Unsupported => PreviewApplyResult::Unsupported {
            message: error.to_string(),
        },
        RenderLiveSettingsErrorKind::Fatal => PreviewApplyResult::Fatal {
            message: error.to_string(),
        },
    }
}

/// Переводит successful renderer rollback outcome в settings-core rollback result.
fn rollback_result_from_render_report(outcome: RenderLiveApplyOutcome) -> RollbackResult {
    match outcome {
        RenderLiveApplyOutcome::Applied => RollbackResult::RolledBack,
        RenderLiveApplyOutcome::NoOp => RollbackResult::Noop,
    }
}

/// Формирует UI status по field reset report-у.
fn status_from_reset(summary: &str, report: &ResetReport) -> SettingsUiStatus {
    SettingsUiStatus {
        summary: Some(summary.to_string()),
        details: vec![format!(
            "Изменены draft fields: {}",
            setting_ids_text(&report.affected_settings)
        )],
    }
}

/// Формирует UI status по cancel/rollback report-у.
fn status_from_cancel(report: &CancelReport) -> SettingsUiStatus {
    let mut details = Vec::new();
    if !report.discarded_changes.is_empty() {
        details.push(format!(
            "Отброшены draft changes: {}",
            setting_ids_text(
                &report
                    .discarded_changes
                    .changes()
                    .iter()
                    .map(|change| change.id.clone())
                    .collect::<Vec<_>>()
            )
        ));
    }
    for rollback in &report.rolled_back_routes {
        details.push(format!(
            "Rollback route `{}`: {:?} ({})",
            rollback.route,
            rollback.result,
            setting_ids_text(&rollback.affected_settings)
        ));
    }

    SettingsUiStatus {
        summary: Some("Изменения отменены".to_string()),
        details,
    }
}

/// Формирует UI status по preview attempts, включая backpressure и non-retry errors.
fn status_from_preview_reports(reports: &[PreviewApplyReport]) -> SettingsUiStatus {
    let has_error = reports.iter().any(|report| {
        matches!(
            report.result,
            PreviewApplyResult::Unsupported { .. } | PreviewApplyResult::Fatal { .. }
        )
    });
    let has_backpressure = reports
        .iter()
        .any(|report| report.result == PreviewApplyResult::Backpressured);
    let summary = if has_error {
        "Live preview не применён полностью"
    } else if has_backpressure {
        "Live preview ждёт renderer"
    } else {
        "Live preview применён"
    };
    SettingsUiStatus {
        summary: Some(summary.to_string()),
        details: reports.iter().map(preview_report_text).collect(),
    }
}

/// Формирует UI status по full apply report-у.
fn status_from_apply_report(report: &ApplyReport) -> SettingsUiStatus {
    let mut details = Vec::new();
    if let Some(persistence) = &report.persistence {
        details.push(persist_report_text(persistence));
    }
    for route in &report.routes {
        details.push(route_report_text(route));
    }
    for conflict in &report.conflicts {
        details.push(format!(
            "Conflict route `{}`: baseline {}, current {}, settings {}",
            conflict.route,
            conflict.baseline.value(),
            conflict.current.value(),
            setting_ids_text(&conflict.affected_settings)
        ));
    }
    for error in &report.errors {
        details.push(error.to_string());
    }

    SettingsUiStatus {
        summary: Some(apply_final_state_text(report.final_state).to_string()),
        details,
    }
}

/// Человекочитаемое резюме final state.
fn apply_final_state_text(final_state: ApplyFinalState) -> &'static str {
    match final_state {
        ApplyFinalState::NoChanges => "Нет изменений для применения",
        ApplyFinalState::ValidationFailed => "Настройки не прошли полную проверку",
        ApplyFinalState::PersistFailed => "Не удалось сохранить TOML",
        ApplyFinalState::BlockedByConflicts => "Применение заблокировано конфликтом runtime route",
        ApplyFinalState::PersistedRuntimeDiverged => {
            "TOML сохранён, но runtime применился не полностью"
        }
        ApplyFinalState::FullyApplied => "Настройки сохранены и применены",
    }
}

/// Форматирует persistence outcome для status details.
fn persist_report_text(report: &PersistReport) -> String {
    let outcome = match report.outcome {
        PersistOutcome::Persisted => "TOML сохранён atomically",
        PersistOutcome::SkippedNoChanges => "TOML не записывался: durable changes отсутствуют",
    };
    match &report.durability_warning {
        Some(warning) => format!("{outcome}; durability warning: {warning}"),
        None => outcome.to_string(),
    }
}

/// Форматирует route apply result без потери partial/error semantics.
fn route_report_text(report: &ApplyRouteReport) -> String {
    format!(
        "Route `{}`: {} ({})",
        report.route,
        apply_route_result_text(&report.result),
        setting_ids_text(&report.affected_settings)
    )
}

/// Форматирует preview result без слияния backpressure и fatal errors.
fn preview_report_text(report: &PreviewApplyReport) -> String {
    format!(
        "Preview route `{}`: {} ({})",
        report.route,
        preview_apply_result_text(&report.result),
        setting_ids_text(&report.affected_settings)
    )
}

/// Человекочитаемый route apply result.
fn apply_route_result_text(result: &ApplyRouteResult) -> String {
    match result {
        ApplyRouteResult::Applied => "applied".to_string(),
        ApplyRouteResult::Noop => "no-op".to_string(),
        ApplyRouteResult::PreviewPromoted => "preview promoted".to_string(),
        ApplyRouteResult::PartialFailure { message } => format!("partial failure: {message}"),
        ApplyRouteResult::Failed { message } => format!("failed: {message}"),
        ApplyRouteResult::Conflict { baseline, current } => {
            format!(
                "conflict: baseline {}, current {}",
                baseline.value(),
                current.value()
            )
        }
    }
}

/// Человекочитаемый preview apply result.
fn preview_apply_result_text(result: &PreviewApplyResult) -> String {
    match result {
        PreviewApplyResult::Applied => "applied".to_string(),
        PreviewApplyResult::Noop => "no-op".to_string(),
        PreviewApplyResult::Backpressured => {
            "backpressured; latest update remains pending".to_string()
        }
        PreviewApplyResult::Unsupported { message } => format!("unsupported: {message}"),
        PreviewApplyResult::Fatal { message } => format!("fatal: {message}"),
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

    /// Stable audio output device id для будущих audio outputs.
    audio_output_device_id: String,
}

impl PlayerRuntimeSnapshot {
    /// Создаёт snapshot из full config-а.
    fn from_config(config: &AppConfig) -> Self {
        Self {
            default_volume: config.audio.volume as f32,
            audio_output_device_id: config.audio.output_device.clone(),
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
    use std::fs;
    use std::io::ErrorKind;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use player_core::{
        PlayerRuntimeApplyResult, PlayerRuntimeSettingId, PlayerRuntimeSettingsUpdate,
    };
    use render_core::{
        RenderLiveApplyPhase, RenderLiveApplyReport, RenderLiveSettingId, RenderLiveSettings,
        RenderLiveSettingsAdapter, RenderLiveSettingsError, RenderLiveSettingsUpdate,
    };
    use rustiplayer_config::{AppConfig, LoadedConfig};
    use rustiplayer_settings::{
        AppRouteApplyResult, AppRuntimeRoute, AppRuntimeRouteApplier, AppRuntimeRouteGroup,
        AppRuntimeRouteGroupUpdate, MediaServiceRuntimeSettingsUpdate,
        PlayerCommittedSettingsUpdate, RuntimeCommittedRoute, RuntimeCommittedUpdate,
        render_live_settings_from_config,
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

    fn replace_audio_option_provider(
        runtime: &mut SettingsRuntime,
        provider: ScriptedOptionProvider,
    ) {
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
            let changed_fields: Vec<RenderLiveSettingId> =
                self.active.changed_fields_from(baseline);
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
            update: PlayerRuntimeSettingsUpdate,
        ) -> PlayerRuntimeApplyResult {
            self.player_updates.push(update.clone());
            if self.fail_player {
                return Err(player_core::PlayerRuntimeApplyError::Backpressure);
            }
            Ok(super::simulated_player_runtime_report(update))
        }

        fn apply_media_service_runtime_settings(
            &mut self,
            _update: &MediaServiceRuntimeSettingsUpdate,
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
        assert_eq!(report.mechanism, ApplyMechanism::WorkerReconfigure);
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
        assert_eq!(report.mechanism, ApplyMechanism::WorkerReconfigure);
        assert_eq!(report.groups[0].result, report.result);
        assert_eq!(appliers.media_service, original_snapshot);
    }

    #[test]
    fn dynamic_options_preserve_unavailable_current_value() {
        let mut config = custom_config_for_test();
        config.audio.output_device = "cpal-0.15-name:Missing%20DAC".to_string();
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
            .find(|field| {
                field.descriptor.id == SettingId::from("render.color_adjustment.brightness")
            })
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
        let mut runtime =
            SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
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
