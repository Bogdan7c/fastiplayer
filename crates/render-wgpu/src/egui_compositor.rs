/// Тонкая egui-specific граница поверх `egui_wgpu::Renderer`.
///
/// Shell остаётся владельцем swapchain lifecycle, а этот модуль владеет только
/// порядком обновления egui texture atlas/buffers и overlay render pass-ом.
pub(crate) struct EguiCompositor {
    /// Непосредственный renderer из egui-wgpu.
    renderer: egui_wgpu::Renderer,
}

impl EguiCompositor {
    /// Создаёт egui compositor для выбранного swapchain format.
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let renderer = egui_wgpu::Renderer::new(
            device,
            surface_format,
            egui_wgpu::RendererOptions {
                depth_stencil_format: None,
                msaa_samples: 1,
                ..Default::default()
            },
        );

        Self { renderer }
    }

    /// Применяет изменения texture atlas-а до записи command buffer-а кадра.
    pub fn update_textures(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        textures_delta: &egui::TexturesDelta,
    ) {
        for (id, image_delta) in &textures_delta.set {
            self.renderer
                .update_texture(device, queue, *id, image_delta);
        }

        for id in &textures_delta.free {
            self.renderer.free_texture(id);
        }
    }

    /// Обновляет egui vertex/index buffers перед overlay pass-ом.
    pub fn update_buffers(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        paint_jobs: &[egui::epaint::ClippedPrimitive],
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
    ) {
        self.renderer
            .update_buffers(device, queue, encoder, paint_jobs, screen_descriptor);
    }

    /// Рендерит egui primitives поверх уже нарисованного video target-а.
    pub fn render_overlay(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        paint_jobs: &[egui::epaint::ClippedPrimitive],
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
    ) {
        let egui_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui overlay pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        self.renderer.render(
            &mut egui_pass.forget_lifetime(),
            paint_jobs,
            screen_descriptor,
        );
    }
}
