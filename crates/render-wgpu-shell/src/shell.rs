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
/// 1. Получаем буфер цели показа (`RenderShell::acquire_surface_frame`, до выбора
///    видеокадра app-ом — сессия 06a)
/// 2. Рендерим decoded video frame или чёрный фон
/// 3. Рендерим egui overlay поверх видео
/// 4. Present на экран
///
/// С сессии 02 плана `user/gles2-renderer/` приложение видит рендер только через
/// контракт `render_shell_api::RenderShell` (`render_shell.rs`), а импортом кадров декодера
/// владеет склад `crate::video_import`.
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use render_core::{
    ColorPipelineSettings, HdrToSdrSettings, RenderLiveApplyReport, RenderLiveSettings,
    RenderLiveSettingsAdapter, RenderLiveSettingsError, RenderLiveSettingsUpdate,
};
use render_shell_api::{RenderFrameInput, RenderableFrameKey, RendererInstanceId};
use render_wgpu_video::{
    HostPlanarUploadCommands, WgpuRenderableFrame, WgpuVideoRenderInput, WgpuVideoRenderer,
};
use tracing::{debug, info, instrument, warn};
use winit::window::Window;

use crate::egui_compositor::{EguiCompositor, EguiFrameRefusal};
use crate::frame::{clamp_video_exclusion_rects_to_screen, clamp_video_viewport_to_screen};
use crate::probe::ProbedVulkanInstance;
use crate::subtitle_overlay::{
    SubtitleOverlayRenderer, SubtitleOverlayTarget, SubtitleOverlayUploadStats,
};
use crate::video_import::{
    WgpuImportDevice, WgpuPreparedFrame, WgpuVideoFrameImport, renderable_frame_from_views,
};
use crate::{
    RenderFrameDropReason, RenderFrameFailure, RenderFrameOutcome, RenderFrameStageTimings,
    RenderFrameTiming, SurfacePresentSettings,
};

pub(crate) mod gpu_context;
mod present_target;
mod render_shell;
mod surface_acquire;

use gpu_context::{GpuContext, vulkan_instance};
use present_target::{PresentTarget, TargetFrame};

/// Превращает длительность в миллисекунды для числовых tracing fields.
fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Полный рендерер: GPU контекст + видеопайплайн + egui рендерер.
///
/// Координирует рендеринг видео и UI overlay в каждом кадре.
pub struct Renderer {
    /// Паспорт экземпляра: им помечены билеты буфера и ключи кадров этого рендера.
    instance_id: RendererInstanceId,

    /// GPU ресурсы (instance, adapter, device, queue, учёт потери устройства).
    gpu: GpuContext,

    /// Куда рисуются кадры: swapchain окна (или offscreen-текстура в тестах).
    target: PresentTarget,

    /// Композитор egui overlay поверх video pass-а.
    egui_compositor: EguiCompositor,

    /// Video renderer facade — скрывает конкретный NV12 shader/backend детали.
    video_renderer: WgpuVideoRenderer,

    /// Финальный compositor контура; отсутствует при opaque/inherit surface fallback.

    /// Субтитры между video и egui.
    subtitle_overlay_renderer: SubtitleOverlayRenderer,

    /// Склад кадров декодера: материализаторы и отложенные освобождения после GPU.
    video_import: WgpuVideoFrameImport,
}

/// Причина, по которой ключ видеокадра нельзя нарисовать этим рендером.
enum VideoFrameKeyRejection {
    /// Ключ подготовлен другим экземпляром рендера.
    Foreign,

    /// Views ключа не собираются в кадр video pass-а (сообщение для логов).
    Unrenderable(String),
}

impl Renderer {
    /// Создаёт полный рендерер.
    ///
    /// Инициализирует:
    /// - GPU контекст (async) и swapchain окна
    /// - video renderer facade
    /// - egui_wgpu renderer
    #[instrument(skip(window))]
    pub fn new(window: Arc<Window>, surface_present: SurfacePresentSettings) -> Result<Self> {
        Self::new_on_instance(window, surface_present, vulkan_instance())
    }

    /// Создаёт рендерер на Vulkan instance, оставшемся от пробы того же запуска: второй
    /// instance (загрузка loader-а и слоёв) не создаётся. Выбор адаптера, устройство и
    /// surface — те же, что у [`Self::new`].
    #[instrument(skip(window, probed_instance))]
    pub fn new_reusing_instance(
        window: Arc<Window>,
        surface_present: SurfacePresentSettings,
        probed_instance: ProbedVulkanInstance,
    ) -> Result<Self> {
        Self::new_on_instance(window, surface_present, probed_instance.into_instance())
    }

    /// Общий путь создания: instance приходит снаружи, остальное не зависит от его источника.
    fn new_on_instance(
        window: Arc<Window>,
        surface_present: SurfacePresentSettings,
        instance: wgpu::Instance,
    ) -> Result<Self> {
        let (gpu, target) = pollster::block_on(GpuContext::new_for_window(
            window,
            surface_present,
            instance,
        ))?;
        Ok(Self::from_gpu_and_target(gpu, target))
    }

