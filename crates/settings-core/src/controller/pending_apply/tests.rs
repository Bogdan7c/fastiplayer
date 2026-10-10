//! Тесты многокадровой транзакции: фаза «ждём маршрут» между begin и poll.

use super::super::tests::{RecordingApplyDelegate, TestDocument, controller_registry};
use crate::{
    ApplyFinalState, ApplyRouteReport, BeginApply, CommittedApplyProgress, CommittedApplyRequest,
    CommittedFinalizeRequest, CommittedRollbackRequest, CommittedSettingsApplier, PersistReport,
    PersistRequest, PollApply, RollbackReport, RouteGeneration, RuntimeSettingCommitRequest,
    SettingId, SettingRouteId, SettingValue, SettingsController, SettingsError, SettingsPersister,
    SettingsResult, SettingsValidator, StepwiseCommittedApplier, ValidationReport,
    ValidationRequest,
};
use std::collections::VecDeque;

/// Fake stepwise-владелец: ответы begin/poll заданы сценарием, остальное пишет `inner`.
struct ScriptedStepwiseDelegate {
    inner: RecordingApplyDelegate,
    begin_script: Option<SettingsResult<CommittedApplyProgress>>,
    poll_script: VecDeque<SettingsResult<CommittedApplyProgress>>,
    cancel_rollbacks: Vec<RollbackReport>,
    polls: usize,
    cancels: usize,
}

impl ScriptedStepwiseDelegate {
    fn new(
        begin: SettingsResult<CommittedApplyProgress>,
        polls: Vec<SettingsResult<CommittedApplyProgress>>,
    ) -> Self {
        Self {
            inner: RecordingApplyDelegate::default(),
            begin_script: Some(begin),
            poll_script: polls.into(),
            cancel_rollbacks: Vec::new(),
            polls: 0,
            cancels: 0,
        }
    }
}

impl SettingsValidator<TestDocument> for ScriptedStepwiseDelegate {
    fn validate(
        &mut self,
        request: ValidationRequest<'_, TestDocument>,
    ) -> SettingsResult<ValidationReport> {
        self.inner.validate(request)
    }
}

impl SettingsPersister<TestDocument> for ScriptedStepwiseDelegate {
    fn persist(
        &mut self,
        request: PersistRequest<'_, TestDocument>,
    ) -> SettingsResult<PersistReport> {
        self.inner.persist(request)
    }
}

impl CommittedSettingsApplier<TestDocument> for ScriptedStepwiseDelegate {
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

impl StepwiseCommittedApplier<TestDocument> for ScriptedStepwiseDelegate {
    fn begin_apply_committed(
        &mut self,
        _request: CommittedApplyRequest<'_, TestDocument>,
    ) -> SettingsResult<CommittedApplyProgress> {
        self.inner.calls.push("begin");
        self.begin_script
            .take()
            .unwrap_or_else(|| Err(SettingsError::access_failed("begin called twice")))
    }

    fn poll_apply_committed(
        &mut self,
        _request: CommittedApplyRequest<'_, TestDocument>,
    ) -> SettingsResult<CommittedApplyProgress> {
        self.polls += 1;
        self.inner.calls.push("poll");
        self.poll_script
            .pop_front()
            .unwrap_or(Ok(CommittedApplyProgress::Pending))
    }

