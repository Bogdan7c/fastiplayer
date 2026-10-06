//! UX-11: routing `audio.muted` (отдельный файл, чтобы не раздувать legacy `routing.rs`).

use fastiplayer_config::AppConfig;
use settings_core::SettingId;

use crate::{
    AppRuntimeRoute, AppRuntimeRouteGroup, AppRuntimeRouteGroupUpdate, RuntimeCommittedUpdate,
    app_config_registry, runtime_route_plan_from_diff,
};

/// UX-11: mute не имеет worker policy — commit `audio.muted` ничего не шлёт в
/// player worker (текущий звук после Apply доставляет app-слой отдельно).
#[test]
fn audio_muted_route_has_no_player_worker_payload() {
    let registry = app_config_registry().expect("registry builds");
    let mut current = AppConfig::default();
    current.audio.muted = true;

    let diff = registry
        .diff(&AppConfig::default(), &current)
        .expect("diff succeeds");
    let plan = runtime_route_plan_from_diff(&registry, &AppConfig::default(), &current, &diff)
        .expect("route plan builds");

    let player_route = plan
        .committed_routes
        .iter()
        .find(|route| route.route == AppRuntimeRoute::Player)
        .expect("player route exists");
    let RuntimeCommittedUpdate::Player(update) = &player_route.update else {
        panic!("audio.muted должен попасть в player route update");
    };

    assert_eq!(
        player_route.groups,
        vec![AppRuntimeRouteGroupUpdate {
            group: AppRuntimeRouteGroup::PlayerDefaultVolume,
            affected_settings: vec![SettingId::from("audio.muted")],
        }]
    );
    assert!(update.player_core.is_empty());
    assert!(update.audio_output_device_id.is_none());
}
