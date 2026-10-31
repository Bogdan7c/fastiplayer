//! Принятие изменений настроек, уже записанных на диск другой копией плеера («общая доска»,
//! решение 135).
//!
//! Другая копия записала `config.toml`; вызывающий код прочитал файл, сравнил его со своим
//! committed и отфильтровал «запоминаемое состояние» (решение 137). Сюда приходит список
//! «id -> значение». Контроллер применяет его к runtime-владельцам ТЕМ ЖЕ конвейером, что Apply:
//!
//! ```text
//! begin_adopt_external_settings -> validate -> preflight -> begin_apply_committed -> Pending
//!   poll_external_adoption (каждый кадр) -> poll_apply_committed -> ... -> Finished
//!   успех: слияние с draft (план) -> finalize -> commit | сбой: компенсирующий откат
//! ```
//!
//! Отличия от пользовательского Apply:
//! - шага persist НЕТ: значения уже на диске. Делегату не нужен даже `SettingsPersister`, так что
//!   записать config отсюда нельзя на уровне типов;
//! - запрошенный документ строится от committed, а не от draft: незавершённые правки
//!   пользователя не применяются и не коммитятся;
//! - открытый draft сливается по правилу «последний победил»: поле, которое пользователь не
//!   трогал (draft == старый committed), получает новое значение; тронутое сохраняет правку и
//!   победит при следующем Apply пользователя. Остальные поля draft и preview не меняются;
//! - отчёт — отдельный тип [`ExternalAdoptionReport`], чтобы UI не показал «Settings saved» и не
//!   звал хуки после записи.
//!
//! Инварианты:
//! - транзакция одна на контроллер (общий слот `pending_apply`): при идущей транзакции —
//!   [`BeginExternalAdoption::Busy`], состояние не меняется;
//! - неизвестный id, ошибка `set_value` и ошибка слияния обнаруживаются ДО мутаций controller-а;
//! - пока маршрут в полёте, draft, preview и baselines пользователя не подменяются (окно
//!   настроек показывает свои правки), committed и generation не меняются;
//! - при отказе, откате или отмене committed, draft, preview и baselines остаются как были.

use super::super::runtime_commit::AffectedRoute;
use super::super::{
    ApplyFinalState, ApplyReport, ApplyRouteReport, CommittedApplyRequest,
    CommittedFinalizeRequest, CommittedRollbackRequest, CommittedSettingsApplier,
    PendingPreviewUpdate, PreviewTransactionState, RouteApplyUpdate, SettingsController,
    SettingsValidator, push_unique,
};
use super::{
    ApplyPlan, CommittedApplyProgress, PendingApply, StepwiseCommittedApplier, TransactionOrigin,
    all_routes_fully_succeeded, compensate_runtime_apply, route_reports_or_failures,
};
use crate::diff::SettingsDiff;
use crate::error::{SettingsError, SettingsResult};
use crate::metadata::{SettingApplyMode, SettingId, SettingRouteId, SettingValue};

/// Одно значение, которое другая копия плеера уже записала на диск.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalSettingChange {
    /// Stable id настройки.
    pub setting_id: SettingId,

    /// Значение, прочитанное из файла.
    pub value: SettingValue,
}

impl ExternalSettingChange {
    /// Создаёт запись «эта настройка на диске теперь такая».
    #[must_use]
    pub fn new(setting_id: impl Into<SettingId>, value: SettingValue) -> Self {
        Self {
            setting_id: setting_id.into(),
            value,
        }
    }
}

/// Итог [`SettingsController::begin_adopt_external_settings`].
#[derive(Debug, Clone, PartialEq)]
pub enum BeginExternalAdoption {
    /// Транзакция завершилась сразу (только синхронные маршруты, отказ validate/preflight,
    /// ошибка маршрута): принято ли — в [`ExternalAdoptionReport::outcome`].
    Completed(Box<ExternalAdoptionReport>),

    /// Маршрут ждёт (например, пересборка медиа после смены декодера): дальше каждый кадр
    /// вызывать [`SettingsController::poll_external_adoption`].
    Pending,

    /// Идёт другая транзакция (свой Apply или другое принятие); ничего не тронуто. Это
    /// «повторить после её конца», а не ошибка.
    Busy,

    /// Список пуст или все значения уже совпадают с committed (например, это была своя же
    /// запись): транзакция не запускалась, состояние не менялось.
    NothingToAdopt,
}

/// Итог [`SettingsController::poll_external_adoption`].
#[derive(Debug, Clone, PartialEq)]
pub enum PollExternalAdoption {
    /// Принятия в полёте нет (не начиналось, уже завершено или отменено). Пользовательский
    /// Apply этим опросом не продвигается.
    Idle,

