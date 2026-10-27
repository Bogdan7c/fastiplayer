//! Кадр: начало (`begin_frame`), рисование, показ (`present`).
//!
//! Тайминги стадий названы как поля `render_shell_api::RenderFrameStageTimings`, чтобы трассы
//! `render_frame_timing` wgpu и GLES2 сравнивались по одним именам:
//! - `surface_acquire` — `eglMakeCurrent` + привязка цели (у EGL задний буфер берётся неявно);
//! - `surface_present` — `eglSwapBuffers` (у offscreen-цели — `glFlush`).

use std::time::{Duration, Instant};

use crate::context::driver::SurfaceSize;
use crate::context::{ContextId, Gles2Context};
use crate::damage::{BufferAge, PixelRect, TargetArea};
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

/// Как кадр показан.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PresentKind {
    /// `eglSwapBuffers`: композитор считает изменённой всю surface.
    Plain,
    /// `eglSwapBuffersWithDamage{KHR,EXT}`: композитору сообщён изменённый прямоугольник.
    WithDamage,
    /// Offscreen-цель: `glFlush` и поворот кольца задних буферов, показа композитору нет.
    OffscreenFlush,
}

/// Показанный кадр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentedFrame {
    /// Тайминги стадий.
    pub timings: Gles2FrameTimings,
    /// Способ показа.
    pub kind: PresentKind,
}

/// Начатый кадр. Пока он жив, контекст занят им (`&mut`). Drop без [`Self::present`] —
/// кадр выброшен: показа нет, нарисованное останется в заднем буфере до следующего кадра.
///
/// # Инвариант области перерисовки
/// Кадр может быть ограничен областью ([`Self::repaint_area`]): собирающий кадр
/// (`FrameCompositor`) выбрал частичную перерисовку, потому что остальное содержимое заднего
/// буфера уже верно. Каждый слой, рисующий в кадр (художник egui, видео, субтитры), обязан не
/// менять пиксели вне этой области: либо отсекать свои прямоугольники через
/// [`Self::clip_to_repaint`], либо не рисовать вовсе. Нарушение портит пиксели, которые никто
/// не перерисует.
#[derive(Debug)]
pub struct ActiveFrame<'context> {
    context: &'context mut Gles2Context,
    size: SurfaceSize,
    surface_acquire: Duration,
    buffer_age: BufferAge,
    repaint_area: TargetArea,
    changed_area: TargetArea,
}

impl<'context> ActiveFrame<'context> {
    pub(crate) fn new(
        context: &'context mut Gles2Context,
        size: SurfaceSize,
        acquire_started_at: Instant,
        buffer_age: BufferAge,
    ) -> Self {
        Self {
            context,
            size,
            surface_acquire: acquire_started_at.elapsed(),
            buffer_age,
            repaint_area: TargetArea::Whole,
            changed_area: TargetArea::Whole,
        }
    }

    /// Возраст заднего буфера, запрошенный при начале кадра (до рисования).
    pub fn buffer_age(&self) -> BufferAge {
        self.buffer_age
    }

    /// Ограничить кадр: рисовать разрешено только в `repaint`, а композитору сообщается, что
    /// изменилось `changed`. Зовёт сборщик кадра один раз до слоёв.
    pub fn restrict_to_partial_repaint(&mut self, repaint: TargetArea, changed: TargetArea) {
        self.repaint_area = repaint;
        self.changed_area = changed;
    }

    /// Область, в которую слоям разрешено рисовать (см. инвариант на [`ActiveFrame`]).
    pub fn repaint_area(&self) -> TargetArea {
        self.repaint_area
    }

    /// Часть `rect`, которую слою разрешено рисовать в этом кадре; `None` — слою здесь нечего
    /// делать. Прямоугольник в пикселях цели, начало слева сверху.
    pub fn clip_to_repaint(&self, rect: PixelRect) -> Option<PixelRect> {
        self.repaint_area.clip(rect)
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

    /// Залить цель цветом (straight RGBA в диапазоне 0..=1). В частично перерисовываемом кадре
    /// заливается только область перерисовки (инвариант на [`ActiveFrame`]).
    pub fn clear(&self, rgba: [f32; 4]) {
        match self.repaint_area {
            TargetArea::Whole => self.context.clear_for_active_frame(rgba),
            TargetArea::Nothing => {}
            TargetArea::Region(rect) => {
                // Прямоугольник EGL/GL отсчитывается снизу: переворот по высоте цели.
                let to_int = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
                let bottom_gap = self
                    .size
                    .height()
                    .saturating_sub(rect.y.saturating_add(rect.height));
                self.context.clear_region_for_active_frame(
                    rgba,
                    (
                        to_int(rect.x),
                        to_int(bottom_gap),
                        to_int(rect.width),
                        to_int(rect.height),
                    ),
                );
            }
        }
    }

    /// Показать кадр.
    ///
    /// Если кадр ограничен ([`Self::restrict_to_partial_repaint`]), композитору уходит
    /// изменённая область кадра (damage-swap при поддержке расширения); иначе — обычный показ.
    pub fn present(self) -> Result<PresentedFrame, Gles2Error> {
        let (surface_present, kind) = self
            .context
            .present_active_frame(self.size, self.changed_area)?;
        Ok(PresentedFrame {
            timings: Gles2FrameTimings {
                surface_acquire: self.surface_acquire,
                surface_present,
            },
            kind,
        })
    }
}
