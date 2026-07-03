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
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use render_core::{
    ColorPipelineSettings, HdrToSdrSettings, RenderCapabilities, RenderDiagnostics,
    RenderLiveApplyReport, RenderLiveSettings, RenderLiveSettingsAdapter, RenderLiveSettingsError,
    RenderLiveSettingsUpdate,
};
use render_wgpu_video::{
    WgpuRenderableFrame, WgpuVideoRenderInput, WgpuVideoRenderer,
    required_wgpu_video_texture_features,
};
use tracing::{debug, info, instrument};
use winit::window::Window;

use crate::egui_compositor::EguiCompositor;
use crate::frame::{
    RenderFrameDropReason, RenderFrameFailure, RenderFrameInput, RenderFrameOutcome,
    RenderFrameStageTimings, RenderFrameTiming, clamp_video_exclusion_rects_to_screen,
    clamp_video_viewport_to_screen,
};

/// Превращает длительность в миллисекунды для числовых tracing fields.
fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Возвращает dropped outcome и логирует timing даже для кадров без успешного present.
fn dropped_frame_after_surface_acquire(
    reason: RenderFrameDropReason,
    renderer_started_at: Instant,
    surface_acquire_started_at: Instant,
) -> RenderFrameOutcome {
    tracing::debug!(
        reason = ?reason,
        renderer_elapsed_ms = duration_ms(renderer_started_at.elapsed()),
        surface_acquire_ms = duration_ms(surface_acquire_started_at.elapsed()),
        "render frame dropped before present"
    );
    RenderFrameOutcome::Dropped(reason)
}

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

/// Предпочтительный present mode swapchain-а в нейтральной форме.
///
/// Живёт в shell-слое, чтобы композиция (`app-egui`) могла прокинуть выбор из
/// config без зависимости shell -> `rustiplayer-config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellPresentMode {
    /// Авто: предпочесть FIFO (VSync), иначе первый доступный режим backend-а.
    Auto,

    /// FIFO: классический VSync, present блокирует до кадрового интервала.
    Fifo,

    /// Mailbox: triple-buffer, present не блокирует, новый кадр вытесняет очередь.
    Mailbox,

    /// Immediate: без синхронизации (возможен tearing).
    Immediate,
}

/// Нейтральные surface present настройки, прокидываемые в shell из композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfacePresentSettings {
    /// Предпочтительный present mode.
    pub present_mode: ShellPresentMode,

    /// Желаемая глубина swapchain latency (в кадрах); 0 нормализуется к 1.
    pub max_frame_latency: u32,
}

impl Default for SurfacePresentSettings {
    /// Безопасный VSync default, совпадающий с прежним hardcoded поведением shell.
    fn default() -> Self {
        Self {
            present_mode: ShellPresentMode::Auto,
            max_frame_latency: 2,
        }
    }
}

