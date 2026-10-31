//! Runtime-originated commit: одно или несколько значений в ОДНОЙ транзакции.
//!
//! Runtime (например, экранный редактор субтитров) знает новые значения нескольких полей и
//! хочет записать их как единое целое: одна валидация, одна запись config-а, одно применение
//! маршрутов. Единственное значение — частный случай батча из одного элемента, поэтому
//! [`SettingsController::commit_runtime_setting`] делегирует сюда, и путь коммита один.
//!
//! Инварианты:
//! - транзакция строится от последнего committed document-а: чужие незавершённые правки draft-а
//!   не попадают в persistence;
//! - неизвестный id или ошибка `set_value` обнаруживаются ДО любой мутации состояния;
//! - на успехе в draft/preview заменяются только поля запроса, соседние правки сохраняются;
//! - baseline generation пересчитывается для КАЖДОЙ затронутой маршрута (не только первой);
//! - при отказе/откате draft, preview и baselines возвращаются точно как были.

use super::{
    ApplyFinalState, ApplyReport, CommittedSettingsApplier, PreviewTransactionState,
    RouteGeneration, RuntimeSettingCommitRequest, SettingsController, SettingsPersister,
    SettingsValidator,
};
use crate::error::{SettingsError, SettingsResult};
use crate::metadata::{SettingId, SettingRouteId, SettingValue};

/// Итог батч-коммита: пустой запрос не маскируется под успешную транзакцию.
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeCommitOutcome {
    /// Запрос не содержал значений: валидация, запись и apply не запускались.
    NothingRequested,
    /// Транзакция выполнялась; итог (в том числе откат) — в отчёте.
    Transaction(Box<ApplyReport>),
}

impl RuntimeCommitOutcome {
    /// Отчёт транзакции, если она запускалась.
    #[must_use]
    pub fn report(&self) -> Option<&ApplyReport> {
        match self {
            Self::NothingRequested => None,
            Self::Transaction(report) => Some(report),
        }
    }
}

/// Всё, что нужно для успешного исхода и откатывается целиком при неуспехе.
struct SavedTransactionState<D> {
    draft: D,
    preview: PreviewTransactionState<D>,
    route_generation_baselines: std::collections::BTreeMap<SettingRouteId, RouteGeneration>,
}

/// Маршрут, затронутый батчем, с данными для перебазирования generation.
///
/// Общий с принятием чужих изменений (`pending_apply/external_adoption.rs`): там коммит тоже
/// приходит не из draft, и соседние правки draft перебазируются тем же правилом.
pub(super) struct AffectedRoute {
    pub(super) route: SettingRouteId,
    pub(super) generation_before_commit: RouteGeneration,
    /// После коммита в draft остались изменения этой маршрута (соседние поля).
    pub(super) still_has_draft_changes: bool,
}

