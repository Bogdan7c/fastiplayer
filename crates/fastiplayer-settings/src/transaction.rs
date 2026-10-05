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

/// Atomic TOML persister backed by `fastiplayer-config::ConfigSaveTarget`.
///
/// Право записи решает config-хранилище при загрузке: если в этот запуск запись
/// запрещена (файл от новой версии, файл не читается), store не пишет диск и сообщает
/// `PersistOutcome::SkippedSessionOnly` — изменения действуют до выхода без отката.
#[derive(Debug, Clone)]
pub struct AppConfigStore {
    save_target: ConfigSaveTarget,
}

impl AppConfigStore {
    /// Creates a writable store for one concrete user config path.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::from_save_target(ConfigSaveTarget::writable(path))
    }

    /// Creates a store that respects the write policy decided when config was loaded.
    #[must_use]
    pub fn from_save_target(save_target: ConfigSaveTarget) -> Self {
        Self { save_target }
    }

    /// Returns the target TOML path used by this store.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.save_target.path()
    }
}

impl SettingsPersister<AppConfig> for AppConfigStore {
    fn persist(&mut self, request: PersistRequest<'_, AppConfig>) -> SettingsResult<PersistReport> {
        // Важный invariant: этот метод не делает partial write сам; вся durability
        // политика и решение «можно ли писать» остаются внутри `fastiplayer-config`.
        let save_outcome = self
            .save_target
            .save(request.document)
            .map_err(settings_error_from_display)?;

        Ok(match save_outcome {
            ConfigSaveOutcome::Saved => PersistReport::persisted(),
            ConfigSaveOutcome::KeptInMemoryOnly(_reason) => PersistReport::kept_for_session_only(),
        })
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

#[cfg(test)]
mod tests {
    //! Режим «без записи» (UX edge cases, сессия 05): изменение настройки применяется
    //! до выхода, файл на диске не трогается, транзакция не откатывается.

    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use fastiplayer_config::{ConfigSavePolicy, ConfigSessionOnlyReason, load_or_recover_at};
    use settings_core::{
        ApplyFinalState, CommittedApplyRequest, CommittedFinalizeRequest, CommittedRollbackRequest,
        PersistOutcome, RollbackReport, SettingValue, SettingsController,
    };

    use super::*;

    /// Минимальный делегат: настоящие validator и store, runtime-маршруты — заглушка,
    /// которая считает откаты (откат здесь означал бы потерю изменения пользователя).
    struct SessionOnlyProbeDelegate {
        store: AppConfigStore,
        rollback_count: usize,
    }

    impl SettingsValidator<AppConfig> for SessionOnlyProbeDelegate {
        fn validate(
            &mut self,
            request: ValidationRequest<'_, AppConfig>,
        ) -> SettingsResult<ValidationReport> {
            AppConfigValidator.validate(request)
        }
    }

    impl SettingsPersister<AppConfig> for SessionOnlyProbeDelegate {
        fn persist(
            &mut self,
            request: PersistRequest<'_, AppConfig>,
        ) -> SettingsResult<PersistReport> {
            self.store.persist(request)
        }
    }

    impl CommittedSettingsApplier<AppConfig> for SessionOnlyProbeDelegate {
        fn preflight_committed(
            &mut self,
            _request: CommittedApplyRequest<'_, AppConfig>,
        ) -> SettingsResult<Vec<ApplyRouteReport>> {
            Ok(Vec::new())
        }

        fn apply_committed(
            &mut self,
            request: CommittedApplyRequest<'_, AppConfig>,
        ) -> SettingsResult<Vec<ApplyRouteReport>> {
            Ok(request
                .route_updates
                .iter()
                .map(|update| {
                    ApplyRouteReport::applied(
                        update.route.clone(),
                        update.affected_settings.clone(),
                    )
                })
                .collect())
        }

        fn rollback_committed(
            &mut self,
            request: CommittedRollbackRequest<'_, AppConfig>,
        ) -> SettingsResult<Vec<RollbackReport>> {
            self.rollback_count += 1;
            Ok(request
                .route_updates
                .iter()
                .map(|update| {
                    RollbackReport::rolled_back(
                        update.route.clone(),
                        update.affected_settings.clone(),
                    )
                })
                .collect())
        }

        fn finalize_committed(&mut self, _request: CommittedFinalizeRequest<'_, AppConfig>) {}
    }

    /// Уникальный временный каталог теста.
    fn unique_test_directory() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "fastiplayer-settings-session-only-{}-{timestamp}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("test directory created");
        directory
    }

    /// Конфиг от новой версии: изменение применено и закоммичено, но не записано.
    #[test]
    fn session_only_store_applies_change_without_writing_or_rollback() {
        let directory = unique_test_directory();
        let config_path = directory.join("config.toml");
        let newer_text = AppConfig::default()
            .to_pretty_toml()
            .expect("defaults serialize")
            .replace("schema_version = 10", "schema_version = 99");
        fs::write(&config_path, &newer_text).expect("newer config written");
        let loaded = load_or_recover_at(&config_path, SystemTime::now()).expect("startup loads");
        assert_eq!(
            loaded.save_policy,
            ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::NewerSchemaVersion {
                found: 99
            })
        );

        let mut controller = SettingsController::new(
            loaded.config.clone(),
            app_config_registry().expect("registry builds"),
        );
        controller
            .set_value(
                SettingId::from("ui.language"),
                SettingValue::Text("en".into()),
            )
            .expect("draft edit succeeds");
        let mut delegate = SessionOnlyProbeDelegate {
            store: AppConfigStore::from_save_target(loaded.save_target()),
            rollback_count: 0,
        };

        let report = controller
            .apply(&mut delegate)
            .expect("apply returns report");

        assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
        assert_eq!(
            report.persistence.map(|persistence| persistence.outcome),
            Some(PersistOutcome::SkippedSessionOnly)
        );
        assert_eq!(delegate.rollback_count, 0);
        // Новое значение действует до выхода.
        assert_eq!(controller.committed().ui.language, "en");
        // Файл новой версии остался байт-в-байт прежним.
        assert_eq!(
            fs::read_to_string(&config_path).expect("config still present"),
            newer_text
        );
        fs::remove_dir_all(&directory).expect("test directory removed");
    }

    /// Обычный store по-прежнему пишет файл.
    #[test]
    fn writable_store_persists_document() {
        let directory = unique_test_directory();
        let config_path = directory.join("config.toml");
        let mut store = AppConfigStore::new(&config_path);
        let mut document = AppConfig::default();
        document.ui.language = "en".into();

        let report = store
            .persist(PersistRequest {
                document: &document,
                changed_settings: &settings_core::SettingsDiff::default(),
            })
            .expect("persist succeeds");

        assert_eq!(report.outcome, PersistOutcome::Persisted);
        let saved = fs::read_to_string(&config_path).expect("saved config exists");
        assert!(saved.contains("language = \"en\""));
        fs::remove_dir_all(&directory).expect("test directory removed");
    }
}
