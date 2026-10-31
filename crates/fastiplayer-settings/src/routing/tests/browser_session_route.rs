//! Маршрут `browser_session.apply`: одна committed route с полным snapshot секции, две
//! группы владельца (окно буфера и запоминаемое окно) и изоляция от соседних владельцев.

use std::time::Duration;

use super::*;

/// Строит committed routes плана для diff «defaults → current».
fn committed_routes_for(current: &AppConfig) -> Vec<RuntimeCommittedRoute> {
    let registry = app_config_registry().expect("registry builds");
    let previous = AppConfig::default();
    let diff = registry.diff(&previous, current).expect("diff succeeds");
    let plan = runtime_route_plan_from_diff(
        &registry,
        &previous,
        current,
        &diff,
        crate::routing::TEST_AUTOMATIC_SURFACE_POOL,
    )
    .expect("route plan builds");
    assert!(plan.preview_routes.is_empty());
    plan.committed_routes
}

/// Окно буфера и окно браузера вместе дают ровно один route с двумя группами владельца
/// и полным snapshot секции; контракт применения читается intent-методами.
#[test]
fn buffer_and_window_changes_form_one_browser_session_route() {
    let mut current = AppConfig::default();
    current.browser_session.buffer_ahead_s = 240;
    current.browser_session.window.inner_width_points = 1600;
    current.browser_session.window.fullscreen = true;

    let routes = committed_routes_for(&current);

    assert_eq!(routes.len(), 1);
    let route = &routes[0];
    assert_eq!(route.route, AppRuntimeRoute::BrowserSession);
    assert_eq!(
        route.source_routes,
        vec![SettingRouteId::from(BROWSER_SESSION_ROUTE_ID)]
    );
    assert_eq!(
        route.groups,
        vec![
            AppRuntimeRouteGroupUpdate {
                group: AppRuntimeRouteGroup::BrowserSessionBuffer,
                affected_settings: vec![SettingId::from("browser_session.buffer_ahead_s")],
            },
            AppRuntimeRouteGroupUpdate {
                group: AppRuntimeRouteGroup::BrowserSessionWindow,
                affected_settings: vec![
                    SettingId::from("browser_session.window.inner_width_points"),
                    SettingId::from("browser_session.window.fullscreen"),
                ],
            },
        ]
    );
    let RuntimeCommittedUpdate::BrowserSession(update) = &route.update else {
        panic!("browser session route must carry BrowserSessionRuntimeSettingsUpdate");
    };
    assert_eq!(update.browser_session, current.browser_session);
    assert_eq!(update.buffer_window().ahead(), Duration::from_secs(240));
    assert_eq!(update.remembered_window().inner_width_points, 1600);
    assert!(update.remembered_window().fullscreen);
}

/// Изменение только запоминаемого окна не попадает в группу окна буфера (владелец
/// источника не получает ложного «изменилось»).
#[test]
fn window_only_change_touches_only_window_group() {
    let mut current = AppConfig::default();
    current.browser_session.window.inner_height_points = 900;

    let routes = committed_routes_for(&current);

    assert_eq!(routes.len(), 1);
    assert_eq!(
        routes[0]
            .groups
            .iter()
            .map(|group| group.group)
            .collect::<Vec<_>>(),
        vec![AppRuntimeRouteGroup::BrowserSessionWindow]
    );
}

/// Ключи браузерного режима не порождают route-ы обычного плеера, а правка обычной
/// настройки не порождает route браузерного режима.
#[test]
fn browser_session_route_is_isolated_from_regular_player_routes() {
    let mut browser_only = AppConfig::default();
    browser_only.browser_session.buffer_behind_s = 60;
    let browser_routes = committed_routes_for(&browser_only);
    assert!(
        browser_routes
            .iter()
            .all(|route| route.route == AppRuntimeRoute::BrowserSession)
    );

    let mut regular_only = AppConfig::default();
    regular_only.playlist.load_siblings = !regular_only.playlist.load_siblings;
    let regular_routes = committed_routes_for(&regular_only);
    assert!(
        regular_routes
            .iter()
            .all(|route| route.route != AppRuntimeRoute::BrowserSession)
    );
}

/// Значение вне диапазона по id реестра отклоняется до документа (валидация метаданных).
#[test]
fn registry_rejects_out_of_range_browser_values_without_mutation() {
    let registry = app_config_registry().expect("registry builds");
    let mut document = AppConfig::default();

    for (setting_id, value) in [
        ("browser_session.buffer_ahead_s", 5),
        ("browser_session.buffer_behind_s", 301),
        ("browser_session.window.inner_width_points", 399),
        ("browser_session.window.inner_height_points", 4321),
    ] {
        let error = registry
            .set_value(
                &mut document,
                &SettingId::from(setting_id),
                SettingValue::Integer(value),
            )
            .expect_err("out of range value rejected");
        assert!(
            matches!(error, SettingsError::InvalidValue { .. }),
            "{setting_id}"
        );
    }
    assert_eq!(document, AppConfig::default());

    registry
        .set_value(
            &mut document,
            &SettingId::from("browser_session.window.fullscreen"),
            SettingValue::Bool(true),
        )
        .expect("toggle accepted");
    assert_eq!(
        registry
            .get_value(
                &document,
                &SettingId::from("browser_session.window.fullscreen")
            )
            .expect("value readable"),
        SettingValue::Bool(true)
    );
}
