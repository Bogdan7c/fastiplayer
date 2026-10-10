//! Панель статуса настроек: пользователь видит человеческий русский текст и названия настроек,
//! а не внутренние id, `Debug`-формы и английские сообщения владельцев.

use settings_core::{
    ApplyReport, ApplyRouteReport, PersistReport, PreviewApplyReport, PreviewApplyResult,
    RollbackReport, RouteGeneration, RouteGenerationConflict, SettingsDiff,
};

use super::*;
use crate::settings_runtime::setting_labels::SettingLabels;
use crate::settings_runtime::status_text::{
    status_from_apply_report, status_from_cancel, status_from_preview_reports, status_from_reset,
};
use crate::settings_ui::SettingsUiStatus;

/// Технические слова, которые не должны дойти до пользователя.
const FORBIDDEN_FRAGMENTS: &[&str] = &[
    "Route",
    "route",
    "Rollback",
    "rollback",
    "Failed",
    "failed",
    "PartialFailure",
    "Conflict",
    "baseline",
    "generation",
    "`",
    "{",
    "TOML",
    "runtime",
    "settings.",
    "network.",
    "audio.",
    "render.",
    "ui.",
    "media owner",
    "renderer recreation",
    "candidate",
];

/// Английское сообщение владельца, которое обязано остаться только в логе.
const OWNER_MESSAGE: &str = "candidate renderer creation failed: device lost";

fn registry() -> settings_core::SettingsRegistry<AppConfig> {
    fastiplayer_settings::app_config_registry().expect("registry должен собраться")
}

fn label_of(registry: &settings_core::SettingsRegistry<AppConfig>, id: &str) -> String {
    registry
        .descriptor(&SettingId::from(id))
        .expect("setting должен быть в registry")
        .text
        .label
        .fallback_ru
        .clone()
}

/// Весь видимый текст панели одной строкой.
fn visible_text(status: &SettingsUiStatus) -> String {
    let mut parts = Vec::new();
    parts.extend(status.summary.clone());
    parts.extend(status.details.iter().cloned());
    parts.join("\n")
}

/// Убирает «названия настроек» (они берутся из metadata) и проверяет остальной текст.
fn assert_user_facing(status: &SettingsUiStatus) {
    let text = visible_text(status);
    for fragment in FORBIDDEN_FRAGMENTS {
        assert!(
            !text.contains(fragment),
            "в тексте статуса остался технический фрагмент `{fragment}`: {text}"
        );
    }
    assert!(
        !text.contains(OWNER_MESSAGE),
        "сообщение владельца просочилось: {text}"
    );

    let mut without_labels = String::new();
    let mut inside_label = false;
    for ch in text.chars() {
        match ch {
            '«' => inside_label = true,
            '»' => inside_label = false,
            _ if !inside_label => without_labels.push(ch),
            _ => {}
        }
    }
    let english: String = without_labels
        .chars()
        .filter(char::is_ascii_alphabetic)
        .collect();
    assert!(
        english.is_empty(),
        "в статусе есть английские буквы `{english}`: {text}"
    );
}

fn route_report(result: ApplyRouteResult, mechanism: ApplyMechanism) -> ApplyRouteReport {
    ApplyRouteReport {
        route: SettingRouteId::from("settings.route.media_services"),
        result,
        mechanism,
        affected_settings: vec![SettingId::from("network.read_ahead_mb")],
    }
}

fn apply_report(final_state: ApplyFinalState, routes: Vec<ApplyRouteReport>) -> ApplyReport {
    ApplyReport {
        validation: None,
        persistence: None,
        routes,
        rollback: Vec::new(),
        changed_settings: SettingsDiff::new(),
        final_state,
        conflicts: Vec::new(),
        errors: Vec::new(),
    }
}

#[test]
fn successful_apply_names_setting_and_hides_route_id() {
    let registry = registry();
    let labels = SettingLabels::new(&registry);
    let mut report = apply_report(
        ApplyFinalState::FullyApplied,
        vec![route_report(
            ApplyRouteResult::Applied,
            ApplyMechanism::InPlace,
        )],
    );
    report.persistence = Some(PersistReport::persisted());

    let status = status_from_apply_report(&labels, &report);

    assert_user_facing(&status);
    assert_eq!(
        status.summary.as_deref(),
        Some("Настройки сохранены и применены")
    );
    let read_ahead = label_of(&registry, "network.read_ahead_mb");
    assert!(
        status
            .details
            .contains(&format!("«{read_ahead}»: применено")),
        "{:?}",
        status.details
    );
}