impl<D> SettingsController<D>
where
    D: Clone + Default,
{
    /// Атомарно коммитит одно runtime-originated значение через обычный transaction pipeline.
    ///
    /// Порядок как у [`Self::apply`]: validation -> runtime apply -> atomic persistence ->
    /// finalize либо compensating rollback. Открытый пользовательский draft и preview не
    /// уничтожаются: заменяется только `request.setting_id`.
    pub fn commit_runtime_setting<A>(
        &mut self,
        request: RuntimeSettingCommitRequest,
        delegate: &mut A,
    ) -> SettingsResult<ApplyReport>
    where
        A: SettingsValidator<D> + SettingsPersister<D> + CommittedSettingsApplier<D>,
    {
        match self.commit_runtime_settings(vec![request], delegate)? {
            RuntimeCommitOutcome::Transaction(report) => Ok(*report),
            // Батч из одного элемента не бывает пустым; ветка остаётся явной ошибкой, а не паникой.
            RuntimeCommitOutcome::NothingRequested => Err(SettingsError::access_failed(
                "runtime commit of a single setting produced no transaction",
            )),
        }
    }

    /// Коммитит несколько runtime-значений одной транзакцией (одна запись config-а).
    ///
    /// Если один id встречается несколько раз, побеждает последнее значение. Пустой список —
    /// [`RuntimeCommitOutcome::NothingRequested`], состояние не меняется.
    pub fn commit_runtime_settings<A>(
        &mut self,
        requests: Vec<RuntimeSettingCommitRequest>,
        delegate: &mut A,
    ) -> SettingsResult<RuntimeCommitOutcome>
    where
        A: SettingsValidator<D> + SettingsPersister<D> + CommittedSettingsApplier<D>,
    {
        if self.pending_apply.is_some() {
            // Runtime-commit подменяет draft: во время stepwise-транзакции это запрещено.
            return Err(SettingsError::access_failed(
                "another settings transaction is still applying",
            ));
        }
        if requests.is_empty() {
            return Ok(RuntimeCommitOutcome::NothingRequested);
        }

        // Фаза 1 (без мутаций self): всё fallible готовим заранее.
        let mut requested_document = self.committed.clone();
        let mut requested_ids: Vec<SettingId> = Vec::new();
        let mut route_ids: Vec<SettingRouteId> = Vec::new();
        for RuntimeSettingCommitRequest { setting_id, value } in requests {
            let route = self
                .registry
                .descriptor(&setting_id)
                .ok_or_else(|| SettingsError::UnknownSetting {
                    id: setting_id.clone(),
                })?
                .route
                .clone();
            self.registry
                .set_value(&mut requested_document, &setting_id, value)?;
            if !requested_ids.contains(&setting_id) {
                requested_ids.push(setting_id);
            }
            if !route_ids.contains(&route) {
                route_ids.push(route);
            }
        }
        // Нормализованные значения берём один раз после всех set_value (учёт дубликатов).
        let mut normalized_values: Vec<(SettingId, SettingValue)> = Vec::new();
        for setting_id in requested_ids {
            let value = self.registry.get_value(&requested_document, &setting_id)?;
            normalized_values.push((setting_id, value));
        }

        let saved = SavedTransactionState {
            draft: self.draft.clone(),
            preview: self.preview.clone(),
            route_generation_baselines: self.route_generation_baselines.clone(),
        };
        let mut synchronized_draft = saved.draft.clone();
        let mut synchronized_preview = saved.preview.clone();
        for (setting_id, value) in &normalized_values {
            self.registry
                .set_value(&mut synchronized_draft, setting_id, value.clone())?;
            self.synchronize_runtime_value_in_preview(
                &mut synchronized_preview,
                setting_id,
                value,
            )?;
        }
        let mut affected_routes = Vec::with_capacity(route_ids.len());
        for route in route_ids {
            affected_routes.push(AffectedRoute {
                generation_before_commit: self.route_generation(&route),
                still_has_draft_changes: self.document_has_changes_for_route(
                    &requested_document,
                    &synchronized_draft,
                    &route,
                )?,
                route,
            });
        }

        // Фаза 2: существующий apply pipeline получает изолированный draft только из запроса.
        self.draft = requested_document;
        self.preview = PreviewTransactionState::default();
        self.route_generation_baselines.clear();
        let apply_result = self.apply(delegate);

        match apply_result {
            Ok(report)
                if matches!(
                    report.final_state,
                    ApplyFinalState::FullyApplied | ApplyFinalState::NoChanges
                ) =>
            {
                self.draft = synchronized_draft;
                self.preview = synchronized_preview;
                self.route_generation_baselines = saved.route_generation_baselines;
                for affected in &affected_routes {
                    self.rebase_after_runtime_commit(affected);
                }
                Ok(RuntimeCommitOutcome::Transaction(Box::new(report)))
            }
            Ok(report) => {
                self.restore_saved_state(saved);
                Ok(RuntimeCommitOutcome::Transaction(Box::new(report)))
            }
            Err(error) => {
                self.restore_saved_state(saved);
                Err(error)
            }
        }
    }

    /// Возвращает draft/preview/baselines в состояние до runtime-коммита.
    fn restore_saved_state(&mut self, saved: SavedTransactionState<D>) {
        self.draft = saved.draft;
        self.preview = saved.preview;
        self.route_generation_baselines = saved.route_generation_baselines;
    }

    /// Перебазирует generation одной маршрута после успешного runtime-коммита.
    ///
    /// Коммит увеличил generation маршрута. Если соседнее draft-поле той же маршрута всё ещё
    /// изменено, оно должно сравниваться с новым поколением, а не давать ложный conflict. Уже
    /// stale baseline не перебазируется: runtime intent не вправе скрывать настоящий conflict.
    pub(super) fn rebase_after_runtime_commit(&mut self, affected: &AffectedRoute) {
        let current_generation = self.route_generation(&affected.route);
        if affected.still_has_draft_changes {
            match self
                .route_generation_baselines
                .get(&affected.route)
                .copied()
            {
                Some(baseline) if baseline != affected.generation_before_commit => {}
                _ => {
                    self.route_generation_baselines
                        .insert(affected.route.clone(), current_generation);
                }
            }
        } else {
            self.route_generation_baselines.remove(&affected.route);
        }
        self.preview.rebase_route_generation(
            &affected.route,
            affected.generation_before_commit,
            current_generation,
        );
    }
}
