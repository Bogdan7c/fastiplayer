//! Один surface transaction: acquire → app video preparation → draw/submit/present.
//! Exclusive borrow запрещает resize/reconfigure и второй acquire до завершения кадра.

use super::*;

/// Полученная поверхность и ещё не отправленные UI-команды одного кадра.
/// Drop без render освобождает retired UI textures и отбрасывает surface без present.
/// После отказа caller восстанавливает surface прежним lifecycle API: Vulkan backend
/// wgpu 29 не возвращает discarded image в swapchain до reconfigure.
#[must_use = "полученный кадр нужно отрисовать либо явно отбросить"]
pub struct AcquiredRenderFrame<'renderer, 'window> {
    renderer: &'renderer mut Renderer,
    window: &'window Window,
    surface_texture: Option<wgpu::SurfaceTexture>,
    encoder: Option<wgpu::CommandEncoder>,
    egui_callback_command_buffers: Vec<wgpu::CommandBuffer>,
    egui_paint_jobs: Vec<egui::epaint::ClippedPrimitive>,
    egui_textures_delta: egui::TexturesDelta,
    screen_descriptor: egui_wgpu::ScreenDescriptor,
    clamped_video_viewport: render_core::RenderViewport,
    clamped_video_exclusion_rects: Vec<render_core::RenderViewport>,
    window_corner_mask: crate::WindowCornerMask,
    renderer_started_at: Instant,
    app_preparation_started_at: Instant,
    stages: RenderFrameStageTimings,
}

impl Renderer {
    /// Подготавливает UI и получает поверхность до выбора app-owned video input.
    /// Ошибки acquisition сохраняют прежние drop/recovery причины; video lease не запрашивается.
    pub fn acquire_frame<'renderer, 'window>(
        &'renderer mut self,
        input: RenderFrameInput<'window>,
    ) -> std::result::Result<AcquiredRenderFrame<'renderer, 'window>, RenderFrameDropReason> {
        let renderer_started_at = Instant::now();
        let RenderFrameInput {
            window,
            egui_paint_jobs,
            egui_textures_delta,
            screen,
            video_viewport,
            video_exclusion_rects,
            window_corner_mask,
        } = input;
        // Window::inner_size может измениться до доставки Resized, особенно в XWayland.
        // Surface должна соответствовать размеру подготовленного UI до acquire:
        // иначе egui scissor выходит за старый attachment при fullscreen переходе.
        if [
            self.gpu.surface_config.width,
            self.gpu.surface_config.height,
        ] != screen.size_in_pixels
        {
            self.gpu
                .resize(screen.size_in_pixels[0], screen.size_in_pixels[1]);
        }
        let clamped_video_viewport = clamp_video_viewport_to_screen(video_viewport, &screen);
        let clamped_video_exclusion_rects =
            clamp_video_exclusion_rects_to_screen(video_exclusion_rects, &screen);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: screen.size_in_pixels,
            pixels_per_point: screen.pixels_per_point,
        };