    /// Маршрут всё ещё ждёт результата.
    Pending,

    /// Принятие завершилось на этом шаге; отчёт забирается ровно один раз.
    Completed(Box<ExternalAdoptionReport>),
}

/// Короткий вердикт принятия для вызывающего кода; подробности — в
/// [`ExternalAdoptionReport::transaction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalAdoptionOutcome {
    /// Все маршруты применили значения: runtime и committed совпадают с диском по принятым id.
    Adopted,

    /// Отказ до первой мутации владельцев (validate отклонил значения или runtime занят):
    /// runtime и committed прежние. Файл на диске при этом уже содержит чужие значения.
    Refused,

    /// Маршрут отказал или принятие отменено; завершённый префикс откатился. Runtime и
    /// committed прежние, файл на диске — с чужими значениями.
    RolledBack,

    /// Откат сам не удался: runtime может расходиться с committed этой копии.
    RollbackFailed,
}

impl ExternalAdoptionOutcome {
    /// Сводит подробное состояние транзакции к вердикту принятия.
    fn from_final_state(final_state: ApplyFinalState) -> Self {
        match final_state {
            ApplyFinalState::FullyApplied => Self::Adopted,
            // NoChanges сюда не доходит (это `NothingToAdopt`), BlockedByConflicts невозможен
            // (baselines пользователя в транзакцию не попадают): оба значат «ничего не применено».
            ApplyFinalState::NoChanges
            | ApplyFinalState::ValidationFailed
            | ApplyFinalState::RuntimeBlocked
            | ApplyFinalState::BlockedByConflicts => Self::Refused,
            // PersistFailed невозможен (persist нет), но по смыслу это тоже «откатилось».
            ApplyFinalState::RuntimeApplyFailed
            | ApplyFinalState::PersistFailed
            | ApplyFinalState::Cancelled => Self::RolledBack,
            ApplyFinalState::RollbackFailed => Self::RollbackFailed,
        }
    }
}

/// Что стало с открытым пользовательским draft после успешного принятия.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalDraftMerge {
    /// Поля, которые пользователь не трогал: draft получил новое значение.
    pub synchronized_into_draft: Vec<SettingId>,

    /// Поля с несохранённой правкой пользователя, отличной от принятого значения: правка
    /// осталась в draft и победит при его следующем Apply.
    pub kept_user_edits: Vec<SettingId>,
}

/// Отчёт о принятии чужих изменений. Отдельный тип от пользовательского [`ApplyReport`], чтобы
/// вызывающий код не принял его за «настройки сохранены».
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalAdoptionReport {
    /// Принято ли.
    pub outcome: ExternalAdoptionOutcome,

    /// Подробности транзакции; `persistence` всегда `None` — запись делала другая копия.
    pub transaction: ApplyReport,

    /// Слияние с открытым draft; пусто, если принятие не удалось.
    pub draft_merge: ExternalDraftMerge,
}

impl ExternalAdoptionReport {
    /// Отчёт неудавшегося принятия: draft не сливался.
    fn not_adopted(transaction: ApplyReport) -> Self {
        Self {
            outcome: ExternalAdoptionOutcome::from_final_state(transaction.final_state),
            transaction,
            draft_merge: ExternalDraftMerge::default(),
        }
    }
}

/// Результат одного шага принятия: ждём дальше или завершено.
enum AdoptionStep {
    Pending,
    Completed(Box<ExternalAdoptionReport>),
}

/// Заранее посчитанное состояние пользователя после принятия: ставится целиком и без ошибок.
struct DraftMergePlan<D> {
    draft: D,
    preview: PreviewTransactionState<D>,
    affected_routes: Vec<AffectedRoute>,
    summary: ExternalDraftMerge,
}

