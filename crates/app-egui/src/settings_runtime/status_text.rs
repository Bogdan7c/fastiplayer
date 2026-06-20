use super::*;

/// Формирует UI status по field reset report-у.
pub(super) fn status_from_reset(summary: &str, report: &ResetReport) -> SettingsUiStatus {
    SettingsUiStatus {
        summary: Some(summary.to_string()),
        details: vec![format!(
            "Изменены draft fields: {}",
            setting_ids_text(&report.affected_settings)
        )],
    }
}

/// Формирует UI status по cancel/rollback report-у.
pub(super) fn status_from_cancel(report: &CancelReport) -> SettingsUiStatus {
    let mut details = Vec::new();
    if !report.discarded_changes.is_empty() {
        details.push(format!(
            "Отброшены draft changes: {}",
            setting_ids_text(
                &report
                    .discarded_changes
                    .changes()
                    .iter()
                    .map(|change| change.id.clone())
                    .collect::<Vec<_>>()
            )
        ));
    }
    for rollback in &report.rolled_back_routes {
        details.push(format!(
            "Rollback route `{}`: {:?} ({})",
            rollback.route,
            rollback.result,
            setting_ids_text(&rollback.affected_settings)
        ));
    }

    SettingsUiStatus {
        summary: Some("Изменения отменены".to_string()),
        details,
    }
}

/// Формирует UI status по preview attempts, включая backpressure и non-retry errors.
pub(super) fn status_from_preview_reports(reports: &[PreviewApplyReport]) -> SettingsUiStatus {
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
        "Live preview не применён полностью"
    } else if has_backpressure {
        "Live preview ждёт renderer"
    } else {
        "Live preview применён"
    };
    SettingsUiStatus {
        summary: Some(summary.to_string()),
        details: reports.iter().map(preview_report_text).collect(),
    }
}

/// Формирует UI status по full apply report-у.
pub(super) fn status_from_apply_report(report: &ApplyReport) -> SettingsUiStatus {
    let mut details = Vec::new();
    if let Some(persistence) = &report.persistence {
        details.push(persist_report_text(persistence));
    }
    for route in &report.routes {
        details.push(route_report_text(route));
    }
    for conflict in &report.conflicts {
        details.push(format!(
            "Conflict route `{}`: baseline {}, current {}, settings {}",
            conflict.route,
            conflict.baseline.value(),
            conflict.current.value(),
            setting_ids_text(&conflict.affected_settings)
        ));
    }
    for error in &report.errors {
        details.push(error.to_string());
    }

    SettingsUiStatus {
        summary: Some(apply_final_state_text(report.final_state).to_string()),
        details,
    }
}

/// Человекочитаемое резюме final state.
fn apply_final_state_text(final_state: ApplyFinalState) -> &'static str {
    match final_state {
        ApplyFinalState::NoChanges => "Нет изменений для применения",
        ApplyFinalState::ValidationFailed => "Настройки не прошли полную проверку",
        ApplyFinalState::PersistFailed => "Не удалось сохранить TOML",
        ApplyFinalState::BlockedByConflicts => "Применение заблокировано конфликтом runtime route",
        ApplyFinalState::PersistedRuntimeDiverged => {
            "TOML сохранён, но runtime применился не полностью"
        }
        ApplyFinalState::FullyApplied => "Настройки сохранены и применены",
    }
}

/// Форматирует persistence outcome для status details.
fn persist_report_text(report: &PersistReport) -> String {
    let outcome = match report.outcome {
        PersistOutcome::Persisted => "TOML сохранён atomically",
        PersistOutcome::SkippedNoChanges => "TOML не записывался: durable changes отсутствуют",
    };
    match &report.durability_warning {
        Some(warning) => format!("{outcome}; durability warning: {warning}"),
        None => outcome.to_string(),
    }
}

/// Форматирует route apply result без потери partial/error semantics.
fn route_report_text(report: &ApplyRouteReport) -> String {
    format!(
        "Route `{}`: {} ({})",
        report.route,
        apply_route_result_text(&report.result),
        setting_ids_text(&report.affected_settings)
    )
}

/// Форматирует preview result без слияния backpressure и fatal errors.
fn preview_report_text(report: &PreviewApplyReport) -> String {
    format!(
        "Preview route `{}`: {} ({})",
        report.route,
        preview_apply_result_text(&report.result),
        setting_ids_text(&report.affected_settings)
    )
}

/// Человекочитаемый route apply result.
fn apply_route_result_text(result: &ApplyRouteResult) -> String {
    match result {
        ApplyRouteResult::Applied => "applied".to_string(),
        ApplyRouteResult::Noop => "no-op".to_string(),
        ApplyRouteResult::PreviewPromoted => "preview promoted".to_string(),
        ApplyRouteResult::PartialFailure { message } => format!("partial failure: {message}"),
        ApplyRouteResult::Failed { message } => format!("failed: {message}"),
        ApplyRouteResult::Conflict { baseline, current } => {
            format!(
                "conflict: baseline {}, current {}",
                baseline.value(),
                current.value()
            )
        }
    }
}

/// Человекочитаемый preview apply result.
fn preview_apply_result_text(result: &PreviewApplyResult) -> String {
    match result {
        PreviewApplyResult::Applied => "applied".to_string(),
        PreviewApplyResult::Noop => "no-op".to_string(),
        PreviewApplyResult::Backpressured => {
            "backpressured; latest update remains pending".to_string()
        }
        PreviewApplyResult::Unsupported { message } => format!("unsupported: {message}"),
        PreviewApplyResult::Fatal { message } => format!("fatal: {message}"),
    }
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

/// Корректирует route-level result для no-op/deferred player groups.
pub(super) fn group_result(
    group: &AppRuntimeRouteGroup,
    route_result: &AppRouteApplyResult,
) -> AppRouteApplyResult {
    match (group, route_result) {
        (
            AppRuntimeRouteGroup::PlayerDefaultVolume,
            AppRouteApplyResult::DeferredTechnicalDebt { .. },
        ) => AppRouteApplyResult::DeferredTechnicalDebt {
            message:
                "Default volume policy сохранён как committed setting; current volume не меняется"
                    .to_string(),
        },
        _ => route_result.clone(),
    }
}

/// Форматирует setting ids для report-а без потери конкретики.
pub(super) fn setting_ids_text(setting_ids: &[SettingId]) -> String {
    setting_ids
        .iter()
        .map(SettingId::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}
