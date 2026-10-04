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

    /// Загружает новые и изменённые egui-текстуры до записи command buffer-а кадра.
    ///
    /// Retired-текстуры здесь намеренно не освобождаются: текущие paint jobs всё ещё
    /// могут ссылаться на них. Владелец frame submission обязан вызвать
    /// [`Self::free_retired_textures`] только после submit-а либо отказа от кадра.
    pub fn upload_changed_textures(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        textures_delta: &egui::TexturesDelta,
    ) {
        // egui 0.36 группирует дельты по текстуре: внутри одного id порядок дельт
        // важен (частичные обновления поверх целой загрузки), между разными id — нет,
        // как и в эталонной интеграции `egui_wgpu::winit`.
        for (id, image_deltas) in &textures_delta.set {
            for image_delta in image_deltas {
                self.renderer
                    .update_texture(device, queue, *id, image_delta);
            }
        }
    }

    /// Освобождает текстуры, которые egui больше не использует.
    ///
    /// Метод является отдельной boundary-операцией, чтобы порядок
    /// `upload -> prepare -> render -> submit -> free` был виден в callsite.
    ///
    /// Это последний шаг жизни `TexturesDelta` кадра, поэтому метод забирает её
    /// по значению. Вызывающий обязан до этого применить `set` через
    /// [`Self::upload_changed_textures`].
    pub fn free_retired_textures(&mut self, mut textures_delta: egui::TexturesDelta) {
        for id in &textures_delta.free {
            self.renderer.free_texture(id);
        }
        // egui 0.36 в debug-сборке паникует, если `TexturesDelta` уничтожается
        // с непустыми `set`/`free` (защита от потерянных обновлений текстур).
        // Здесь обе части уже применены: `set` — при upload в начале кадра,
        // `free` — циклом выше. Очищаем явно, как требует egui.
        textures_delta.clear();
    }

    /// Обновляет egui vertex/index buffers и возвращает callback command buffers.
    ///
    /// `egui-wgpu` подготавливает пользовательские paint callbacks отдельно от
    /// основного encoder-а. Владелец frame submission обязан отправить эти buffers
    /// в ту же очередь перед command buffer-ом основного кадра.
    pub fn update_buffers(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        paint_jobs: &[egui::epaint::ClippedPrimitive],
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
    ) -> Vec<wgpu::CommandBuffer> {
        self.renderer
            .update_buffers(device, queue, encoder, paint_jobs, screen_descriptor)
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
