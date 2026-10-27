use super::*;
use fastiplayer_config::SubtitleDecorationPreference;

/// Изменение двух полей субтитров даёт ровно один committed route с полным snapshot секции.
#[test]
fn subtitle_changes_form_one_exact_subtitles_route() {
    let registry = app_config_registry().expect("registry builds");
    let previous = AppConfig::default();
    let mut current = previous.clone();
    current.subtitles.enabled = true;
    current.subtitles.decoration = SubtitleDecorationPreference::Outline;

    let diff = registry.diff(&previous, &current).expect("diff succeeds");
    let plan = runtime_route_plan_from_diff(
        &registry,
        &previous,
        &current,
        &diff,
        crate::routing::TEST_AUTOMATIC_SURFACE_POOL,
    )
    .expect("subtitle route plan builds");

    assert!(plan.preview_routes.is_empty());
    assert_eq!(plan.committed_routes.len(), 1);
    let route = &plan.committed_routes[0];
    assert_eq!(route.route, AppRuntimeRoute::Subtitles);
    assert_eq!(
        route.source_routes,
        vec![SettingRouteId::from(SUBTITLES_ROUTE_ID)]
    );
    assert_eq!(
        route.groups,
        vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::SubtitlePolicy,
            affected_settings: vec![
                SettingId::from("subtitles.enabled"),
                SettingId::from("subtitles.decoration"),
            ],
        }]
    );
    let RuntimeCommittedUpdate::Subtitles(update) = &route.update else {
        panic!("subtitles route must carry SubtitlesRuntimeSettingsUpdate");
    };
    assert_eq!(update.subtitles, current.subtitles);
}

/// Правка не-субтитровой настройки не создаёт route субтитров (изоляция владельцев).
#[test]
fn non_subtitle_change_does_not_touch_subtitles_route() {
    let registry = app_config_registry().expect("registry builds");
    let previous = AppConfig::default();
    let mut current = previous.clone();
    current.playlist.load_siblings = !current.playlist.load_siblings;

    let diff = registry.diff(&previous, &current).expect("diff succeeds");
    let plan = runtime_route_plan_from_diff(
        &registry,
        &previous,
        &current,
        &diff,
        crate::routing::TEST_AUTOMATIC_SURFACE_POOL,
    )
    .expect("route plan builds");

    assert!(
        plan.committed_routes
            .iter()
            .all(|route| route.route != AppRuntimeRoute::Subtitles)
    );
}
