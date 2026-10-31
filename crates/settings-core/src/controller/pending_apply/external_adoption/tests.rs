//! Тесты принятия изменений, уже записанных на диск другой копией (без persist, многокадрово).

use super::super::tests::{
    RecordingApplyDelegate, RecordingPreviewer, RecordingRollbacker, TestDocument,
    controller_registry,
};
use crate::{
    ApplyFinalState, ApplyRouteReport, BeginApply, BeginExternalAdoption, CommittedApplyProgress,
    CommittedApplyRequest, CommittedFinalizeRequest, CommittedRollbackRequest,
    CommittedSettingsApplier, ExternalAdoptionOutcome, ExternalAdoptionReport,
    ExternalSettingChange, PersistReport, PersistRequest, PollApply, PollExternalAdoption,
    RollbackReport, RouteGeneration, RuntimeSettingCommitRequest, SettingId, SettingRouteId,
    SettingValue, SettingsController, SettingsError, SettingsPersister, SettingsResult,
    SettingsValidator, StepwiseCommittedApplier, ValidationReport, ValidationRequest,
};
use std::collections::VecDeque;

/// Сценарий одного шага stepwise-владельца.
#[derive(Clone)]
enum RouteStep {
    /// Маршрут ждёт (пересборка медиа).
    Pending,
    /// Все маршруты запроса применились.
    AllApplied,
    /// Все маршруты запроса отказали.
    AllFailed,
}

/// Fake stepwise-владелец: шаги по сценарию, документы запросов записываются для проверок.
struct AdoptionDelegate {
    inner: RecordingApplyDelegate,
    begin_step: RouteStep,
    poll_steps: VecDeque<RouteStep>,
    /// Документы, которые владельцы получили в begin/poll (что реально применялось).
    applied_documents: Vec<TestDocument>,
    cancels: usize,
}

impl AdoptionDelegate {
    fn new(begin_step: RouteStep, poll_steps: Vec<RouteStep>) -> Self {
        Self {
            inner: RecordingApplyDelegate::default(),
            begin_step,
            poll_steps: poll_steps.into(),
            applied_documents: Vec::new(),
            cancels: 0,
        }
    }

    fn synchronous() -> Self {
        Self::new(RouteStep::AllApplied, Vec::new())
    }

    fn progress(
        step: RouteStep,
        request: &CommittedApplyRequest<'_, TestDocument>,
    ) -> CommittedApplyProgress {
        let reports = |failed: bool| {
            request
                .route_updates
                .iter()
                .map(|update| {
                    if failed {
                        ApplyRouteReport::failed(
                            update.route.clone(),
                            update.affected_settings.clone(),
                            "media rebuild failed",
                        )
                    } else {
                        ApplyRouteReport::applied(
                            update.route.clone(),
                            update.affected_settings.clone(),
                        )
                    }
                })
                .collect()
        };
        match step {
            RouteStep::Pending => CommittedApplyProgress::Pending,
            RouteStep::AllApplied => CommittedApplyProgress::Finished(reports(false)),
            RouteStep::AllFailed => CommittedApplyProgress::Finished(reports(true)),
        }
    }
}

impl SettingsValidator<TestDocument> for AdoptionDelegate {
    fn validate(
        &mut self,
        request: ValidationRequest<'_, TestDocument>,
    ) -> SettingsResult<ValidationReport> {
        self.inner.validate(request)
    }
}

// Persist реализован намеренно: если принятие когда-нибудь начнёт писать config, тесты увидят
// это по `inner.calls` / `inner.persisted_documents`.
impl SettingsPersister<TestDocument> for AdoptionDelegate {
    fn persist(
        &mut self,
        request: PersistRequest<'_, TestDocument>,
    ) -> SettingsResult<PersistReport> {
        self.inner.persist(request)
    }
}

impl CommittedSettingsApplier<TestDocument> for AdoptionDelegate {
    fn preflight_committed(
        &mut self,
        request: CommittedApplyRequest<'_, TestDocument>,
    ) -> SettingsResult<Vec<ApplyRouteReport>> {
        self.inner.preflight_committed(request)
    }

