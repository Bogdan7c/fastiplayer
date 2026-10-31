//! Многокадровая транзакция настроек (C2): пересборка media не блокирует кадр.
//!
//! Fake host притворяется владельцем пересборки: `begin_*` возвращает `Pending`, а итог по
//! сценарию приходит из `poll_media_rebuild` на следующих кадрах. Тесты проверяют наблюдаемое:
//! статус окна, наличие config-файла, отчёты отката и то, что синхронный путь не вызывался.

use std::collections::VecDeque;

use super::*;
use crate::settings_runtime::{MediaRebuildProgress, PlayerApplyStart, PlayerMediaContinuation};

/// Fake host с управляемой пересборкой; остальное делегирует записывающему адаптеру.
pub(super) struct StepwiseTestAdapter {
    pub(super) inner: RecordingRuntimeAdapter,
    /// Ответы `poll_media_rebuild` по порядку; после исчерпания — `Pending`.
    poll_script: VecDeque<MediaRebuildProgress>,
    /// Сколько раз runtime запускал пересборку без блокировки.
    pub(super) begin_calls: usize,
    /// Сколько раз runtime опрашивал пересборку.
    pub(super) poll_calls: usize,
    /// Сколько раз пересборку прервали на lifecycle-границе.
    pub(super) cancel_calls: usize,
    /// Итоги пересборки, пришедшие в продолжение player route.
    player_finish_results: Vec<AppRouteApplyResult>,
    /// Запускать ли пересборку player route (иначе её запускает media-service route).
    player_route_rebuilds: bool,
}

impl StepwiseTestAdapter {
    pub(super) fn new(config: &AppConfig, poll_script: Vec<MediaRebuildProgress>) -> Self {
        Self {
            inner: RecordingRuntimeAdapter::from_config(config).expect("The adapter should start."),
            poll_script: poll_script.into(),
            begin_calls: 0,
            poll_calls: 0,
            cancel_calls: 0,
            player_finish_results: Vec::new(),
            player_route_rebuilds: false,
        }
    }
}

impl RenderLiveSettingsAdapter for StepwiseTestAdapter {
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.inner.preview_live_settings(update)
    }

    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.inner.commit_live_settings(settings)
    }

    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.inner.rollback_live_settings(baseline)
    }
}

impl SettingsRuntimeReconfigureHost for StepwiseTestAdapter {
    fn sync_committed_config_snapshot(&mut self, snapshot: CommittedConfigSnapshot) {
        self.inner.sync_committed_config_snapshot(snapshot);
    }

    fn finalize_settings_transaction(&mut self) {
        self.inner.finalize_settings_transaction();
    }

    fn recreate_renderer(
        &mut self,
        previous: &RenderCommittedSettingsUpdate,
        next: &RenderCommittedSettingsUpdate,
    ) -> AppRouteApplyResult {
        self.inner.recreate_renderer(previous, next)
    }

    fn apply_player_runtime_settings(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
        target_policy: SettingsRouteTargetPolicy,
    ) -> PlayerRuntimeApplyResult {
        self.inner
            .apply_player_runtime_settings(update, target_policy)
    }

    fn apply_media_service_runtime_settings(
        &mut self,
        update: &MediaServiceRuntimeSettingsUpdate,
        affected_settings: &[SettingId],
        target_policy: SettingsRouteTargetPolicy,
    ) -> AppRouteApplyResult {
        self.inner
            .apply_media_service_runtime_settings(update, affected_settings, target_policy)
    }

    fn begin_media_service_runtime_settings(
        &mut self,
        _update: &MediaServiceRuntimeSettingsUpdate,
        _affected_settings: &[SettingId],
        _target_policy: SettingsRouteTargetPolicy,
    ) -> MediaRebuildProgress {
        self.begin_calls += 1;
        MediaRebuildProgress::Pending
    }

    fn begin_player_runtime_settings(
        &mut self,
        update: &PlayerCommittedSettingsUpdate,
        target_policy: SettingsRouteTargetPolicy,
    ) -> PlayerApplyStart {
        if !self.player_route_rebuilds {
            return PlayerApplyStart::Finished(
                self.apply_player_runtime_settings(update, target_policy),
            );
        }
        self.begin_calls += 1;
        PlayerApplyStart::AwaitingMediaRebuild(Box::new(PlayerMediaContinuation {
            report: player_core::PlayerRuntimeApplyReport::empty(),
            remaining_player_update: update.player_core.clone(),
            update: update.clone(),
        }))
    }