#[test]
fn rebuild_mechanism_warns_about_short_playback_interruption() {
    let registry = registry();
    let labels = SettingLabels::new(&registry);
    let report = apply_report(
        ApplyFinalState::FullyApplied,
        vec![route_report(
            ApplyRouteResult::Applied,
            ApplyMechanism::PipelineRebuild,
        )],
    );

    let status = status_from_apply_report(&labels, &report);

    assert_user_facing(&status);
    assert!(visible_text(&status).contains("могло ненадолго прерваться"));
}

#[test]
fn partial_failure_hides_owner_message_and_says_partially() {
    let registry = registry();
    let labels = SettingLabels::new(&registry);
    let report = apply_report(
        ApplyFinalState::RuntimeApplyFailed,
        vec![route_report(
            ApplyRouteResult::PartialFailure {
                message: OWNER_MESSAGE.to_string(),
            },
            ApplyMechanism::RendererRecreate,
        )],
    );

    let status = status_from_apply_report(&labels, &report);

    assert_user_facing(&status);
    assert!(visible_text(&status).contains("применено только частично"));
}

#[test]
fn failed_route_with_failed_rollback_asks_to_restart() {
    let registry = registry();
    let labels = SettingLabels::new(&registry);
    let mut report = apply_report(
        ApplyFinalState::RollbackFailed,
        vec![route_report(
            ApplyRouteResult::Failed {
                message: OWNER_MESSAGE.to_string(),
            },
            ApplyMechanism::RendererRecreate,
        )],
    );
    report.rollback.push(RollbackReport::failed(
        SettingRouteId::from("settings.route.render"),
        vec![SettingId::from("network.read_ahead_mb")],
        "rollback failed: device lost",
    ));

    let status = status_from_apply_report(&labels, &report);

    assert_user_facing(&status);
    let text = visible_text(&status);
    assert!(text.contains("не удалось применить"), "{text}");
    assert!(
        text.contains("не удалось вернуть прежнее значение"),
        "{text}"
    );
    assert!(text.contains("перезапустите приложение"), "{text}");
}

#[test]
fn busy_runtime_and_conflict_are_explained_without_generation_numbers() {
    let registry = registry();
    let labels = SettingLabels::new(&registry);
    let mut report = apply_report(
        ApplyFinalState::RuntimeBlocked,
        vec![route_report(
            ApplyRouteResult::RuntimeBusy {
                activity: "PipelineLifecycle".to_string(),
            },
            ApplyMechanism::InPlace,
        )],
    );
    report.conflicts.push(RouteGenerationConflict {
        route: SettingRouteId::from("settings.route.media_services"),
        baseline: RouteGeneration::new(41),
        current: RouteGeneration::new(42),
        affected_settings: vec![SettingId::from("network.read_ahead_mb")],
    });

    let status = status_from_apply_report(&labels, &report);

    assert_user_facing(&status);
    let text = visible_text(&status);
    assert!(text.contains("плеер занят"), "{text}");
    assert!(!text.contains("PipelineLifecycle"), "{text}");
    assert!(!text.contains("41") && !text.contains("42"), "{text}");
    assert!(text.contains("нажмите «Применить» ещё раз"), "{text}");
}

#[test]
fn settings_error_in_report_is_mapped_to_label_phrase() {
    let registry = registry();
    let labels = SettingLabels::new(&registry);
    let mut report = apply_report(ApplyFinalState::ValidationFailed, Vec::new());
    report.errors.push(SettingsError::ReadOnlySetting {
        id: SettingId::from("network.read_ahead_mb"),
    });

    let status = status_from_apply_report(&labels, &report);

    assert_user_facing(&status);
    let read_ahead = label_of(&registry, "network.read_ahead_mb");
    assert_eq!(
        status.details,
        vec![format!("«{read_ahead}» нельзя изменить")]
    );
}

#[test]
fn preview_statuses_cover_applied_backpressure_and_failures() {
    let registry = registry();
    let labels = SettingLabels::new(&registry);
    let route = SettingRouteId::from("settings.route.render_preview");
    let affected = vec![SettingId::from("audio.volume")];
    let report = |result| PreviewApplyReport {
        route: route.clone(),
        result,
        affected_settings: affected.clone(),
    };

    let applied = status_from_preview_reports(&labels, &[report(PreviewApplyResult::Applied)]);
    let waiting =
        status_from_preview_reports(&labels, &[report(PreviewApplyResult::Backpressured)]);
    let failed = status_from_preview_reports(
        &labels,
        &[
            report(PreviewApplyResult::Unsupported {
                message: OWNER_MESSAGE.to_string(),
            }),
            report(PreviewApplyResult::Fatal {
                message: OWNER_MESSAGE.to_string(),
            }),
        ],
    );

    for status in [&applied, &waiting, &failed] {
        assert_user_facing(status);
    }
    assert_eq!(applied.summary.as_deref(), Some("Предпросмотр применён"));
    assert_eq!(
        waiting.summary.as_deref(),
        Some("Предпросмотр ждёт отрисовщик")
    );
    assert_eq!(
        failed.summary.as_deref(),
        Some("Предпросмотр применён не полностью")
    );
    let volume = label_of(&registry, "audio.volume");
    assert_eq!(
        failed.details,
        vec![
            format!("«{volume}»: для этой настройки предпросмотр недоступен"),
            format!("«{volume}»: не удалось показать предпросмотр"),
        ]
    );
}

