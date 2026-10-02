//! Debounce и сохранение ширины sidebar, единственный владелец AppConfig.

use super::*;

/// Latest-only debounce заменяет старое значение, не переносит deadline для того же
/// округлённого width и будит idle loop ровно к новому quiet-period.
#[test]
fn sidebar_resize_debounce_coalesces_and_persists_latest_width() {
    let config = custom_config_for_test();
    let path = temp_config_path("sidebar-resize-debounce");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime should build");
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter should build");
    let started_at = Instant::now();

    assert!(runtime.record_sidebar_width_change(sidebar_change(450), started_at));
    let first_deadline = runtime
        .next_sidebar_resize_deadline()
        .expect("first resize should schedule deadline");
    assert!(
        !runtime.record_sidebar_width_change(
            sidebar_change(450),
            started_at + Duration::from_millis(100)
        )
    );
    assert_eq!(runtime.next_sidebar_resize_deadline(), Some(first_deadline));

    let replacement_at = started_at + Duration::from_millis(200);
    assert!(runtime.record_sidebar_width_change(sidebar_change(480), replacement_at));
    let replacement_deadline = replacement_at + Duration::from_millis(500);
    assert_eq!(
        runtime.next_sidebar_resize_deadline(),
        Some(replacement_deadline)
    );
    assert_eq!(
        runtime
            .flush_due_sidebar_resize(
                replacement_deadline - Duration::from_millis(1),
                &mut adapter
            )
            .expect("early tick should stay pending"),
        super::super::SidebarResizeFlushOutcome::NoPending
    );
    assert_eq!(runtime.committed_config().ui.sidebar.width_points, 420);

    assert_eq!(
        runtime
            .flush_due_sidebar_resize(replacement_deadline, &mut adapter)
            .expect("deadline tick should commit"),
        super::super::SidebarResizeFlushOutcome::Succeeded
    );
    assert_eq!(runtime.next_sidebar_resize_deadline(), None);
    assert_eq!(runtime.committed_config().ui.sidebar.width_points, 480);
    assert_eq!(
        adapter
            .committed_snapshots
            .last()
            .expect("successful resize should sync snapshot")
            .sidebar_width_points(),
        480
    );
    let persisted =
        fastiplayer_config::load_from_path(&path).expect("persisted sidebar config should reload");
    assert_eq!(persisted.config.ui.sidebar.width_points, 480);
    remove_file_if_exists(&path);
}

/// Drag commit сохраняет соседний draft, Apply применяет его без conflict, а более позднее
/// ручное редактирование width выигрывает у pending drag.
#[test]
fn sidebar_resize_order_preserves_draft_and_last_direct_action_wins() {
    let config = custom_config_for_test();
    let path = temp_config_path("sidebar-resize-order");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime should build");
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter should build");

    runtime
        .handle_ui_actions_with_runtime_adapter(
            vec![
                SettingsUiAction::Open,
                SettingsUiAction::SetValue {
                    setting_id: SettingId::from("ui.language"),
                    value: SettingValue::Text("en".to_owned()),
                },
            ],
            &mut adapter,
        )
        .expect("neighbor UI draft should open");
    runtime.record_sidebar_width_change(sidebar_change(460), Instant::now());
    run_runtime_actions(&mut runtime, vec![SettingsUiAction::Apply], &mut adapter);
    assert_eq!(runtime.committed_config().ui.sidebar.width_points, 460);
    assert_eq!(runtime.committed_config().ui.language, "en");

    runtime.record_sidebar_width_change(sidebar_change(480), Instant::now());
    runtime
        .handle_ui_actions_with_runtime_adapter(
            vec![SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.sidebar.width_points"),
                value: SettingValue::Integer(520),
            }],
            &mut adapter,
        )
        .expect("manual width edit should first flush pending drag");
    run_runtime_actions(&mut runtime, vec![SettingsUiAction::Apply], &mut adapter);
    assert_eq!(runtime.committed_config().ui.sidebar.width_points, 520);

    runtime
        .handle_ui_actions_with_runtime_adapter(
            vec![SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.language"),
                value: SettingValue::Text("ru".to_owned()),
            }],
            &mut adapter,
        )
        .expect("second neighbor draft should be accepted");
    runtime.record_sidebar_width_change(sidebar_change(490), Instant::now());
    assert_eq!(
        runtime
            .flush_pending_sidebar_resize(&mut adapter)
            .expect("lifecycle-style force flush should ignore future deadline"),
        super::super::SidebarResizeFlushOutcome::Succeeded
    );
    runtime
        .handle_ui_actions_with_runtime_adapter(vec![SettingsUiAction::Cancel], &mut adapter)
        .expect("Cancel should discard only remaining draft");
    assert_eq!(runtime.committed_config().ui.sidebar.width_points, 490);
    assert_eq!(runtime.committed_config().ui.language, "en");

    remove_file_if_exists(&path);
}