    fn apply_committed(
        &mut self,
        request: CommittedApplyRequest<'_, TestDocument>,
    ) -> SettingsResult<Vec<ApplyRouteReport>> {
        self.inner.apply_committed(request)
    }

    fn rollback_committed(
        &mut self,
        request: CommittedRollbackRequest<'_, TestDocument>,
    ) -> SettingsResult<Vec<RollbackReport>> {
        self.inner.rollback_committed(request)
    }

    fn finalize_committed(&mut self, request: CommittedFinalizeRequest<'_, TestDocument>) {
        self.inner.finalize_committed(request);
    }
}

impl StepwiseCommittedApplier<TestDocument> for AdoptionDelegate {
    fn begin_apply_committed(
        &mut self,
        request: CommittedApplyRequest<'_, TestDocument>,
    ) -> SettingsResult<CommittedApplyProgress> {
        self.inner.calls.push("begin");
        self.applied_documents.push(request.requested.clone());
        Ok(Self::progress(self.begin_step.clone(), &request))
    }

    fn poll_apply_committed(
        &mut self,
        request: CommittedApplyRequest<'_, TestDocument>,
    ) -> SettingsResult<CommittedApplyProgress> {
        self.inner.calls.push("poll");
        self.applied_documents.push(request.requested.clone());
        let step = self.poll_steps.pop_front().unwrap_or(RouteStep::Pending);
        Ok(Self::progress(step, &request))
    }

    fn cancel_apply_committed(
        &mut self,
        request: CommittedRollbackRequest<'_, TestDocument>,
    ) -> SettingsResult<Vec<RollbackReport>> {
        self.cancels += 1;
        self.inner.calls.push("cancel");
        Ok(request
            .route_updates
            .iter()
            .map(|update| {
                RollbackReport::rolled_back(update.route.clone(), update.affected_settings.clone())
            })
            .collect())
    }
}

fn external(id: &str, value: SettingValue) -> ExternalSettingChange {
    ExternalSettingChange::new(id, value)
}

fn text(value: &str) -> SettingValue {
    SettingValue::Text(value.to_owned())
}

fn ui_route() -> SettingRouteId {
    SettingRouteId::from("ui")
}

fn render_route() -> SettingRouteId {
    SettingRouteId::from("render")
}

fn fresh_controller() -> SettingsController<TestDocument> {
    SettingsController::new(TestDocument::default(), controller_registry())
}

/// Контроллер с открытым окном: пользователь поменял заголовок (ui) и яркость (render, preview).
fn controller_with_user_edits() -> SettingsController<TestDocument> {
    let mut controller = fresh_controller();
    controller
        .set_value(SettingId::from("ui.title"), text("Правка пользователя"))
        .expect("user title edit");
    controller
        .set_value(
            SettingId::from("render.brightness"),
            SettingValue::Float(0.8),
        )
        .expect("user brightness preview edit");
    controller
}

fn completed_report(outcome: BeginExternalAdoption) -> ExternalAdoptionReport {
    match outcome {
        BeginExternalAdoption::Completed(report) => *report,
        other => panic!("expected completed adoption, got {other:?}"),
    }
}

/// Снимок всего состояния controller-а, которое принятие обязано вернуть при неуспехе.
#[derive(Debug, PartialEq)]
struct ControllerSnapshot {
    committed: TestDocument,
    draft: TestDocument,
    ui_baseline: Option<RouteGeneration>,
    render_baseline: Option<RouteGeneration>,
    ui_generation: RouteGeneration,
    render_generation: RouteGeneration,
    render_preview_baseline: Option<TestDocument>,
    render_preview_active: Option<TestDocument>,
    render_preview_pending: Option<TestDocument>,
}

