/// Модуль рендеринга: инициализация wgpu, swapchain и video render backend.
///
/// Отвечает за:
/// - создание wgpu instance, adapter, device, queue
/// - настройку surface и swapchain для окна
/// - создание render backend facade для decoded frames
/// - рендеринг видео + egui overlay в swapchain
///
/// Почему не eframe:
/// eframe скрывает детали инициализации swapchain и render pass.
/// Нам нужен прямой контроль для будущего zero-copy video path,
/// где decoded VkImage будет рендериться напрямую без CPU readback.
///
/// Архитектура рендеринга:
/// 1. Получаем surface texture из swapchain
/// 2. Рендерим decoded video frame или чёрный фон
/// 3. Рендерим egui overlay поверх видео
/// 4. Present на экран
use std::sync::Arc;

use anyhow::{Context, Result};
use render_core::{ColorPipelineSettings, HdrToSdrSettings, RenderCapabilities, RenderDiagnostics};
use tracing::{debug, info, instrument};
use winit::window::Window;

use crate::{WgpuRenderableFrame, WgpuVideoRenderer};
use video_vulkan::UnifiedVulkanInstance;

/// Выбирает формат swapchain для SDR-видео.
fn choose_surface_format(formats: &[wgpu::TextureFormat]) -> Result<wgpu::TextureFormat> {
    // Для текущего NV12 renderer предпочитаем обычный 8-bit формат.
    const PREFERRED_FORMATS: &[wgpu::TextureFormat] = &[
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    ];

    // Сначала ищем явно поддерживаемый 8-bit формат.
    for preferred_format in PREFERRED_FORMATS {
        if formats.contains(preferred_format) {
            return Ok(*preferred_format);
        }
    }

    // Если 8-bit форматов нет, используем первый формат из capabilities.
    formats
        .first()
        .copied()
        .context("Surface capabilities не вернул ни одного texture format")
}

/// Выбирает present mode без тихого fallback на пустой список capabilities.
fn choose_present_mode(present_modes: &[wgpu::PresentMode]) -> Result<wgpu::PresentMode> {
    // FIFO остаётся безопасным VSync default для текущего shell.
    if present_modes.contains(&wgpu::PresentMode::Fifo) {
        return Ok(wgpu::PresentMode::Fifo);
    }

    // Если FIFO нет, берём первый режим, явно сообщённый backend-ом.
    present_modes
        .first()
        .copied()
        .context("Surface capabilities не вернул ни одного present mode")
}

/// Выбирает alpha mode без неявного panic на некорректных capabilities.
fn choose_alpha_mode(alpha_modes: &[wgpu::CompositeAlphaMode]) -> Result<wgpu::CompositeAlphaMode> {
    // Phase 8.5 не вводит отдельную alpha policy, поэтому используем первый режим backend-а.
    alpha_modes
        .first()
        .copied()
        .context("Surface capabilities не вернул ни одного alpha mode")
}

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

        // Предпочитаем 8-bit формат: текущий SDR/NV12 shader пишет обычный RGBA.
        let surface_format = choose_surface_format(&surface_caps.formats)?;

        let present_mode = choose_present_mode(&surface_caps.present_modes)?;
        let alpha_mode = choose_alpha_mode(&surface_caps.alpha_modes)?;

        let size = window.inner_size();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
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

/// Итог одного render-frame вызова.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderFrameOutcome {
    /// Кадр был отправлен в swapchain и представлен.
    Presented,

    /// Кадр был пропущен из-за состояния surface/window.
    Dropped(RenderFrameDropReason),

    /// Video render path failed; caller must treat this as fatal media error.
    Failed(RenderFrameFailure),
}

/// Ошибка video render path, которую app/player layer не должен превращать в fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderFrameFailure {
    /// Сообщение renderer-а для логов и UI.
    pub message: String,
}

impl RenderFrameFailure {
    /// Создаёт failure из renderer error.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Причина пропуска кадра renderer backend-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFrameDropReason {
    /// Surface acquisition не успел завершиться.
    SurfaceTimeout,