    /// Собирает рендер поверх готового GPU-контекста и цели показа.
    fn from_gpu_and_target(gpu: GpuContext, target: PresentTarget) -> Self {
        let instance_id = RendererInstanceId::allocate();
        let egui_compositor = EguiCompositor::new(&gpu.device, gpu.target_format);
        let video_renderer = WgpuVideoRenderer::new(&gpu.device, gpu.target_format);

        let subtitle_overlay_renderer =
            SubtitleOverlayRenderer::new(&gpu.device, gpu.target_format);

        // Склад кадров знает возможности рендера: они неизменны до пересоздания рендера.
        let video_import = WgpuVideoFrameImport::new(
            instance_id,
            WgpuImportDevice {
                instance: gpu.instance.clone(),
                adapter: gpu.adapter.clone(),
                device: gpu.device.clone(),
                queue: gpu.queue.clone(),
            },
            video_renderer.capabilities().clone(),
        );

        info!(
            renderer_instance = instance_id.value(),
            "Renderer is fully initialized"
        );

        Self {
            instance_id,
            gpu,
            target,
            egui_compositor,
            video_renderer,
            subtitle_overlay_renderer,
            video_import,
        }
    }

    /// Рендер без окна: кадры пишутся в offscreen-текстуру `size` (conformance и пиксельные
    /// тесты, решение владельца в сессии 02). `Ok(None)` — в системе нет Vulkan-адаптера.
    #[cfg(test)]
    pub(crate) fn new_headless(size: [u32; 2]) -> Result<Option<Self>> {
        use present_target::OffscreenPresentTarget;

        /// 8-битный линейный формат: пиксели readback-а сравниваются без sRGB-кривой.
        const HEADLESS_TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

        let Some(gpu) = pollster::block_on(GpuContext::new_headless(HEADLESS_TARGET_FORMAT))?
        else {
            return Ok(None);
        };
        let target = PresentTarget::Offscreen(OffscreenPresentTarget::new(
            &gpu.device,
            size,
            gpu.target_format,
        ));
        Ok(Some(Self::from_gpu_and_target(gpu, target)))
    }

