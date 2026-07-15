//! End-to-end settings transaction coordinator for app-owned runtime routes.

use super::*;

/// Делегат связывает neutral controller с store и конкретными runtime owners.
pub(super) struct SettingsRuntimeApplyDelegate<'runtime, A> {
    /// Authoritative full-document validator.
    validator: AppConfigValidator,
    /// Atomic TOML store, owned by `SettingsRuntime`.
    store: &'runtime mut AppConfigStore,
    /// App-owned route snapshots and intent appliers.
    route_appliers: &'runtime mut SettingsRuntimeRouteAppliers,
    /// Renderer/player/media owners behind narrow boundaries.
    runtime_adapter: &'runtime mut A,
    /// Completed route prefix that must be compensated after a failure.
    applied_route_count: usize,
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
        self.store.persist(PersistRequest {
            document: request.document,
            changed_settings: request.changed_settings,
        })
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
        )?;
        let mut reports = Vec::with_capacity(routes.len());
        for route in routes {
            let report = self
                .route_appliers
                .apply_committed_route_with_render_adapter(route, self.runtime_adapter)?;
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
        let rollback_routes = committed_routes_for_updates(
            request.attempted,
            request.previous_committed,
            request.route_updates,
        )?;
        let mut reports = Vec::with_capacity(self.applied_route_count);
        for route in rollback_routes
            .into_iter()
            .take(self.applied_route_count)
            .rev()
        {
            reports.push(
                self.route_appliers
                    .rollback_committed_route_with_render_adapter(route, self.runtime_adapter)?
                    .into_rollback_report(),
            );
        }
        self.applied_route_count = 0;
        Ok(reports)
    }

    fn finalize_committed(&mut self, _request: CommittedFinalizeRequest<'_, AppConfig>) {
        self.runtime_adapter.finalize_settings_transaction();
        self.applied_route_count = 0;
    }
}

impl SettingsRuntime {
    /// Test-only apply path with a renderer-neutral adapter.
    #[cfg(test)]
    pub(crate) fn apply_draft<A>(&mut self, render_adapter: &mut A) -> SettingsResult<ApplyReport>
    where
        A: RenderLiveSettingsAdapter,
    {
        let mut runtime_adapter = RenderOnlySettingsRuntimeAdapter { render_adapter };
        self.apply_draft_with_runtime_adapter(&mut runtime_adapter)
    }

    /// Production apply path: validate, preflight, runtime commit, persistence, final commit.
    pub(crate) fn apply_draft_with_runtime_adapter<A>(
        &mut self,
        runtime_adapter: &mut A,
    ) -> SettingsResult<ApplyReport>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        self.invalidate_ui_model();
        let report = {
            let mut delegate = SettingsRuntimeApplyDelegate {
                validator: AppConfigValidator,
                store: &mut self.store,
                route_appliers: &mut self.route_appliers,
                runtime_adapter,
                applied_route_count: 0,
            };
            self.controller.apply(&mut delegate)?
        };
        if report.final_state == ApplyFinalState::FullyApplied {
            runtime_adapter.sync_committed_config_snapshot(CommittedConfigSnapshot::from_config(
                self.controller.committed(),
            ));
        }
        self.latest_apply_report = Some(report.clone());
        self.status = status_from_apply_report(&report);
        Ok(report)
    }
}
