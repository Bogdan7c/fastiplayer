//! End-to-end settings transaction coordinator for app-owned runtime routes.

use super::stepwise_apply::StepwiseRouteSequence;
use super::*;

/// Делегат связывает neutral controller с store и конкретными runtime owners.
pub(super) struct SettingsRuntimeApplyDelegate<'runtime, A> {
    /// Authoritative full-document validator.
    pub(super) validator: AppConfigValidator,
    /// Atomic TOML store, owned by `SettingsRuntime`.
    pub(super) store: &'runtime mut AppConfigStore,
    /// App-owned route snapshots and intent appliers.
    pub(super) route_appliers: &'runtime mut SettingsRuntimeRouteAppliers,
    /// Renderer/player/media owners behind narrow boundaries.
    pub(super) runtime_adapter: &'runtime mut A,
    /// Completed route prefix that must be compensated after a failure.
    pub(super) applied_route_count: usize,
    /// Размер пула поверхностей активного рендерера для «авто» (0) в config.
    pub(super) automatic_surface_pool: player_core::AutomaticSurfacePool,
    /// Многокадровая очередь маршрутов (C2); синхронные пути передают пустой слот.
    pub(super) route_sequence: &'runtime mut Option<StepwiseRouteSequence>,
}

impl<A> SettingsValidator<AppConfig> for SettingsRuntimeApplyDelegate<'_, A> {
    fn validate(
        &mut self,
        request: ValidationRequest<'_, AppConfig>,
    ) -> SettingsResult<ValidationReport> {
        self.validator.validate(request)
    }
}

impl<A> SettingsPersister<AppConfig> for SettingsRuntimeApplyDelegate<'_, A>
where
    A: SettingsRuntimeReconfigureHost,
{
    fn persist(&mut self, request: PersistRequest<'_, AppConfig>) -> SettingsResult<PersistReport> {
        let report = self.store.persist(PersistRequest {
            document: request.document,
            changed_settings: request.changed_settings,
        })?;
        if report.outcome == PersistOutcome::SkippedSessionOnly {
            // Изменение применено без записи на диск — пользователь должен это знать.
            self.runtime_adapter.report_settings_kept_for_session_only();
        }
        Ok(report)
    }
}

impl<A> CommittedSettingsApplier<AppConfig> for SettingsRuntimeApplyDelegate<'_, A>
where
    A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
{
    fn preflight_committed(
        &mut self,
        request: CommittedApplyRequest<'_, AppConfig>,
    ) -> SettingsResult<Vec<ApplyRouteReport>> {
        let routes = committed_routes_for_updates(
            request.previous_committed,
            request.requested,
            request.route_updates,
            self.automatic_surface_pool,
        )?;
        match self.runtime_adapter.preflight_settings_transaction(&routes) {
            Ok(()) => Ok(Vec::new()),
            Err(failure) => Ok(routes
                .into_iter()
                .find(|route| route.route == failure.route)
                .map(|route| {
                    SettingsRuntimeRouteAppliers::route_report(
                        route,
                        failure.result,
                        ApplyMechanism::InPlace,
                    )
                    .into_core_report()
                })
                .into_iter()
                .collect()),
        }
    }

    fn apply_committed(
        &mut self,
        request: CommittedApplyRequest<'_, AppConfig>,
    ) -> SettingsResult<Vec<ApplyRouteReport>> {
        self.applied_route_count = 0;
        let routes = committed_routes_for_updates(
            request.previous_committed,
            request.requested,
            request.route_updates,
            self.automatic_surface_pool,
        )?;
        let target_policy = SettingsRouteTargetPolicy::from_config(request.requested);
        let mut reports = Vec::with_capacity(routes.len());
        for route in routes {
            let report = self
                .route_appliers
                .apply_committed_route_with_render_adapter(
                    route,
                    target_policy,
                    self.runtime_adapter,
                )?;
            let full_success = report.result.is_success();
            if full_success || report.result.needs_compensation() {
                self.applied_route_count += 1;
            }
            reports.push(report.into_core_report());
            if !full_success {
                break;
            }
        }
        Ok(reports)
    }

    fn rollback_committed(
        &mut self,
        request: CommittedRollbackRequest<'_, AppConfig>,
    ) -> SettingsResult<Vec<RollbackReport>> {
        self.rollback_applied_routes(request)
    }

    fn finalize_committed(&mut self, _request: CommittedFinalizeRequest<'_, AppConfig>) {
        self.runtime_adapter.finalize_settings_transaction();
        self.applied_route_count = 0;
    }
}

