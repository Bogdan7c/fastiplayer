//! Контракт применения `render.profile`: только после перезапуска.

use super::*;

/// Профиль рендера применяется только после перезапуска, остальные renderer-bound
/// настройки по-прежнему пересоздают рендер вживую; у обоих один и тот же маршрут.
#[test]
fn render_profile_applies_after_restart_while_siblings_recreate_renderer() {
    let mechanism_of = |setting_id: &str| {
        setting_application_contract(&SettingId::from(setting_id))
            .expect("render setting has a contract")
    };

    let profile = mechanism_of("render.profile");
    assert_eq!(
        profile.mechanism,
        SettingApplyMechanism::AppliesAfterRestart
    );
    assert_eq!(profile.route, AppRuntimeRoute::RenderCommitted);
    assert_eq!(profile.state_owner, SettingStateOwner::RendererLifecycle);
    for sibling in [
        "render.tone_mapping",
        "render.vulkan.present_mode",
        "render.vulkan.max_frame_latency",
    ] {
        assert_eq!(
            mechanism_of(sibling).mechanism,
            SettingApplyMechanism::RendererRecreate,
            "{sibling}"
        );
    }
    assert!(setting_application_contract(&SettingId::from("render.opengles.enabled")).is_none());
}
