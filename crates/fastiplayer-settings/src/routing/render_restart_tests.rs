//! Маршрутизация `render.profile` («после перезапуска») внутри committed render route.

use fastiplayer_config::{AppConfig, RenderProfile};
use settings_core::{ApplyMechanism, ApplyRouteResult, SettingId, SettingRouteId};

use super::*;

/// `render.profile` едет в `RenderCommitted`, но в отдельной группе «после перезапуска»,
/// а payload пересоздания рендера профиль больше не содержит.
#[test]
fn render_profile_change_lands_in_restart_group_apart_from_recreation() {
    let registry = app_config_registry().expect("registry builds");
    let mut current = AppConfig::default();
    current.render.profile = RenderProfile::OpenGles;
    current.render.vulkan.max_frame_latency += 1;

    let diff = registry
        .diff(&AppConfig::default(), &current)
        .expect("diff succeeds");
    let plan = runtime_route_plan_from_diff(&registry, &AppConfig::default(), &current, &diff)
        .expect("route plan builds");

    let render_route = plan
        .committed_routes
        .iter()
        .find(|route| route.route == AppRuntimeRoute::RenderCommitted)
        .expect("render committed route exists");
    assert_eq!(
        render_route.groups,
        vec![
            AppRuntimeRouteGroupUpdate {
                group: AppRuntimeRouteGroup::RenderBackendLifecycle,
                affected_settings: vec![SettingId::from("render.vulkan.max_frame_latency")],
            },
            AppRuntimeRouteGroupUpdate {
                group: AppRuntimeRouteGroup::RenderProfileRestart,
                affected_settings: vec![SettingId::from("render.profile")],
            },
        ]
    );
}

/// Исход «применится после перезапуска» — успех, не требует компенсации и для
/// транзакции настроек сводится к `Applied`, но остаётся отличимым от `Noop`.
#[test]
fn applies_after_restart_is_a_distinct_successful_outcome() {
    let outcome = AppRouteApplyResult::AppliesAfterRestart;

    assert!(outcome.is_success());
    assert!(!outcome.needs_compensation());
    assert_ne!(outcome, AppRouteApplyResult::Noop);
    assert_ne!(outcome, AppRouteApplyResult::Applied);

    let report = AppRouteApplyReport {
        route: AppRuntimeRoute::RenderCommitted,
        source_routes: vec![SettingRouteId::from(RENDER_COMMITTED_ROUTE_ID)],
        result: outcome,
        mechanism: ApplyMechanism::InPlace,
        affected_settings: vec![SettingId::from("render.profile")],
        groups: Vec::new(),
    };
    assert_eq!(
        report.clone().into_core_report().result,
        ApplyRouteResult::Applied
    );
    assert_eq!(
        report.into_rollback_report().result,
        settings_core::RollbackResult::RolledBack
    );
}
