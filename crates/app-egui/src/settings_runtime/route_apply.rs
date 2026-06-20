use super::*;

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
pub(super) struct NoopSettingsRuntimeReconfigureHost;

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
pub(super) struct RenderOnlySettingsRuntimeAdapter<'adapter, A> {
    /// Реальный renderer-neutral adapter.
    pub(super) render_adapter: &'adapter mut A,
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
pub(super) fn simulated_player_runtime_report(
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
/// Delegate, который позволяет controller-у использовать store/appliers без передачи ownership.
pub(super) struct SettingsRuntimeApplyDelegate<'runtime, A> {
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
pub(super) struct SettingsRuntimeRouteAppliers {
    /// Последний UI runtime snapshot.
    ui: UiConfig,

    /// Последний live render snapshot, известный settings runtime-у.
    pub(super) render_live: RenderLiveSettings,

    /// Последний committed renderer lifecycle snapshot.
    render_committed: RenderCommittedRuntimeSnapshot,

    /// Последний player settings snapshot, не смешанный с current playback controls.
    pub(super) player: PlayerRuntimeSnapshot,

    /// Shared audio output device selection owner из concrete audio crate.
    pub(super) audio_output_device_controller: audio::AudioOutputDeviceController,

    /// Последний media/service policy snapshot.
    pub(super) media_service: MediaServiceRuntimeSnapshot,
}

impl SettingsRuntimeRouteAppliers {
    /// Инициализирует route snapshots из startup committed config-а.
    pub(super) fn from_config(config: &AppConfig) -> SettingsResult<Self> {
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
    pub(super) fn apply_committed_route_with_render_adapter<A>(
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
pub(super) struct PlayerRuntimeSnapshot {
    /// Default volume policy для будущих media.
    pub(super) default_volume: f32,

    /// Stable audio output device id для будущих audio outputs.
    audio_output_device_id: String,
}

impl PlayerRuntimeSnapshot {
    /// Создаёт snapshot из full config-а.
    pub(super) fn from_config(config: &AppConfig) -> Self {
        Self {
            default_volume: config.audio.volume as f32,
            audio_output_device_id: config.audio.output_device.clone(),
        }
    }
}

/// Snapshot media/service settings без владения конкретными network jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MediaServiceRuntimeSnapshot {
    /// Network/source cache policy.
    network: NetworkConfig,

    /// YouTube service policy.
    youtube: YoutubeConfig,
}

impl MediaServiceRuntimeSnapshot {
    /// Создаёт snapshot из full config-а.
    pub(super) fn from_config(config: &AppConfig) -> Self {
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

impl SettingsRuntime {
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
}
