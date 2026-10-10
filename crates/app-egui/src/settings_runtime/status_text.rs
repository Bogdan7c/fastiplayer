//! Тексты панели статуса настроек (окно настроек, строка под полями).
//!
//! Контракт: пользователь видит только русские фразы и человеческие названия настроек из
//! registry. Внутренние id, имена route-ов, `Debug`-формы и английские message владельцев
//! (renderer, player, media services) в панель не попадают — они пишутся в `tracing`.
//! Каждый вид результата перечислен явно (`match` без `_`).

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
            "Изменены поля: {}",
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
            "Отброшены несохранённые изменения: {}",
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
        summary: Some("Изменения отменены".to_string()),
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
        "Предпросмотр применён не полностью"
    } else if has_backpressure {
        "Предпросмотр ждёт отрисовщик"
    } else {
        "Предпросмотр применён"
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
            "Применение настроек остановлено конфликтом поколений route-а"
        );
        details.push(format!(
            "{}: значение изменилось в другом месте, пока вы редактировали",
            labels.quoted_list(&conflict.affected_settings)
        ));
    }
    for error in &report.errors {
        tracing::warn!(%error, "Ошибка в отчёте применения настроек");
        details.push(settings_error_text(labels, error));
    }
    if matches!(
        report.final_state,
        ApplyFinalState::RuntimeBlocked | ApplyFinalState::BlockedByConflicts
    ) {
        details.push(
            "Черновик сохранён. После завершения активной операции нажмите «Применить» ещё раз."
                .to_string(),
        );
    }

    let kept_for_session_only = report
        .persistence
        .as_ref()
        .is_some_and(|persistence| persistence.outcome == PersistOutcome::SkippedSessionOnly);
    let summary = if kept_for_session_only {
        // «Сохранены» было бы неправдой: файл сознательно не записан.
        crate::config_startup_notice::SETTINGS_KEPT_FOR_SESSION_ONLY_MESSAGE
    } else {
        apply_final_state_text(report.final_state)
    };
    SettingsUiStatus {
        summary: Some(summary.to_string()),
        details,
    }
}

/// Человекочитаемое резюме final state.
fn apply_final_state_text(final_state: ApplyFinalState) -> &'static str {
    match final_state {
        ApplyFinalState::NoChanges => "Нет изменений для применения",
        ApplyFinalState::ValidationFailed => "Настройки не прошли проверку",
        ApplyFinalState::PersistFailed => "Не удалось сохранить файл настроек; изменения отменены",
        ApplyFinalState::BlockedByConflicts => {
            "Применение остановлено: настройки изменились в другом месте"
        }
        ApplyFinalState::RuntimeBlocked => "Плеер сейчас занят; черновик сохранён",
        ApplyFinalState::RuntimeApplyFailed => "Не удалось применить настройки; изменения отменены",
        ApplyFinalState::RollbackFailed => {
            "Не удалось полностью вернуть прежние настройки; если плеер ведёт себя странно, перезапустите приложение"
        }
        ApplyFinalState::FullyApplied => "Настройки сохранены и применены",
    }
}

/// Форматирует persistence outcome для status details.
fn persist_report_text(report: &PersistReport) -> String {
    let outcome = match report.outcome {
        PersistOutcome::Persisted => "Файл настроек записан",
        PersistOutcome::SkippedNoChanges => "Файл настроек не менялся: сохранять нечего",
        PersistOutcome::SkippedSessionOnly => "Файл настроек в этом запуске не изменяется",
    };
    match &report.durability_warning {
        Some(warning) => {
            tracing::warn!(%warning, "Настройки записаны, но надёжность записи на диск не подтверждена");
            format!("{outcome}, но система не подтвердила надёжную запись на диск")
        }
        None => outcome.to_string(),
    }
}

/// Строка про один откат: успешный откат без изменений пользователю не интересен.
fn rollback_report_text(labels: &SettingLabels<'_>, report: &RollbackReport) -> Option<String> {
    let settings = labels.quoted_list(&report.affected_settings);
    match &report.result {
        RollbackResult::RolledBack => Some(format!("{settings}: прежнее значение возвращено")),
        RollbackResult::Noop => None,
        RollbackResult::Failed { message } => {
            tracing::warn!(
                route = report.route.as_str(),
                %message,
                "Не удалось откатить route настроек"
            );
            Some(format!("{settings}: не удалось вернуть прежнее значение"))
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
        ApplyRouteResult::Noop => "уже было в таком состоянии",
        ApplyRouteResult::PartialFailure { message } => {
            log_route_failure(report, message);
            "применено только частично"
        }
        ApplyRouteResult::Failed { message } => {
            log_route_failure(report, message);
            "не удалось применить"
        }
        ApplyRouteResult::RuntimeBusy { activity } => {
            tracing::info!(
                route = report.route.as_str(),
                %activity,
                "Route настроек не применён: runtime занят"
            );
            "плеер занят перемоткой или сменой источника, изменение не применено"
        }
        ApplyRouteResult::Conflict { baseline, current } => {
            tracing::warn!(
                route = report.route.as_str(),
                baseline_generation = baseline.value(),
                current_generation = current.value(),
                "Конфликт поколений route-а настроек"
            );
            "значение изменилось в другом месте, пока вы редактировали"
        }
    };
    format!("{settings}: {outcome}")
}

/// Текст успешного применения: механизм важен, когда пользователь мог заметить паузу.
fn applied_route_text(mechanism: ApplyMechanism) -> &'static str {
    match mechanism {
        ApplyMechanism::InPlace
        | ApplyMechanism::WorkerReconfigure
        | ApplyMechanism::PreviewPromoted => "применено",
        ApplyMechanism::PipelineRebuild | ApplyMechanism::RendererRecreate => {
            "применено; воспроизведение могло ненадолго прерваться"
        }
    }
}

/// Технические детали сбоя route-а — только в лог.
fn log_route_failure(report: &ApplyRouteReport, message: &str) {
    tracing::warn!(
        route = report.route.as_str(),
        mechanism = ?report.mechanism,
        %message,
        "Route настроек не применён"
    );
}

/// Форматирует preview result без слияния backpressure и fatal errors.
fn preview_report_text(labels: &SettingLabels<'_>, report: &PreviewApplyReport) -> String {
    let settings = labels.quoted_list(&report.affected_settings);
    let outcome = match &report.result {
        PreviewApplyResult::Applied => "показано в предпросмотре",
        PreviewApplyResult::Noop => "без изменений",
        PreviewApplyResult::Backpressured => "ждёт отрисовщик, применится чуть позже",
        PreviewApplyResult::Unsupported { message } => {
            log_preview_failure(report, message);
            "для этой настройки предпросмотр недоступен"
        }
        PreviewApplyResult::Fatal { message } => {
            log_preview_failure(report, message);
            "не удалось показать предпросмотр"
        }
    };
    format!("{settings}: {outcome}")
}

/// Технические детали сбоя preview — только в лог.
fn log_preview_failure(report: &PreviewApplyReport, message: &str) {
    tracing::warn!(route = report.route.as_str(), %message, "Preview настроек не применён");
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
    let _ = group;
    route_result.clone()
}
