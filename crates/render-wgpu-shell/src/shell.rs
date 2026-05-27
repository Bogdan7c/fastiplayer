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
/// Нам нужен прямой контроль для текущего zero-copy video path,
/// где decoded VkImage будет рендериться напрямую без CPU readback.
///
/// Архитектура рендеринга:
/// 1. Получаем surface texture из swapchain
/// 2. Рендерим decoded video frame или чёрный фон
/// 3. Рендерим egui overlay поверх видео
/// 4. Present на экран
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use render_core::{ColorPipelineSettings, HdrToSdrSettings, RenderCapabilities, RenderDiagnostics};
use render_wgpu_video::{
    WgpuRenderableFrame, WgpuVideoRenderer, required_wgpu_video_texture_features,
};
use tracing::{debug, info, instrument};
use winit::window::Window;

use crate::egui_compositor::EguiCompositor;
use crate::frame::{
    RenderFrameDropReason, RenderFrameFailure, RenderFrameInput, RenderFrameOutcome,
    RenderFrameTiming,
};

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
    /// Создаёт GPU контекст асинхронно через стандартный WGPU Vulkan instance.
    ///
    /// Последовательность:
    /// 1. WGPU Vulkan instance без display handle ownership
    /// 2. Surface — привязка к окну
    /// 3. Adapter — выбор GPU
    /// 4. Device/Queue — с feature gates, нужными video renderer boundary
    /// 5. Surface configuration — настройка swapchain
    #[instrument(skip(window), fields(window = "youtube-player"))]
    pub async fn new(window: Arc<Window>) -> Result<Self> {
        info!("Инициализация WGPU Vulkan instance");

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let surface = instance
            .create_surface(window.clone())
            .context("Не удалось создать surface для окна")?;

        info!("Запрос адаптера GPU");

        let adapter = instance
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

        let required_features = required_wgpu_video_texture_features(&adapter);
        info!(
            nv12_texture_format = required_features.contains(wgpu::Features::TEXTURE_FORMAT_NV12),
            p010_texture_format = required_features.contains(wgpu::Features::TEXTURE_FORMAT_P010),
            texture_format_16bit_norm =
                required_features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM),
            "Запрос WGPU device с video texture features"
        );

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("youtube-player device"),
                required_features,
                required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            })
            .await
            .context("Не удалось создать wgpu device с video texture features")?;

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
            instance,
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
    gpu: GpuContext,

    /// Композитор egui overlay поверх video pass-а.
    egui_compositor: EguiCompositor,

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

        let egui_compositor = EguiCompositor::new(&gpu.device, gpu.surface_format);
        let video_renderer = WgpuVideoRenderer::new(&gpu.device, gpu.surface_format);

        info!("Рендерер полностью инициализирован");

        Ok(Self {
            gpu,
            egui_compositor,
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
    pub fn render_frame(&mut self, input: RenderFrameInput<'_>) -> RenderFrameOutcome {
        let RenderFrameInput {
            window,
            video_frame,
            egui_paint_jobs,
            egui_textures_delta,
            screen,
        } = input;
        let video_frame: Option<&WgpuRenderableFrame<'_>> = video_frame;
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: screen.size_in_pixels,
            pixels_per_point: screen.pixels_per_point,
        };

        // Обновляем egui текстуры (новые и удалённые).
        self.egui_compositor.update_textures(
            &self.gpu.device,
            &self.gpu.queue,
            &egui_textures_delta,
        );

        // Создаём command encoder для этого кадра
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            });

        // Обновляем egui буферы (vertex/index) перед рендерингом
        self.egui_compositor.update_buffers(
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

        // Рендерим egui поверх видео.
        self.egui_compositor.render_overlay(
            &mut encoder,
            &surface_view,
            &egui_paint_jobs,
            &screen_descriptor,
        );

        let submit_present_started_at = Instant::now();

        // Отправляем команды на GPU
        self.gpu.queue.submit(std::iter::once(encoder.finish()));

        // Продвигаем wgpu callbacks для submitted work.
        // Zero-copy imports теперь живут в bounded persistent pool, поэтому poll
        // больше не является основным механизмом выживания resource cleanup-а.
        if let Err(error) = self.gpu.device.poll(wgpu::PollType::Poll) {
            tracing::warn!(error = %error, "wgpu device poll завершился ошибкой во время GPU callback polling");
        }

        // Сообщаем winit, что сейчас будет present: это помогает backend/compositor timing.
        window.pre_present_notify();

        // Показываем кадр на экране.
        surface_texture.present();
        RenderFrameOutcome::Presented(RenderFrameTiming {
            submit_present_elapsed: submit_present_started_at.elapsed(),
        })
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