fn snapshot(controller: &SettingsController<TestDocument>) -> ControllerSnapshot {
    let render_state = controller.preview().route_state(&render_route());
    ControllerSnapshot {
        committed: controller.committed().clone(),
        draft: controller.draft().clone(),
        ui_baseline: controller.route_generation_baseline(&ui_route()),
        render_baseline: controller.route_generation_baseline(&render_route()),
        ui_generation: controller.route_generation(&ui_route()),
        render_generation: controller.route_generation(&render_route()),
        render_preview_baseline: render_state.map(|state| state.baseline_document.clone()),
        render_preview_active: render_state.map(|state| state.active_document.clone()),
        render_preview_pending: controller
            .preview()
            .pending_update(&render_route())
            .map(|update| update.document.clone()),
    }
}

#[test]
fn synchronous_routes_adopt_without_persist_and_commit_the_external_value() {
    let mut controller = fresh_controller();
    let mut delegate = AdoptionDelegate::synchronous();

    let report = completed_report(
        controller
            .begin_adopt_external_settings(
                vec![external("ui.title", text("Имя от соседа"))],
                &mut delegate,
            )
            .expect("adoption"),
    );

    assert_eq!(report.outcome, ExternalAdoptionOutcome::Adopted);
    assert_eq!(
        report.transaction.final_state,
        ApplyFinalState::FullyApplied
    );
    assert_eq!(report.transaction.persistence, None);
    assert_eq!(controller.committed().title, "Имя от соседа");
    assert_eq!(controller.draft().title, "Имя от соседа");
    assert_eq!(
        controller.route_generation(&ui_route()),
        RouteGeneration::new(1)
    );
    // Тот же конвейер, что у Apply, но без шага записи.
    assert_eq!(
        delegate.inner.calls,
        vec!["validate", "preflight", "begin", "finalize"]
    );
    assert!(delegate.inner.persisted_documents.is_empty());
    assert!(!controller.has_pending_apply());
}

#[test]
fn pending_route_adopts_over_several_frames_and_never_persists() {
    let mut controller = controller_with_user_edits();
    let user_draft = controller.draft().clone();
    let mut delegate = AdoptionDelegate::new(
        RouteStep::Pending,
        vec![RouteStep::Pending, RouteStep::AllApplied],
    );

    let started = controller
        .begin_adopt_external_settings(
            vec![external(
                "ui.sidebar.width_points",
                SettingValue::Integer(500),
            )],
            &mut delegate,
        )
        .expect("begin");

    assert_eq!(started, BeginExternalAdoption::Pending);
    assert!(controller.has_pending_apply());
    assert!(controller.has_pending_external_adoption());
    // Пока маршрут в полёте, окно настроек видит свои правки, committed прежний.
    assert_eq!(controller.draft(), &user_draft);
    assert_eq!(controller.committed().sidebar_width, 420);
    assert_eq!(
        controller.poll_external_adoption(&mut delegate),
        PollExternalAdoption::Pending
    );
    assert_eq!(controller.committed().sidebar_width, 420);

    let completed = controller.poll_external_adoption(&mut delegate);

    let PollExternalAdoption::Completed(report) = completed else {
        panic!("expected completed adoption, got {completed:?}");
    };
    assert_eq!(report.outcome, ExternalAdoptionOutcome::Adopted);
    assert_eq!(controller.committed().sidebar_width, 500);
    assert_eq!(controller.draft().sidebar_width, 500);
    assert_eq!(
        delegate.inner.calls,
        vec!["validate", "preflight", "begin", "poll", "poll", "finalize"]
    );
    assert!(delegate.inner.persisted_documents.is_empty());
    // Владельцы получали документ от committed: правки пользователя туда не утекли.
    for applied in &delegate.applied_documents {
        assert_eq!(applied.sidebar_width, 500);
        assert_eq!(applied.title, "Исходное имя");
        assert_eq!(applied.brightness, 0.5);
    }
    assert!(!controller.has_pending_apply());
    assert_eq!(
        controller.poll_external_adoption(&mut delegate),
        PollExternalAdoption::Idle
    );
}