    /// Окно occluded, compositor не принимает кадр.
    SurfaceOccluded,

    /// Surface потерян; renderer выполнил reconfigure и ждёт следующий redraw.
    SurfaceLost,

    /// Surface validation error при acquisition.
    SurfaceValidation,

    /// Повторный acquisition после Outdated/Reconfigure тоже не дал frame.
    SurfaceOutdatedRecoveryFailed,
}

/// Полный рендерер: GPU контекст + видеопайплайн + egui рендерер.
///
/// Координирует рендеринг видео и UI overlay в каждом кадре.
pub struct Renderer {
    /// GPU ресурсы (device, queue, surface).
    gpu: GpuContext,

    /// Рендерер egui для отрисовки UI поверх видео.
    egui_renderer: egui_wgpu::Renderer,

    /// Video renderer facade — скрывает конкретный NV12 shader/backend детали.
    video_renderer: WgpuVideoRenderer,
}

impl Renderer {
    /// Создаёт полный рендерер.
    ///
    /// Инициализирует:
    /// - GPU контекст (async)
    /// - video renderer facade
    /// - egui_wgpu renderer
    #[instrument(skip(window))]
    pub fn new(window: Arc<Window>) -> Result<Self> {
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

        let video_renderer = WgpuVideoRenderer::new(&gpu.device, gpu.surface_format);

        info!("Рендерер полностью инициализирован");

        Ok(Self {
            gpu,
            egui_renderer,
            video_renderer,
        })
    }

    /// Возвращает wgpu instance для decoder backend-ов, которым нужен shared GPU context.
    #[must_use]
    pub const fn instance(&self) -> &wgpu::Instance {
        &self.gpu.instance
    }

    /// Возвращает выбранный wgpu adapter.
    #[must_use]
    pub const fn adapter(&self) -> &wgpu::Adapter {
        &self.gpu.adapter
    }

    /// Возвращает wgpu device для decoder texture pool.
    #[must_use]
    pub const fn device(&self) -> &wgpu::Device {
        &self.gpu.device
    }

    /// Возвращает wgpu queue для decoder uploads.
    #[must_use]
    pub const fn queue(&self) -> &wgpu::Queue {
        &self.gpu.queue
    }

