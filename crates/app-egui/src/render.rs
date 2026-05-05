/// Модуль рендеринга: инициализация wgpu, swapchain, синтетическое видео.
///
/// Отвечает за:
/// - создание wgpu instance, adapter, device, queue
/// - настройку surface и swapchain для окна
/// - создание render pipeline для синтетического видео
/// - рендеринг видео + egui overlay в swapchain
///
/// Почему не eframe:
/// eframe скрывает детали инициализации swapchain и render pass.
/// Нам нужен прямой контроль для будущего zero-copy video path,
/// где decoded VkImage будет рендериться напрямую без CPU readback.
///
/// Архитектура рендеринга:
/// 1. Получаем surface texture из swapchain
/// 2. Рендерим синтетическое видео (полноэкранный pass)
/// 3. Рендерим egui overlay поверх видео
/// 4. Present на экран
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{debug, info, instrument};
use winit::window::Window;

use crate::telemetry::Telemetry;
// VideoRenderer trait not used directly — we use Nv12VideoRenderer concrete type.
use video_vulkan::UnifiedVulkanInstance;

/// GPU ресурсы: device, queue, surface и их конфигурация.
///
/// Владеет всеми wgpu объектами, необходимыми для рендеринга.
/// При ресайзе окна пересоздаёт surface configuration.
pub struct GpuContext {
    /// wgpu instance — нужна для zero-copy DMA-BUF import в video decoder.
    pub instance: wgpu::Instance,

    /// wgpu adapter — нужен для zero-copy DMA-BUF import в video decoder.
    pub adapter: wgpu::Adapter,

    /// Surface — абстракция wgpu над платформенным swapchain.
    pub surface: wgpu::Surface<'static>,

    /// Логическое устройство GPU, через которое создаются ресурсы.
    pub device: wgpu::Device,

    /// Очередь команд для отправки buffer/texture updates на GPU.
    pub queue: wgpu::Queue,

    /// Текущая конфигурация surface (формат, размер, present mode).
    pub surface_config: wgpu::SurfaceConfiguration,

    /// Формат surface, выбранный из поддерживаемых адаптером.
    pub surface_format: wgpu::TextureFormat,
}

impl GpuContext {
    /// Создаёт GPU контекст асинхронно с единым Vulkan instance.
    ///
    /// Последовательность:
    /// 1. UnifiedVulkanInstance — ash instance с video extensions + wgpu::Instance
    /// 2. Surface — привязка к окну
    /// 3. Adapter — выбор GPU
    /// 4. Device/Queue — через hal open_with_callback с video decode extensions
    /// 5. Surface configuration — настройка swapchain
    #[instrument(skip(window), fields(window = "youtube-player"))]
    pub async fn new(window: Arc<Window>) -> Result<Self> {
        info!("Инициализация UnifiedVulkanInstance");

        let unified =
            UnifiedVulkanInstance::new().context("Не удалось создать UnifiedVulkanInstance")?;

        let surface = unified
            .wgpu_instance
            .create_surface(window.clone())
            .context("Не удалось создать surface для окна")?;

        info!("Запрос адаптера GPU");

        let adapter = unified
            .wgpu_instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("Не удалось получить GPU адаптер. Проверьте драйверы Vulkan")?;

        let adapter_info = adapter.get_info();
        info!(
            name = %adapter_info.name,
            vendor = %adapter_info.vendor,
            device_type = ?adapter_info.device_type,
            backend = ?adapter_info.backend,
            "Выбран GPU адаптер"
        );

        let (device, queue) = unified
            .create_device_with_video(&adapter)
            .await
            .context("Не удалось создать wgpu device с video decode extensions")?;

        // unified больше не нужен после инициализации — wgpu device и surface
        // не зависят от него во время работы. Раньше хранили для Vulkan Video decode,
        // но сейчас используем VA-API, которая имеет собственный display.

        let surface_caps = surface.get_capabilities(&adapter);

        // Предпочитаем non-sRGB формат для видео (цвета управляем сами)
        let surface_format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(surface_caps.formats[0]);

        let present_mode = if surface_caps
            .present_modes
            .contains(&wgpu::PresentMode::Fifo)
        {
            wgpu::PresentMode::Fifo
        } else {
            surface_caps.present_modes[0]
        };

        let size = window.inner_size();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        surface.configure(&device, &surface_config);

        info!(
            format = ?surface_format,
            present_mode = ?present_mode,
            width = size.width,
            height = size.height,
            "Surface настроен"
        );

        Ok(Self {
            instance: unified.wgpu_instance,
            adapter,
            surface,
            device,
            queue,
            surface_config,
            surface_format,
        })
    }

    /// Пересоздаёт surface с новыми размерами.
    ///
    /// Вызывается при WindowEvent::Resized.
    /// Игнорирует нулевые размеры (окно свёрнуто).
    #[instrument(skip(self), fields(width, height))]
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            debug!("Пропуск ресайза: нулевой размер");
            return;
        }

        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
        debug!(width, height, "Surface resized");
    }
}

/// Полный рендерер: GPU контекст + видеопайплайн + egui рендерер.
///
/// Координирует рендеринг видео и UI overlay в каждом кадре.
pub struct Renderer {
    /// GPU ресурсы (device, queue, surface).
    pub gpu: GpuContext,

    /// Рендерер egui для отрисовки UI поверх видео.
    pub egui_renderer: egui_wgpu::Renderer,

    /// Ссылка на телеметрию для логирования в render pass.
    telemetry: Arc<Telemetry>,

    /// Video renderer — NV12 renderer for decoded frames.
    pub video_renderer: Option<render::nv12_renderer::Nv12VideoRenderer>,
}

