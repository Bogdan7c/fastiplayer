use super::*;
use std::num::NonZeroUsize;

/// Возвращает пул рендерера из положительного числа (в тестах числа всегда валидны).
fn automatic_pool_of(frames: usize) -> AutomaticSurfacePool {
    AutomaticSurfacePool::new(NonZeroUsize::new(frames).expect("positive pool size"))
}

/// Возврат hardware-пула в «авто» (0) на живом маршруте даёт decoder-у размер рендерера,
/// а не нуль: оба поля раскрываются через переданный `AutomaticSurfacePool`.
#[test]
fn switching_surface_pool_to_automatic_reaches_decoder_config_as_renderer_default() {
    let registry = app_config_registry().expect("registry builds");
    let mut previous = AppConfig::default();
    previous.video.decoder_surface_pool_frames = 12;
    let current = AppConfig::default();
    assert_eq!(current.video.decoder_surface_pool_frames, 0);

    let diff = registry.diff(&previous, &current).expect("diff succeeds");
    let plan =
        runtime_route_plan_from_diff(&registry, &previous, &current, &diff, automatic_pool_of(16))
            .expect("route plan builds");

    let player_route = plan
        .committed_routes
        .iter()
        .find(|route| route.route == AppRuntimeRoute::Player)
        .expect("player route exists");
    let RuntimeCommittedUpdate::Player(update) = &player_route.update else {
        panic!("surface pool change must be a player route update");
    };
    let decoder_config = &update
        .player_core
        .decoder_thread_config
        .as_ref()
        .expect("decoder-thread update expected")
        .decoder_thread_config;
    assert_eq!(decoder_config.decoder_surface_pool_frames, 16);
    assert_eq!(decoder_config.zero_copy_surface_pool_slots, 16);
}