impl<D> SettingsController<D>
where
    D: Clone + Default,
{
    /// Идёт ли именно принятие чужих изменений (а не пользовательский Apply).
    #[must_use]
    pub fn has_pending_external_adoption(&self) -> bool {
        self.pending_apply
            .as_ref()
            .is_some_and(|pending| pending.origin == TransactionOrigin::ExternalAdoption)
    }

    /// Начинает принятие значений, которые другая копия уже записала на диск.
    ///
    /// Конвейер тот же, что у [`Self::begin_apply`], но без persist. Если один id встречается
    /// несколько раз, побеждает последнее значение. Ошибка (`Err`) — неизвестный id или
    /// значение, которое registry не принимает; она возвращается до любой мутации.
    pub fn begin_adopt_external_settings<A>(
        &mut self,
        changes: Vec<ExternalSettingChange>,
        delegate: &mut A,
    ) -> SettingsResult<BeginExternalAdoption>
    where
        A: SettingsValidator<D> + StepwiseCommittedApplier<D>,
    {
        if changes.is_empty() {
            return Ok(BeginExternalAdoption::NothingToAdopt);
        }
        if self.pending_apply.is_some() {
            return Ok(BeginExternalAdoption::Busy);
        }
        let requested = self.committed_with_external_changes(changes)?;
        let plan = match self.prepare_adoption_in_isolation(&requested, delegate)? {
            Ok(plan) => plan,
            // Все значения уже совпадают с committed: своя запись или повторное событие.
            Err(report) if report.final_state == ApplyFinalState::NoChanges => {
                return Ok(BeginExternalAdoption::NothingToAdopt);
            }
            Err(report) => {
                return Ok(BeginExternalAdoption::Completed(Box::new(
                    ExternalAdoptionReport::not_adopted(*report),
                )));
            }
        };
        let progress = delegate.begin_apply_committed(CommittedApplyRequest {
            previous_committed: &self.committed,
            requested: &requested,
            route_updates: &plan.route_updates,
            changed_settings: &plan.changed_settings,
        });
        let pending = PendingApply {
            plan,
            requested,
            origin: TransactionOrigin::ExternalAdoption,
        };
        Ok(
            match self.settle_external_adoption(delegate, pending, progress) {
                AdoptionStep::Pending => BeginExternalAdoption::Pending,
                AdoptionStep::Completed(report) => BeginExternalAdoption::Completed(report),
            },
        )
    }

    /// Неблокирующе продвигает принятие в полёте; вызывается на каждом кадре UI.
    pub fn poll_external_adoption<A>(&mut self, delegate: &mut A) -> PollExternalAdoption
    where
        A: StepwiseCommittedApplier<D>,
    {
        let Some(pending) = self.take_pending(TransactionOrigin::ExternalAdoption) else {
            return PollExternalAdoption::Idle;
        };
        let progress = delegate.poll_apply_committed(CommittedApplyRequest {
            previous_committed: &self.committed,
            requested: &pending.requested,
            route_updates: &pending.plan.route_updates,
            changed_settings: &pending.plan.changed_settings,
        });
        match self.settle_external_adoption(delegate, pending, progress) {
            AdoptionStep::Pending => PollExternalAdoption::Pending,
            AdoptionStep::Completed(report) => PollExternalAdoption::Completed(report),
        }
    }

    /// Отменяет принятие в полёте (suspend/shutdown): маршрут прерывается, завершённые
    /// откатываются, committed и draft прежние. `None` — отменять нечего. Пользовательский
    /// Apply здесь не отменяется (для него [`Self::cancel_pending_apply`]).
    pub fn cancel_external_adoption<A>(
        &mut self,
        delegate: &mut A,
    ) -> Option<Box<ExternalAdoptionReport>>
    where
        A: StepwiseCommittedApplier<D>,
    {
        let pending = self.take_pending(TransactionOrigin::ExternalAdoption)?;
        let transaction = self.cancel_transaction(delegate, pending);
        Some(Box::new(ExternalAdoptionReport::not_adopted(transaction)))
    }

    /// Committed + чужие значения. Только чтение `self`: ошибка не оставляет следов.
    fn committed_with_external_changes(
        &self,
        changes: Vec<ExternalSettingChange>,
    ) -> SettingsResult<D> {
        let mut requested = self.committed.clone();
        for ExternalSettingChange { setting_id, value } in changes {
            // Registry сам отвечает UnknownSetting / ReadOnlySetting / InvalidValue.
            self.registry
                .set_value(&mut requested, &setting_id, value)?;
        }
        Ok(requested)
    }

    /// Validate + preflight принятия теми же шагами, что у Apply (`prepare_apply`).
    ///
    /// `prepare_apply` читает draft и baselines контроллера. На время подготовки на их месте
    /// документ принятия и пустые baselines: чужие значения не сравниваются с поколениями,
    /// которые захватил черновик пользователя (иначе его правка блокировала бы чужую запись).
    /// Состояние пользователя возвращается до выхода, в том числе при ошибке.
    fn prepare_adoption_in_isolation<A>(
        &mut self,
        requested: &D,
        delegate: &mut A,
    ) -> SettingsResult<Result<ApplyPlan, Box<ApplyReport>>>
    where
        A: SettingsValidator<D> + CommittedSettingsApplier<D>,
    {
        let user_draft = std::mem::replace(&mut self.draft, requested.clone());
        let user_baselines = std::mem::take(&mut self.route_generation_baselines);
        let prepared = self.prepare_apply(delegate);
        self.draft = user_draft;
        self.route_generation_baselines = user_baselines;
        prepared
    }

    /// Развилка шага: ждать дальше или завершить принятие.
    fn settle_external_adoption<A>(
        &mut self,
        delegate: &mut A,
        pending: PendingApply<D>,
        progress: SettingsResult<CommittedApplyProgress>,
    ) -> AdoptionStep
    where
        A: CommittedSettingsApplier<D>,
    {
        let apply_result = match progress {
            Ok(CommittedApplyProgress::Pending) => {
                self.pending_apply = Some(pending);
                return AdoptionStep::Pending;
            }
            Ok(CommittedApplyProgress::Finished(routes)) => Ok(routes),
            Err(error) => Err(error),
        };
        let PendingApply {
            plan, requested, ..
        } = pending;
        AdoptionStep::Completed(Box::new(self.finish_external_adoption(
            delegate,
            plan,
            requested,
            apply_result,
        )))
    }

    /// Хвост принятия: откат при сбое, иначе слияние (план) -> finalize -> commit. Без persist.
    fn finish_external_adoption<A>(
        &mut self,
        delegate: &mut A,
        plan: ApplyPlan,
        requested: D,
        apply_result: SettingsResult<Vec<ApplyRouteReport>>,
    ) -> ExternalAdoptionReport
    where
        A: CommittedSettingsApplier<D>,
    {
        let ApplyPlan {
            changed_settings,
            route_updates,
            validation,
        } = plan;
        let mut errors = Vec::new();
        let routes = route_reports_or_failures(&route_updates, apply_result, &mut errors);

        // Слияние считается ДО мутаций: если оно невозможно, чужие значения откатываются,
        // а не коммитятся с испорченным черновиком.
        let merge = if all_routes_fully_succeeded(&routes) {
            match self.plan_draft_merge(&requested, &changed_settings, &route_updates) {
                Ok(merge) => Some(merge),
                Err(error) => {
                    errors.push(error);
                    None
                }
            }
        } else {
            None
        };
        let Some(merge) = merge else {
            let (rollback, final_state) = compensate_runtime_apply(
                delegate,
                CommittedRollbackRequest {
                    previous_committed: &self.committed,
                    attempted: &requested,
                    route_updates: &route_updates,
                    changed_settings: &changed_settings,
                },
                ApplyFinalState::RuntimeApplyFailed,
                &mut errors,
            );
            return ExternalAdoptionReport::not_adopted(ApplyReport {
                validation: Some(validation),
                persistence: None,
                routes,
                rollback,
                changed_settings,
                final_state,
                conflicts: Vec::new(),
                errors,
            });
        };

        delegate.finalize_committed(CommittedFinalizeRequest {
            previous_committed: &self.committed,
            committed: &requested,
            route_updates: &route_updates,
            changed_settings: &changed_settings,
        });
        let draft_merge = self.install_adopted_document(requested, &route_updates, merge);
        ExternalAdoptionReport {
            outcome: ExternalAdoptionOutcome::Adopted,
            transaction: ApplyReport {
                validation: Some(validation),
                persistence: None,
                routes,
                rollback: Vec::new(),
                changed_settings,
                final_state: ApplyFinalState::FullyApplied,
                conflicts: Vec::new(),
                errors,
            },
            draft_merge,
        }
    }

    /// Считает draft/preview пользователя после принятия, ничего не меняя в `self`.
    ///
    /// Правило для каждого принятого id (`before` — старый committed, `after` — принятое):
    /// - draft == before (пользователь не трогал): новое значение получают draft и все
    ///   preview-снимки;
    /// - иначе правка пользователя остаётся в draft и в preview-документах, а новое значение
    ///   получает только точка отката preview (`baseline_document`): runtime теперь держит его,
    ///   и «Отмена» должна вернуть именно его.
    fn plan_draft_merge(
        &self,
        requested: &D,
        changed_settings: &SettingsDiff,
        route_updates: &[RouteApplyUpdate],
    ) -> SettingsResult<DraftMergePlan<D>> {
        let mut draft = self.draft.clone();
        let mut preview = self.preview.clone();
        let mut summary = ExternalDraftMerge::default();
        for change in changed_settings.changes() {
            let user_value = self.registry.get_value(&self.draft, &change.id)?;
            if user_value == change.before {
                self.registry
                    .set_value(&mut draft, &change.id, change.after.clone())?;
                self.synchronize_runtime_value_in_preview(&mut preview, &change.id, &change.after)?;
                summary.synchronized_into_draft.push(change.id.clone());
            } else {
                self.synchronize_preview_rollback_point(&mut preview, &change.id, &change.after)?;
                if user_value != change.after {
                    summary.kept_user_edits.push(change.id.clone());
                }
            }
        }
        self.reassert_user_preview_on_adopted_routes(
            &mut preview,
            &draft,
            requested,
            route_updates,
        )?;

        let mut affected_routes = Vec::with_capacity(route_updates.len());
        for update in route_updates {
            affected_routes.push(AffectedRoute {
                route: update.route.clone(),
                generation_before_commit: self.route_generation(&update.route),
                still_has_draft_changes: self.document_has_changes_for_route(
                    requested,
                    &draft,
                    &update.route,
                )?,
            });
        }
        Ok(DraftMergePlan {
            draft,
            preview,
            affected_routes,
            summary,
        })
    }

    /// Новое runtime-значение тронутого пользователем поля становится точкой отката preview.
    fn synchronize_preview_rollback_point(
        &self,
        preview: &mut PreviewTransactionState<D>,
        setting_id: &SettingId,
        value: &SettingValue,
    ) -> SettingsResult<()> {
        for state in preview.routes.values_mut() {
            self.registry
                .set_value(&mut state.baseline_document, setting_id, value.clone())?;
        }
        Ok(())
    }

    /// Ставит в очередь preview заново для маршрут принятия, где у пользователя есть
    /// незавершённые preview-правки.
    ///
    /// Committed-маршрут применяет весь запрошенный документ своей маршрута, то есть затирает в
    /// runtime и то, что пользователь сейчас видит как preview (например, яркость, пока другая
    /// копия поменяла соседний цвет). Повторная preview-заявка возвращает на экран его правки.
    fn reassert_user_preview_on_adopted_routes(
        &self,
        preview: &mut PreviewTransactionState<D>,
        merged_draft: &D,
        requested: &D,
        route_updates: &[RouteApplyUpdate],
    ) -> SettingsResult<()> {
        let remaining_user_edits = self.registry.diff(requested, merged_draft)?;
        for update in route_updates {
            let Some(route_state) = preview.routes.get_mut(&update.route) else {
                continue;
            };
            let affected_settings =
                self.preview_edits_on_route(&remaining_user_edits, &update.route)?;
            if affected_settings.is_empty() {
                continue;
            }
            for setting_id in &affected_settings {
                push_unique(&mut route_state.affected_settings, setting_id.clone());
            }
            // Поколение — как у обычной preview-заявки; rebase после коммита перенесёт его.
            let baseline_generation = self
                .route_generation_baselines
                .get(&update.route)
                .copied()
                .unwrap_or_default();
            preview.pending.insert(
                update.route.clone(),
                PendingPreviewUpdate {
                    route: update.route.clone(),
                    document: merged_draft.clone(),
                    affected_settings,
                    baseline_generation,
                },
            );
        }
        Ok(())
    }

    /// Preview-настройки маршрута, по которым у пользователя осталась правка.
    fn preview_edits_on_route(
        &self,
        user_edits: &SettingsDiff,
        route: &SettingRouteId,
    ) -> SettingsResult<Vec<SettingId>> {
        let mut preview_edits = Vec::new();
        for change in user_edits.changes() {
            let descriptor = self.registry.descriptor(&change.id).ok_or_else(|| {
                SettingsError::UnknownSetting {
                    id: change.id.clone(),
                }
            })?;
            if &descriptor.route == route
                && descriptor.apply_mode == SettingApplyMode::ImmediatePreview
            {
                preview_edits.push(change.id.clone());
            }
        }
        Ok(preview_edits)
    }

    /// Коммитит принятый документ и ставит заранее посчитанное состояние пользователя.
    /// Ошибок здесь быть не может: всё fallible посчитано в [`Self::plan_draft_merge`].
    fn install_adopted_document(
        &mut self,
        requested: D,
        route_updates: &[RouteApplyUpdate],
        merge: DraftMergePlan<D>,
    ) -> ExternalDraftMerge {
        // `commit_full_success` коммитит draft и сбрасывает baselines/preview; baselines
        // пользователя сохраняем, чтобы перебазировать их, а не потерять.
        let user_baselines = std::mem::take(&mut self.route_generation_baselines);
        self.draft = requested;
        self.commit_full_success(route_updates.iter().map(|update| update.route.clone()));
        self.draft = merge.draft;
        self.preview = merge.preview;
        self.route_generation_baselines = user_baselines;
        for affected in &merge.affected_routes {
            self.rebase_after_runtime_commit(affected);
        }
        merge.summary
    }
}