#[test]
fn draft_keeps_touched_user_edit_and_pulls_untouched_fields() {
    let mut controller = controller_with_user_edits();
    let mut delegate = AdoptionDelegate::synchronous();

    let report = completed_report(
        controller
            .begin_adopt_external_settings(
                vec![
                    external("ui.title", text("Имя от соседа")),
                    external("ui.sidebar.width_points", SettingValue::Integer(500)),
                    external(
                        "render.rgb",
                        SettingValue::NumericVector(vec![0.2, 0.3, 0.4]),
                    ),
                ],
                &mut delegate,
            )
            .expect("adoption"),
    );

    assert_eq!(report.outcome, ExternalAdoptionOutcome::Adopted);
    // Committed = диск по принятым id; правка яркости пользователя не закоммичена.
    assert_eq!(controller.committed().title, "Имя от соседа");
    assert_eq!(controller.committed().sidebar_width, 500);
    assert_eq!(controller.committed().rgb, vec![0.2, 0.3, 0.4]);
    assert_eq!(controller.committed().brightness, 0.5);
    // Тронутое поле: правка пользователя осталась (победит при его Apply).
    assert_eq!(controller.draft().title, "Правка пользователя");
    // Нетронутые поля подтянулись, соседняя правка цела.
    assert_eq!(controller.draft().sidebar_width, 500);
    assert_eq!(controller.draft().rgb, vec![0.2, 0.3, 0.4]);
    assert_eq!(controller.draft().brightness, 0.8);
    assert_eq!(
        report.draft_merge.kept_user_edits,
        vec![SettingId::from("ui.title")]
    );
    assert!(
        report
            .draft_merge
            .synchronized_into_draft
            .contains(&SettingId::from("ui.sidebar.width_points"))
    );
    assert!(
        report
            .draft_merge
            .synchronized_into_draft
            .contains(&SettingId::from("render.rgb"))
    );
}

#[test]
fn user_preview_on_an_adopted_route_is_queued_again_and_cancel_returns_to_external_value() {
    let mut controller = controller_with_user_edits();
    // Preview яркости уже на экране: очередь пуста, активный документ — правка пользователя.
    let mut previewer = RecordingPreviewer::default();
    controller
        .apply_pending_preview(&render_route(), &mut previewer)
        .expect("preview apply");
    assert!(
        controller
            .preview()
            .pending_update(&render_route())
            .is_none()
    );
    let mut delegate = AdoptionDelegate::synchronous();

    completed_report(
        controller
            .begin_adopt_external_settings(
                vec![external(
                    "render.rgb",
                    SettingValue::NumericVector(vec![0.2, 0.3, 0.4]),
                )],
                &mut delegate,
            )
            .expect("adoption"),
    );

    // Committed-маршрут render затёр preview в runtime — заявка возвращает правку на экран.
    let pending = controller
        .preview()
        .pending_update(&render_route())
        .expect("user preview must be queued again after the render route was re-applied");
    assert_eq!(pending.document.brightness, 0.8);
    assert_eq!(pending.document.rgb, vec![0.2, 0.3, 0.4]);
    assert_eq!(
        pending.affected_settings,
        vec![SettingId::from("render.brightness")]
    );
    let state = controller
        .preview()
        .route_state(&render_route())
        .expect("preview baseline survives adoption");
    assert_eq!(state.baseline_document.brightness, 0.5);
    assert_eq!(state.baseline_document.rgb, vec![0.2, 0.3, 0.4]);

    // «Отмена» в окне возвращает runtime к принятому значению, а не к старому.
    let mut rollbacker = RecordingRollbacker::default();
    controller.cancel(&mut rollbacker).expect("cancel");
    assert_eq!(controller.draft().rgb, vec![0.2, 0.3, 0.4]);
    assert_eq!(rollbacker.rolled_back_routes, vec![render_route()]);
}

