//! Многокадровое применение настроек (C2): Apply не блокирует UI-поток.
//!
//! Владелец фазы «ждём пересборку» в controller-е — `settings-core` (`begin_apply` / `poll_apply`).
//! Здесь живёт app-часть: очередь маршрутов, которая шагает между кадрами
//! ([`StepwiseRouteSequence`]), и реализация [`StepwiseCommittedApplier`] для делегата.
//!
//! Порядок: маршруты применяются по очереди; маршрут с пересборкой media отдаёт `Pending`,
//! остальные маршруты ждут, пока он не завершится. Persist/finalize делает controller, и только
//! после успеха всех маршрутов. Откат при сбое остаётся синхронным (блокирующая пересборка):
//! он нужен редко и только после сбоя.

use settings_core::{BeginApply, CommittedApplyProgress, PollApply, StepwiseCommittedApplier};

use super::route_apply::{PendingRoute, RouteApplyStart};
use super::transaction::SettingsRuntimeApplyDelegate;
use super::*;

/// Явная фаза транзакции в `SettingsRuntime.pending_apply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingApplyPhase {
    /// Статус «Применяем…» опубликован; транзакция стартует на ближайшем кадре.
    Scheduled,
    /// Controller ждёт маршрут; каждый кадр опрашивает пересборку.
    InFlight {
        /// Громкость до Apply: после успеха нужно понять, менял ли её пользователь.
        audio_level_before_apply: crate::user_audio_level::UserAudioLevel,
    },
}

/// Очередь маршрутов одной транзакции между кадрами.
pub(super) struct StepwiseRouteSequence {
    /// Маршруты в стабильном порядке применения.
    routes: std::vec::IntoIter<RuntimeCommittedRoute>,
    /// Целевая политика маршрутов (из ещё не записанного документа).
    target_policy: SettingsRouteTargetPolicy,
    /// Отчёты уже завершённых маршрутов.
    reports: Vec<ApplyRouteReport>,
    /// Завершённый префикс, который надо компенсировать при сбое.
    applied_route_count: usize,
    /// Маршрут, который ждёт результата пересборки.
    in_flight: Option<PendingRoute>,
}

impl StepwiseRouteSequence {
    fn new(routes: Vec<RuntimeCommittedRoute>, target_policy: SettingsRouteTargetPolicy) -> Self {
        Self {
            routes: routes.into_iter(),
            target_policy,
            reports: Vec::new(),
            applied_route_count: 0,
            in_flight: None,
        }
    }

    /// Записывает отчёт маршрута; `true` — можно продолжать со следующим маршрутом.
    fn record(&mut self, report: AppRouteApplyReport) -> bool {
        let full_success = report.result.is_success();
        if full_success || report.result.needs_compensation() {
            self.applied_route_count += 1;
        }
        self.reports.push(report.into_core_report());
        full_success
    }
}

impl<A> SettingsRuntimeApplyDelegate<'_, A>
where
    A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
{
    /// Продвигает очередь: сначала маршрут в полёте, затем следующие, пока что-то не ждёт.
    fn drive_route_sequence(&mut self) -> SettingsResult<CommittedApplyProgress> {
        let Some(mut sequence) = self.route_sequence.take() else {
            return Err(SettingsError::access_failed(
                "settings route sequence is not active",
            ));
        };
        loop {
            if let Some(pending) = sequence.in_flight.take() {
                match self.runtime_adapter.poll_media_rebuild() {
                    MediaRebuildProgress::Pending => {
                        sequence.in_flight = Some(pending);
                        *self.route_sequence = Some(sequence);
                        return Ok(CommittedApplyProgress::Pending);
                    }
                    MediaRebuildProgress::Finished(rebuild_result) => {
                        let report = self.route_appliers.finish_pending_route(
                            pending,
                            rebuild_result,
                            self.runtime_adapter,
                        );
                        let keep_going = sequence.record(report);
                        self.applied_route_count = sequence.applied_route_count;
                        if !keep_going {
                            break;
                        }
                    }
                }
            }
            let Some(route) = sequence.routes.next() else {
                break;
            };
            let start = match self.route_appliers.begin_committed_route_stepwise(
                route,
                sequence.target_policy,
                self.runtime_adapter,
            ) {
                Ok(start) => start,
                Err(error) => {
                    // Уже завершённый префикс всё равно должен откатиться.
                    self.applied_route_count = sequence.applied_route_count;
                    return Err(error);
                }
            };
            match start {
                RouteApplyStart::Finished(report) => {
                    let keep_going = sequence.record(report);
                    self.applied_route_count = sequence.applied_route_count;
                    if !keep_going {
                        break;
                    }
                }
                RouteApplyStart::Pending(pending) => {
                    // Отдаём кадр UI: первый опрос пересборки случится на следующем кадре.
                    sequence.in_flight = Some(pending);
                    *self.route_sequence = Some(sequence);
                    return Ok(CommittedApplyProgress::Pending);
                }
            }
        }
        Ok(CommittedApplyProgress::Finished(sequence.reports))
    }
}