    fn cancel_apply_committed(
        &mut self,
        _request: CommittedRollbackRequest<'_, TestDocument>,
    ) -> SettingsResult<Vec<RollbackReport>> {
        self.cancels += 1;
        self.inner.calls.push("cancel");
        Ok(self.cancel_rollbacks.clone())
    }
}

fn title_route_update_reports() -> Vec<ApplyRouteReport> {
    vec![ApplyRouteReport::applied(
        SettingRouteId::from("ui"),
        vec![SettingId::from("ui.title")],
    )]
}

fn controller_with_edited_title() -> SettingsController<TestDocument> {
    let mut controller = SettingsController::new(TestDocument::default(), controller_registry());
    controller
        .set_value(
            SettingId::from("ui.title"),
            SettingValue::Text("Новое имя".to_owned()),
        )
        .expect("draft edit should succeed");
    controller
}

#[test]
fn pending_route_blocks_persist_and_keeps_committed_until_it_finishes() {
    let mut controller = controller_with_edited_title();
    let mut delegate = ScriptedStepwiseDelegate::new(Ok(CommittedApplyProgress::Pending), vec![]);

    let started = controller
        .begin_apply(&mut delegate)
        .expect("begin should not fail");

    assert_eq!(started, BeginApply::Pending);
    assert!(controller.has_pending_apply());
    assert_eq!(controller.committed().title, "Исходное имя");
    assert!(delegate.inner.persisted_documents.is_empty());
    assert!(!delegate.inner.calls.contains(&"persist"));
    // Пока маршрут ждёт, каждый опрос тоже ничего не пишет.
    assert_eq!(
        controller
            .poll_apply(&mut delegate)
            .expect("poll should not fail"),
        PollApply::Pending
    );
    assert!(delegate.inner.persisted_documents.is_empty());
    assert_eq!(controller.committed().title, "Исходное имя");
}

#[test]
fn finished_route_persists_finalizes_and_commits_exactly_once() {
    let mut controller = controller_with_edited_title();
    let mut delegate = ScriptedStepwiseDelegate::new(
        Ok(CommittedApplyProgress::Pending),
        vec![
            Ok(CommittedApplyProgress::Pending),
            Ok(CommittedApplyProgress::Finished(
                title_route_update_reports(),
            )),
        ],
    );
    assert_eq!(
        controller.begin_apply(&mut delegate).expect("begin"),
        BeginApply::Pending
    );
    assert_eq!(
        controller.poll_apply(&mut delegate).expect("poll"),
        PollApply::Pending
    );

    let completed = controller.poll_apply(&mut delegate).expect("poll");

    let PollApply::Completed(report) = completed else {
        panic!("expected completed transaction, got {completed:?}");
    };
    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    assert_eq!(delegate.inner.persisted_documents.len(), 1);
    assert_eq!(delegate.inner.persisted_documents[0].title, "Новое имя");
    assert_eq!(controller.committed().title, "Новое имя");
    assert_eq!(
        controller.route_generation(&SettingRouteId::from("ui")),
        RouteGeneration::new(1)
    );
    assert_eq!(
        delegate.inner.calls,
        vec![
            "validate",
            "preflight",
            "begin",
            "poll",
            "poll",
            "persist",
            "finalize"
        ]
    );
    assert!(!controller.has_pending_apply());
    // Терминал забирается один раз: следующий опрос ничего не находит.
    assert_eq!(
        controller.poll_apply(&mut delegate).expect("poll"),
        PollApply::Idle
    );
}

#[test]
fn failed_route_after_pending_rolls_back_and_never_persists() {
    let mut controller = controller_with_edited_title();
    let failed = vec![ApplyRouteReport::failed(
        SettingRouteId::from("ui"),
        vec![SettingId::from("ui.title")],
        "rebuild failed".to_owned(),
    )];
    let mut delegate = ScriptedStepwiseDelegate::new(
        Ok(CommittedApplyProgress::Pending),
        vec![Ok(CommittedApplyProgress::Finished(failed))],
    );
    assert_eq!(
        controller.begin_apply(&mut delegate).expect("begin"),
        BeginApply::Pending
    );

    let completed = controller.poll_apply(&mut delegate).expect("poll");

    let PollApply::Completed(report) = completed else {
        panic!("expected completed transaction, got {completed:?}");
    };
    assert_eq!(report.final_state, ApplyFinalState::RuntimeApplyFailed);
    assert!(delegate.inner.persisted_documents.is_empty());
    assert!(delegate.inner.calls.contains(&"rollback"));
    assert!(!delegate.inner.calls.contains(&"finalize"));
    assert_eq!(controller.committed().title, "Исходное имя");
    assert!(!controller.has_pending_apply());
}

#[test]
fn delegate_error_while_polling_is_reported_as_failed_route_with_rollback() {
    let mut controller = controller_with_edited_title();
    let mut delegate = ScriptedStepwiseDelegate::new(
        Ok(CommittedApplyProgress::Pending),
        vec![Err(SettingsError::access_failed("poll exploded"))],
    );
    assert_eq!(
        controller.begin_apply(&mut delegate).expect("begin"),
        BeginApply::Pending
    );

    let PollApply::Completed(report) = controller.poll_apply(&mut delegate).expect("poll") else {
        panic!("expected completed transaction");
    };

    assert_eq!(report.final_state, ApplyFinalState::RuntimeApplyFailed);
    assert_eq!(report.errors.len(), 1);
    assert!(delegate.inner.persisted_documents.is_empty());
    assert!(delegate.inner.calls.contains(&"rollback"));
}

#[test]
fn second_begin_is_busy_and_keeps_the_first_transaction() {
    let mut controller = controller_with_edited_title();
    let mut delegate = ScriptedStepwiseDelegate::new(
        Ok(CommittedApplyProgress::Pending),
        vec![Ok(CommittedApplyProgress::Finished(
            title_route_update_reports(),
        ))],
    );
    assert_eq!(
        controller.begin_apply(&mut delegate).expect("begin"),
        BeginApply::Pending
    );
    let calls_before = delegate.inner.calls.clone();

    let second = controller.begin_apply(&mut delegate).expect("second begin");

    assert_eq!(second, BeginApply::Busy);
    // Busy не трогает владельцев: ни validate, ни begin второй раз.
    assert_eq!(delegate.inner.calls, calls_before);
    // Первая транзакция не потеряна и доходит до успеха.
    let PollApply::Completed(report) = controller.poll_apply(&mut delegate).expect("poll") else {
        panic!("first transaction must still complete");
    };
    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    assert_eq!(controller.committed().title, "Новое имя");
}

#[test]
fn sync_apply_and_runtime_commit_refuse_to_run_over_a_pending_transaction() {
    let mut controller = controller_with_edited_title();
    let mut delegate = ScriptedStepwiseDelegate::new(Ok(CommittedApplyProgress::Pending), vec![]);
    controller.begin_apply(&mut delegate).expect("begin");
    let draft_before = controller.draft().clone();

    let sync_apply = controller.apply(&mut delegate);
    let runtime_commit = controller.commit_runtime_setting(
        RuntimeSettingCommitRequest::new("ui.sidebar.width_points", SettingValue::Integer(500)),
        &mut delegate,
    );

    assert!(sync_apply.is_err());
    assert!(runtime_commit.is_err());
    assert!(controller.has_pending_apply());
    assert_eq!(controller.draft(), &draft_before);
}

#[test]
fn cancel_rolls_back_without_persist_and_leaves_committed_untouched() {
    let mut controller = controller_with_edited_title();
    let mut delegate = ScriptedStepwiseDelegate::new(Ok(CommittedApplyProgress::Pending), vec![]);
    delegate.cancel_rollbacks = vec![RollbackReport::rolled_back(
        SettingRouteId::from("ui"),
        vec![SettingId::from("ui.title")],
    )];
    controller.begin_apply(&mut delegate).expect("begin");

    let report = controller
        .cancel_pending_apply(&mut delegate)
        .expect("pending transaction must produce a cancel report");

    assert_eq!(report.final_state, ApplyFinalState::Cancelled);
    assert_eq!(report.rollback.len(), 1);
    assert_eq!(delegate.cancels, 1);
    assert!(delegate.inner.persisted_documents.is_empty());
    assert!(!delegate.inner.calls.contains(&"finalize"));
    assert_eq!(controller.committed().title, "Исходное имя");
    assert!(!controller.has_pending_apply());
    // Повторная отмена и опрос после отмены — no-op, а не паника.
    assert!(controller.cancel_pending_apply(&mut delegate).is_none());
    assert_eq!(
        controller.poll_apply(&mut delegate).expect("poll"),
        PollApply::Idle
    );
    assert_eq!(delegate.polls, 0);
}

#[test]
fn route_set_without_pending_completes_in_a_single_begin_like_sync_apply() {
    let mut controller = controller_with_edited_title();
    let mut delegate = ScriptedStepwiseDelegate::new(
        Ok(CommittedApplyProgress::Finished(
            title_route_update_reports(),
        )),
        vec![],
    );

    let started = controller.begin_apply(&mut delegate).expect("begin");

    let BeginApply::Completed(report) = started else {
        panic!("sync-only routes must complete inside begin, got {started:?}");
    };
    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    assert_eq!(controller.committed().title, "Новое имя");
    assert!(!controller.has_pending_apply());
    assert_eq!(delegate.polls, 0);
}

#[test]
fn begin_without_changes_reports_no_changes_and_never_starts_routes() {
    let mut controller = SettingsController::new(TestDocument::default(), controller_registry());
    let mut delegate = ScriptedStepwiseDelegate::new(Ok(CommittedApplyProgress::Pending), vec![]);

    let started = controller.begin_apply(&mut delegate).expect("begin");

    let BeginApply::Completed(report) = started else {
        panic!("no changes must complete immediately");
    };
    assert_eq!(report.final_state, ApplyFinalState::NoChanges);
    assert!(!delegate.inner.calls.contains(&"begin"));
    assert!(!controller.has_pending_apply());
}