#[test]
fn touched_preview_field_keeps_user_value_but_rolls_back_to_the_adopted_one() {
    let mut controller = controller_with_user_edits();
    controller
        .apply_pending_preview(&render_route(), &mut RecordingPreviewer::default())
        .expect("preview apply");
    let mut delegate = AdoptionDelegate::synchronous();

    let report = completed_report(
        controller
            .begin_adopt_external_settings(
                vec![external("render.brightness", SettingValue::Float(0.3))],
                &mut delegate,
            )
            .expect("adoption"),
    );

    assert_eq!(controller.committed().brightness, 0.3);
    assert_eq!(controller.draft().brightness, 0.8);
    assert_eq!(
        report.draft_merge.kept_user_edits,
        vec![SettingId::from("render.brightness")]
    );
    let state = controller
        .preview()
        .route_state(&render_route())
        .expect("preview state");
    assert_eq!(state.baseline_document.brightness, 0.3);
    assert_eq!(state.active_document.brightness, 0.8);
    let pending = controller
        .preview()
        .pending_update(&render_route())
        .expect("preview pending");
    assert_eq!(pending.document.brightness, 0.8);
}

#[test]
fn adoption_while_user_apply_is_in_flight_is_busy_and_changes_nothing() {
    let mut controller = controller_with_user_edits();
    let mut delegate = AdoptionDelegate::new(RouteStep::Pending, vec![RouteStep::AllApplied]);
    assert_eq!(
        controller.begin_apply(&mut delegate).expect("user apply"),
        BeginApply::Pending
    );
    let before = snapshot(&controller);
    let calls_before = delegate.inner.calls.clone();

    let outcome = controller
        .begin_adopt_external_settings(
            vec![external(
                "ui.sidebar.width_points",
                SettingValue::Integer(500),
            )],
            &mut delegate,
        )
        .expect("busy is not an error");

    assert_eq!(outcome, BeginExternalAdoption::Busy);
    assert_eq!(snapshot(&controller), before);
    assert_eq!(delegate.inner.calls, calls_before);
    assert!(!controller.has_pending_external_adoption());
    // Опрос принятия не трогает пользовательский Apply, а тот доходит до конца.
    assert_eq!(
        controller.poll_external_adoption(&mut delegate),
        PollExternalAdoption::Idle
    );
    assert!(controller.cancel_external_adoption(&mut delegate).is_none());
    let PollApply::Completed(user_report) = controller.poll_apply(&mut delegate).expect("poll")
    else {
        panic!("user apply must complete");
    };
    assert_eq!(user_report.final_state, ApplyFinalState::FullyApplied);
}

#[test]
fn user_transactions_wait_while_adoption_is_in_flight() {
    let mut controller = controller_with_user_edits();
    let mut delegate = AdoptionDelegate::new(RouteStep::Pending, vec![RouteStep::AllApplied]);
    controller
        .begin_adopt_external_settings(
            vec![external(
                "ui.sidebar.width_points",
                SettingValue::Integer(500),
            )],
            &mut delegate,
        )
        .expect("begin adoption");
    let draft_before = controller.draft().clone();

    assert_eq!(
        controller.begin_apply(&mut delegate).expect("begin apply"),
        BeginApply::Busy
    );
    assert!(controller.apply(&mut delegate).is_err());
    assert!(
        controller
            .commit_runtime_setting(
                RuntimeSettingCommitRequest::new(
                    "ui.sidebar.width_points",
                    SettingValue::Integer(450)
                ),
                &mut delegate,
            )
            .is_err()
    );
    // Опрос и отмена пользовательского Apply не продвигают и не отменяют чужое принятие.
    assert_eq!(
        controller.poll_apply(&mut delegate).expect("poll"),
        PollApply::Idle
    );
    assert!(controller.cancel_pending_apply(&mut delegate).is_none());
    assert!(controller.has_pending_external_adoption());
    assert_eq!(controller.draft(), &draft_before);
    assert!(delegate.inner.persisted_documents.is_empty());

    let PollExternalAdoption::Completed(report) = controller.poll_external_adoption(&mut delegate)
    else {
        panic!("adoption must still complete");
    };
    assert_eq!(report.outcome, ExternalAdoptionOutcome::Adopted);
    assert_eq!(controller.committed().sidebar_width, 500);
}