impl Renderer {
    /// Создаёт полный рендерер.
    ///
    /// Инициализирует:
    /// - GPU контекст (async)
    /// - synthetic video pipeline
    /// - egui_wgpu renderer
    #[instrument(skip(window, telemetry))]
    pub fn new(window: Arc<Window>, telemetry: Arc<Telemetry>) -> Result<Self> {
        let gpu = pollster::block_on(GpuContext::new(window.clone()))?;

        let egui_renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            gpu.surface_format,
            egui_wgpu::RendererOptions {
                depth_stencil_format: None,
                msaa_samples: 1,
                ..Default::default()
            },
        );

        let video_renderer =
            render::nv12_renderer::Nv12VideoRenderer::new(&gpu.device, gpu.surface_format);

        info!("Рендерер полностью инициализирован");

        Ok(Self {
            gpu,
            egui_renderer,
            telemetry,
            video_renderer: Some(video_renderer),
        })
    }

    /// Рендерит один полный кадр: видео + egui overlay.
    ///
    /// Последовательность:
    /// 1. Обновляем uniform buffer временем
    /// 2. Получаем surface texture из swapchain
    /// 3. Рендерим NV12 видео (если есть y/uv views) или синтетическое видео
    /// 4. Обновляем egui текстуры и буферы
    /// 5. Рендерим egui поверх видео
    /// 6. Submit и present
    pub fn render_frame(
        &mut self,
        _time: f32,
        video_frame: Option<&video_core::DecodedFrame>,
        video_y_view: Option<&wgpu::TextureView>,
        video_uv_view: Option<&wgpu::TextureView>,
        egui_paint_jobs: Vec<egui::epaint::ClippedPrimitive>,
        egui_textures_delta: egui::TexturesDelta,
        screen_descriptor: egui_wgpu::ScreenDescriptor,
    ) {
        // Обновляем egui текстуры (новые и удалённые)
        for (id, image_delta) in &egui_textures_delta.set {
            self.egui_renderer
                .update_texture(&self.gpu.device, &self.gpu.queue, *id, image_delta);
        }
        for id in &egui_textures_delta.free {
            self.egui_renderer.free_texture(id);
        }

        // Создаём command encoder для этого кадра
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            });

        // Обновляем egui буферы (vertex/index) перед рендерингом
        self.egui_renderer.update_buffers(
            &self.gpu.device,
            &self.gpu.queue,
            &mut encoder,
            &egui_paint_jobs,
            &screen_descriptor,
        );

        // Получаем surface texture для текущего кадра
        let surface_texture = match self.gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated => {
                // Surface был уничтожен и воссоздан (например, при смене монитора)
                self.gpu
                    .surface
                    .configure(&self.gpu.device, &self.gpu.surface_config);
                match self.gpu.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(frame)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
                    other => {
                        tracing::error!(
                            "Не удалось получить surface texture после reconfigure: {:?}",
                            other
                        );
                        return;
                    }
                }
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                // Таймаут — пропускаем кадр, не блокируем
                self.telemetry.record_dropped_frame();
                return;
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                // Окно скрыто другим окном — пропускаем кадр
                tracing::debug!("Surface occluded — skipping frame");
                self.telemetry.record_dropped_frame();
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                // Surface потерян: сначала пробуем штатный reconfigure текущей surface.
                // Если драйвер не восстановит surface, следующий redraw снова попадёт
                // сюда, и внешний lifecycle сможет пересоздать runtime через resumed/suspend.
                tracing::warn!("Surface lost — пробуем reconfigure");
                self.gpu
                    .surface
                    .configure(&self.gpu.device, &self.gpu.surface_config);
                self.telemetry.record_dropped_frame();
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                // Validation error при получении surface texture — пропускаем кадр
                tracing::warn!("Surface validation error — skipping frame");
                self.telemetry.record_dropped_frame();
                return;
            }
        };

        let surface_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Render NV12 video frame if views available, otherwise clear to black
        let mut video_rendered = false;
        if let (Some(frame), Some(y_view), Some(uv_view)) =
            (video_frame, video_y_view, video_uv_view)
        {
            if let Some(ref mut renderer) = self.video_renderer {
                renderer.set_window_size(
                    self.gpu.surface_config.width,
                    self.gpu.surface_config.height,
                );
                match renderer.render_frame(
                    y_view,
                    uv_view,
                    frame.render_width,
                    frame.render_height,
                    &surface_view,
                    &mut encoder,
                    &self.gpu.device,
                    &self.gpu.queue,
                ) {
                    Ok(()) => {
                        video_rendered = true;
                        self.telemetry.record_video_frame_presented();
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "NV12 render failed");
                    }
                }
            }
        }

        if !video_rendered {
            // Если видео не отрендерено — заливаем чёрным
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear to black pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &surface_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        // Рендерим egui поверх видео
        {
            let egui_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui overlay pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &surface_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load, // Сохраняем видео, рисуем поверх
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            self.egui_renderer.render(
                &mut egui_pass.forget_lifetime(),
                &egui_paint_jobs,
                &screen_descriptor,
            );
        }

        // Отправляем команды на GPU
        self.gpu.queue.submit(std::iter::once(encoder.finish()));

        // Форсируем cleanup pending GPU resources (destroyed textures, buffers).
        // Критично для zero-copy DMA-BUF import: каждый кадр создаёт и уничтожает
        // wgpu textures, и без poll wgpu откладывает destruction до неопределённого момента,
        // что приводит к Out of Memory через несколько десятков секунд 4K playback.
        let _ = self.gpu.device.poll(wgpu::PollType::Poll);

        // Показываем кадр на экране
        surface_texture.present();
        self.telemetry.record_presented_frame();
    }
}
