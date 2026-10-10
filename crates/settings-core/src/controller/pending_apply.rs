//! Многокадровая (stepwise) транзакция применения настроек.
//!
//! Обычный [`SettingsController::apply`] синхронный: `apply_committed` обязан вернуть итог сразу.
//! Некоторым владельцам (пересборка media после смены сетевых настроек) итог нужен не сразу, а
//! через несколько кадров UI. Этот модуль добавляет транзакции явную фазу «ждём маршрут»:
//!
//! ```text
//! begin_apply -> validate -> preflight -> begin_apply_committed -> Pending
//!   poll_apply (каждый кадр) -> poll_apply_committed -> ... -> Finished
//!   finish_apply -> (rollback) | persist -> finalize -> commit
//! ```
//!
//! Инварианты:
//! - persist/finalize/commit происходят ТОЛЬКО после того, как все маршруты завершились успехом
//!   (общий `finish_apply`, тот же, что у синхронного `apply`);
//! - пока транзакция в полёте, committed документ и generation не меняются;
//! - транзакция одна: повторный `begin_apply` возвращает [`BeginApply::Busy`], первая цела;
//! - порядок маршрутов, откат завершённого префикса и отчёты те же, что у синхронного пути.

use super::PersistReport;
use super::{
    ApplyFinalState, ApplyMechanism, ApplyReport, ApplyRouteReport, ApplyRouteResult,
    CommittedApplyRequest, CommittedFinalizeRequest, CommittedRollbackRequest,
    CommittedSettingsApplier, PersistRequest, RollbackReport, RollbackResult, RouteApplyUpdate,
    SettingsController, SettingsPersister, SettingsValidator, ValidationReport, ValidationRequest,
};
use crate::diff::SettingsDiff;
use crate::error::SettingsResult;

/// Промежуточный итог одного шага применения маршрутов у stepwise-владельца.
#[derive(Debug, Clone, PartialEq)]
pub enum CommittedApplyProgress {
    /// Все маршруты отработали (успешно или с ошибкой): отчёты в порядке применения.
    Finished(Vec<ApplyRouteReport>),
    /// Какой-то маршрут ждёт внешнего результата; опрос повторится на следующем кадре.
    Pending,
}

/// Владелец runtime-маршрутов, умеющий применять их по шагам.
///
/// Реализация хранит собственное состояние между вызовами (какой маршрут в полёте, сколько
/// маршрутов завершено): controller этого состояния не знает и не читает.
pub trait StepwiseCommittedApplier<D>: CommittedSettingsApplier<D> {
    /// Начинает применение маршрутов; синхронные маршруты завершаются прямо здесь.
    fn begin_apply_committed(
        &mut self,
        request: CommittedApplyRequest<'_, D>,
    ) -> SettingsResult<CommittedApplyProgress>;

    /// Неблокирующе продвигает маршрут в полёте и следующие за ним.
    fn poll_apply_committed(
        &mut self,
        request: CommittedApplyRequest<'_, D>,
    ) -> SettingsResult<CommittedApplyProgress>;

    /// Прерывает маршрут в полёте и откатывает уже завершённые маршруты в обратном порядке.
    fn cancel_apply_committed(
        &mut self,
        request: CommittedRollbackRequest<'_, D>,
    ) -> SettingsResult<Vec<RollbackReport>>;
}

/// Итог [`SettingsController::begin_apply`].
#[derive(Debug, Clone, PartialEq)]
pub enum BeginApply {
    /// Транзакция завершилась сразу (нет изменений, отказ validate/preflight, только
    /// синхронные маршруты, ошибка маршрута): отчёт такой же, как у синхронного `apply`.
    Completed(ApplyReport),
    /// Транзакция ждёт маршрут; дальше вызывать [`SettingsController::poll_apply`].
    Pending,
    /// Другая транзакция ещё в полёте и осталась нетронутой: это «занято», а не ошибка.
    Busy,
}

/// Итог [`SettingsController::poll_apply`].
#[derive(Debug, Clone, PartialEq)]
pub enum PollApply {
    /// Транзакции в полёте нет (не начиналась, уже завершена или отменена).
    Idle,
    /// Маршрут всё ещё ждёт результата.
    Pending,
    /// Транзакция завершилась на этом шаге; забирается ровно один раз.
    Completed(ApplyReport),
}

