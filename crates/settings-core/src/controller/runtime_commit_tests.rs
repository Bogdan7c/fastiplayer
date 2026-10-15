//! Тесты батч-коммита runtime-значений одной транзакцией.

use super::super::tests::{RecordingApplyDelegate, TestDocument, controller_registry};
use crate::{
    ApplyFinalState, RouteGeneration, RuntimeCommitOutcome, RuntimeSettingCommitRequest, SettingId,
    SettingRouteId, SettingValue, SettingsController, SettingsError,
};

fn width_and_title_batch(width: i64, title: &str) -> Vec<RuntimeSettingCommitRequest> {
    vec![
        RuntimeSettingCommitRequest::new("ui.sidebar.width_points", SettingValue::Integer(width)),
        RuntimeSettingCommitRequest::new("ui.title", SettingValue::Text(title.to_owned())),
    ]
}

#[test]
fn batch_commits_all_values_in_one_persistence_write() {
    let mut controller = SettingsController::new(TestDocument::default(), controller_registry());
    let mut delegate = RecordingApplyDelegate::default();

    let outcome = controller
        .commit_runtime_settings(width_and_title_batch(480, "Batch title"), &mut delegate)
        .expect("batch should commit");

    let report = outcome.report().expect("transaction must have run");
    assert_eq!(report.final_state, ApplyFinalState::FullyApplied);
    // Один validate, один apply, одна запись, один finalize на все поля.
    assert_eq!(
        delegate.calls,
        vec!["validate", "preflight", "apply", "persist", "finalize"]
    );
    assert_eq!(delegate.persisted_documents.len(), 1);
    assert_eq!(delegate.persisted_documents[0].sidebar_width, 480);
    assert_eq!(delegate.persisted_documents[0].title, "Batch title");
    assert_eq!(controller.committed().sidebar_width, 480);
    assert_eq!(controller.committed().title, "Batch title");
    assert_eq!(controller.draft().title, "Batch title");
}

#[test]
fn batch_failure_rolls_back_every_value_and_keeps_draft() {
    let mut controller = SettingsController::new(TestDocument::default(), controller_registry());
    controller
        .set_value(
            SettingId::from("render.brightness"),
            SettingValue::Float(0.9),
        )
        .expect("neighbour draft edit");
    let mut delegate = RecordingApplyDelegate {
        persist_error: Some(SettingsError::access_failed("disk unavailable")),
        ..RecordingApplyDelegate::default()
    };

    let outcome = controller
        .commit_runtime_settings(width_and_title_batch(500, "Lost title"), &mut delegate)
        .expect("persistence failure is reported, not thrown");

    assert_eq!(
        outcome.report().expect("ran").final_state,
        ApplyFinalState::PersistFailed
    );
    assert_eq!(controller.committed().sidebar_width, 420);
    assert_eq!(controller.committed().title, TestDocument::default().title);
    assert_eq!(controller.draft().sidebar_width, 420);
    assert_eq!(controller.draft().brightness, 0.9);
}

#[test]
fn batch_validation_error_leaves_all_state_untouched() {
    let mut controller = SettingsController::new(TestDocument::default(), controller_registry());
    let mut delegate = RecordingApplyDelegate {
        validate_error: Some(SettingsError::access_failed("invalid document")),
        ..RecordingApplyDelegate::default()
    };

    let outcome = controller
        .commit_runtime_settings(width_and_title_batch(500, "Rejected"), &mut delegate)
        .expect("validator rejection is reported through the transaction report");

    assert_eq!(
        outcome.report().expect("ran").final_state,
        ApplyFinalState::ValidationFailed
    );
    assert_eq!(controller.committed(), &TestDocument::default());
    assert_eq!(controller.draft(), &TestDocument::default());
    assert!(delegate.persisted_documents.is_empty());
}

