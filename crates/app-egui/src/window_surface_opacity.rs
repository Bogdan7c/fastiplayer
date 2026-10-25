//! Подсказка композитору «поверхность окна непрозрачна» (сессия 10 плана gles2-renderer).
//!
//! Окно создаётся прозрачным (`with_transparent(true)`): у обычного окна скруглённые углы,
//! и вне скругления рендер пишет альфу 0. Но композитор не знает, что в фуллскрине и
//! развёрнутом окне углы квадратные (`window_corner_policy`) и каждый пиксель кадра имеет
//! альфу 1 (фон кадра чёрный непрозрачный, вырез углов не рисуется). Для прозрачной
//! поверхности winit на Wayland снимает opaque region, и KWin обязан на каждом кадре
//! перерисовывать всё, что лежит под окном, и не может отдать полноэкранное окно на
//! плоскость дисплея напрямую (direct scanout). Замер сессии 10: композитор тратил на
//! Fastiplayer заметно больше CPU, чем на mpv, у которого поверхность непрозрачна.
//!
//! Модуль владеет последней объявленной подсказкой и меняет её только при смене формы
//! кадра, а не на каждом кадре. На X11 winit умеет задать прозрачность только при создании
//! окна, поэтому там вызов — no-op библиотеки (ограничение winit, а не наше решение).

use render_shell_api::WindowCornerMask;
use winit::window::Window;

/// Что композитор должен считать про альфу поверхности.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceOpacityHint {
    /// Каждый пиксель кадра непрозрачен: композитор может не рисовать окна под нами.
    Opaque,
    /// В кадре есть прозрачные пиксели (скруглённые углы): композитор смешивает с фоном.
    Translucent,
}

impl SurfaceOpacityHint {
    /// Подсказка для формы кадра: непрозрачен только кадр без скругления углов.
    pub(crate) fn for_corner_mask(mask: WindowCornerMask) -> Self {
        if mask.radius_points() > 0.0 {
            Self::Translucent
        } else {
            Self::Opaque
        }
    }
}

/// Окно, которому можно сообщить подсказку о прозрачности.
pub(crate) trait SurfaceOpacityPort {
    /// Передаёт подсказку композитору (на Wayland — opaque region поверхности).
    fn declare_surface_opacity(&self, hint: SurfaceOpacityHint);
}

impl SurfaceOpacityPort for Window {
    fn declare_surface_opacity(&self, hint: SurfaceOpacityHint) {
        // Регион применяется с ближайшим commit поверхности, то есть вместе с кадром,
        // который уже нарисован в новой форме.
        self.set_transparent(hint == SurfaceOpacityHint::Translucent);
    }
}

/// Итог [`SurfaceOpacityDeclaration::sync`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceOpacitySync {
    /// Подсказка изменилась и передана окну.
    Declared(SurfaceOpacityHint),
    /// Форма кадра прежняя: окно не трогали.
    Unchanged,
}

/// Последняя подсказка, объявленная окну.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SurfaceOpacityDeclaration {
    declared: SurfaceOpacityHint,
}

impl SurfaceOpacityDeclaration {
    /// Состояние только что созданного окна: оно построено с `with_transparent(true)`.
    pub(crate) const fn for_transparent_window() -> Self {
        Self {
            declared: SurfaceOpacityHint::Translucent,
        }
    }

    /// Приводит подсказку окна к форме кадра; окно вызывается только при изменении.
    pub(crate) fn sync(
        &mut self,
        mask: WindowCornerMask,
        window: &impl SurfaceOpacityPort,
    ) -> SurfaceOpacitySync {
        let hint = SurfaceOpacityHint::for_corner_mask(mask);
        if hint == self.declared {
            return SurfaceOpacitySync::Unchanged;
        }
        window.declare_surface_opacity(hint);
        self.declared = hint;
        SurfaceOpacitySync::Declared(hint)
    }
}

#[cfg(test)]
mod tests;