        // Загружаем новые/изменённые egui текстуры.
        // Retired-текстуры остаются живы до submit-а текущих paint jobs.
        let stage_started_at = Instant::now();
        self.egui_compositor.upload_changed_textures(
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
        let egui_callback_command_buffers = self.egui_compositor.update_buffers(
            &self.gpu.device,
            &self.gpu.queue,
            &mut encoder,
            &egui_paint_jobs,
            &screen_descriptor,
        );
        let egui_buffer_update_elapsed = stage_started_at.elapsed();

        // Получаем surface texture для текущего кадра
        // Отдельный opt-in target позволяет связать ожидание поверхности с
        // выбором lease в app, не включая общий высокочастотный GPU debug log.
        tracing::trace!(target: "fastiplayer::frame_cadence", "surface acquire started");
        let stage_started_at = Instant::now();
        let surface_acquire_started_at = stage_started_at;
        let surface_texture_result = match self.gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => Ok(frame),
            wgpu::CurrentSurfaceTexture::Outdated => {
                // Surface был уничтожен и воссоздан (например, при смене монитора)
                self.gpu
                    .surface
                    .configure(&self.gpu.device, &self.gpu.surface_config);
                match self.gpu.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(frame)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => Ok(frame),
                    other => {
                        tracing::error!(
                            "Не удалось получить surface texture после reconfigure: {:?}",
                            other
                        );
                        Err(dropped_frame_after_surface_acquire(
                            RenderFrameDropReason::SurfaceOutdatedRecoveryFailed,
                            renderer_started_at,
                            surface_acquire_started_at,
                        ))
                    }
                }
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                // Таймаут — пропускаем кадр, не блокируем
                Err(dropped_frame_after_surface_acquire(
                    RenderFrameDropReason::SurfaceTimeout,
                    renderer_started_at,
                    surface_acquire_started_at,
                ))
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                // Окно скрыто другим окном — пропускаем кадр
                tracing::debug!("Surface occluded — skipping frame");
                Err(dropped_frame_after_surface_acquire(
                    RenderFrameDropReason::SurfaceOccluded,
                    renderer_started_at,
                    surface_acquire_started_at,
                ))
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                // Surface потерян: сначала пробуем штатный reconfigure текущей surface.
                // Если драйвер не восстановит surface, следующий redraw снова попадёт
                // сюда, и внешний lifecycle сможет пересоздать runtime через resumed/suspend.
                tracing::warn!("Surface lost — пробуем reconfigure");
                self.gpu
                    .surface
                    .configure(&self.gpu.device, &self.gpu.surface_config);
                Err(dropped_frame_after_surface_acquire(
                    RenderFrameDropReason::SurfaceLost,
                    renderer_started_at,
                    surface_acquire_started_at,
                ))
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                // Validation error при получении surface texture — пропускаем кадр
                tracing::warn!("Surface validation error — skipping frame");
                Err(dropped_frame_after_surface_acquire(
                    RenderFrameDropReason::SurfaceValidation,
                    renderer_started_at,
                    surface_acquire_started_at,
                ))
            }
        };
        // Это wall wait, а не CPU work. Завершающее событие есть и при drop:
        // анализ не должен принимать неудачное acquisition за успешный present.
        tracing::trace!(
            target: "fastiplayer::frame_cadence",
            acquired = surface_texture_result.is_ok(),
            "surface acquire finished"
        );
        let surface_texture = match surface_texture_result {
            Ok(surface_texture) => surface_texture,
            Err(dropped_frame) => {
                // Текущий encoder не будет submitted, поэтому его paint jobs больше
                // не удерживают retired-текстуры. Предыдущие submit-ы уже переданы wgpu.
                self.egui_compositor
                    .free_retired_textures(&egui_textures_delta);
                return Err(dropped_frame);
            }
        };
        let surface_acquire_elapsed = stage_started_at.elapsed();

        Ok(AcquiredRenderFrame {
            renderer: self,
            window,
            surface_texture: Some(surface_texture),
            encoder: Some(encoder),
            egui_callback_command_buffers,
            egui_paint_jobs,
            egui_textures_delta,
            screen_descriptor,
            clamped_video_viewport,
            clamped_video_exclusion_rects,
            window_corner_mask,
            renderer_started_at,
            app_preparation_started_at: Instant::now(),
            stages: RenderFrameStageTimings {
                egui_texture_update: egui_texture_update_elapsed,
                encoder_creation: encoder_creation_elapsed,
                egui_buffer_update: egui_buffer_update_elapsed,
                surface_acquire: surface_acquire_elapsed,
                ..RenderFrameStageTimings::default()
            },
        })
    }
}

impl AcquiredRenderFrame<'_, '_> {
    /// Read-only renderer API нужен app для существующего DMA-BUF fallback.
    /// Mutable access запрещён: активная surface не должна пережить reconfigure.
    #[must_use]
    pub fn renderer(&self) -> &Renderer {
        self.renderer
    }

    /// Освобождает retired atlas entries ровно один раз, включая отказ от кадра.
    fn free_retired_ui_textures(&mut self) {
        self.renderer
            .egui_compositor
            .free_retired_textures(&self.egui_textures_delta);
        // Drop страхует только незавершённую отправку; повторное освобождение не требуется.
        self.egui_textures_delta.free.clear();
    }

