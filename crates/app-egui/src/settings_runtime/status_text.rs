//! Тексты панели статуса настроек (окно настроек, строка под полями).
//!
//! Контракт: пользователь видит понятные английские фразы и человеческие названия настроек из
//! registry. Внутренние id, имена route-ов, `Debug`-формы и технические message владельцев
//! (renderer, player, media services) в панель не попадают — они пишутся в `tracing`.
//! Каждый вид результата перечислен явно (`match` без `_`).

use fastiplayer_settings::{SettingsFilePersistOutcome, settings_file_persist_outcome};

use super::setting_labels::SettingLabels;
use super::settings_error_message::settings_error_text;
use super::*;

/// Формирует UI status по field reset report-у.
pub(super) fn status_from_reset(
    labels: &SettingLabels<'_>,
    summary: &str,
    report: &ResetReport,
) -> SettingsUiStatus {
    let details = if report.affected_settings.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "Changed fields: {}",
            labels.quoted_list(&report.affected_settings)
        )]
    };
    SettingsUiStatus {
        summary: Some(summary.to_string()),
        details,
    }
}

/// Формирует UI status по cancel/rollback report-у.
pub(super) fn status_from_cancel(
    labels: &SettingLabels<'_>,
    report: &CancelReport,
) -> SettingsUiStatus {
    let mut details = Vec::new();
    if !report.discarded_changes.is_empty() {
        let discarded_ids = report
            .discarded_changes
            .changes()
            .iter()
            .map(|change| change.id.clone())
            .collect::<Vec<_>>();
        details.push(format!(
            "Discarded unsaved changes: {}",
            labels.quoted_list(&discarded_ids)
        ));
    }
    details.extend(
        report
            .rolled_back_routes
            .iter()
            .filter_map(|rollback| rollback_report_text(labels, rollback)),
    );

    SettingsUiStatus {
        summary: Some("Changes canceled".to_string()),
        details,
    }
}

/// Формирует UI status по preview attempts, включая backpressure и non-retry errors.
pub(super) fn status_from_preview_reports(
    labels: &SettingLabels<'_>,
    reports: &[PreviewApplyReport],
) -> SettingsUiStatus {
    let has_error = reports.iter().any(|report| {
        matches!(
            report.result,
            PreviewApplyResult::Unsupported { .. } | PreviewApplyResult::Fatal { .. }
        )
    });
    let has_backpressure = reports
        .iter()
        .any(|report| report.result == PreviewApplyResult::Backpressured);
    let summary = if has_error {
        "Preview applied partially"
    } else if has_backpressure {
        "Preview is waiting for the renderer"
    } else {
        "Preview applied"
    };
    SettingsUiStatus {
        summary: Some(summary.to_string()),
        details: reports
            .iter()
            .map(|report| preview_report_text(labels, report))
            .collect(),
    }
}

/// Формирует UI status по full apply report-у.
pub(super) fn status_from_apply_report(
    labels: &SettingLabels<'_>,
    report: &ApplyReport,
) -> SettingsUiStatus {
    let mut details = Vec::new();
    if let Some(persistence) = &report.persistence {
        details.push(persist_report_text(persistence));
    }
    for route in &report.routes {
        details.push(route_report_text(labels, route));
    }
    details.extend(
        report
            .rollback
            .iter()
            .filter_map(|rollback| rollback_report_text(labels, rollback)),
    );
    for conflict in &report.conflicts {
        tracing::warn!(
            route = conflict.route.as_str(),
            baseline_generation = conflict.baseline.value(),
            current_generation = conflict.current.value(),
            "Settings application stopped by a route generation conflict"
        );
        details.push(format!(
            "{}: the value changed elsewhere while you were editing",
            labels.quoted_list(&conflict.affected_settings)
        ));
    }
    for error in &report.errors {
        tracing::warn!(%error, "Error in settings application report");
        details.push(settings_error_text(labels, error));
    }
    if matches!(
        report.final_state,
        ApplyFinalState::RuntimeBlocked | ApplyFinalState::BlockedByConflicts
    ) {
        details.push(
            "Your draft is preserved. Click Apply again after the active operation finishes."
                .to_string(),
        );
    }

    let summary = match report
        .persistence
        .as_ref()
        .map(settings_file_persist_outcome)
    {
        // «Сохранены» было бы неправдой: файл сознательно не записан на весь запуск.
        Some(SettingsFilePersistOutcome::SessionOnly) => {
            crate::config_startup_notice::SETTINGS_KEPT_FOR_SESSION_ONLY_MESSAGE
        }
        // Файл сейчас записать нельзя (занят, повреждён, новее…): изменения действуют,
        // причина и «сохраним позже» — в строке деталей (решение 135).
        Some(SettingsFilePersistOutcome::NotSavedYet { .. }) => SETTINGS_APPLIED_NOT_SAVED_YET,
        Some(
            SettingsFilePersistOutcome::Saved
            | SettingsFilePersistOutcome::SavedDurabilityUnconfirmed { .. }
            | SettingsFilePersistOutcome::NothingToSave,
        )
        | None => apply_final_state_text(report.final_state),
    };
    SettingsUiStatus {
        summary: Some(summary.to_string()),
        details,
    }
}

