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
/// Config — «общая доска» нескольких копий плеера (решение 135): сохранение под замком
/// перечитывает файл и вписывает только изменённые транзакцией настройки
/// (`shared_persist.rs`), поэтому соседняя копия не теряет свои ключи.
///
/// Право записи решает config-хранилище при загрузке: если в этот запуск запись
/// запрещена (файл от новой версии, файл не читается), store не пишет диск и сообщает
/// `PersistOutcome::SkippedSessionOnly` — изменения действуют до выхода без отката.
///
/// Если запись разрешена, но сейчас не удалась (файл занят, битый, новее…), транзакция
/// тоже не откатывается: store помнит id несохранённых настроек и дописывает их при
/// следующей успешной записи. Как прочитать такой отчёт — [`settings_file_persist_outcome`].
///
/// Store владеет знанием об общем файле: какой документ диска эта копия видела последним
/// (старт, своя запись, чтение «общей доски» — [`AppConfigStore::note_disk_config_seen`]).
/// От него считается, кто записал ключ позже — мы или сосед (`shared_persist/unsaved.rs`).
#[derive(Debug, Clone)]
pub struct AppConfigStore {
    save_target: ConfigSaveTarget,
    /// Сколько ждать замок записи config-а до отказа «файл занят».
    lock_wait: Duration,
    /// Последний виденный документ диска и изменения, ещё не записанные в файл.
    knowledge: SharedFileKnowledge,
}

impl AppConfigStore {
    /// Creates a writable store for one concrete user config path.
    ///
    /// Store не видел документа диска: первый увиденный документ станет базой для
    /// несохранённых изменений (см. `shared_persist/unsaved.rs`).
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::from_save_target(ConfigSaveTarget::writable(path))
    }

    /// Creates a store that respects the write policy decided when config was loaded.
    ///
    /// Без знания о том, что лежит на диске; приложение создаёт store через
    /// [`AppConfigStore::from_loaded_config`].
    #[must_use]
    pub fn from_save_target(save_target: ConfigSaveTarget) -> Self {
        Self {
            save_target,
            lock_wait: SETTINGS_FILE_LOCK_WAIT,
            knowledge: SharedFileKnowledge::starting_from(None),
        }
    }

    /// Store по итогу загрузки: право записи и документ, который лежит на диске (если он
    /// известен — `LoadedConfig::config_seen_on_disk`).
    #[must_use]
    pub fn from_loaded_config(loaded_config: &LoadedConfig) -> Self {
        Self {
            save_target: loaded_config.save_target(),
            lock_wait: SETTINGS_FILE_LOCK_WAIT,
            knowledge: SharedFileKnowledge::starting_from(loaded_config.config_seen_on_disk()),
        }
    }

    /// Задаёт срок ожидания замка записи (по умолчанию [`SETTINGS_FILE_LOCK_WAIT`]).
    #[must_use]
    pub fn with_lock_wait(mut self, lock_wait: Duration) -> Self {
        self.lock_wait = lock_wait;
        self
    }

    /// Returns the target TOML path used by this store.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.save_target.path()
    }

    /// Право записи в этот запуск (может смениться на запись после
    /// [`AppConfigStore::note_disk_config_seen`]).
    #[must_use]
    pub fn save_policy(&self) -> &ConfigSavePolicy {
        self.save_target.policy()
    }

    /// Есть ли применённые изменения, которые ещё не записаны в файл.
    #[must_use]
    pub fn has_unsaved_changes(&self) -> bool {
        self.knowledge.has_unsaved()
    }

    /// Id несохранённых настроек (для текста и диагностики, без значений).
    #[must_use]
    pub fn unsaved_setting_ids(&self) -> Vec<SettingId> {
        self.knowledge.unsaved_setting_ids()
    }

    /// Копия прочитала config-файл, и он нормальный (`SharedConfigRead::Parsed`).
    ///
    /// Звать после **каждого** успешного чтения файла «общей доской» (до дописывания
    /// несохранённого и до принятия значений соседа):
    /// - документ становится «последним виденным» — от него считаются базы следующих
    ///   несохранённых изменений;
    /// - если запись была запрещена восстановимой причиной (битый файл, недоступный замок
    ///   при старте…), запрет снимается: итог [`ConfigWriteResumption::Resumed`]. После
    ///   этого несохранённое пора дописать ([`AppConfigStore::save_unsaved_changes`]).
    ///
    /// Ошибка — только отказ реестра (ошибка программы); тогда store ничего не меняет.
    pub fn note_disk_config_seen(
        &mut self,
        disk_document: &AppConfig,
    ) -> SettingsResult<ConfigWriteResumption> {
        note_disk_config_seen(&mut self.save_target, &mut self.knowledge, disk_document)
    }

    /// Дописывает несохранённые изменения в файл, значения берёт из `current`.
    ///
    /// Звать, когда файл снова можно записать (слежение увидело исправный файл, после
    /// принятия значений соседа). `current` — committed документ этой копии. Значение,
    /// которое после нашего изменения записала другая копия, не затирается (правило в
    /// `shared_persist/unsaved.rs`).
    pub fn save_unsaved_changes(&mut self, current: &AppConfig) -> UnsavedChangesSave {
        save_unsaved_settings(
            &self.save_target,
            self.lock_wait,
            &mut self.knowledge,
            current,
        )
    }
}