    fn finish_player_runtime_settings(
        &mut self,
        continuation: PlayerMediaContinuation,
        media_result: AppRouteApplyResult,
    ) -> PlayerRuntimeApplyResult {
        self.player_finish_results.push(media_result.clone());
        let mut report = continuation.report;
        let group_report = if media_result.is_success() {
            player_core::PlayerRuntimeApplyGroupReport::accepted(
                player_core::PlayerRuntimeApplyGroup::MediaPipeline,
                std::iter::empty(),
                player_core::PlayerRuntimeAcceptedChange::Applied,
                "fake media pipeline rebuilt",
            )
        } else {
            player_core::PlayerRuntimeApplyGroupReport::fatal(
                player_core::PlayerRuntimeApplyGroup::MediaPipeline,
                std::iter::empty(),
                "fake media pipeline failed",
            )
        };
        report.push(group_report);
        Ok(report)
    }

    fn poll_media_rebuild(&mut self) -> MediaRebuildProgress {
        self.poll_calls += 1;
        self.poll_script
            .pop_front()
            .unwrap_or(MediaRebuildProgress::Pending)
    }

    fn cancel_media_rebuild(&mut self) {
        self.cancel_calls += 1;
    }
}

fn language_and_network_actions(config: &AppConfig) -> Vec<SettingsUiAction> {
    vec![
        SettingsUiAction::Open,
        SettingsUiAction::SetValue {
            setting_id: SettingId::from("ui.language"),
            value: SettingValue::Text("en".to_string()),
        },
        SettingsUiAction::SetValue {
            setting_id: SettingId::from("network.read_ahead_mb"),
            value: SettingValue::Integer(config.network.read_ahead_mb as i64 + 1),
        },
        SettingsUiAction::Apply,
    ]
}

fn runtime_at(config: &AppConfig, test_name: &str) -> (SettingsRuntime, PathBuf) {
    let path = temp_config_path(test_name);
    remove_file_if_exists(&path);
    let runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime should be built");
    (runtime, path)
}

/// Один кадр UI с заданными действиями.
fn frame(
    runtime: &mut SettingsRuntime,
    adapter: &mut StepwiseTestAdapter,
    actions: Vec<SettingsUiAction>,
) {
    runtime
        .handle_ui_actions_with_runtime_adapter(actions, adapter)
        .expect("The settings frame should be processed");
}

fn status_summary(runtime: &mut SettingsRuntime) -> String {
    runtime
        .ui_model()
        .status
        .summary
        .clone()
        .unwrap_or_default()
}

#[test]
fn apply_with_media_rebuild_hands_control_back_and_writes_config_only_after_success() {
    let config = custom_config_for_test();
    let (mut runtime, path) = runtime_at(&config, "c2-success");
    let mut adapter = StepwiseTestAdapter::new(
        &config,
        vec![
            MediaRebuildProgress::Pending,
            MediaRebuildProgress::Finished(AppRouteApplyResult::Applied),
        ],
    );

    frame(
        &mut runtime,
        &mut adapter,
        language_and_network_actions(&config),
    );
    // Кадр запуска: пересборка стартовала, но кадр вернулся, не дожидаясь результата.
    frame(&mut runtime, &mut adapter, Vec::new());

    assert!(runtime.has_pending_apply(), "reassembly still under way");
    assert_eq!(adapter.begin_calls, 1);
    assert_eq!(
        adapter.inner.media_updates, 0,
        "blocking path should not be called"
    );
    assert_eq!(status_summary(&mut runtime), "Applying settings…");
    assert!(
        runtime.ui_model().command_state.is_busy,
        "Apply/OK must be inactive."
    );
    assert!(
        !path.exists(),
        "config You can't write until the reassembly is successful"
    );
    assert_eq!(runtime.committed_config().ui.language, config.ui.language);
    assert_eq!(adapter.inner.finalize_calls, 0);

    // Первый опрос: пересборка всё ещё идёт.
    frame(&mut runtime, &mut adapter, Vec::new());
    assert!(runtime.has_pending_apply());
    assert!(!path.exists());

    // Второй опрос: пересборка завершилась успехом.
    frame(&mut runtime, &mut adapter, Vec::new());
    assert_eq!(adapter.poll_calls, 2);
    assert!(!runtime.has_pending_apply());
    assert_eq!(
        runtime
            .latest_apply_report()
            .expect("final report")
            .final_state,
        ApplyFinalState::FullyApplied
    );
    assert_eq!(status_summary(&mut runtime), "Settings saved and applied");
    assert!(path.exists(), "successively config recorded");
    assert_eq!(runtime.committed_config().ui.language, "en");
    assert_eq!(adapter.inner.finalize_calls, 1);
    assert_eq!(adapter.inner.snapshot_synced_after_finalize, vec![true]);
    remove_file_if_exists(&path);
}