impl<A> StepwiseCommittedApplier<AppConfig> for SettingsRuntimeApplyDelegate<'_, A>
where
    A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
{
    fn begin_apply_committed(
        &mut self,
        request: CommittedApplyRequest<'_, AppConfig>,
    ) -> SettingsResult<CommittedApplyProgress> {
        self.applied_route_count = 0;
        let routes = committed_routes_for_updates(
            request.previous_committed,
            request.requested,
            request.route_updates,
            self.automatic_surface_pool,
        )?;
        let target_policy = SettingsRouteTargetPolicy::from_config(request.requested);
        *self.route_sequence = Some(StepwiseRouteSequence::new(routes, target_policy));
        self.drive_route_sequence()
    }

    fn poll_apply_committed(
        &mut self,
        _request: CommittedApplyRequest<'_, AppConfig>,
    ) -> SettingsResult<CommittedApplyProgress> {
        self.drive_route_sequence()
    }

    fn cancel_apply_committed(
        &mut self,
        request: CommittedRollbackRequest<'_, AppConfig>,
    ) -> SettingsResult<Vec<RollbackReport>> {
        let Some(mut sequence) = self.route_sequence.take() else {
            return Ok(Vec::new());
        };
        if let Some(pending) = sequence.in_flight.take() {
            // Пересборка прерывается, а маршрут завершается как сбой: так он сам вернёт то,
            // что успел изменить до ожидания (например, audio-устройство).
            self.runtime_adapter.cancel_media_rebuild();
            let _cancelled_report = self.route_appliers.finish_pending_route(
                pending,
                AppRouteApplyResult::Failed {
                    message: "settings application interrupted".to_owned(),
                },
                self.runtime_adapter,
            );
        }
        self.applied_route_count = sequence.applied_route_count;
        self.rollback_applied_routes(request)
    }
}

/// Текст статуса, пока пересборка media идёт в фоне.
fn apply_in_progress_status() -> SettingsUiStatus {
    SettingsUiStatus {
        summary: Some("Applying settings…".to_string()),
        details: vec![
            "Rebuilding the source with new settings; the window remains responsive.".to_string(),
            "Settings will be saved only after successful application.".to_string(),
        ],
    }
}

/// Текст статуса, пока Apply пользователя ждёт конца принятия настроек соседнего окна.
fn apply_waits_for_other_window_status() -> SettingsUiStatus {
    SettingsUiStatus {
        summary: Some("Applying settings…".to_string()),
        details: vec![
            "Settings changed in another Fastiplayer window are being applied first; yours \
             will follow."
                .to_string(),
        ],
    }
}

