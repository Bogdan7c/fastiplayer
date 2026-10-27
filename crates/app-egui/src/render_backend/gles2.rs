//! Кандидат каталога «OpenGL ES 2.0 через EGL» (старое железо без аппаратного Vulkan).
//!
//! Единственное место `app-egui`, где создаётся конкретный `render_gles2_shell::Gles2Renderer`.
//! Показ GLES2 не зависит от `[render.vulkan]`: своих ручек показа у него нет, всегда vsync.

use fastiplayer_config::VulkanConfig;
use render_gles2_shell::{Gles2Renderer, probe_gles2_renderer};
use render_shell_api::{
    RenderShell, RendererCreateError, RendererProbe, ShellPresentMode, SurfacePresentSettings,
};

use super::{ProbeLeftovers, RendererCreateRequest, RendererInitialColor};

/// Проба кандидата без создания рендера: EGL 1.5, конфиг GLES2, нужные расширения.
pub(super) fn probe(_leftovers: &mut ProbeLeftovers) -> RendererProbe {
    probe_gles2_renderer()
}

/// Создаёт GLES2-рендер на окне запроса.
pub(super) fn create(
    request: RendererCreateRequest<'_>,
) -> Result<Box<dyn RenderShell>, RendererCreateError> {
    let mut renderer =
        Gles2Renderer::new(request.window, surface_present_settings(request.vulkan))?;
    match request.initial_color {
        RendererInitialColor::CommittedConfig(initial) => {
            renderer.set_color_pipeline_settings(initial.color_pipeline);
            renderer.set_hdr_to_sdr_settings(initial.hdr_to_sdr);
        }
        RendererInitialColor::RendererDefaults => {}
    }
    Ok(Box::new(renderer))
}

/// Настройки показа GLES2: `[render.vulkan]` их не меняет (параметр нужен только единой
/// сигнатуре кандидата каталога).
pub(super) fn surface_present_settings(_vulkan: &VulkanConfig) -> SurfacePresentSettings {
    SurfacePresentSettings {
        present_mode: ShellPresentMode::Fifo,
        ..SurfacePresentSettings::default()
    }
}

#[cfg(test)]
mod tests {
    use fastiplayer_config::{AppConfig, VulkanPresentMode};
    use render_shell_api::ShellPresentMode;

    use super::surface_present_settings;

    /// Показ GLES2 всегда vsync, что бы ни стояло в `[render.vulkan]`.
    #[test]
    fn present_settings_ignore_the_vulkan_section() {
        for mode in [VulkanPresentMode::Immediate, VulkanPresentMode::Mailbox] {
            let mut vulkan = AppConfig::default().render.vulkan;
            vulkan.present_mode = mode;
            vulkan.max_frame_latency = 4;

            let settings = surface_present_settings(&vulkan);

            assert_eq!(settings.present_mode, ShellPresentMode::Fifo);
            assert_eq!(
                settings,
                surface_present_settings(&AppConfig::default().render.vulkan)
            );
        }
    }
}
