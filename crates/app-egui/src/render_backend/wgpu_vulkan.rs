//! Кандидат каталога «wgpu поверх Vulkan» (Linux).
//!
//! Единственное место `app-egui`, где создаётся конкретный `render_wgpu_shell::Renderer`.
//! Здесь же — единственный маппинг `[render.vulkan]` в настройки показа: раньше он был
//! продублирован в `render_settings.rs` (старт) и `renderer_recreation.rs` (пересоздание).

use fastiplayer_config::{VulkanConfig, VulkanPresentMode};
use render_shell_api::{
    RenderShell, RendererCreateError, RendererCreateStage, RendererProbe, ShellPresentMode,
    SurfaceAlphaPreference, SurfacePresentSettings,
};
use render_wgpu_shell::Renderer;

use super::{ProbeLeftovers, RendererCreateRequest, RendererInitialColor};

/// Проба кандидата без создания рендера: есть ли аппаратный Vulkan-адаптер с NV12.
///
/// Только перечисление адаптеров (`render_wgpu_shell::probe_hardware_vulkan_adapter`): ни окна,
/// ни устройства. Машина с одним lavapipe/llvmpipe или без Vulkan получает «негоден».
///
/// Vulkan instance пробы остаётся в `leftovers`: рендер этого запуска создаётся на нём, а не на
/// втором instance.
pub(super) fn probe(leftovers: &mut ProbeLeftovers) -> RendererProbe {
    let outcome = render_wgpu_shell::probe_hardware_vulkan_adapter_keeping_instance();
    leftovers.keep_wgpu_vulkan_instance(outcome.instance);
    outcome.verdict
}

/// Создаёт wgpu-рендер на окне запроса.
pub(super) fn create(
    request: RendererCreateRequest<'_>,
) -> Result<Box<dyn RenderShell>, RendererCreateError> {
    let surface_present_settings = surface_present_settings(request.vulkan);
    let mut probe_leftovers = request.probe_leftovers;
    let created = match probe_leftovers.take_wgpu_vulkan_instance() {
        // Старт после пробы: тот же instance, второй не создаётся.
        Some(probed_instance) => Renderer::new_reusing_instance(
            request.window,
            surface_present_settings,
            probed_instance,
        ),
        // Пересоздание рендера или старт без пробы: свой instance.
        None => Renderer::new(request.window, surface_present_settings),
    };
    let mut renderer = created.map_err(|error| {
        // Этапы создания wgpu-рендера не различаются в его ошибке; текст — прежний `{:#}`.
        RendererCreateError::new(RendererCreateStage::DeviceCreation, format!("{error:#}"))
    })?;
    match request.initial_color {
        RendererInitialColor::CommittedConfig(initial) => {
            renderer.set_color_pipeline_settings(initial.color_pipeline);
            renderer.set_hdr_to_sdr_settings(initial.hdr_to_sdr);
        }
        RendererInitialColor::RendererDefaults => {}
    }
    Ok(Box::new(renderer))
}

/// Маппинг validated `[render.vulkan]` в настройки показа surface.
///
/// Окно всегда просит прозрачную композицию (скруглённые углы); если её нет, рендер сам
/// откатывается на непрозрачную surface. Тот же маппинг использует перенастройка показа на
/// месте (сессия 02a) — через [`super::presentation_settings`].
pub(super) fn surface_present_settings(vulkan: &VulkanConfig) -> SurfacePresentSettings {
    let present_mode = match vulkan.present_mode {
        VulkanPresentMode::Auto => ShellPresentMode::Auto,
        VulkanPresentMode::Fifo => ShellPresentMode::Fifo,
        VulkanPresentMode::Mailbox => ShellPresentMode::Mailbox,
        VulkanPresentMode::Immediate => ShellPresentMode::Immediate,
    };
    SurfacePresentSettings {
        present_mode,
        max_frame_latency: vulkan.max_frame_latency,
        alpha_preference: SurfaceAlphaPreference::TransparentPreferred,
    }
}

#[cfg(test)]
mod tests {
    use fastiplayer_config::{AppConfig, VulkanPresentMode};
    use render_shell_api::{ShellPresentMode, SurfaceAlphaPreference};

    use super::{ProbeLeftovers, probe, surface_present_settings};

    /// Создание рендера всегда запрашивает прозрачную surface-композицию.
    #[test]
    fn surface_settings_prefer_transparency() {
        let settings = surface_present_settings(&AppConfig::default().render.vulkan);

        assert_eq!(
            settings.alpha_preference,
            SurfaceAlphaPreference::TransparentPreferred
        );
    }

    /// Каждый present mode config-а доезжает до своего режима показа, задержка — как есть.
    #[test]
    fn every_present_mode_and_latency_map_one_to_one() {
        let cases = [
            (VulkanPresentMode::Auto, ShellPresentMode::Auto),
            (VulkanPresentMode::Fifo, ShellPresentMode::Fifo),
            (VulkanPresentMode::Mailbox, ShellPresentMode::Mailbox),
            (VulkanPresentMode::Immediate, ShellPresentMode::Immediate),
        ];
        for (config_mode, expected_mode) in cases {
            let mut vulkan = AppConfig::default().render.vulkan;
            vulkan.present_mode = config_mode;
            vulkan.max_frame_latency = 3;

            let settings = surface_present_settings(&vulkan);

            assert_eq!(settings.present_mode, expected_mode);
            assert_eq!(settings.max_frame_latency, 3);
        }
    }

    /// Проба оставляет свой instance для рендера этого запуска и даёт тот же вердикт, что
    /// проба без сохранения; повторная проба не копит instance-ы (остаётся один).
    #[test]
    fn probe_keeps_its_instance_for_the_renderer_of_the_same_run() {
        let mut leftovers = ProbeLeftovers::default();

        let verdict = probe(&mut leftovers);

        assert_eq!(verdict, render_wgpu_shell::probe_hardware_vulkan_adapter());
        assert!(leftovers.holds_wgpu_vulkan_instance());
        probe(&mut leftovers);
        assert!(leftovers.take_wgpu_vulkan_instance().is_some());
        assert!(
            leftovers.take_wgpu_vulkan_instance().is_none(),
            "a repeated probe replaces the instance instead of stacking them"
        );
    }
}