#[test]
fn failed_rebuild_rolls_back_applied_routes_and_never_writes_config() {
    let config = custom_config_for_test();
    let (mut runtime, path) = runtime_at(&config, "c2-failure");
    let mut adapter = StepwiseTestAdapter::new(
        &config,
        vec![MediaRebuildProgress::Finished(
            AppRouteApplyResult::Failed {
                message: "install failed: technical detail".to_string(),
            },
        )],
    );
    let mut actions = language_and_network_actions(&config);
    // Яркость — маршрут, который применяется ДО пересборки и должен откатиться.
    actions.insert(1, brightness_action(0.33));

    frame(&mut runtime, &mut adapter, actions);
    frame(&mut runtime, &mut adapter, Vec::new());
    assert!(runtime.has_pending_apply());
    assert_eq!(
        adapter.inner.render.commits.len(),
        1,
        "brightness applied before waiting for reassembly"
    );
    frame(&mut runtime, &mut adapter, Vec::new());

    let report = runtime.latest_apply_report().expect("failure report");
    assert_eq!(report.final_state, ApplyFinalState::RuntimeApplyFailed);
    assert!(report.persistence.is_none());
    assert!(!path.exists());
    assert_eq!(runtime.committed_config().ui.language, config.ui.language);
    assert_eq!(
        adapter.inner.render.rollbacks.len(),
        1,
        "previously applied brightness should be pumped out"
    );
    assert_eq!(
        adapter
            .inner
            .render
            .active
            .color_pipeline
            .adjustment
            .brightness,
        config.render.color_adjustment.brightness,
        "The brightness has returned to its former"
    );
    let summary = status_summary(&mut runtime);
    assert_eq!(summary, "Could not apply settings; changes rolled back");
    let model_text = format!("{:?}", runtime.ui_model().status);
    assert!(
        !model_text.contains("technical detail"),
        "Technical text should not be allowed to enter the window: {model_text}"
    );
    assert!(!runtime.has_pending_apply());
}

#[test]
fn second_apply_during_rebuild_is_busy_and_first_transaction_survives() {
    let config = custom_config_for_test();
    let (mut runtime, path) = runtime_at(&config, "c2-busy");
    let mut adapter = StepwiseTestAdapter::new(
        &config,
        vec![
            MediaRebuildProgress::Pending,
            MediaRebuildProgress::Finished(AppRouteApplyResult::Applied),
        ],
    );
    frame(
        &mut runtime,
        &mut adapter,
        language_and_network_actions(&config),
    );
    frame(&mut runtime, &mut adapter, Vec::new());
    assert!(runtime.has_pending_apply());

    // Повторный Apply и правка поля во время пересборки.
    frame(
        &mut runtime,
        &mut adapter,
        vec![
            SettingsUiAction::Apply,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.language"),
                value: SettingValue::Text("ru".to_string()),
            },
        ],
    );

    assert_eq!(
        adapter.begin_calls, 1,
        "The second transaction did not start."
    );
    assert!(runtime.has_pending_apply());
    let details = runtime.ui_model().status.details.join("\n");
    assert!(
        details.contains("The previous application is still in progress"),
        "The user should see \"busy\": {details}"
    );
    assert_eq!(
        runtime.controller.draft().ui.language,
        "en",
        "correction during application discarded"
    );

    // Первая транзакция доходит до конца и пишет именно свой документ.
    frame(&mut runtime, &mut adapter, Vec::new());
    frame(&mut runtime, &mut adapter, Vec::new());
    assert_eq!(
        runtime
            .latest_apply_report()
            .expect("report of the first transaction")
            .final_state,
        ApplyFinalState::FullyApplied
    );
    assert_eq!(runtime.committed_config().ui.language, "en");
    assert!(path.exists());
    remove_file_if_exists(&path);
}

#[test]
fn shutdown_during_rebuild_cancels_without_persisting() {
    let config = custom_config_for_test();
    let (mut runtime, path) = runtime_at(&config, "c2-shutdown");
    let mut adapter = StepwiseTestAdapter::new(&config, Vec::new());
    let mut actions = language_and_network_actions(&config);
    actions.insert(1, brightness_action(0.27));
    frame(&mut runtime, &mut adapter, actions);
    frame(&mut runtime, &mut adapter, Vec::new());
    assert!(runtime.has_pending_apply());

    runtime.cancel_pending_apply(&mut adapter);

    assert_eq!(adapter.cancel_calls, 1, "reassembly interrupted");
    assert!(!runtime.has_pending_apply());
    assert!(!path.exists(), "config not recorded");
    assert_eq!(runtime.committed_config().ui.language, config.ui.language);
    let report = runtime.latest_apply_report().expect("cancellation");
    assert_eq!(report.final_state, ApplyFinalState::Cancelled);
    assert_eq!(
        adapter.inner.render.rollbacks.len(),
        1,
        "previously applied route is rolled back"
    );
    // Кадры после отмены ничего не продвигают и не паникуют.
    frame(&mut runtime, &mut adapter, Vec::new());
    assert_eq!(adapter.poll_calls, 0);
    assert!(!path.exists());
    // Повторная отмена — no-op.
    runtime.cancel_pending_apply(&mut adapter);
    assert_eq!(adapter.cancel_calls, 1);
}

