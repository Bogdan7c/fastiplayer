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
/// 1. Получаем surface texture из swapchain (`Renderer::acquire_surface_frame`, до выбора
///    видеокадра app-ом — сессия 06a)
/// 2. Рендерим decoded video frame или чёрный фон
/// 3. Рендерим egui overlay поверх видео
/// 4. Present на экран
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use render_core::{
    ColorPipelineSettings, HdrToSdrSettings, RenderCapabilities, RenderDiagnostics,
    RenderLiveApplyReport, RenderLiveSettings, RenderLiveSettingsAdapter, RenderLiveSettingsError,
    RenderLiveSettingsUpdate,
};
use render_wgpu_video::{
    HostPlanarUploadCommands, WgpuRenderableFrame, WgpuVideoRenderInput, WgpuVideoRenderer,
    required_wgpu_video_texture_features,
};
use tracing::{debug, info, instrument};
use winit::window::Window;

use crate::egui_compositor::EguiCompositor;
use crate::frame::{
    AcquiredSurfaceFrame, RenderFrameInput, clamp_video_exclusion_rects_to_screen,
    clamp_video_viewport_to_screen,
};
use crate::subtitle_overlay::{
    SubtitleOverlayRenderer, SubtitleOverlayTarget, SubtitleOverlayUploadStats,
};
use crate::surface_alpha::choose_alpha_mode;
use crate::surface_settings::{choose_present_mode, choose_surface_format};
use crate::window_corner_mask::{SurfaceAlphaEncoding, WindowCornerMaskRenderer};
use crate::{
    RenderFrameDropReason, RenderFrameFailure, RenderFrameOutcome, RenderFrameStageTimings,
    RenderFrameTiming, RenderScreenDescriptor, SurfacePresentSettings,
};

mod surface_acquire;
mod video_hot_path;

/// Превращает длительность в миллисекунды для числовых tracing fields.
fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
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

    /// Typed device-lost state, который callback WGPU публикует lifecycle owner-у.
    device_lost: Arc<Mutex<Option<GpuDeviceLost>>>,

    /// Подтверждённое surface-кодирование alpha; `None` запрещает прозрачную маску.
    window_alpha_encoding: Option<SurfaceAlphaEncoding>,
}

/// Доказанный WGPU device-lost cause без string flattening lifecycle state-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuDeviceLost {
    /// Причина, сообщённая WGPU backend-ом.
    pub reason: wgpu::DeviceLostReason,

    /// Backend diagnostic message для логов и UI failure report-а.
    pub message: String,
}

/// Ошибка ожидания старой GPU queue перед renderer commit-ом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendererGpuDrainError {
    /// WGPU callback доказал потерю device-а.
    DeviceLost(GpuDeviceLost),

    /// GPU work не завершилась за bounded lifecycle timeout.
    Timeout,

    /// WGPU отклонил submission index при ожидании.
    WrongSubmissionIndex,

    /// Device-lost state mutex poisoned, поэтому lifecycle state недостоверен.
    DeviceLostStatePoisoned,
}

impl std::fmt::Display for RendererGpuDrainError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeviceLost(device_lost) => write!(
                formatter,
                "WGPU device lost ({:?}): {}",
                device_lost.reason, device_lost.message
            ),
            Self::Timeout => {
                formatter.write_str("The wait for the old GPU queue ended with timeout")
            }
            Self::WrongSubmissionIndex => {
                formatter.write_str("WGPU rejects submission index while waiting for GPU queue")
            }
            Self::DeviceLostStatePoisoned => {
                formatter.write_str("device-lost lifecycle state poisoned")
            }
        }
    }
}

impl std::error::Error for RendererGpuDrainError {}