/// Человекочитаемое резюме final state.
fn apply_final_state_text(final_state: ApplyFinalState) -> &'static str {
    match final_state {
        ApplyFinalState::NoChanges => "No changes to apply",
        ApplyFinalState::ValidationFailed => "Settings failed validation",
        ApplyFinalState::PersistFailed => "Could not save the settings file; changes rolled back",
        ApplyFinalState::BlockedByConflicts => "Application stopped: settings changed elsewhere",
        ApplyFinalState::RuntimeBlocked => "The player is busy; your draft is preserved",
        ApplyFinalState::RuntimeApplyFailed => "Could not apply settings; changes rolled back",
        ApplyFinalState::RollbackFailed => {
            "Could not fully restore previous settings; restart the app if the player behaves unexpectedly"
        }
        ApplyFinalState::FullyApplied => "Settings saved and applied",
        ApplyFinalState::Cancelled => "Settings application interrupted; changes rolled back",
    }
}

/// Резюме, когда изменения применены, но файл настроек сейчас записать нельзя.
const SETTINGS_APPLIED_NOT_SAVED_YET: &str = "Settings applied but not saved yet";

/// Форматирует persistence outcome для status details.
///
/// Отчёт читается только через `settings_file_persist_outcome`: «не сохранено сейчас» и
/// «запись запрещена на весь запуск» различает `fastiplayer-settings`, а не эта функция.
fn persist_report_text(report: &PersistReport) -> String {
    match settings_file_persist_outcome(report) {
        SettingsFilePersistOutcome::Saved => "Settings file saved".to_string(),
        SettingsFilePersistOutcome::SavedDurabilityUnconfirmed { warning } => {
            tracing::warn!(%warning, "Settings written, but durable storage could not be confirmed");
            "Settings file saved, but the system did not confirm durable storage".to_string()
        }
        // Сообщение уже английское и для пользователя: причина и что будет с изменениями.
        SettingsFilePersistOutcome::NotSavedYet { notice } => notice.to_string(),
        SettingsFilePersistOutcome::SessionOnly => {
            "The settings file cannot be modified during this session".to_string()
        }
        SettingsFilePersistOutcome::NothingToSave => {
            "Settings file unchanged: nothing to save".to_string()
        }
    }
}

/// Строка про один откат: успешный откат без изменений пользователю не интересен.
fn rollback_report_text(labels: &SettingLabels<'_>, report: &RollbackReport) -> Option<String> {
    let settings = labels.quoted_list(&report.affected_settings);
    match &report.result {
        RollbackResult::RolledBack => Some(format!("{settings}: previous value restored")),
        RollbackResult::Noop => None,
        RollbackResult::Failed { message } => {
            tracing::warn!(
                route = report.route.as_str(),
                %message,
                "Could not roll back the settings route"
            );
            Some(format!("{settings}: could not restore the previous value"))
        }
    }
}

/// Форматирует route apply result без потери partial/error semantics.
fn route_report_text(labels: &SettingLabels<'_>, report: &ApplyRouteReport) -> String {
    let settings = labels.quoted_list(&report.affected_settings);
    let outcome = match &report.result {
        ApplyRouteResult::Applied | ApplyRouteResult::PreviewPromoted => {
            applied_route_text(report.mechanism)
        }
        ApplyRouteResult::Noop => "already in this state",
        ApplyRouteResult::PartialFailure { message } => {
            log_route_failure(report, message);
            "applied partially"
        }
        ApplyRouteResult::Failed { message } => {
            log_route_failure(report, message);
            "could not apply"
        }
        ApplyRouteResult::RuntimeBusy { activity } => {
            tracing::info!(
                route = report.route.as_str(),
                %activity,
                "Settings route not applied: runtime is busy"
            );
            "the player is busy seeking or switching sources; the change was not applied"
        }
        ApplyRouteResult::Conflict { baseline, current } => {
            tracing::warn!(
                route = report.route.as_str(),
                baseline_generation = baseline.value(),
                current_generation = current.value(),
                "Settings route generation conflict"
            );
            "the value changed elsewhere while you were editing"
        }
    };
    format!("{settings}: {outcome}")
}

/// Текст успешного применения: механизм важен, когда пользователь мог заметить паузу.
fn applied_route_text(mechanism: ApplyMechanism) -> &'static str {
    match mechanism {
        ApplyMechanism::InPlace
        | ApplyMechanism::WorkerReconfigure
        | ApplyMechanism::PreviewPromoted => "applied",
        ApplyMechanism::PipelineRebuild | ApplyMechanism::RendererRecreate => {
            "applied; playback may have been briefly interrupted"
        }
        ApplyMechanism::AppliesAfterRestart => "saved; applies after restart",
    }
}