#[test]
fn failed_route_rolls_back_and_leaves_committed_draft_preview_and_baselines_untouched() {
    let mut controller = controller_with_user_edits();
    let before = snapshot(&controller);
    let mut delegate = AdoptionDelegate::new(RouteStep::Pending, vec![RouteStep::AllFailed]);
    controller
        .begin_adopt_external_settings(
            vec![
                external("ui.title", text("Имя от соседа")),
                external(
                    "render.rgb",
                    SettingValue::NumericVector(vec![0.2, 0.3, 0.4]),
                ),
            ],
            &mut delegate,
        )
        .expect("begin");

    let PollExternalAdoption::Completed(report) = controller.poll_external_adoption(&mut delegate)
    else {
        panic!("expected completed adoption");
    };

    assert_eq!(report.outcome, ExternalAdoptionOutcome::RolledBack);
    assert_eq!(
        report.transaction.final_state,
        ApplyFinalState::RuntimeApplyFailed
    );
    assert_eq!(report.transaction.rollback.len(), 2);
    assert_eq!(report.draft_merge, Default::default());
    assert_eq!(snapshot(&controller), before);
    assert!(delegate.inner.calls.contains(&"rollback"));
    assert!(!delegate.inner.calls.contains(&"finalize"));
    assert!(delegate.inner.persisted_documents.is_empty());
}

#[test]
fn cancel_at_lifecycle_boundary_rolls_back_without_persist() {
    let mut controller = controller_with_user_edits();
    let before = snapshot(&controller);
    let mut delegate = AdoptionDelegate::new(RouteStep::Pending, Vec::new());
    controller
        .begin_adopt_external_settings(
            vec![external("ui.title", text("Имя от соседа"))],
            &mut delegate,
        )
        .expect("begin");

    let report = controller
        .cancel_external_adoption(&mut delegate)
        .expect("adoption in flight must produce a cancel report");

    assert_eq!(report.outcome, ExternalAdoptionOutcome::RolledBack);
    assert_eq!(report.transaction.final_state, ApplyFinalState::Cancelled);
    assert_eq!(delegate.cancels, 1);
    assert_eq!(snapshot(&controller), before);
    assert!(!controller.has_pending_apply());
    assert!(delegate.inner.persisted_documents.is_empty());
    assert!(controller.cancel_external_adoption(&mut delegate).is_none());
}

#[test]
fn unknown_or_read_only_id_is_an_error_before_any_mutation() {
    let mut controller = controller_with_user_edits();
    let before = snapshot(&controller);
    let mut delegate = AdoptionDelegate::synchronous();

    let unknown = controller.begin_adopt_external_settings(
        vec![
            external("ui.sidebar.width_points", SettingValue::Integer(500)),
            external("ui.no_such_setting", SettingValue::Integer(1)),
        ],
        &mut delegate,
    );
    let read_only = controller.begin_adopt_external_settings(
        vec![external("system.schema_version", SettingValue::Integer(9))],
        &mut delegate,
    );

    assert!(matches!(
        unknown,
        Err(SettingsError::UnknownSetting { ref id }) if id == &SettingId::from("ui.no_such_setting")
    ));
    assert!(matches!(
        read_only,
        Err(SettingsError::ReadOnlySetting { .. })
    ));
    assert_eq!(snapshot(&controller), before);
    assert!(delegate.inner.calls.is_empty());
    assert!(!controller.has_pending_apply());
}

#[test]
fn empty_list_and_own_write_are_nothing_to_adopt_without_a_transaction() {
    let mut controller = controller_with_user_edits();
    let before = snapshot(&controller);
    let mut delegate = AdoptionDelegate::synchronous();

    let empty = controller
        .begin_adopt_external_settings(Vec::new(), &mut delegate)
        .expect("empty");
    // Своя запись: на диске то же, что в committed.
    let own_write = controller
        .begin_adopt_external_settings(
            vec![external(
                "ui.sidebar.width_points",
                SettingValue::Integer(420),
            )],
            &mut delegate,
        )
        .expect("own write");

    assert_eq!(empty, BeginExternalAdoption::NothingToAdopt);
    assert_eq!(own_write, BeginExternalAdoption::NothingToAdopt);
    assert!(delegate.inner.calls.is_empty());
    assert_eq!(snapshot(&controller), before);
}