#[test]
fn transaction_without_rebuild_completes_in_one_frame_like_before() {
    let config = custom_config_for_test();
    let (mut runtime, path) = runtime_at(&config, "c2-sync-regression");
    let mut adapter = StepwiseTestAdapter::new(&config, Vec::new());
    frame(
        &mut runtime,
        &mut adapter,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("ui.language"),
                value: SettingValue::Text("en".to_string()),
            },
            SettingsUiAction::Apply,
        ],
    );
    assert!(
        runtime.has_pending_apply(),
        "Apply publishes its status first"
    );

    frame(&mut runtime, &mut adapter, Vec::new());

    assert!(!runtime.has_pending_apply());
    assert_eq!(adapter.begin_calls, 0);
    assert_eq!(adapter.poll_calls, 0);
    assert_eq!(
        runtime.latest_apply_report().expect("report").final_state,
        ApplyFinalState::FullyApplied
    );
    assert!(path.exists());
    remove_file_if_exists(&path);
}

#[test]
fn player_route_rebuild_finishes_remaining_steps_only_after_media_result() {
    let config = custom_config_for_test();
    let (mut runtime, path) = runtime_at(&config, "c2-player-route");
    let mut adapter = StepwiseTestAdapter::new(
        &config,
        vec![MediaRebuildProgress::Finished(AppRouteApplyResult::Applied)],
    );
    adapter.player_route_rebuilds = true;
    frame(
        &mut runtime,
        &mut adapter,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("player.demux.max_consecutive_corrupted_packets"),
                value: SettingValue::Integer(
                    config.player.demux.max_consecutive_corrupted_packets as i64 + 1,
                ),
            },
            SettingsUiAction::Apply,
        ],
    );
    frame(&mut runtime, &mut adapter, Vec::new());

    assert!(runtime.has_pending_apply());
    assert!(adapter.player_finish_results.is_empty());
    assert!(!path.exists());

    frame(&mut runtime, &mut adapter, Vec::new());

    assert_eq!(
        adapter.player_finish_results,
        vec![AppRouteApplyResult::Applied]
    );
    assert_eq!(
        runtime.latest_apply_report().expect("report").final_state,
        ApplyFinalState::FullyApplied
    );
    assert!(path.exists());
    remove_file_if_exists(&path);
}

#[test]
fn runtime_originated_commits_wait_until_the_transaction_finishes() {
    let config = custom_config_for_test();
    let (mut runtime, path) = runtime_at(&config, "c2-runtime-commit");
    let mut adapter = StepwiseTestAdapter::new(
        &config,
        vec![MediaRebuildProgress::Finished(AppRouteApplyResult::Applied)],
    );
    frame(
        &mut runtime,
        &mut adapter,
        language_and_network_actions(&config),
    );
    frame(&mut runtime, &mut adapter, Vec::new());
    assert!(runtime.has_pending_apply());
    let started_at = Instant::now();
    assert!(runtime.record_sidebar_width_change(
        SidebarWidthChange {
            width_points: SidebarWidthPoints::from_committed(470),
        },
        started_at
    ));

    // Пока транзакция идёт, запись ширины не стартует и не будит цикл просроченным deadline.
    let due_outcome = runtime
        .flush_due_sidebar_resize(started_at + Duration::from_secs(5), &mut adapter)
        .expect("Due flush should not fall");
    assert_eq!(
        due_outcome,
        crate::settings_runtime::SidebarResizeFlushOutcome::NoPending
    );
    assert_eq!(runtime.next_sidebar_resize_deadline(), None);

    // После завершения транзакции отложенная ширина записывается обычным путём.
    frame(&mut runtime, &mut adapter, Vec::new());
    assert!(!runtime.has_pending_apply());
    assert!(runtime.next_sidebar_resize_deadline().is_some());
    let flushed = runtime
        .flush_pending_sidebar_resize(&mut adapter)
        .expect("flush after application");
    assert_eq!(
        flushed,
        crate::settings_runtime::SidebarResizeFlushOutcome::Succeeded
    );
    assert_eq!(runtime.committed_config().ui.sidebar.width_points, 470);
    remove_file_if_exists(&path);
}