/// Выбирает present mode с учётом предпочтения и без тихого fallback на пустой список.
fn choose_present_mode(
    present_modes: &[wgpu::PresentMode],
    preference: ShellPresentMode,
) -> Result<wgpu::PresentMode> {
    // Auto и недоступный запрошенный режим откатываются к FIFO -> первый доступный.
    let auto_choice = || -> Result<wgpu::PresentMode> {
        if present_modes.contains(&wgpu::PresentMode::Fifo) {
            return Ok(wgpu::PresentMode::Fifo);
        }
        present_modes
            .first()
            .copied()
            .context("Surface capabilities не вернул ни одного present mode")
    };

    let requested = match preference {
        ShellPresentMode::Auto => return auto_choice(),
        ShellPresentMode::Fifo => wgpu::PresentMode::Fifo,
        ShellPresentMode::Mailbox => wgpu::PresentMode::Mailbox,
        ShellPresentMode::Immediate => wgpu::PresentMode::Immediate,
    };

    if present_modes.contains(&requested) {
        return Ok(requested);
    }

    // Запрошенный режим surface не поддерживает: явный warn и безопасный fallback,
    // вместо тихого выбора чего попало.
    tracing::warn!(
        requested = ?requested,
        available = ?present_modes,
        "Запрошенный present mode не поддержан surface-ом; откат к FIFO/первому доступному"
    );
    auto_choice()
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
    #[instrument(skip(window), fields(window = "rustiplayer"))]
    pub async fn new(window: Arc<Window>, surface_present: SurfacePresentSettings) -> Result<Self> {
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
                label: Some("rustiplayer device"),
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

        let present_mode =
            choose_present_mode(&surface_caps.present_modes, surface_present.present_mode)?;
        let alpha_mode = choose_alpha_mode(&surface_caps.alpha_modes)?;

        // 0 кадров latency недопустимо для swapchain — нормализуем к 1.
        let desired_maximum_frame_latency = surface_present.max_frame_latency.max(1);

        let size = window.inner_size();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency,
        };

        surface.configure(&device, &surface_config);

        info!(
            format = ?surface_format,
            present_mode = ?present_mode,
            desired_maximum_frame_latency,
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
    pub fn new(window: Arc<Window>, surface_present: SurfacePresentSettings) -> Result<Self> {
        let gpu = pollster::block_on(GpuContext::new(window.clone(), surface_present))?;

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
        let renderer_started_at = Instant::now();
        let RenderFrameInput {
            window,
            video_frame,
            egui_paint_jobs,
            egui_textures_delta,
            screen,
            video_viewport,
            video_exclusion_rects,
        } = input;
        let video_frame: Option<&WgpuRenderableFrame<'_>> = video_frame;
        let clamped_video_viewport = clamp_video_viewport_to_screen(video_viewport, &screen);
        let clamped_video_exclusion_rects =
            clamp_video_exclusion_rects_to_screen(video_exclusion_rects, &screen);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: screen.size_in_pixels,
            pixels_per_point: screen.pixels_per_point,
        };

        // Обновляем egui текстуры (новые и удалённые).
        let stage_started_at = Instant::now();
        self.egui_compositor.update_textures(
            &self.gpu.device,
            &self.gpu.queue,
            &egui_textures_delta,
        );
        let egui_texture_update_elapsed = stage_started_at.elapsed();

        // Создаём command encoder для этого кадра
        let stage_started_at = Instant::now();
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            });
        let encoder_creation_elapsed = stage_started_at.elapsed();

        // Обновляем egui буферы (vertex/index) перед рендерингом
        let stage_started_at = Instant::now();
        self.egui_compositor.update_buffers(
            &self.gpu.device,
            &self.gpu.queue,
            &mut encoder,
            &egui_paint_jobs,
            &screen_descriptor,
        );
        let egui_buffer_update_elapsed = stage_started_at.elapsed();

        // Получаем surface texture для текущего кадра
        let stage_started_at = Instant::now();
        let surface_acquire_started_at = stage_started_at;
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
                        return dropped_frame_after_surface_acquire(
                            RenderFrameDropReason::SurfaceOutdatedRecoveryFailed,
                            renderer_started_at,
                            surface_acquire_started_at,
                        );
                    }
                }
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                // Таймаут — пропускаем кадр, не блокируем
                return dropped_frame_after_surface_acquire(
                    RenderFrameDropReason::SurfaceTimeout,
                    renderer_started_at,
                    surface_acquire_started_at,
                );
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                // Окно скрыто другим окном — пропускаем кадр
                tracing::debug!("Surface occluded — skipping frame");
                return dropped_frame_after_surface_acquire(
                    RenderFrameDropReason::SurfaceOccluded,
                    renderer_started_at,
                    surface_acquire_started_at,
                );
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                // Surface потерян: сначала пробуем штатный reconfigure текущей surface.
                // Если драйвер не восстановит surface, следующий redraw снова попадёт
                // сюда, и внешний lifecycle сможет пересоздать runtime через resumed/suspend.
                tracing::warn!("Surface lost — пробуем reconfigure");
                self.gpu
                    .surface
                    .configure(&self.gpu.device, &self.gpu.surface_config);
                return dropped_frame_after_surface_acquire(
                    RenderFrameDropReason::SurfaceLost,
                    renderer_started_at,
                    surface_acquire_started_at,
                );
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                // Validation error при получении surface texture — пропускаем кадр
                tracing::warn!("Surface validation error — skipping frame");
                return dropped_frame_after_surface_acquire(
                    RenderFrameDropReason::SurfaceValidation,
                    renderer_started_at,
                    surface_acquire_started_at,
                );
            }
        };
        let surface_acquire_elapsed = stage_started_at.elapsed();

        let stage_started_at = Instant::now();
        let surface_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let surface_view_creation_elapsed = stage_started_at.elapsed();

        self.video_renderer.resize(
            self.gpu.surface_config.width,
            self.gpu.surface_config.height,
        );

        let stage_started_at = Instant::now();
        match self.video_renderer.render_or_clear(WgpuVideoRenderInput {
            frame: video_frame,
            video_viewport: clamped_video_viewport,
            video_exclusion_rects: &clamped_video_exclusion_rects,
            target: &surface_view,
            encoder: &mut encoder,
            device: &self.gpu.device,
            queue: &self.gpu.queue,
        }) {
            Ok(_video_rendered) => {}
            Err(error) => {
                tracing::error!(error = %error, "Video render failed");
                return RenderFrameOutcome::Failed(RenderFrameFailure::new(error.to_string()));
            }
        }
        let video_render_elapsed = stage_started_at.elapsed();

        // Рендерим egui поверх видео.
        let stage_started_at = Instant::now();
        self.egui_compositor.render_overlay(
            &mut encoder,
            &surface_view,
            &egui_paint_jobs,
            &screen_descriptor,
        );
        let egui_render_elapsed = stage_started_at.elapsed();

        // Отправляем команды на GPU
        let stage_started_at = Instant::now();
        self.gpu.queue.submit(std::iter::once(encoder.finish()));
        let queue_submit_elapsed = stage_started_at.elapsed();

        // Продвигаем wgpu callbacks для submitted work.
        // Zero-copy imports теперь живут в bounded persistent pool, поэтому poll
        // больше не является основным механизмом выживания resource cleanup-а.
        let stage_started_at = Instant::now();
        if let Err(error) = self.gpu.device.poll(wgpu::PollType::Poll) {
            tracing::warn!(error = %error, "wgpu device poll завершился ошибкой во время GPU callback polling");
        }
        let device_poll_elapsed = stage_started_at.elapsed();

        // Сообщаем winit, что сейчас будет present: это помогает backend/compositor timing.
        let stage_started_at = Instant::now();
        window.pre_present_notify();
        let pre_present_notify_elapsed = stage_started_at.elapsed();

        // Показываем кадр на экране.
        let stage_started_at = Instant::now();
        surface_texture.present();
        let surface_present_elapsed = stage_started_at.elapsed();

        let stages = RenderFrameStageTimings {
            egui_texture_update: egui_texture_update_elapsed,
            encoder_creation: encoder_creation_elapsed,
            egui_buffer_update: egui_buffer_update_elapsed,
            surface_acquire: surface_acquire_elapsed,
            surface_view_creation: surface_view_creation_elapsed,
            video_render: video_render_elapsed,
            egui_render: egui_render_elapsed,
            queue_submit: queue_submit_elapsed,
            device_poll: device_poll_elapsed,
            pre_present_notify: pre_present_notify_elapsed,
            surface_present: surface_present_elapsed,
        };
        RenderFrameOutcome::Presented(RenderFrameTiming::new(
            stages,
            renderer_started_at.elapsed(),
        ))
    }
}