impl SettingsRuntime {
    /// Стартует запланированный Apply или продвигает уже идущий; `true` — UI нужно перерисовать.
    pub(super) fn advance_pending_apply<A>(
        &mut self,
        runtime_adapter: &mut A,
    ) -> SettingsResult<bool>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        let Some(pending) = self.pending_apply.take() else {
            return Ok(false);
        };
        self.invalidate_ui_model();
        let (completed, audio_level_before_apply) = match pending.phase {
            PendingApplyPhase::Scheduled => {
                let audio_level_before_apply = self.committed_user_audio_level();
                match self.begin_apply_with_runtime_adapter(runtime_adapter)? {
                    BeginApply::Completed(report) => (Some(report), audio_level_before_apply),
                    BeginApply::Pending => (None, audio_level_before_apply),
                    BeginApply::Busy => {
                        // Контроллер занят принятием изменений соседней копии (общая доска):
                        // Apply пользователя не теряется, а остаётся запланированным и
                        // начнётся на первом кадре после конца принятия.
                        self.pending_apply = Some(pending);
                        self.status = apply_waits_for_other_window_status();
                        return Ok(false);
                    }
                }
            }
            PendingApplyPhase::InFlight {
                audio_level_before_apply,
            } => match self.poll_apply_with_runtime_adapter(runtime_adapter)? {
                PollApply::Completed(report) => (Some(report), audio_level_before_apply),
                PollApply::Pending => (None, audio_level_before_apply),
                PollApply::Idle => {
                    tracing::error!("Settings transaction was lost in the controller");
                    (None, audio_level_before_apply)
                }
            },
        };
        match completed {
            Some(report) => {
                self.deliver_user_applied_audio_level(
                    audio_level_before_apply,
                    runtime_adapter,
                    Instant::now(),
                );
                if pending.close_on_success
                    && matches!(
                        report.final_state,
                        ApplyFinalState::FullyApplied | ApplyFinalState::NoChanges
                    )
                {
                    self.settings_window_open = false;
                }
            }
            None if self.is_apply_in_flight() => {
                self.pending_apply = Some(PendingSettingsApply {
                    close_on_success: pending.close_on_success,
                    phase: PendingApplyPhase::InFlight {
                        audio_level_before_apply,
                    },
                });
                self.status = apply_in_progress_status();
            }
            None => {}
        }
        Ok(true)
    }

    /// Действие пришло, пока Apply ещё идёт: окно остаётся, первая транзакция продолжается.
    pub(super) fn report_apply_still_running(&mut self) {
        let mut status = apply_in_progress_status();
        status.details.insert(
            0,
            "The previous application is still in progress; wait for it to finish.".to_string(),
        );
        self.status = status;
        self.invalidate_ui_model();
    }

    /// Идёт ли многокадровая работа настроек, ради которой циклу нужно просыпаться
    /// (планировщик event loop и lifecycle-границы): свой Apply (запланирован или ждёт
    /// маршрут) либо принятие изменений соседней копии (в полёте или ждёт своей очереди).
    #[must_use]
    pub(crate) fn has_pending_apply(&self) -> bool {
        self.pending_apply.is_some() || self.has_shared_board_work()
    }

    /// Идёт ли именно фаза ожидания маршрута: runtime-commit-ы в это время запрещены.
    pub(super) fn is_apply_in_flight(&self) -> bool {
        self.controller.has_pending_apply()
    }

    /// Начинает транзакцию; синхронные маршруты завершаются здесь же.
    pub(super) fn begin_apply_with_runtime_adapter<A>(
        &mut self,
        runtime_adapter: &mut A,
    ) -> SettingsResult<BeginApply>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        self.invalidate_ui_model();
        let outcome = {
            let automatic_surface_pool = self.automatic_surface_pool();
            let mut delegate = SettingsRuntimeApplyDelegate {
                validator: AppConfigValidator,
                store: &mut self.store,
                route_appliers: &mut self.route_appliers,
                runtime_adapter,
                applied_route_count: 0,
                automatic_surface_pool,
                route_sequence: &mut self.route_sequence,
                save_notices: self.shared_board.save_notices_mut(),
            };
            self.controller.begin_apply(&mut delegate)?
        };
        if let BeginApply::Completed(report) = &outcome {
            self.conclude_apply(report, runtime_adapter);
        }
        Ok(outcome)
    }

    /// Неблокирующе продвигает транзакцию, ждущую маршрут.
    pub(super) fn poll_apply_with_runtime_adapter<A>(
        &mut self,
        runtime_adapter: &mut A,
    ) -> SettingsResult<PollApply>
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        let outcome = {
            let automatic_surface_pool = self.automatic_surface_pool();
            let mut delegate = SettingsRuntimeApplyDelegate {
                validator: AppConfigValidator,
                store: &mut self.store,
                route_appliers: &mut self.route_appliers,
                runtime_adapter,
                applied_route_count: 0,
                automatic_surface_pool,
                route_sequence: &mut self.route_sequence,
                save_notices: self.shared_board.save_notices_mut(),
            };
            self.controller.poll_apply(&mut delegate)?
        };
        if let PollApply::Completed(report) = &outcome {
            self.invalidate_ui_model();
            self.conclude_apply(report, runtime_adapter);
        }
        Ok(outcome)
    }

    /// Отменяет транзакцию на lifecycle-границе (suspend/shutdown): persist не происходит.
    pub(crate) fn cancel_pending_apply<A>(&mut self, runtime_adapter: &mut A)
    where
        A: RenderLiveSettingsAdapter + SettingsRuntimeReconfigureHost,
    {
        self.pending_apply = None;
        self.invalidate_ui_model();
        let report = {
            let automatic_surface_pool = self.automatic_surface_pool();
            let mut delegate = SettingsRuntimeApplyDelegate {
                validator: AppConfigValidator,
                store: &mut self.store,
                route_appliers: &mut self.route_appliers,
                runtime_adapter,
                applied_route_count: 0,
                automatic_surface_pool,
                route_sequence: &mut self.route_sequence,
                save_notices: self.shared_board.save_notices_mut(),
            };
            self.controller.cancel_pending_apply(&mut delegate)
        };
        if let Some(report) = report {
            tracing::info!(
                final_state = ?report.final_state,
                "Settings application interrupted at a lifecycle boundary"
            );
            self.latest_apply_report = Some(report.clone());
            self.status = status_from_apply_report(&self.setting_labels(), &report);
        }
    }
}