/// Технические детали сбоя route-а — только в лог.
fn log_route_failure(report: &ApplyRouteReport, message: &str) {
    tracing::warn!(
        route = report.route.as_str(),
        mechanism = ?report.mechanism,
        %message,
        "Settings route not applied"
    );
}

/// Форматирует preview result без слияния backpressure и fatal errors.
fn preview_report_text(labels: &SettingLabels<'_>, report: &PreviewApplyReport) -> String {
    let settings = labels.quoted_list(&report.affected_settings);
    let outcome = match &report.result {
        PreviewApplyResult::Applied => "shown in preview",
        PreviewApplyResult::Noop => "unchanged",
        PreviewApplyResult::Backpressured => "waiting for the renderer; will apply shortly",
        PreviewApplyResult::Unsupported { message } => {
            log_preview_failure(report, message);
            "preview is unavailable for this setting"
        }
        PreviewApplyResult::Fatal { message } => {
            log_preview_failure(report, message);
            "could not show preview"
        }
    };
    format!("{settings}: {outcome}")
}

/// Технические детали сбоя preview — только в лог.
fn log_preview_failure(report: &PreviewApplyReport, message: &str) {
    tracing::warn!(route = report.route.as_str(), %message, "Settings preview not applied");
}

/// Строит per-group reports, не теряя affected setting ids.
pub(super) fn group_reports(
    groups: Vec<AppRuntimeRouteGroupUpdate>,
    result: AppRouteApplyResult,
) -> Vec<AppRouteGroupReport> {
    groups
        .into_iter()
        .map(|group| AppRouteGroupReport {
            group: group.group,
            result: group_result(&group.group, &result),
            affected_settings: group.affected_settings,
        })
        .collect()
}

/// Сохраняет route-level result для owner group без compatibility aliases.
pub(super) fn group_result(
    group: &AppRuntimeRouteGroup,
    route_result: &AppRouteApplyResult,
) -> AppRouteApplyResult {
    match group {
        // Профиль рендера при успехе транзакции только сохранён: эффект после перезапуска.
        AppRuntimeRouteGroup::RenderProfileRestart if route_result.is_success() => {
            AppRouteApplyResult::AppliesAfterRestart
        }
        _ => route_result.clone(),
    }
}

#[cfg(test)]
mod deferred_save_tests {
    //! Файл настроек сейчас записать нельзя (решение 135): статус окна настроек называет
    //! причину по-английски и говорит, что изменения действуют и будут сохранены позже.

    use settings_core::{PersistRequest, SettingsDiff, SettingsPersister};

    use super::*;

    /// Отчёт сохранения от настоящего store при битом файле на диске.
    fn persist_report_with_broken_file() -> PersistReport {
        let config_path = std::env::temp_dir().join(format!(
            "fastiplayer-status-deferred-save-{}.toml",
            std::process::id()
        ));
        std::fs::write(&config_path, "[[[").expect("broken settings file written");
        let registry = fastiplayer_settings::app_config_registry().expect("registry builds");
        let mut document = AppConfig::default();
        document.ui.sidebar.width_points = 480;
        let changed_settings = registry
            .diff(&AppConfig::default(), &document)
            .expect("diff builds");
        let report = fastiplayer_settings::AppConfigStore::new(&config_path)
            .persist(PersistRequest {
                document: &document,
                changed_settings: &changed_settings,
            })
            .expect("a failed write is not a transaction error");
        assert_eq!(
            std::fs::read_to_string(&config_path).expect("file still there"),
            "[[["
        );
        std::fs::remove_file(&config_path).expect("test file removed");
        report
    }

    #[test]
    fn status_names_the_reason_and_promises_a_later_save() {
        let registry = fastiplayer_settings::app_config_registry().expect("registry builds");
        let labels = SettingLabels::new(&registry);
        let report = ApplyReport {
            validation: None,
            persistence: Some(persist_report_with_broken_file()),
            routes: Vec::new(),
            rollback: Vec::new(),
            changed_settings: SettingsDiff::new(),
            final_state: ApplyFinalState::FullyApplied,
            conflicts: Vec::new(),
            errors: Vec::new(),
        };

        let status = status_from_apply_report(&labels, &report);

        assert_eq!(
            status.summary.as_deref(),
            Some(SETTINGS_APPLIED_NOT_SAVED_YET)
        );
        let details = status.details.join("\n");
        assert!(details.contains("settings file is corrupted"), "{details}");
        assert!(
            details.contains("in effect until you close Fastiplayer"),
            "{details}"
        );
        assert!(
            details.contains("will be saved with the next change"),
            "{details}"
        );
        assert!(!details.contains("Could not read or write"), "{details}");
    }
}
