//! Кадр: начало (`begin_frame`), рисование, показ (`present`).
//!
//! Тайминги стадий названы как поля `render_shell_api::RenderFrameStageTimings`, чтобы трассы
//! `render_frame_timing` wgpu и GLES2 сравнивались по одним именам:
//! - `surface_acquire` — `eglMakeCurrent` + привязка цели (у EGL задний буфер берётся неявно);
//! - `surface_present` — `eglSwapBuffers` (у offscreen-цели — `glFlush`).

use std::time::{Duration, Instant};

use crate::context::driver::SurfaceSize;
use crate::context::{ContextId, Gles2Context};
use crate::error::Gles2Error;

/// Тайминги стадий кадра.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Gles2FrameTimings {
    /// Привязка контекста и цели.
    pub surface_acquire: Duration,
    /// Показ (`eglSwapBuffers`).
    pub surface_present: Duration,
}

/// Почему кадр не начат.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameSkipReason {
    /// Окно отпущено (`detach_presentation`) — рисовать некуда.
    PresentationDetached,
    /// Окно свёрнуто до 0×0.
    ZeroSize,
}

/// Итог `begin_frame`.
#[derive(Debug)]
pub enum FrameStart<'context> {
    /// Кадр начат: цель привязана, viewport на весь размер.
    Ready(ActiveFrame<'context>),
    /// Кадра не будет.
    Skipped(FrameSkipReason),
}

/// Показанный кадр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentedFrame {
    /// Тайминги стадий.
    pub timings: Gles2FrameTimings,
}

/// Начатый кадр. Пока он жив, контекст занят им (`&mut`). Drop без [`Self::present`] —
/// кадр выброшен: показа нет, нарисованное останется в заднем буфере до следующего кадра.
#[derive(Debug)]
pub struct ActiveFrame<'context> {
    context: &'context mut Gles2Context,
    size: SurfaceSize,
    surface_acquire: Duration,
}

impl<'context> ActiveFrame<'context> {
    pub(crate) fn new(
        context: &'context mut Gles2Context,
        size: SurfaceSize,
        acquire_started_at: Instant,
    ) -> Self {
        Self {
            context,
            size,
            surface_acquire: acquire_started_at.elapsed(),
        }
    }

    /// Размер цели в пикселях.
    pub fn size(&self) -> SurfaceSize {
        self.size
    }

    /// Идентичность контекста, в котором идёт кадр.
    pub fn context_id(&self) -> ContextId {
        self.context.id()
    }

    /// Таблица функций GL. Контекст текущий, цель привязана.
    pub fn gl(&self) -> &glow::Context {
        self.context.gl_for_active_frame()
    }

    /// Залить цель цветом (straight RGBA в диапазоне 0..=1).
    pub fn clear(&self, rgba: [f32; 4]) {
        self.context.clear_for_active_frame(rgba);
    }

    /// Показать кадр.
    pub fn present(self) -> Result<PresentedFrame, Gles2Error> {
        let surface_present = self.context.present_active_frame()?;
        Ok(PresentedFrame {
            timings: Gles2FrameTimings {
                surface_acquire: self.surface_acquire,
                surface_present,
            },
        })
    }
}