impl GpuContext {
    /// Создаёт GPU контекст асинхронно через стандартный WGPU Vulkan instance.
    ///
    /// Последовательность:
    /// 1. WGPU Vulkan instance без display handle ownership
    /// 2. Surface — привязка к окну
    /// 3. Adapter — выбор GPU
    /// 4. Device/Queue — с feature gates, нужными video renderer boundary
    /// 5. Surface configuration — настройка swapchain
    #[instrument(skip(window), fields(window = "fastiplayer"))]
    pub async fn new(window: Arc<Window>, surface_present: SurfacePresentSettings) -> Result<Self> {
        info!("Initialization of WGPU Vulkan instance");

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let surface = instance
            .create_surface(window.clone())
            .context("Could not create the window surface")?;

        info!("Requesting a GPU adapter");

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                // wgpu 30: `false` сохраняет прежнее поведение — реальные лимиты адаптера
                // без округления до «корзин» (защита от fingerprinting нужна браузерам).
                apply_limit_buckets: false,
            })
            .await
            .context("Couldn't get a GPU adapter. Check Vulkan drivers")?;

        let adapter_info = adapter.get_info();
        info!(
            name = %adapter_info.name,
            vendor = %adapter_info.vendor,
            device_type = ?adapter_info.device_type,
            backend = ?adapter_info.backend,
            "Selected GPU adapter"
        );

        let required_features = required_wgpu_video_texture_features(&adapter);
        info!(
            nv12_texture_format = required_features.contains(wgpu::Features::TEXTURE_FORMAT_NV12),
            p010_texture_format = required_features.contains(wgpu::Features::TEXTURE_FORMAT_P010),
            texture_format_16bit_norm =
                required_features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM),
            "WGPU device with video texture features"
        );

        // Устройство создаётся с `VK_EXT_queue_family_foreign`, если adapter его даёт:
        // это позволяет video renderer-у переиспользовать DMA-BUF image между кадрами.
        let (device, queue, foreign_queue_family) = crate::vulkan_device::create_video_device(
            &adapter,
            wgpu::DeviceDescriptor {
                label: Some("fastiplayer device"),
                required_features,
                required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            },
        )
        .await?;
        info!(
            foreign_queue_family = foreign_queue_family.diagnostic_label(),
            "WGPU device external memory synchronization"
        );

        let device_lost = Arc::new(Mutex::new(None));
        let device_lost_callback_state = device_lost.clone();
        device.set_device_lost_callback(move |reason, message| {
            match device_lost_callback_state.lock() {
                Ok(mut state) => {
                    *state = Some(GpuDeviceLost { reason, message });
                }
                Err(_) => {
                    tracing::error!("Could not write WGPU device-lost state: mutex poisoned");
                }
            }
        });

        let surface_caps = surface.get_capabilities(&adapter);

        // Предпочитаем 8-bit формат: текущий SDR/NV12 shader пишет обычный RGBA.
        let surface_format = choose_surface_format(&surface_caps.formats)?;

        let present_mode =
            choose_present_mode(&surface_caps.present_modes, surface_present.present_mode)?;
        let (alpha_mode, window_alpha_encoding) =
            choose_alpha_mode(&surface_caps.alpha_modes, surface_present.alpha_preference)?;

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
            // wgpu 30: `Auto` воспроизводит историческое поведение (sRGB для наших
            // 8-битных форматов). HDR-вывод на экран — отдельная фича из бэклога.
            color_space: wgpu::SurfaceColorSpace::Auto,
        };

        surface.configure(&device, &surface_config);

        info!(
            format = ?surface_format,
            present_mode = ?present_mode,
            desired_maximum_frame_latency,
            width = size.width,
            height = size.height,
            "Surface is set to be set up"
        );

        Ok(Self {
            instance,
            adapter,
            surface,
            device,
            queue,
            surface_config,
            surface_format,
            device_lost,
            window_alpha_encoding,
        })
    }

    /// Пересоздаёт surface с новыми размерами.
    ///
    /// Вызывается при WindowEvent::Resized.
    /// Игнорирует нулевые размеры (окно свёрнуто).
    #[instrument(skip(self), fields(width, height))]
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            debug!("Resize skip: Zero size");
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

    /// Финальный compositor контура; отсутствует при opaque/inherit surface fallback.
    window_corner_mask_renderer: Option<WindowCornerMaskRenderer>,

    /// Субтитры между video и egui.
    subtitle_overlay_renderer: SubtitleOverlayRenderer,
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
        let window_corner_mask_renderer = gpu.window_alpha_encoding.map(|alpha_encoding| {
            WindowCornerMaskRenderer::new(&gpu.device, gpu.surface_format, alpha_encoding)
        });

        let subtitle_overlay_renderer =
            SubtitleOverlayRenderer::new(&gpu.device, gpu.surface_format);

        info!("Renderer is fully initialized");

        Ok(Self {
            gpu,
            egui_compositor,
            video_renderer,
            window_corner_mask_renderer,
            subtitle_overlay_renderer,
        })
    }

    /// Счётчики загрузок субтитров в GPU (диагностика и приёмочные тесты).
    #[must_use]
    pub fn subtitle_overlay_upload_stats(&self) -> SubtitleOverlayUploadStats {
        self.subtitle_overlay_renderer.upload_stats()
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

    /// Возвращает live render snapshot, который candidate обязан сохранить при recreation.
    #[must_use]
    pub fn live_settings(&self) -> RenderLiveSettings {
        self.video_renderer.live_settings().clone()
    }

    /// Дожидается завершения уже submitted GPU work перед заменой device/surface.
    pub fn wait_for_gpu_idle(
        &self,
        timeout: Duration,
    ) -> std::result::Result<(), RendererGpuDrainError> {
        if let Some(device_lost) = self.device_lost_state()? {
            return Err(RendererGpuDrainError::DeviceLost(device_lost));
        }

        let poll_result = self.gpu.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(timeout),
        });

        if let Some(device_lost) = self.device_lost_state()? {
            return Err(RendererGpuDrainError::DeviceLost(device_lost));
        }

        match poll_result {
            Ok(_) => Ok(()),
            Err(wgpu::PollError::Timeout) => Err(RendererGpuDrainError::Timeout),
            Err(wgpu::PollError::WrongSubmissionIndex(_, _)) => {
                Err(RendererGpuDrainError::WrongSubmissionIndex)
            }
        }
    }

    /// Читает callback state без предположений о причине poll error-а.
    fn device_lost_state(
        &self,
    ) -> std::result::Result<Option<GpuDeviceLost>, RendererGpuDrainError> {
        self.gpu
            .device_lost
            .lock()
            .map(|state| state.clone())
            .map_err(|_| RendererGpuDrainError::DeviceLostStatePoisoned)
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

    /// `true`, если shell может нарисовать интерфейс прежними примитивами для этого размера
    /// экрана ([`crate::EguiFrameContent::ReusePrevious`]). App спрашивает это ДО решения пропустить
    /// egui-проход; после пересоздания renderer-а или смены размера ответ `false`.
    #[must_use]
    pub fn can_reuse_retained_ui(&self, screen: RenderScreenDescriptor) -> bool {
        self.egui_compositor
            .retained_frame_availability(&egui_wgpu::ScreenDescriptor {
                size_in_pixels: screen.size_in_pixels,
                pixels_per_point: screen.pixels_per_point,
            })
            .is_ok()
    }

    /// Передаёт пользовательские SDR color settings во внутренний video renderer.
    pub fn set_color_pipeline_settings(&mut self, settings: ColorPipelineSettings) {
        self.video_renderer.set_color_pipeline_settings(settings);
    }

    /// Передаёт HDR-to-SDR settings во внутренний P010 renderer.
    pub fn set_hdr_to_sdr_settings(&mut self, settings: HdrToSdrSettings) {
        self.video_renderer.set_hdr_to_sdr_settings(settings);
    }

    /// Рендерит один полный кадр: видео + egui overlay + контур desktop-окна.
    ///
    /// Последовательность:
    /// 1. Обновляем egui textures/buffers
    /// 2. Рендерим video frame через backend facade или очищаем target в уже полученный
    ///    буфер swapchain (`input.surface`, см. `acquire_surface_frame`)
    /// 3. Рендерим egui поверх видео
    /// 4. Последним pass-ом применяем прозрачный контур окна
    /// 5. Submit и present
    ///
    /// `renderer_elapsed` итогового timing-а = ожидание буфера + эта функция; выбор
    /// видеокадра app-ом между ними сюда не входит.
    pub fn render_frame(&mut self, input: RenderFrameInput<'_>) -> RenderFrameOutcome {
        let renderer_started_at = Instant::now();
        let RenderFrameInput {
            window,
            surface,
            video_frame,
            egui,
            screen,
            video_viewport,
            video_exclusion_rects,
            window_corner_mask,
            subtitle_overlay,
        } = input;
        // Размер surface уже приведён к `screen` в `acquire_surface_frame`.
        let AcquiredSurfaceFrame {
            surface_texture,
            acquire_elapsed: surface_acquire_elapsed,
        } = surface;
        let video_frame: Option<&WgpuRenderableFrame<'_>> = video_frame;
        let clamped_video_viewport = clamp_video_viewport_to_screen(video_viewport, &screen);
        let clamped_video_exclusion_rects =
            clamp_video_exclusion_rects_to_screen(video_exclusion_rects, &screen);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: screen.size_in_pixels,
            pixels_per_point: screen.pixels_per_point,
        };

        // Свежий кадр: загружаем новые/изменённые egui текстуры (retired-текстуры остаются
        // живы до submit-а текущих paint jobs). Повторный кадр: проверяем, что удержанные
        // примитивы существуют и загружены для этого же размера экрана — иначе отказ до
        // записи (буфер swapchain вернётся без показа при drop-е), и app повторит кадр
        // полным egui-проходом.
        let stage_started_at = Instant::now();
        let pending_egui = match self.egui_compositor.begin_frame_content(
            &self.gpu.device,
            &self.gpu.queue,
            egui,
            &screen_descriptor,
        ) {
            Ok(pending_egui) => pending_egui,
            Err(reason) => {
                debug!(
                    ?reason,
                    "Retained egui frame unavailable; the app must redraw with a fresh UI pass"
                );
                return RenderFrameOutcome::Dropped(RenderFrameDropReason::RetainedUiUnavailable);
            }
        };
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

        // Свежий кадр обновляет egui буферы (vertex/index) и становится удержанным;
        // повторный кадр читает буферы последнего свежего без новой загрузки.
        let stage_started_at = Instant::now();
        let (egui_callback_command_buffers, egui_textures_delta) =
            self.egui_compositor.prepare_pending_content(
                &self.gpu.device,
                &self.gpu.queue,
                &mut encoder,
                pending_egui,
                screen_descriptor,
            );
        let egui_buffer_update_elapsed = stage_started_at.elapsed();

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
        // Барьеры DMA-BUF кадра (если image переиспользуется) отправляются вокруг
        // основного command buffer-а, а GPU-копия плоскостей host-planar кадра — перед
        // ним; renderer их отдал, shell обязан их submit-ить в том же submit-е.
        let video_render_outcome = match self.video_renderer.render_or_clear(WgpuVideoRenderInput {
            frame: video_frame,
            video_viewport: clamped_video_viewport,
            video_exclusion_rects: &clamped_video_exclusion_rects,
            target: &surface_view,
            encoder: &mut encoder,
            device: &self.gpu.device,
            queue: &self.gpu.queue,
        }) {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::error!(error = %error, "Video render failed");
                // Encoder с egui paint jobs отбрасывается вместе с failed frame.
                self.egui_compositor
                    .free_retired_textures_if_any(egui_textures_delta);
                return RenderFrameOutcome::Failed(RenderFrameFailure::new(error.to_string()));
            }
        };
        let video_render_elapsed = stage_started_at.elapsed();

        // Субтитры: поверх видео, под egui; запись pass-а целиком в subtitle_overlay.
        let stage_started_at = Instant::now();
        self.subtitle_overlay_renderer.render(
            SubtitleOverlayTarget {
                device: &self.gpu.device,
                queue: &self.gpu.queue,
                encoder: &mut encoder,
                target: &surface_view,
                screen_size: screen.size_in_pixels,
                visible_area: clamped_video_viewport,
            },
            subtitle_overlay.as_ref(),
        );
        let subtitle_overlay_elapsed = stage_started_at.elapsed();

        // Рендерим egui поверх видео: удержанные примитивы — свежие или прошлого кадра.
        let stage_started_at = Instant::now();
        self.egui_compositor
            .render_retained_overlay(&mut encoder, &surface_view);
        let egui_render_elapsed = stage_started_at.elapsed();

        // Маска идёт строго последней: она одинаково обрезает video, egui и hover surfaces.
        let stage_started_at = Instant::now();
        if let Some(mask_renderer) = &self.window_corner_mask_renderer {
            mask_renderer.render(
                &self.gpu.queue,
                &mut encoder,
                &surface_view,
                [
                    self.gpu.surface_config.width,
                    self.gpu.surface_config.height,
                ],
                screen.pixels_per_point,
                window_corner_mask,
            );
        }
        let window_corner_mask_elapsed = stage_started_at.elapsed();

        // Отправляем команды на GPU. Порядок внутри одного submit-а задаёт
        // очередность исполнения: загрузка host-planar кадра в текстуры → acquire
        // DMA-BUF кадра → egui callbacks → основной encoder (video + overlay + egui) →
        // release DMA-BUF кадра декодеру.
        let stage_started_at = Instant::now();
        let video_host_upload = video_render_outcome
            .host_upload
            .map(HostPlanarUploadCommands::into_command_buffer);
        let (video_acquire, video_release) = match video_render_outcome.external_sync {
            Some(sync) => (Some(sync.acquire), Some(sync.release)),
            None => (None, None),
        };
        self.gpu.queue.submit(
            video_host_upload
                .into_iter()
                .chain(video_acquire)
                .chain(egui_callback_command_buffers)
                .chain(std::iter::once(encoder.finish()))
                .chain(video_release),
        );
        // После submit-а wgpu самостоятельно удерживает ресурсы до завершения GPU work.
        // Теперь retired-текстуры можно безопасно уничтожить, не повреждая текущий кадр.
        self.egui_compositor
            .free_retired_textures_if_any(egui_textures_delta);
        let queue_submit_elapsed = stage_started_at.elapsed();

        // Продвигаем wgpu callbacks для submitted work.
        // Zero-copy imports теперь живут в bounded persistent pool, поэтому poll
        // больше не является основным механизмом выживания resource cleanup-а.
        let stage_started_at = Instant::now();
        if let Err(error) = self.gpu.device.poll(wgpu::PollType::Poll) {
            tracing::warn!(error = %error, "wgpu device poll resulted in an error during the GPU callback polling");
        }
        let device_poll_elapsed = stage_started_at.elapsed();

        // Сообщаем winit, что сейчас будет present: это помогает backend/compositor timing.
        let stage_started_at = Instant::now();
        window.pre_present_notify();
        let pre_present_notify_elapsed = stage_started_at.elapsed();

        // Показываем кадр на экране.
        let stage_started_at = Instant::now();
        // wgpu 30: `SurfaceTexture::present()` заменён на `Queue::present`; замер
        // покрывает тот же участок — только сам вызов present.
        self.gpu.queue.present(surface_texture);
        let surface_present_elapsed = stage_started_at.elapsed();

        let stages = RenderFrameStageTimings {
            egui_texture_update: egui_texture_update_elapsed,
            encoder_creation: encoder_creation_elapsed,
            egui_buffer_update: egui_buffer_update_elapsed,
            surface_acquire: surface_acquire_elapsed,
            surface_view_creation: surface_view_creation_elapsed,
            video_render: video_render_elapsed,
            subtitle_overlay: subtitle_overlay_elapsed,
            egui_render: egui_render_elapsed,
            window_corner_mask: window_corner_mask_elapsed,
            queue_submit: queue_submit_elapsed,
            device_poll: device_poll_elapsed,
            pre_present_notify: pre_present_notify_elapsed,
            surface_present: surface_present_elapsed,
        };
        RenderFrameOutcome::Presented(RenderFrameTiming::new(
            stages,
            surface_acquire_elapsed + renderer_started_at.elapsed(),
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