#[test]
fn batch_preserves_neighbouring_dirty_fields_and_rebases_every_route() {
    let mut controller = SettingsController::new(TestDocument::default(), controller_registry());
    // Грязное соседнее поле маршрута `ui` (не входит в запрос).
    controller
        .set_value(
            SettingId::from("ui.sidebar.width_points"),
            SettingValue::Integer(555),
        )
        .expect("dirty width");
    controller
        .set_value(
            SettingId::from("render.brightness"),
            SettingValue::Float(0.8),
        )
        .expect("render preview edit");
    let ui_route = SettingRouteId::from("ui");
    let render_route = SettingRouteId::from("render");
    let mut delegate = RecordingApplyDelegate::default();

    controller
        .commit_runtime_settings(
            vec![
                RuntimeSettingCommitRequest::new("ui.title", SettingValue::Text("T".to_owned())),
                RuntimeSettingCommitRequest::new("render.brightness", SettingValue::Float(0.3)),
            ],
            &mut delegate,
        )
        .expect("batch over two routes commits");

    assert_eq!(controller.committed().title, "T");
    assert_eq!(controller.committed().brightness, 0.3);
    // Незавершённые правки соседних полей пережили коммит.
    assert_eq!(controller.draft().sidebar_width, 555);
    assert_eq!(controller.draft().brightness, 0.3);
    // Baseline каждой затронутой маршрута сдвинут на новое поколение: apply не видит conflict.
    assert_eq!(
        controller.route_generation_baseline(&ui_route),
        Some(controller.route_generation(&ui_route))
    );
    assert_eq!(
        controller.route_generation(&ui_route),
        RouteGeneration::new(1)
    );
    // Маршрут без соседних правок: baseline снят, generation тоже вырос.
    assert_eq!(controller.route_generation_baseline(&render_route), None);
    assert_eq!(
        controller.route_generation(&render_route),
        RouteGeneration::new(1)
    );
    let mut user_delegate = RecordingApplyDelegate::default();
    let report = controller.apply(&mut user_delegate).expect("user apply");
    assert!(report.conflicts.is_empty());
    assert_eq!(controller.committed().sidebar_width, 555);
}

#[test]
fn batch_with_unknown_id_fails_before_any_mutation() {
    let mut controller = SettingsController::new(TestDocument::default(), controller_registry());
    controller
        .set_value(
            SettingId::from("ui.title"),
            SettingValue::Text("Draft".to_owned()),
        )
        .expect("draft edit");
    let mut delegate = RecordingApplyDelegate::default();

    let error = controller
        .commit_runtime_settings(
            vec![
                RuntimeSettingCommitRequest::new(
                    "ui.sidebar.width_points",
                    SettingValue::Integer(480),
                ),
                RuntimeSettingCommitRequest::new("no.such.setting", SettingValue::Bool(true)),
            ],
            &mut delegate,
        )
        .expect_err("unknown id is an error");

    assert!(matches!(error, SettingsError::UnknownSetting { .. }));
    assert!(
        delegate.calls.is_empty(),
        "no validation or persistence ran"
    );
    assert_eq!(controller.committed(), &TestDocument::default());
    assert_eq!(controller.draft().title, "Draft");
}

#[test]
fn batch_with_out_of_range_value_fails_before_any_mutation() {
    let mut controller = SettingsController::new(TestDocument::default(), controller_registry());
    let mut delegate = RecordingApplyDelegate::default();

    let result = controller
        .commit_runtime_settings(width_and_title_batch(10_000, "Never saved"), &mut delegate);

    assert!(
        result.is_err(),
        "width above 600 violates the descriptor range"
    );
    assert!(delegate.calls.is_empty());
    assert_eq!(controller.committed(), &TestDocument::default());
}

#[test]
fn empty_batch_is_an_explicit_noop() {
    let mut controller = SettingsController::new(TestDocument::default(), controller_registry());
    let mut delegate = RecordingApplyDelegate::default();

    let outcome = controller
        .commit_runtime_settings(Vec::new(), &mut delegate)
        .expect("empty batch is not an error");

    assert_eq!(outcome, RuntimeCommitOutcome::NothingRequested);
    assert!(outcome.report().is_none());
    assert!(delegate.calls.is_empty());
    assert_eq!(controller.committed(), &TestDocument::default());
}

#[test]
fn duplicate_ids_in_a_batch_keep_the_last_value() {
    let mut controller = SettingsController::new(TestDocument::default(), controller_registry());
    let mut delegate = RecordingApplyDelegate::default();

    controller
        .commit_runtime_settings(
            vec![
                RuntimeSettingCommitRequest::new(
                    "ui.sidebar.width_points",
                    SettingValue::Integer(400),
                ),
                RuntimeSettingCommitRequest::new(
                    "ui.sidebar.width_points",
                    SettingValue::Integer(450),
                ),
            ],
            &mut delegate,
        )
        .expect("duplicate batch commits");

    assert_eq!(controller.committed().sidebar_width, 450);
    assert_eq!(delegate.persisted_documents.len(), 1);
}