    /// Пересоздаёт surface configuration при изменении размера окна.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.gpu.resize(width, height);
    }

    /// Возвращает renderer capabilities для общего system capability report.
    #[must_use]
    pub fn render_capabilities(&self) -> RenderCapabilities {
        self.video_renderer.capabilities().clone()
    }

    /// Возвращает renderer-neutral диагностику последнего video pass.
    #[must_use]
    pub fn diagnostics(&self) -> RenderDiagnostics {
        self.video_renderer.diagnostics().clone()
    }

    /// Передаёт пользовательские SDR color settings во внутренний video renderer.
    pub fn set_color_pipeline_settings(&mut self, settings: ColorPipelineSettings) {
        self.video_renderer.set_color_pipeline_settings(settings);
    }

    /// Передаёт HDR-to-SDR settings во внутренний P010 renderer.
    pub fn set_hdr_to_sdr_settings(&mut self, settings: HdrToSdrSettings) {
        self.video_renderer.set_hdr_to_sdr_settings(settings);
    }

    /// Рендерит один полный кадр: видео + egui overlay.
    ///
    /// Последовательность:
    /// 1. Обновляем egui textures/buffers
    /// 2. Получаем surface texture из swapchain
    /// 3. Рендерим video frame через backend facade или очищаем target
    /// 4. Рендерим egui поверх видео
    /// 5. Submit и present
    pub fn render_frame(
        &mut self,
        window: &Window,
        _time: f32,
        video_frame: Option<&WgpuRenderableFrame<'_>>,
        egui_paint_jobs: Vec<egui::epaint::ClippedPrimitive>,
        egui_textures_delta: egui::TexturesDelta,
        screen_size_in_pixels: [u32; 2],
        pixels_per_point: f32,
    ) -> RenderFrameOutcome {
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: screen_size_in_pixels,
            pixels_per_point,
        };

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
                        return RenderFrameOutcome::Dropped(
                            RenderFrameDropReason::SurfaceOutdatedRecoveryFailed,
                        );
                    }
                }
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                // Таймаут — пропускаем кадр, не блокируем
                return RenderFrameOutcome::Dropped(RenderFrameDropReason::SurfaceTimeout);
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                // Окно скрыто другим окном — пропускаем кадр
                tracing::debug!("Surface occluded — skipping frame");
                return RenderFrameOutcome::Dropped(RenderFrameDropReason::SurfaceOccluded);
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                // Surface потерян: сначала пробуем штатный reconfigure текущей surface.
                // Если драйвер не восстановит surface, следующий redraw снова попадёт
                // сюда, и внешний lifecycle сможет пересоздать runtime через resumed/suspend.
                tracing::warn!("Surface lost — пробуем reconfigure");
                self.gpu
                    .surface
                    .configure(&self.gpu.device, &self.gpu.surface_config);
                return RenderFrameOutcome::Dropped(RenderFrameDropReason::SurfaceLost);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                // Validation error при получении surface texture — пропускаем кадр
                tracing::warn!("Surface validation error — skipping frame");
                return RenderFrameOutcome::Dropped(RenderFrameDropReason::SurfaceValidation);
            }
        };

        let surface_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        self.video_renderer.resize(
            self.gpu.surface_config.width,
            self.gpu.surface_config.height,
        );

        match self.video_renderer.render_or_clear(
            video_frame,
            &surface_view,
            &mut encoder,
            &self.gpu.device,
            &self.gpu.queue,
        ) {
            Ok(_video_rendered) => {}
            Err(error) => {
                tracing::error!(error = %error, "Video render failed");
                return RenderFrameOutcome::Failed(RenderFrameFailure::new(error.to_string()));
            }
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
        if let Err(error) = self.gpu.device.poll(wgpu::PollType::Poll) {
            tracing::warn!(error = %error, "wgpu device poll завершился ошибкой во время GPU cleanup");
        }

        // Сообщаем winit, что сейчас будет present: это помогает backend/compositor timing.
        window.pre_present_notify();

        // Показываем кадр на экране.
        surface_texture.present();
        RenderFrameOutcome::Presented
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что SDR path предпочитает Unorm, чтобы не включить implicit sRGB transfer.
    #[test]
    fn surface_format_prefers_current_unorm_path_before_srgb() {
        let formats = [
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Bgra8Unorm,
        ];

        let selected_format = choose_surface_format(&formats).expect("surface format selected");

        assert_eq!(selected_format, wgpu::TextureFormat::Bgra8Unorm);
    }

    /// Проверяет явную ошибку вместо panic при некорректных surface capabilities.
    #[test]
    fn empty_surface_format_list_is_reported_as_error() {
        let error = choose_surface_format(&[]).expect_err("empty format list rejected");

        assert!(
            error
                .to_string()
                .contains("Surface capabilities не вернул ни одного texture format")
        );
    }

    /// Проверяет VSync-friendly present mode policy.
    #[test]
    fn present_mode_prefers_fifo_when_available() {
        let present_modes = [wgpu::PresentMode::Immediate, wgpu::PresentMode::Fifo];

        let selected_present_mode =
            choose_present_mode(&present_modes).expect("present mode selected");

        assert_eq!(selected_present_mode, wgpu::PresentMode::Fifo);
    }

    /// Проверяет явную ошибку вместо panic при пустом списке present modes.
    #[test]
    fn empty_present_mode_list_is_reported_as_error() {
        let error = choose_present_mode(&[]).expect_err("empty present mode list rejected");

        assert!(
            error
                .to_string()
                .contains("Surface capabilities не вернул ни одного present mode")
        );
    }

    /// Проверяет явную ошибку вместо panic при пустом списке alpha modes.
    #[test]
    fn empty_alpha_mode_list_is_reported_as_error() {
        let error = choose_alpha_mode(&[]).expect_err("empty alpha mode list rejected");

        assert!(
            error
                .to_string()
                .contains("Surface capabilities не вернул ни одного alpha mode")
        );
    }
}