impl RenderLiveSettingsAdapter for Renderer {
    /// Делегирует preview во внутренний WGPU video renderer без surface/pipeline rebuild.
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> std::result::Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.video_renderer.preview_live_settings(update)
    }

    /// Делегирует committed apply во внутренний WGPU video renderer.
    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> std::result::Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.video_renderer.commit_live_settings(settings)
    }

    /// Делегирует rollback к preview baseline во внутренний WGPU video renderer.
    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> std::result::Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.video_renderer.rollback_live_settings(baseline)
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

    /// Auto предпочитает FIFO как безопасный VSync default, когда он доступен.
    #[test]
    fn present_mode_auto_prefers_fifo_when_available() {
        let present_modes = [wgpu::PresentMode::Immediate, wgpu::PresentMode::Fifo];

        let selected_present_mode = choose_present_mode(&present_modes, ShellPresentMode::Auto)
            .expect("present mode selected");

        assert_eq!(selected_present_mode, wgpu::PresentMode::Fifo);
    }

    /// Явный запрошенный поддерживаемый режим (Mailbox) выбирается как есть.
    #[test]
    fn present_mode_uses_requested_when_supported() {
        let present_modes = [wgpu::PresentMode::Fifo, wgpu::PresentMode::Mailbox];

        let selected_present_mode = choose_present_mode(&present_modes, ShellPresentMode::Mailbox)
            .expect("present mode selected");

        assert_eq!(selected_present_mode, wgpu::PresentMode::Mailbox);
    }

    /// Запрошенный, но не поддержанный режим откатывается к FIFO без ошибки.
    #[test]
    fn present_mode_falls_back_to_fifo_when_requested_unsupported() {
        let present_modes = [wgpu::PresentMode::Fifo, wgpu::PresentMode::Immediate];

        let selected_present_mode = choose_present_mode(&present_modes, ShellPresentMode::Mailbox)
            .expect("present mode selected");

        assert_eq!(selected_present_mode, wgpu::PresentMode::Fifo);
    }

    /// Проверяет явную ошибку вместо panic при пустом списке present modes.
    #[test]
    fn empty_present_mode_list_is_reported_as_error() {
        let error = choose_present_mode(&[], ShellPresentMode::Auto)
            .expect_err("empty present mode list rejected");

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