    /// Потребляет полученную поверхность с video input, выбранным ПОСЛЕ acquire.
    /// Lease принадлежит app; этот метод не принимает решений о decoder release.
    pub fn render(mut self, video_frame: Option<&WgpuRenderableFrame<'_>>) -> RenderFrameOutcome {
        let renderer_started_at = self.renderer_started_at;
        // App preparation имеет собственный timing; не считаем её shell work дважды.
        let app_preparation_elapsed = self.app_preparation_started_at.elapsed();
        let egui_texture_update_elapsed = self.stages.egui_texture_update;
        let encoder_creation_elapsed = self.stages.encoder_creation;
        let egui_buffer_update_elapsed = self.stages.egui_buffer_update;
        let surface_acquire_elapsed = self.stages.surface_acquire;
        let mut encoder = self
            .encoder
            .take()
            .expect("surface frame owns one unsubmitted encoder");
        let stage_started_at = Instant::now();
        let surface_view = self
            .surface_texture
            .as_ref()
            .expect("acquired surface exists until present")
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let surface_view_creation_elapsed = stage_started_at.elapsed();

        self.renderer.video_renderer.resize(
            self.renderer.gpu.surface_config.width,
            self.renderer.gpu.surface_config.height,
        );

        let stage_started_at = Instant::now();
        match self
            .renderer
            .video_renderer
            .render_or_clear(WgpuVideoRenderInput {
                frame: video_frame,
                video_viewport: self.clamped_video_viewport,
                video_exclusion_rects: &self.clamped_video_exclusion_rects,
                target: &surface_view,
                encoder: &mut encoder,
                device: &self.renderer.gpu.device,
                queue: &self.renderer.gpu.queue,
            }) {
            Ok(_video_rendered) => {}
            Err(error) => {
                tracing::error!(error = %error, "Video render failed");
                // Encoder с egui paint jobs отбрасывается вместе с failed frame.
                return RenderFrameOutcome::Failed(RenderFrameFailure::new(error.to_string()));
            }
        }
        let video_render_elapsed = stage_started_at.elapsed();

        // Рендерим egui поверх видео.
        let stage_started_at = Instant::now();
        self.renderer.egui_compositor.render_overlay(
            &mut encoder,
            &surface_view,
            &self.egui_paint_jobs,
            &self.screen_descriptor,
        );
        let egui_render_elapsed = stage_started_at.elapsed();

        // Маска идёт строго последней: она одинаково обрезает video, egui и hover surfaces.
        let stage_started_at = Instant::now();
        if let Some(mask_renderer) = &self.renderer.window_corner_mask_renderer {
            mask_renderer.render(
                &self.renderer.gpu.queue,
                &mut encoder,
                &surface_view,
                [
                    self.renderer.gpu.surface_config.width,
                    self.renderer.gpu.surface_config.height,
                ],
                self.screen_descriptor.pixels_per_point,
                self.window_corner_mask,
            );
        }
        let window_corner_mask_elapsed = stage_started_at.elapsed();

        // Отправляем команды на GPU
        let stage_started_at = Instant::now();
        self.renderer.gpu.queue.submit(
            self.egui_callback_command_buffers
                .drain(..)
                .chain(std::iter::once(encoder.finish())),
        );
        // Сохраняем прежний lifecycle: free после submit, но до poll/present.
        self.free_retired_ui_textures();
        let queue_submit_elapsed = stage_started_at.elapsed();

        // Продвигаем wgpu callbacks для submitted work.
        // Zero-copy imports теперь живут в bounded persistent pool, поэтому poll
        // больше не является основным механизмом выживания resource cleanup-а.
        let stage_started_at = Instant::now();
        if let Err(error) = self.renderer.gpu.device.poll(wgpu::PollType::Poll) {
            tracing::warn!(error = %error, "wgpu device poll завершился ошибкой во время GPU callback polling");
        }
        let device_poll_elapsed = stage_started_at.elapsed();

        // Сообщаем winit, что сейчас будет present: это помогает backend/compositor timing.
        let stage_started_at = Instant::now();
        self.window.pre_present_notify();
        let pre_present_notify_elapsed = stage_started_at.elapsed();

        // Показываем кадр на экране.
        let stage_started_at = Instant::now();
        self.surface_texture
            .take()
            .expect("surface is presented once")
            .present();
        let surface_present_elapsed = stage_started_at.elapsed();

        let stages = RenderFrameStageTimings {
            egui_texture_update: egui_texture_update_elapsed,
            encoder_creation: encoder_creation_elapsed,
            egui_buffer_update: egui_buffer_update_elapsed,
            surface_acquire: surface_acquire_elapsed,
            surface_view_creation: surface_view_creation_elapsed,
            video_render: video_render_elapsed,
            egui_render: egui_render_elapsed,
            window_corner_mask: window_corner_mask_elapsed,
            queue_submit: queue_submit_elapsed,
            device_poll: device_poll_elapsed,
            pre_present_notify: pre_present_notify_elapsed,
            surface_present: surface_present_elapsed,
        };
        RenderFrameOutcome::Presented(RenderFrameTiming::new(
            stages,
            renderer_started_at
                .elapsed()
                .saturating_sub(app_preparation_elapsed),
        ))
    }
}

impl Drop for AcquiredRenderFrame<'_, '_> {
    fn drop(&mut self) {
        // Encoder уже submitted либо будет отброшен: retired UI resources больше не
        // нужны будущей отправке. Предыдущие submit-ы удерживаются самим wgpu.
        self.free_retired_ui_textures();
    }
}
