//! Получение буфера swapchain до выбора видеокадра (cpu-parity, сессия 06a).
//!
//! Раньше `Renderer::render_frame` сам вызывал `get_current_texture`, а видеокадр app
//! выбирал ещё до входа в shell. Под FIFO получение блокируется до свободного места в
//! очереди показа (wgpu: «Calls to `Surface::get_current_texture()` will block until there is
//! a spot in the queue»), под XWayland — почти весь интервал vsync. Выбранный кадр уходил
//! на экран на ~16 мс старше, чем мог бы. Теперь app сначала вызывает
//! `RenderShell::acquire_surface_frame`, потом выбирает кадр и субтитры, потом
//! `render_frame` записывает и показывает уже полученный буфер.
//!
//! С сессии 02 плана `user/gles2-renderer/` буфер лежит в нейтральном билете
//! `AcquiredSurfaceFrame` с паспортом этого рендера (одна маленькая аллокация на кадр).

use std::time::Instant;

use render_shell_api::{AcquiredSurfaceFrame, SurfaceFrameAcquire};

use super::{Renderer, duration_ms};
use crate::RenderScreenDescriptor;

impl Renderer {
    /// Получает буфер цели показа для кадра размера `screen` (у окна может ждать FIFO).
    ///
    /// Цель приводится к размеру подготовленного UI до получения: `Window::inner_size`
    /// может измениться раньше доставки `Resized` (особенно в XWayland), а egui scissor не
    /// должен выйти за старый attachment при переходе в fullscreen.
    pub(super) fn acquire_target_frame(
        &mut self,
        screen: RenderScreenDescriptor,
    ) -> SurfaceFrameAcquire {
        if self.target.size() != screen.size_in_pixels {
            self.target.resize(
                &self.gpu.device,
                screen.size_in_pixels[0],
                screen.size_in_pixels[1],
            );
        }
        let acquire_started_at = Instant::now();
        match self.target.acquire(&self.gpu.device) {
            Ok(target_frame) => SurfaceFrameAcquire::Acquired(AcquiredSurfaceFrame::new(
                self.instance_id,
                target_frame,
                acquire_started_at.elapsed(),
            )),
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
