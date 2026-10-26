//! Подсказка композитору «поверхность окна непрозрачна» (сессия 10 плана gles2-renderer).
//!
//! Окно создаётся прозрачным (`with_transparent(true)`): у обычного окна скруглённые углы,
//! и вне скругления рендер пишет альфу 0. Но композитор не знает, что в фуллскрине и
//! развёрнутом окне углы квадратные (`window_corner_policy`) и каждый пиксель кадра имеет
//! альфу 1 (фон кадра чёрный непрозрачный, вырез углов не рисуется). Для прозрачной
//! поверхности композитор обязан на каждом кадре перерисовывать всё, что лежит под окном,
//! и не может отдать полноэкранное окно на плоскость дисплея напрямую (direct scanout).
//! Замер сессии 10: композитор тратил на Fastiplayer заметно больше CPU, чем на mpv, у
//! которого поверхность непрозрачна.
//!
//! Модуль разделён на политику и платформенные адаптеры:
//! - политика ([`SurfaceOpacityDeclaration`]) решает, *когда* и *что* объявить окну, и ничего
//!   не знает о winit/X11 — она тестируется поддельным окном через [`SurfaceOpacityPort`];
//! - `native_window` — владелец для `AppState`: выбирает платформенный путь по
//!   `raw_window_handle` и один раз пишет в лог отказ платформы;
//! - `x11_opaque_region` — X11-путь через EWMH `_NET_WM_OPAQUE_REGION`.
//!
//! Платформы различаются тем, кто следит за размером непрозрачного региона:
//! - Wayland: winit сам ставит opaque region на всю поверхность и пересобирает его при каждом
//!   resize (`set_transparent(false)`), поэтому политика зовёт окно только при смене формы;
//! - X11: winit умеет задать прозрачность только при создании окна (32-битный ARGB visual),
//!   `set_transparent` после создания — no-op. Регион объявляется свойством окна в пикселях
//!   окна, то есть это снимок размера: при смене размера в непрозрачном состоянии его надо
//!   объявить заново.

use render_shell_api::WindowCornerMask;

mod native_window;
#[cfg(target_os = "linux")]
mod x11_opaque_region;

pub(crate) use native_window::WindowSurfaceOpacity;

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

/// Размер клиентской области окна в физических пикселях (координаты окна X11 / буфера).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SurfacePixelSize {
    /// Ширина в пикселях.
    pub(crate) width: u32,
    /// Высота в пикселях.
    pub(crate) height: u32,
}

impl From<winit::dpi::PhysicalSize<u32>> for SurfacePixelSize {
    fn from(size: winit::dpi::PhysicalSize<u32>) -> Self {
        Self {
            width: size.width,
            height: size.height,
        }
    }
}

/// Кто держит непрозрачный регион в соответствии с текущим размером окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpaqueRegionSizing {
    /// Оконная система сама растягивает регион за окном (Wayland: winit при каждом resize).
    FollowsWindow,
    /// Регион — прямоугольник фиксированного размера (X11 EWMH): после resize объявлять заново.
    FixedRectangle,
}

/// Что именно политика просит объявить окну.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceOpacityRequest {
    /// В кадре есть прозрачные пиксели: непрозрачного региона нет.
    Translucent,
    /// Вся поверхность непрозрачна, за размером региона следит оконная система.
    OpaqueWholeSurface,
    /// Непрозрачен прямоугольник `(0, 0, ширина, высота)` в координатах окна — снимок размера.
    OpaqueRectangle(SurfacePixelSize),
}

impl SurfaceOpacityRequest {
    /// Запрос для подсказки с учётом того, кто следит за размером региона.
    ///
    /// Размер окна спрашивается только когда он действительно нужен (X11 и непрозрачный кадр):
    /// на Wayland и для прозрачного кадра лишнего обращения к окну нет.
    fn for_hint(hint: SurfaceOpacityHint, port: &impl SurfaceOpacityPort) -> Self {
        match (hint, port.opaque_region_sizing()) {
            (SurfaceOpacityHint::Translucent, _) => Self::Translucent,
            (SurfaceOpacityHint::Opaque, OpaqueRegionSizing::FollowsWindow) => {
                Self::OpaqueWholeSurface
            }
            (SurfaceOpacityHint::Opaque, OpaqueRegionSizing::FixedRectangle) => {
                Self::OpaqueRectangle(port.surface_pixel_size())
            }
        }
    }
}

/// Окно, которому можно сообщить подсказку о прозрачности.
pub(crate) trait SurfaceOpacityPort {
    /// Отказ платформы передать подсказку (для X11 — ошибка соединения или запроса).
    type Error;

    /// Кто держит регион в размере окна; для одного окна не меняется.
    fn opaque_region_sizing(&self) -> OpaqueRegionSizing;

    /// Текущий размер клиентской области окна.
    fn surface_pixel_size(&self) -> SurfacePixelSize;

    /// Передаёт запрос композитору (Wayland — opaque region, X11 — `_NET_WM_OPAQUE_REGION`).
    fn declare_surface_opacity(
        &mut self,
        request: SurfaceOpacityRequest,
    ) -> Result<(), Self::Error>;
}

/// Итог [`SurfaceOpacityDeclaration::sync`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SurfaceOpacitySync<E> {
    /// Запрос изменился и передан окну.
    Declared(SurfaceOpacityRequest),
    /// Форма кадра и (для X11) размер прежние: окно не трогали.
    Unchanged,
    /// Запрос изменился, но платформа его не приняла. Повтора на следующих кадрах не будет:
    /// окно снова вызывается только при следующем изменении запроса.
    Failed {
        /// Запрос, который не удалось объявить.
        request: SurfaceOpacityRequest,
        /// Причина отказа платформы.
        error: E,
    },
}

/// Последний запрос, отправленный окну.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SurfaceOpacityDeclaration {
    /// Последний отправленный запрос — даже если платформа его отвергла. Так отказ не
    /// превращается в попытку на каждом кадре (синхронизация идёт на каждом полном проходе UI).
    declared: SurfaceOpacityRequest,
}

impl SurfaceOpacityDeclaration {
    /// Состояние только что созданного окна: оно построено с `with_transparent(true)`,
    /// на X11 свойства `_NET_WM_OPAQUE_REGION` у нового окна нет.
    pub(crate) const fn for_transparent_window() -> Self {
        Self {
            declared: SurfaceOpacityRequest::Translucent,
        }
    }

    /// Приводит подсказку окна к форме кадра; окно вызывается только при изменении запроса.
    ///
    /// Для `FixedRectangle` изменением считается и новый размер окна в непрозрачном состоянии:
    /// размер фуллскрина/развёрнутого окна приходит асинхронно, а развёрнутое окно может
    /// менять размер (смена монитора, панели).
    pub(crate) fn sync<P: SurfaceOpacityPort>(
        &mut self,
        mask: WindowCornerMask,
        window: &mut P,
    ) -> SurfaceOpacitySync<P::Error> {
        let request =
            SurfaceOpacityRequest::for_hint(SurfaceOpacityHint::for_corner_mask(mask), window);
        if request == self.declared {
            return SurfaceOpacitySync::Unchanged;
        }
        self.declared = request;
        match window.declare_surface_opacity(request) {
            Ok(()) => SurfaceOpacitySync::Declared(request),
            Err(error) => SurfaceOpacitySync::Failed { request, error },
        }
    }
}

#[cfg(test)]
mod tests;