impl<A> SettingsRuntimeApplyDelegate<'_, A>
where
    A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
{
    /// Откатывает завершённый префикс маршрутов в обратном порядке (общий для всех путей).
    pub(super) fn rollback_applied_routes(
        &mut self,
        request: CommittedRollbackRequest<'_, AppConfig>,
    ) -> SettingsResult<Vec<RollbackReport>> {
        let rollback_routes = committed_routes_for_updates(
            request.attempted,
            request.previous_committed,
            request.route_updates,
            self.automatic_surface_pool,
        )?;
        let target_policy = SettingsRouteTargetPolicy::from_config(request.previous_committed);
        let mut reports = Vec::with_capacity(self.applied_route_count);
        for route in rollback_routes
            .into_iter()
            .take(self.applied_route_count)
            .rev()
        {
            reports.push(
                self.route_appliers
                    .rollback_committed_route_with_render_adapter(
                        route,
                        target_policy,
                        self.runtime_adapter,
                    )?
                    .into_rollback_report(),
            );
        }
        self.applied_route_count = 0;
        Ok(reports)
    }
}

impl SettingsRuntime {
    /// Пропускает одно runtime-originated значение через тот же transaction delegate.
    ///
    /// Частный случай батча из одного значения: путь коммита общий.
    pub(super) fn commit_runtime_setting_with_runtime_adapter<A>(
        &mut self,
        request: RuntimeSettingCommitRequest,
        runtime_adapter: &mut A,
    ) -> SettingsResult<ApplyReport>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        match self.commit_runtime_settings_with_runtime_adapter(vec![request], runtime_adapter)? {
            RuntimeCommitOutcome::Transaction(report) => Ok(*report),
            RuntimeCommitOutcome::NothingRequested => Err(SettingsError::access_failed(
                "runtime commit of a single setting produced no transaction",
            )),
        }
    }

    /// Пропускает несколько runtime-originated значений ОДНОЙ транзакцией: одна валидация,
    /// одна запись config-а, одно применение маршрутов, один sync committed snapshot.
    ///
    /// Пустой список — [`RuntimeCommitOutcome::NothingRequested`] без побочных эффектов.
    pub(crate) fn commit_runtime_settings_with_runtime_adapter<A>(
        &mut self,
        requests: Vec<RuntimeSettingCommitRequest>,
        runtime_adapter: &mut A,
    ) -> SettingsResult<RuntimeCommitOutcome>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        self.invalidate_ui_model();
        let mut unused_route_sequence = None;
        let automatic_surface_pool = self.automatic_surface_pool();
        let outcome = {
            let mut delegate = SettingsRuntimeApplyDelegate {
                validator: AppConfigValidator,
                store: &mut self.store,
                route_appliers: &mut self.route_appliers,
                runtime_adapter,
                applied_route_count: 0,
                automatic_surface_pool,
                route_sequence: &mut unused_route_sequence,
            };
            self.controller
                .commit_runtime_settings(requests, &mut delegate)?
        };
        if let Some(report) = outcome.report() {
            if report.final_state == ApplyFinalState::FullyApplied {
                runtime_adapter.sync_committed_config_snapshot(
                    CommittedConfigSnapshot::from_config(self.controller.committed()),
                );
            }
            self.latest_apply_report = Some(report.clone());
        }
        Ok(outcome)
    }

    /// Test-only apply path with a renderer-neutral adapter.
    #[cfg(test)]
    pub(crate) fn apply_draft<A>(&mut self, render_adapter: &mut A) -> SettingsResult<ApplyReport>
    where
        A: RenderLiveSettingsAdapter,
    {
        let mut runtime_adapter = RenderOnlySettingsRuntimeAdapter { render_adapter };
        self.apply_draft_with_runtime_adapter(&mut runtime_adapter)
    }

    /// Синхронный apply: validate, preflight, runtime commit, persistence, final commit.
    ///
    /// Production идёт многокадровым путём (`begin_apply_with_runtime_adapter`); этот вариант
    /// остаётся для тестов и проверяет, что транзакция без пересборки завершается за один вызов.
    #[cfg(test)]
    pub(crate) fn apply_draft_with_runtime_adapter<A>(
        &mut self,
        runtime_adapter: &mut A,
    ) -> SettingsResult<ApplyReport>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        self.invalidate_ui_model();
        let mut unused_route_sequence = None;
        let automatic_surface_pool = self.automatic_surface_pool();
        let report = {
            let mut delegate = SettingsRuntimeApplyDelegate {
                validator: AppConfigValidator,
                store: &mut self.store,
                route_appliers: &mut self.route_appliers,
                runtime_adapter,
                applied_route_count: 0,
                automatic_surface_pool,
                route_sequence: &mut unused_route_sequence,
            };
            self.controller.apply(&mut delegate)?
        };
        self.conclude_apply(&report, runtime_adapter);
        Ok(report)
    }

    /// Итог транзакции для остальных слоёв: snapshot, последний отчёт и статус панели.
    pub(super) fn conclude_apply<A>(&mut self, report: &ApplyReport, runtime_adapter: &mut A)
    where
        A: SettingsRuntimeReconfigureHost,
    {
        if report.final_state == ApplyFinalState::FullyApplied {
            runtime_adapter.sync_committed_config_snapshot(CommittedConfigSnapshot::from_config(
                self.controller.committed(),
            ));
        }
        self.latest_apply_report = Some(report.clone());
        self.status = status_from_apply_report(&self.setting_labels(), report);
    }
}