impl SettingsPersister<AppConfig> for AppConfigStore {
    fn persist(&mut self, request: PersistRequest<'_, AppConfig>) -> SettingsResult<PersistReport> {
        // Важный invariant: этот метод не делает partial write сам; замок, перечитывание,
        // durability и решение «можно ли писать» остаются внутри `fastiplayer-config`,
        // а здесь только «какие ключи вписать» (изменённые транзакцией и несохранённые).
        persist_changed_settings(
            &self.save_target,
            self.lock_wait,
            &mut self.knowledge,
            request,
        )
    }
}

/// Combined transactional delegate for non-egui app-level runtime wiring.
pub struct AppConfigSettingsDelegate<A> {
    validator: AppConfigValidator,
    store: AppConfigStore,
    runtime_applier: A,
    /// Размер пула поверхностей активного рендерера для значения «авто» (0) в config.
    automatic_surface_pool: AutomaticSurfacePool,
    applied_route_count: usize,
}

impl<A> AppConfigSettingsDelegate<A> {
    /// Creates a delegate that validates, preflights, commits runtime, then persists.
    #[must_use]
    pub fn new(
        path: impl Into<PathBuf>,
        runtime_applier: A,
        automatic_surface_pool: AutomaticSurfacePool,
    ) -> Self {
        Self {
            validator: AppConfigValidator,
            store: AppConfigStore::new(path),
            runtime_applier,
            automatic_surface_pool,
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
            self.automatic_surface_pool,
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
            self.automatic_surface_pool,
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
            self.automatic_surface_pool,
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
            .replace("schema_version = 30", "schema_version = 99");
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

    /// Обычный store по-прежнему пишет файл: изменённая настройка попадает на диск.
    #[test]
    fn writable_store_persists_document() {
        let directory = unique_test_directory();
        let config_path = directory.join("config.toml");
        let mut store = AppConfigStore::new(&config_path);
        let mut document = AppConfig::default();
        document.ui.language = "en".into();
        let changed_settings = app_config_registry()
            .expect("registry builds")
            .diff(&AppConfig::default(), &document)
            .expect("diff builds");

        let report = store
            .persist(PersistRequest {
                document: &document,
                changed_settings: &changed_settings,
            })
            .expect("persist succeeds");

        assert_eq!(report.outcome, PersistOutcome::Persisted);
        let saved = fs::read_to_string(&config_path).expect("saved config exists");
        assert!(saved.contains("language = \"en\""));
        fs::remove_dir_all(&directory).expect("test directory removed");
    }
}
