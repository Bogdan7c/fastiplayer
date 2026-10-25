//! Committed render route: пересоздание рендера внутри семейства.
//!
//! `render.profile` в payload НЕ входит: смена профиля действует после перезапуска и
//! рендер вживую не меняет (решение владельца, сессия 09, №37). Её отдельная группа —
//! `AppRuntimeRouteGroup::RenderProfileRestart`.

use super::*;

/// Renderer committed lifecycle payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderCommittedSettingsUpdate {
    /// Compatibility committed tone-mapping config.
    pub tone_mapping: ConfigToneMappingMode,

    /// Vulkan-specific committed settings.
    pub vulkan: VulkanConfig,
}

pub(super) fn render_committed_route(
    current: &AppConfig,
    input: RouteInput,
) -> RuntimeCommittedRoute {
    let groups = input.groups();

    RuntimeCommittedRoute {
        route: AppRuntimeRoute::RenderCommitted,
        source_routes: input.source_routes,
        affected_settings: input.affected_settings,
        groups,
        update: RuntimeCommittedUpdate::RenderCommitted(RenderCommittedSettingsUpdate {
            tone_mapping: current.render.tone_mapping,
            vulkan: current.render.vulkan.clone(),
        }),
    }
}
