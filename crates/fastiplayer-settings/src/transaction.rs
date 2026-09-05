//! Транзакционная связка neutral controller contracts с AppConfig validation/persistence/runtime apply.

use super::*;

pub fn app_config_registry() -> SettingsResult<SettingsRegistry<AppConfig>> {
    AppConfig::settings_registry()
}

/// Full-document validator that delegates to the authoritative config layer.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppConfigValidator;

impl SettingsValidator<AppConfig> for AppConfigValidator {
    fn validate(
        &mut self,
        request: ValidationRequest<'_, AppConfig>,
    ) -> SettingsResult<ValidationReport> {
        request
            .draft
            .validate()
            .map_err(settings_error_from_display)?;

        Ok(ValidationReport::valid(setting_ids_from_diff(
            request.changed_settings,
        )))
    }
}

/// Atomic TOML persister backed by `fastiplayer-config::save_validated_atomic_at`.
#[derive(Debug, Clone)]
pub struct AppConfigStore {
    path: PathBuf,
}

impl AppConfigStore {
    /// Creates a store for one concrete user config path.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the target TOML path used by this store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl SettingsPersister<AppConfig> for AppConfigStore {
    fn persist(&mut self, request: PersistRequest<'_, AppConfig>) -> SettingsResult<PersistReport> {
        // Важный invariant: этот метод не делает partial write сам; вся durability
        // политика остаётся внутри `fastiplayer-config`.
        save_validated_atomic_at(&self.path, request.document)
            .map_err(settings_error_from_display)?;

        Ok(PersistReport::persisted())
    }
}

/// Combined transactional delegate for non-egui app-level runtime wiring.
pub struct AppConfigSettingsDelegate<A> {
    validator: AppConfigValidator,
    store: AppConfigStore,
    runtime_applier: A,
    applied_route_count: usize,
}

impl<A> AppConfigSettingsDelegate<A> {
    /// Creates a delegate that validates, preflights, commits runtime, then persists.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, runtime_applier: A) -> Self {
        Self {
            validator: AppConfigValidator,
            store: AppConfigStore::new(path),
            runtime_applier,
            applied_route_count: 0,
        }
    }

    /// Returns the concrete config path used by the persistence delegate.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.store.path()
    }

    /// Returns the wrapped runtime applier.
    #[must_use]
    pub fn runtime_applier(&self) -> &A {
        &self.runtime_applier
    }

    /// Returns the wrapped runtime applier mutably.
    #[must_use]
    pub fn runtime_applier_mut(&mut self) -> &mut A {
        &mut self.runtime_applier
    }

    /// Consumes the delegate and returns the wrapped runtime applier.
    #[must_use]
    pub fn into_runtime_applier(self) -> A {
        self.runtime_applier
    }
}

impl<A> SettingsValidator<AppConfig> for AppConfigSettingsDelegate<A> {
    fn validate(
        &mut self,
        request: ValidationRequest<'_, AppConfig>,
    ) -> SettingsResult<ValidationReport> {
        self.validator.validate(request)
    }
}

impl<A> SettingsPersister<AppConfig> for AppConfigSettingsDelegate<A> {
    fn persist(&mut self, request: PersistRequest<'_, AppConfig>) -> SettingsResult<PersistReport> {
        self.store.persist(request)
    }
}

impl<A> CommittedSettingsApplier<AppConfig> for AppConfigSettingsDelegate<A>
where
    A: AppRuntimeRouteApplier,
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
        Ok(self
            .runtime_applier
            .preflight_committed_routes(&routes)?
            .into_iter()
            .map(AppRouteApplyReport::into_core_report)
            .collect())
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
            let report = self.runtime_applier.apply_committed_route(route)?;
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
                self.runtime_applier
                    .rollback_committed_route(route)?
                    .into_rollback_report(),
            );
        }
        self.applied_route_count = 0;
        Ok(reports)
    }

    fn finalize_committed(&mut self, _request: CommittedFinalizeRequest<'_, AppConfig>) {
        self.runtime_applier.finalize_committed_routes();
        self.applied_route_count = 0;
    }
}