/// Persistence failure сохраняет committed/draft, компенсирует runtime route и явно
/// возвращает live host к последней сохранённой ширине.
#[test]
fn sidebar_resize_persistence_failure_rolls_back_live_width_and_preserves_draft() {
    let config = custom_config_for_test();
    let path = temp_config_path("sidebar-resize-persist-failure");
    remove_file_if_exists(&path);
    fs::create_dir_all(&path).expect("target directory creates deterministic rename failure");
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime should build");
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter should build");
    runtime
        .handle_ui_actions_with_runtime_adapter(
            vec![
                SettingsUiAction::Open,
                SettingsUiAction::SetValue {
                    setting_id: SettingId::from("ui.language"),
                    value: SettingValue::Text("en".to_owned()),
                },
            ],
            &mut adapter,
        )
        .expect("neighbor draft should be accepted");
    runtime.record_sidebar_width_change(sidebar_change(500), Instant::now());

    assert_eq!(
        runtime
            .flush_pending_sidebar_resize(&mut adapter)
            .expect("typed persistence failure should stay in apply report"),
        super::super::SidebarResizeFlushOutcome::Failed
    );
    assert_eq!(
        runtime
            .latest_apply_report()
            .expect("failure report should be retained")
            .final_state,
        ApplyFinalState::PersistFailed
    );
    assert_eq!(runtime.committed_config().ui.sidebar.width_points, 420);
    assert_eq!(runtime.controller.draft().ui.sidebar.width_points, 420);
    assert_eq!(runtime.controller.draft().ui.language, "en");
    assert!(adapter.committed_snapshots.is_empty());
    assert_eq!(
        adapter.restored_sidebar_widths,
        vec![SidebarWidthPoints::from_committed(420)]
    );
    assert!(
        runtime
            .ui_model()
            .status
            .summary
            .as_deref()
            .is_some_and(|summary| summary.contains("сохран"))
    );
    fs::remove_dir_all(&path).expect("test target directory should be removed");
}

#[test]
fn app_shell_and_app_state_do_not_keep_duplicate_mutable_app_config_owner() {
    let app_shell_source = include_str!("../../app_shell/mod.rs");
    let app_state_source = include_str!("../../state.rs");

    assert!(
        !app_shell_source.contains("app_config: AppConfig"),
        "AppShell должен владеть SettingsRuntime, а не отдельным AppConfig"
    );
    assert!(
        !app_state_source.contains("app_config: AppConfig"),
        "AppState должен хранить controlled snapshot, а не второй AppConfig owner"
    );
    assert!(
        !app_state_source.contains("pub app_config"),
        "AppState не должен открывать mutable config storage наружу"
    );
}

fn sidebar_change(width_points: u16) -> SidebarWidthChange {
    SidebarWidthChange {
        width_points: SidebarWidthPoints::from_committed(width_points),
    }
}