#[test]
fn cancel_status_names_discarded_settings_and_reports_rollback_failure() {
    let registry = registry();
    let labels = SettingLabels::new(&registry);
    let mut discarded = SettingsDiff::new();
    discarded.push(settings_core::SettingChange::new(
        SettingId::from("audio.volume"),
        settings_core::SettingPath::from("audio.volume"),
        SettingValue::Float(0.1),
        SettingValue::Float(0.2),
    ));
    let report = settings_core::CancelReport {
        discarded_changes: discarded,
        rolled_back_routes: vec![RollbackReport::failed(
            SettingRouteId::from("settings.route.render_preview"),
            vec![SettingId::from("audio.volume")],
            OWNER_MESSAGE,
        )],
    };

    let status = status_from_cancel(&labels, &report);

    assert_user_facing(&status);
    let volume = label_of(&registry, "audio.volume");
    assert_eq!(status.summary.as_deref(), Some("Изменения отменены"));
    assert_eq!(
        status.details,
        vec![
            format!("Отброшены несохранённые изменения: «{volume}»"),
            format!("«{volume}»: не удалось вернуть прежнее значение"),
        ]
    );
}

#[test]
fn reset_status_lists_labels_not_ids() {
    let registry = registry();
    let labels = SettingLabels::new(&registry);
    let report = settings_core::ResetReport {
        scope: settings_core::ResetScope::All,
        affected_settings: vec![SettingId::from("audio.volume")],
        changed_settings: SettingsDiff::new(),
        preview_routes: Vec::new(),
    };

    let status = status_from_reset(&labels, "Все настройки сброшены", &report);

    assert_user_facing(&status);
    let volume = label_of(&registry, "audio.volume");
    assert_eq!(status.details, vec![format!("Изменены поля: «{volume}»")]);
}

#[test]
fn unknown_setting_id_is_never_shown() {
    let registry = registry();
    let labels = SettingLabels::new(&registry);

    let text = labels.quoted(&SettingId::from("internal.secret_id"));

    assert_eq!(text, "«неизвестная настройка»");
}

/// Сквозной сценарий: реальный сбой media-owner-а и сбой отката в живом `SettingsRuntime`.
#[test]
fn live_apply_failure_shows_human_status_in_ui_model() {
    let config = custom_config_for_test();
    let path = temp_config_path("status-text-human-failure");
    remove_file_if_exists(&path);
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        config.clone(),
        path.clone(),
    ))
    .expect("settings runtime должен построиться");
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&config).expect("adapter должен стартовать");
    adapter.fail_media = true;
    adapter.render.fail_rollback = true;

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            brightness_action(0.30),
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("network.read_ahead_mb"),
                value: SettingValue::Integer(config.network.read_ahead_mb as i64 + 1),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    let status = runtime.ui_model().status.clone();
    assert_user_facing(&status);
    let read_ahead = label_of(&registry(), "network.read_ahead_mb");
    let text = visible_text(&status);
    assert!(
        text.contains(&format!("«{read_ahead}»: не удалось применить")),
        "{text}"
    );
    assert!(text.contains("перезапустите приложение"), "{text}");
    remove_file_if_exists(&path);
}

/// Недопустимое значение поля: подсказка под полем и статус без id и английского.
#[test]
fn invalid_field_value_status_uses_label_and_russian_reason() {
    let config = AppConfig::default();
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");

    runtime
        .handle_ui_actions(
            vec![
                SettingsUiAction::Open,
                SettingsUiAction::SetValue {
                    setting_id: SettingId::from("audio.volume"),
                    value: SettingValue::Float(7.5),
                },
            ],
            &mut render_adapter,
        )
        .expect("actions должны пройти");

    let model = runtime.ui_model().clone();
    assert_user_facing(&model.status);
    let field_error = model
        .fields
        .iter()
        .find(|field| field.descriptor.id == SettingId::from("audio.volume"))
        .and_then(|field| field.validation_error.clone())
        .expect("у поля должна быть ошибка проверки");
    assert!(
        field_error.starts_with("Значение должно быть от"),
        "{field_error}"
    );
    assert!(!field_error.contains("range violation"));
}