    /// Пиксель RGBA8 последнего кадра offscreen-цели (только рендер без окна).
    #[cfg(test)]
    pub(crate) fn read_offscreen_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        let offscreen = self.target.offscreen()?;
        Some(crate::test_support::read_texture_pixel(
            &self.gpu.device,
            &self.gpu.queue,
            offscreen.texture(),
            [x, y],
        ))
    }

    /// Весь последний кадр offscreen-цели, RGBA8 построчно (только рендер без окна).
    #[cfg(test)]
    pub(crate) fn read_offscreen_frame(&self) -> Option<crate::test_support::RgbaFrame> {
        let offscreen = self.target.offscreen()?;
        Some(crate::test_support::read_texture_frame(
            &self.gpu.device,
            &self.gpu.queue,
            offscreen.texture(),
        ))
    }

    /// Счётчики загрузок субтитров в GPU (диагностика и приёмочные тесты).
    #[must_use]
    pub fn subtitle_overlay_upload_stats(&self) -> SubtitleOverlayUploadStats {
        self.subtitle_overlay_renderer.upload_stats()
    }

    /// Задаёт стартовые SDR color settings (до первого кадра, без проверки live-контракта).
    ///
    /// Вызывает фабрика каталога рендеров приложения: это прежнее стартовое поведение
    /// (`set_*`), а не `commit_live_settings`, который отклоняет неподдержанные поля.
    pub fn set_color_pipeline_settings(&mut self, settings: ColorPipelineSettings) {
        self.video_renderer.set_color_pipeline_settings(settings);
    }

    /// Задаёт стартовые HDR-to-SDR settings (см. [`Self::set_color_pipeline_settings`]).
    pub fn set_hdr_to_sdr_settings(&mut self, settings: HdrToSdrSettings) {
        self.video_renderer.set_hdr_to_sdr_settings(settings);
    }

    /// Открывает подготовленные ресурсы ключа и собирает кадр video pass-а.
    fn renderable_frame_for_key<'key>(
        &self,
        key: &'key RenderableFrameKey,
    ) -> Result<WgpuRenderableFrame<'key>, VideoFrameKeyRejection> {
        let prepared = key
            .prepared_for::<WgpuPreparedFrame>(self.instance_id)
            .ok_or(VideoFrameKeyRejection::Foreign)?;
        renderable_frame_from_views(key.decoded_frame(), &prepared.views)
            .map_err(|error| VideoFrameKeyRejection::Unrenderable(error.to_string()))
    }

    /// Рендерит один полный кадр: видео + egui overlay + контур desktop-окна.
    ///
    /// Последовательность:
    /// 1. Загружаем изменения egui-текстур (сессия 02a: это состояние рендера, оно применяется
    ///    даже в отклонённом кадре; `free` отклонённого кадра — сразу)
    /// 2. Открываем билет буфера и ключ видеокадра (чужие — `Failed` без записи)
    /// 3. Проверяем, можно ли рисовать интерфейс (рассинхронизированные текстуры, удержанный
    ///    кадр для повтора), и обновляем egui buffers
    /// 4. Рендерим video frame через backend facade или очищаем target в уже полученный
    ///    буфер (`input.surface`, см. `acquire_surface_frame`)
    /// 5. Рендерим egui поверх видео
    /// 6. Последним pass-ом применяем прозрачный контур окна
    /// 7. Submit и present
    ///
    /// `renderer_elapsed` итогового timing-а = ожидание буфера + эта функция; выбор
    /// видеокадра app-ом между ними сюда не входит.
    fn record_and_present_frame(&mut self, input: RenderFrameInput<'_>) -> RenderFrameOutcome {
        let renderer_started_at = Instant::now();
        let RenderFrameInput {
            surface,
            video_frame,
            egui,
            screen,
            video_viewport,
            video_exclusion_rects,
            subtitle_overlay,
        } = input;

        // Текстуры загружаются первыми: приложение второй раз эти изменения не пришлёт, и
        // отказ кадра ниже не должен развести набор текстур рендера и egui.
        let stage_started_at = Instant::now();
        let applied_egui =
            self.egui_compositor
                .apply_frame_textures(&self.gpu.device, &self.gpu.queue, egui);
        let egui_texture_update_elapsed = stage_started_at.elapsed();

        // Билет открывает только выдавший его рендер; чужой буфер роняется внутри
        // `into_buffer` и возвращается своему swapchain-у.
        let Ok(opened_surface) = surface.into_buffer::<TargetFrame>(self.instance_id) else {
            self.egui_compositor.discard_applied_content(applied_egui);
            return RenderFrameOutcome::Failed(RenderFrameFailure::foreign_surface_frame());
        };
        // Размер цели уже приведён к `screen` в `acquire_surface_frame`.
        let target_frame = opened_surface.buffer;
        let surface_acquire_elapsed = opened_surface.acquire_elapsed;
        let video_frame = match video_frame.map(|key| self.renderable_frame_for_key(key)) {
            None => None,
            Some(Ok(video_frame)) => Some(video_frame),
            Some(Err(VideoFrameKeyRejection::Foreign)) => {
                self.egui_compositor.discard_applied_content(applied_egui);
                return RenderFrameOutcome::Failed(RenderFrameFailure::foreign_video_frame());
            }
            Some(Err(VideoFrameKeyRejection::Unrenderable(message))) => {
                tracing::error!(error = %message, "Failed to build WGPU renderable frame");
                self.egui_compositor.discard_applied_content(applied_egui);
                return RenderFrameOutcome::Failed(RenderFrameFailure::new(message));
            }
        };
        let clamped_video_viewport = clamp_video_viewport_to_screen(video_viewport, &screen);
        let clamped_video_exclusion_rects =
            clamp_video_exclusion_rects_to_screen(video_exclusion_rects, &screen);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: screen.size_in_pixels,
            pixels_per_point: screen.pixels_per_point,
        };

        // Свежий кадр с рассинхронизированными текстурами и повтор без удержанных примитивов
        // того же размера отклоняются до записи (буфер swapchain вернётся без показа при
        // drop-е): app пришлёт полный набор текстур / повторит кадр полным egui-проходом.
        let pending_egui = match self
            .egui_compositor
            .pending_content_for_drawing(applied_egui, &screen_descriptor)
        {
            Ok(pending_egui) => pending_egui,
            Err(EguiFrameRefusal::TexturesOutOfSync { texture_id }) => {
                warn!(
                    ?texture_id,
                    "UI texture edit for a texture the renderer does not have; the app must send the full UI texture set"
                );
                return RenderFrameOutcome::Dropped(RenderFrameDropReason::UiTexturesOutOfSync);
            }
            Err(EguiFrameRefusal::RetainedUnavailable(reason)) => {
                debug!(
                    ?reason,
                    "Retained egui frame unavailable; the app must redraw with a fresh UI pass"
                );
                return RenderFrameOutcome::Dropped(RenderFrameDropReason::RetainedUiUnavailable);
            }
        };

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
        let surface_view = target_frame
            .texture()
            .create_view(&wgpu::TextureViewDescriptor::default());
        let surface_view_creation_elapsed = stage_started_at.elapsed();

        let [target_width, target_height] = self.target.size();
        self.video_renderer.resize(target_width, target_height);

        let stage_started_at = Instant::now();
        // Барьеры DMA-BUF кадра (если image переиспользуется) отправляются вокруг
        // основного command buffer-а, а GPU-копия плоскостей host-planar кадра — перед
        // ним; renderer их отдал, shell обязан их submit-ить в том же submit-е.
        let video_render_outcome = match self.video_renderer.render_or_clear(WgpuVideoRenderInput {
            frame: video_frame.as_ref(),
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
        self.target.pre_present_notify();
        let pre_present_notify_elapsed = stage_started_at.elapsed();

        // Показываем кадр на экране.
        let stage_started_at = Instant::now();
        // wgpu 30: `SurfaceTexture::present()` заменён на `Queue::present`; замер
        // покрывает тот же участок — только сам вызов present.
        PresentTarget::present(&self.gpu.queue, target_frame);
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