/// Что известно после validate + preflight и нужно до самого конца транзакции.
pub(super) struct ApplyPlan {
    pub(super) changed_settings: SettingsDiff,
    pub(super) route_updates: Vec<RouteApplyUpdate>,
    pub(super) validation: ValidationReport,
}

/// Транзакция в полёте. Документ `requested` зафиксирован на старте: коммитится ровно то,
/// что применялось, даже если draft кто-то изменит (владелец UI этого не делает).
pub(super) struct PendingApply<D> {
    plan: ApplyPlan,
    requested: D,
}

/// Результат одного шага: транзакция либо ждёт, либо завершена.
enum Settled {
    Pending,
    Completed(ApplyReport),
}

impl<D> SettingsController<D>
where
    D: Clone + Default,
{
    /// Есть ли транзакция, которая ждёт маршрут.
    #[must_use]
    pub fn has_pending_apply(&self) -> bool {
        self.pending_apply.is_some()
    }

    /// Начинает транзакцию Apply, которая может занять несколько кадров.
    ///
    /// Порядок и отчёты до применения маршрутов совпадают с [`Self::apply`].
    pub fn begin_apply<A>(&mut self, delegate: &mut A) -> SettingsResult<BeginApply>
    where
        A: SettingsValidator<D> + SettingsPersister<D> + StepwiseCommittedApplier<D>,
    {
        if self.pending_apply.is_some() {
            return Ok(BeginApply::Busy);
        }
        let plan = match self.prepare_apply(delegate)? {
            Ok(plan) => plan,
            Err(report) => return Ok(BeginApply::Completed(*report)),
        };
        let requested = self.draft.clone();
        let progress = delegate.begin_apply_committed(CommittedApplyRequest {
            previous_committed: &self.committed,
            requested: &requested,
            route_updates: &plan.route_updates,
            changed_settings: &plan.changed_settings,
        });
        Ok(
            match self.settle(delegate, PendingApply { plan, requested }, progress) {
                Settled::Pending => BeginApply::Pending,
                Settled::Completed(report) => BeginApply::Completed(report),
            },
        )
    }

    /// Неблокирующе продвигает транзакцию в полёте; вызывается на каждом кадре UI.
    pub fn poll_apply<A>(&mut self, delegate: &mut A) -> SettingsResult<PollApply>
    where
        A: SettingsValidator<D> + SettingsPersister<D> + StepwiseCommittedApplier<D>,
    {
        let Some(pending) = self.pending_apply.take() else {
            return Ok(PollApply::Idle);
        };
        let progress = delegate.poll_apply_committed(CommittedApplyRequest {
            previous_committed: &self.committed,
            requested: &pending.requested,
            route_updates: &pending.plan.route_updates,
            changed_settings: &pending.plan.changed_settings,
        });
        Ok(match self.settle(delegate, pending, progress) {
            Settled::Pending => PollApply::Pending,
            Settled::Completed(report) => PollApply::Completed(report),
        })
    }

    /// Отменяет транзакцию в полёте (shutdown/suspend): без persist и без finalize.
    ///
    /// Маршрут в полёте прерывается, завершённые маршруты откатываются; committed документ
    /// остаётся прежним. Возвращает `None`, если отменять нечего.
    pub fn cancel_pending_apply<A>(&mut self, delegate: &mut A) -> Option<ApplyReport>
    where
        A: StepwiseCommittedApplier<D>,
    {
        let pending = self.pending_apply.take()?;
        let mut errors = Vec::new();
        let rollback = match delegate.cancel_apply_committed(CommittedRollbackRequest {
            previous_committed: &self.committed,
            attempted: &pending.requested,
            route_updates: &pending.plan.route_updates,
            changed_settings: &pending.plan.changed_settings,
        }) {
            Ok(reports) => reports,
            Err(error) => {
                errors.push(error);
                Vec::new()
            }
        };
        let rollback_failed = !errors.is_empty()
            || rollback
                .iter()
                .any(|report| matches!(report.result, RollbackResult::Failed { .. }));
        let PendingApply { plan, .. } = pending;
        Some(ApplyReport {
            validation: Some(plan.validation),
            persistence: None,
            routes: Vec::new(),
            rollback,
            changed_settings: plan.changed_settings,
            final_state: if rollback_failed {
                ApplyFinalState::RollbackFailed
            } else {
                ApplyFinalState::Cancelled
            },
            conflicts: Vec::new(),
            errors,
        })
    }

    /// Общая развилка шага: ждать дальше или завершить транзакцию общим хвостом.
    fn settle<A>(
        &mut self,
        delegate: &mut A,
        pending: PendingApply<D>,
        progress: SettingsResult<CommittedApplyProgress>,
    ) -> Settled
    where
        A: SettingsPersister<D> + CommittedSettingsApplier<D>,
    {
        let apply_result = match progress {
            Ok(CommittedApplyProgress::Pending) => {
                self.pending_apply = Some(pending);
                return Settled::Pending;
            }
            Ok(CommittedApplyProgress::Finished(routes)) => Ok(routes),
            Err(error) => Err(error),
        };
        let PendingApply { plan, requested } = pending;
        // Коммитится ровно примененный документ (см. инвариант `PendingApply`).
        self.draft = requested;
        Settled::Completed(self.finish_apply(delegate, plan, apply_result))
    }

    /// Validate + generation conflicts + preflight: всё, что идёт до первой мутации владельцев.
    ///
    /// `Err(report)` внутри — ранний итог транзакции (нет изменений, конфликт, отказ validate
    /// или preflight); `Ok(plan)` — можно применять маршруты.
    pub(super) fn prepare_apply<A>(
        &mut self,
        delegate: &mut A,
    ) -> SettingsResult<Result<ApplyPlan, Box<ApplyReport>>>
    where
        A: SettingsValidator<D> + CommittedSettingsApplier<D>,
    {
        let changed_settings = self.diff()?;
        if changed_settings.is_empty() {
            return Ok(Err(Box::new(ApplyReport {
                validation: None,
                persistence: Some(PersistReport::skipped_no_changes()),
                routes: Vec::new(),
                rollback: Vec::new(),
                changed_settings,
                final_state: ApplyFinalState::NoChanges,
                conflicts: Vec::new(),
                errors: Vec::new(),
            })));
        }

        let route_updates = self.route_updates_for_diff(&changed_settings)?;
        let conflicts = self.generation_conflicts(&route_updates);
        if !conflicts.is_empty() {
            let routes = conflicts
                .iter()
                .map(|conflict| ApplyRouteReport {
                    route: conflict.route.clone(),
                    result: ApplyRouteResult::Conflict {
                        baseline: conflict.baseline,
                        current: conflict.current,
                    },
                    mechanism: ApplyMechanism::InPlace,
                    affected_settings: conflict.affected_settings.clone(),
                })
                .collect();
            return Ok(Err(Box::new(ApplyReport {
                validation: None,
                persistence: None,
                routes,
                rollback: Vec::new(),
                changed_settings,
                final_state: ApplyFinalState::BlockedByConflicts,
                conflicts,
                errors: Vec::new(),
            })));
        }

        let validation = match delegate.validate(ValidationRequest {
            committed: &self.committed,
            draft: &self.draft,
            changed_settings: &changed_settings,
        }) {
            Ok(report) => report,
            Err(error) => {
                return Ok(Err(Box::new(ApplyReport {
                    validation: None,
                    persistence: None,
                    routes: Vec::new(),
                    rollback: Vec::new(),
                    changed_settings,
                    final_state: ApplyFinalState::ValidationFailed,
                    conflicts: Vec::new(),
                    errors: vec![error],
                })));
            }
        };

        let apply_request = CommittedApplyRequest {
            previous_committed: &self.committed,
            requested: &self.draft,
            route_updates: &route_updates,
            changed_settings: &changed_settings,
        };
        let preflight_routes = match delegate.preflight_committed(apply_request) {
            Ok(reports) => reports,
            Err(error) => {
                return Ok(Err(Box::new(ApplyReport {
                    validation: Some(validation),
                    persistence: None,
                    routes: Vec::new(),
                    rollback: Vec::new(),
                    changed_settings,
                    final_state: ApplyFinalState::RuntimeBlocked,
                    conflicts: Vec::new(),
                    errors: vec![error],
                })));
            }
        };
        if !preflight_routes.is_empty() {
            return Ok(Err(Box::new(ApplyReport {
                validation: Some(validation),
                persistence: None,
                routes: preflight_routes,
                rollback: Vec::new(),
                changed_settings,
                final_state: ApplyFinalState::RuntimeBlocked,
                conflicts: Vec::new(),
                errors: Vec::new(),
            })));
        }

        Ok(Ok(ApplyPlan {
            changed_settings,
            route_updates,
            validation,
        }))
    }

    /// Хвост транзакции после применения маршрутов: откат при сбое, иначе persist -> finalize
    /// -> commit. Общий для синхронного `apply` и для stepwise-пути.
    pub(super) fn finish_apply<A>(
        &mut self,
        delegate: &mut A,
        plan: ApplyPlan,
        apply_result: SettingsResult<Vec<ApplyRouteReport>>,
    ) -> ApplyReport
    where
        A: SettingsPersister<D> + CommittedSettingsApplier<D>,
    {
        let ApplyPlan {
            changed_settings,
            route_updates,
            validation,
        } = plan;
        let mut errors = Vec::new();
        let routes = match apply_result {
            Ok(routes) => routes,
            Err(error) => {
                let message = error.to_string();
                errors.push(error);
                route_updates
                    .iter()
                    .map(|update| {
                        ApplyRouteReport::failed(
                            update.route.clone(),
                            update.affected_settings.clone(),
                            message.clone(),
                        )
                    })
                    .collect()
            }
        };

        let runtime_commit_succeeded =
            !routes.is_empty() && routes.iter().all(|report| report.result.is_full_success());
        if !runtime_commit_succeeded {
            let (rollback, rollback_delegate_failed) =
                match delegate.rollback_committed(CommittedRollbackRequest {
                    previous_committed: &self.committed,
                    attempted: &self.draft,
                    route_updates: &route_updates,
                    changed_settings: &changed_settings,
                }) {
                    Ok(reports) => (reports, false),
                    Err(error) => {
                        errors.push(error);
                        (Vec::new(), true)
                    }
                };
            let rollback_failed = rollback_delegate_failed
                || rollback
                    .iter()
                    .any(|report| matches!(report.result, RollbackResult::Failed { .. }));
            return ApplyReport {
                validation: Some(validation),
                persistence: None,
                routes,
                rollback,
                changed_settings,
                final_state: if rollback_failed {
                    ApplyFinalState::RollbackFailed
                } else {
                    ApplyFinalState::RuntimeApplyFailed
                },
                conflicts: Vec::new(),
                errors,
            };
        }

        let persistence = match delegate.persist(PersistRequest {
            document: &self.draft,
            changed_settings: &changed_settings,
        }) {
            Ok(report) => report,
            Err(persist_error) => {
                errors.push(persist_error);
                let (rollback, rollback_delegate_failed) =
                    match delegate.rollback_committed(CommittedRollbackRequest {
                        previous_committed: &self.committed,
                        attempted: &self.draft,
                        route_updates: &route_updates,
                        changed_settings: &changed_settings,
                    }) {
                        Ok(reports) => (reports, false),
                        Err(rollback_error) => {
                            errors.push(rollback_error);
                            (Vec::new(), true)
                        }
                    };
                let rollback_failed = rollback_delegate_failed
                    || rollback
                        .iter()
                        .any(|report| matches!(report.result, RollbackResult::Failed { .. }));
                return ApplyReport {
                    validation: Some(validation),
                    persistence: None,
                    routes,
                    rollback,
                    changed_settings,
                    final_state: if rollback_failed {
                        ApplyFinalState::RollbackFailed
                    } else {
                        ApplyFinalState::PersistFailed
                    },
                    conflicts: Vec::new(),
                    errors,
                };
            }
        };

        delegate.finalize_committed(CommittedFinalizeRequest {
            previous_committed: &self.committed,
            committed: &self.draft,
            route_updates: &route_updates,
            changed_settings: &changed_settings,
        });

        self.commit_full_success(route_updates.iter().map(|update| update.route.clone()));
        ApplyReport {
            validation: Some(validation),
            persistence: Some(persistence),
            routes,
            rollback: Vec::new(),
            changed_settings,
            final_state: ApplyFinalState::FullyApplied,
            conflicts: Vec::new(),
            errors,
        }
    }
}