#[test]
fn validation_or_busy_owner_refuses_before_mutation() {
    let mut controller = controller_with_user_edits();
    let before = snapshot(&controller);
    let mut invalid = AdoptionDelegate::synchronous();
    invalid.inner.validate_error = Some(SettingsError::access_failed("rejected"));
    let mut busy = AdoptionDelegate::synchronous();
    busy.inner.preflight_reports = Some(vec![ApplyRouteReport::failed(
        ui_route(),
        vec![SettingId::from("ui.title")],
        "seek in progress",
    )]);
    let change = || vec![external("ui.title", text("Имя от соседа"))];

    let rejected = completed_report(
        controller
            .begin_adopt_external_settings(change(), &mut invalid)
            .expect("validation refusal"),
    );
    let blocked = completed_report(
        controller
            .begin_adopt_external_settings(change(), &mut busy)
            .expect("preflight refusal"),
    );

    assert_eq!(rejected.outcome, ExternalAdoptionOutcome::Refused);
    assert_eq!(
        rejected.transaction.final_state,
        ApplyFinalState::ValidationFailed
    );
    assert_eq!(blocked.outcome, ExternalAdoptionOutcome::Refused);
    assert_eq!(
        blocked.transaction.final_state,
        ApplyFinalState::RuntimeBlocked
    );
    assert!(!invalid.inner.calls.contains(&"begin"));
    assert!(!busy.inner.calls.contains(&"begin"));
    assert_eq!(snapshot(&controller), before);
}

#[test]
fn later_user_apply_of_a_neighbor_on_the_same_route_has_no_false_conflict() {
    let mut controller = controller_with_user_edits();
    assert_eq!(
        controller.route_generation_baseline(&ui_route()),
        Some(RouteGeneration::new(0))
    );
    let mut delegate = AdoptionDelegate::synchronous();
    completed_report(
        controller
            .begin_adopt_external_settings(
                vec![external(
                    "ui.sidebar.width_points",
                    SettingValue::Integer(500),
                )],
                &mut delegate,
            )
            .expect("adoption"),
    );
    assert_eq!(
        controller.route_generation_baseline(&ui_route()),
        Some(RouteGeneration::new(1))
    );

    let mut user_delegate = RecordingApplyDelegate::default();
    let user_report = controller.apply(&mut user_delegate).expect("user apply");

    assert_eq!(user_report.final_state, ApplyFinalState::FullyApplied);
    assert!(user_report.conflicts.is_empty());
    assert_eq!(controller.committed().title, "Правка пользователя");
    // Запись пользователя не откатывает значение соседа.
    assert_eq!(user_delegate.persisted_documents[0].sidebar_width, 500);
    assert_eq!(controller.committed().sidebar_width, 500);
}

#[test]
fn genuine_user_conflict_neither_blocks_adoption_nor_gets_hidden() {
    let mut controller = controller_with_user_edits();
    // Владелец маршрута ui изменился в обход черновика: у пользователя настоящий конфликт.
    controller.set_route_generation(ui_route(), RouteGeneration::new(5));
    let mut delegate = AdoptionDelegate::synchronous();

    let report = completed_report(
        controller
            .begin_adopt_external_settings(
                vec![external(
                    "ui.sidebar.width_points",
                    SettingValue::Integer(500),
                )],
                &mut delegate,
            )
            .expect("adoption"),
    );

    assert_eq!(report.outcome, ExternalAdoptionOutcome::Adopted);
    assert_eq!(controller.committed().sidebar_width, 500);
    let user_report = controller
        .apply(&mut RecordingApplyDelegate::default())
        .expect("user apply");
    assert_eq!(user_report.final_state, ApplyFinalState::BlockedByConflicts);
}
