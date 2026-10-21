//! Получение буфера swapchain до выбора видеокадра (cpu-parity, сессия 06a).
//!
//! Раньше `Renderer::render_frame` сам вызывал `get_current_texture`, а видеокадр app
//! выбирал ещё до входа в shell. Под FIFO получение блокируется до свободного места в
//! очереди показа (wgpu: «Calls to `Surface::get_current_texture()` will block until there is
//! a spot in the queue»), под XWayland — почти весь интервал vsync. Выбранный кадр уходил
//! на экран на ~16 мс старше, чем мог бы. Теперь app сначала вызывает
//! [`Renderer::acquire_surface_frame`], потом выбирает кадр и субтитры, потом
//! `render_frame` записывает и показывает уже полученный буфер.

use std::time::Instant;

use super::{GpuContext, Renderer, duration_ms};
use crate::frame::{
    AcquiredSurfaceFrame, RenderFrameDropReason, RenderScreenDescriptor, SurfaceFrameAcquire,
};

impl Renderer {
    /// Получает буфер swapchain для кадра размера `screen` (может ждать FIFO).
    ///
    /// Surface приводится к размеру подготовленного UI до получения: `Window::inner_size`
    /// может измениться раньше доставки `Resized` (особенно в XWayland), а egui scissor не
    /// должен выйти за старый attachment при переходе в fullscreen.
    pub fn acquire_surface_frame(&mut self, screen: RenderScreenDescriptor) -> SurfaceFrameAcquire {
        if [
            self.gpu.surface_config.width,
            self.gpu.surface_config.height,
        ] != screen.size_in_pixels
        {
            self.gpu
                .resize(screen.size_in_pixels[0], screen.size_in_pixels[1]);
        }
        let acquire_started_at = Instant::now();
        match self.gpu.acquire_current_texture() {
            Ok(surface_texture) => SurfaceFrameAcquire::Acquired(AcquiredSurfaceFrame {
                surface_texture,
                acquire_elapsed: acquire_started_at.elapsed(),
            }),
            Err(reason) => {
                tracing::debug!(
                    reason = ?reason,
                    surface_acquire_ms = duration_ms(acquire_started_at.elapsed()),
                    "render frame dropped before present"
                );
                SurfaceFrameAcquire::Dropped(reason)
            }
        }
    }
}

impl GpuContext {
    /// Один шаг получения текстуры swapchain с восстановлением устаревшей/потерянной surface.
    fn acquire_current_texture(&mut self) -> Result<wgpu::SurfaceTexture, RenderFrameDropReason> {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => Ok(frame),
            wgpu::CurrentSurfaceTexture::Outdated => {
                // Surface был уничтожен и воссоздан (например, при смене монитора).
                self.surface.configure(&self.device, &self.surface_config);
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(frame)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => Ok(frame),
                    other => {
                        tracing::error!(
                            "Failed to obtain surface texture afterward reconfigure: {:?}",
                            other
                        );
                        Err(RenderFrameDropReason::SurfaceOutdatedRecoveryFailed)
                    }
                }
            }
            // Таймаут — пропускаем кадр, не блокируем.
            wgpu::CurrentSurfaceTexture::Timeout => Err(RenderFrameDropReason::SurfaceTimeout),
            wgpu::CurrentSurfaceTexture::Occluded => {
                // Окно скрыто другим окном — пропускаем кадр.
                tracing::debug!("Surface occluded — skipping frame");
                Err(RenderFrameDropReason::SurfaceOccluded)
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                // Surface потерян: сначала пробуем штатный reconfigure текущей surface.
                // Если драйвер не восстановит surface, следующий redraw снова попадёт
                // сюда, и внешний lifecycle сможет пересоздать runtime через resumed/suspend.
                tracing::warn!("Surface lost — try out reconfigure");
                self.surface.configure(&self.device, &self.surface_config);
                Err(RenderFrameDropReason::SurfaceLost)
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                // Validation error при получении surface texture — пропускаем кадр.
                tracing::warn!("Surface validation error — skipping frame");
                Err(RenderFrameDropReason::SurfaceValidation)
            }
        }
    }
}
