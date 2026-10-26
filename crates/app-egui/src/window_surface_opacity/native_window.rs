//! Владелец подсказки непрозрачности для настоящего окна winit.
//!
//! Выбирает платформенный путь по `raw_window_handle` (один раз, при первой синхронизации),
//! передаёт политике [`SurfaceOpacityDeclaration`] адаптер окна и пишет в лог итог: смену
//! подсказки — на debug, отказ платформы — один раз на warn (дальше на debug, чтобы лог не
//! заливало при каждом входе в фуллскрин).

use winit::raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use super::{
    OpaqueRegionSizing, SurfaceOpacityDeclaration, SurfaceOpacityPort, SurfaceOpacityRequest,
    SurfaceOpacitySync, SurfacePixelSize,
};

#[cfg(target_os = "linux")]
use super::x11_opaque_region::{X11OpaqueRegionError, X11OpaqueRegionWriter, X11WindowId};

/// Отказ платформы; вне Linux X11-пути нет и отказов не бывает.
#[cfg(target_os = "linux")]
type NativeOpacityError = X11OpaqueRegionError;
#[cfg(not(target_os = "linux"))]
type NativeOpacityError = std::convert::Infallible;

/// Куда уходит подсказка для этого окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeOpacityTarget {
    /// Прозрачность окна winit (`Window::set_transparent`): на Wayland это opaque region,
    /// который winit сам держит в размере окна; на прочих платформах — то, что умеет winit.
    WinitTransparency,
    /// Окно X11 (Xorg или XWayland): свойство `_NET_WM_OPAQUE_REGION`.
    #[cfg(target_os = "linux")]
    X11Window(X11WindowId),
}

impl NativeOpacityTarget {
    /// Определяет путь по native handle окна.
    ///
    /// Если handle недоступен, остаётся путь winit — ровно поведение до X11-пути.
    fn detect(window: &Window) -> Self {
        match window.window_handle() {
            Ok(handle) => Self::for_raw_handle(handle.as_raw()),
            Err(error) => {
                Self::warn_unknown_handle(&error);
                Self::WinitTransparency
            }
        }
    }

    /// Путь для конкретного вида handle-а.
    fn for_raw_handle(handle: RawWindowHandle) -> Self {
        match handle {
            // winit 0.30 на X11 отдаёт Xlib handle; Xcb разбираем на случай смены бэкенда.
            #[cfg(target_os = "linux")]
            RawWindowHandle::Xlib(xlib) => match u32::try_from(xlib.window) {
                Ok(xid) => Self::X11Window(X11WindowId::new(xid)),
                Err(_) => {
                    // XID по протоколу X11 — 29 бит; иное значение — сломанный handle.
                    tracing::warn!(
                        xid = xlib.window,
                        "Xlib window id does not fit X11 XID; opacity hint stays with winit"
                    );
                    Self::WinitTransparency
                }
            },
            #[cfg(target_os = "linux")]
            RawWindowHandle::Xcb(xcb) => Self::X11Window(X11WindowId::new(xcb.window.get())),
            _ => Self::WinitTransparency,
        }
    }

    /// Лог отказа native handle-а (один раз: определение пути выполняется однократно).
    fn warn_unknown_handle(error: &HandleError) {
        tracing::warn!(
            %error,
            "Native window handle unavailable; surface opacity hint uses winit transparency only"
        );
    }
}

/// Подсказка непрозрачности окна приложения: политика + платформенный путь + лог отказов.
pub(crate) struct WindowSurfaceOpacity {
    /// Когда и что объявлять (не знает о платформе).
    declaration: SurfaceOpacityDeclaration,
    /// Платформенный путь; определяется при первой синхронизации (окна до неё может не быть).
    target: Option<NativeOpacityTarget>,
    /// Писатель свойства X11 со своим ленивым соединением.
    #[cfg(target_os = "linux")]
    x11_writer: X11OpaqueRegionWriter,
    /// Отказ платформы уже записан на warn: следующие — только на debug.
    failure_reported: bool,
}

impl WindowSurfaceOpacity {
    /// Состояние только что созданного окна: оно построено с `with_transparent(true)`.
    pub(crate) const fn for_transparent_window() -> Self {
        Self {
            declaration: SurfaceOpacityDeclaration::for_transparent_window(),
            target: None,
            #[cfg(target_os = "linux")]
            x11_writer: X11OpaqueRegionWriter::new(),
            failure_reported: false,
        }
    }

    /// Приводит подсказку окна к форме кадра (вызывается на каждом полном проходе UI).
    pub(crate) fn sync(&mut self, mask: render_shell_api::WindowCornerMask, window: &Window) {
        let target = *self
            .target
            .get_or_insert_with(|| NativeOpacityTarget::detect(window));
        let mut port = WinitSurfaceOpacityPort {
            window,
            target,
            #[cfg(target_os = "linux")]
            x11_writer: &mut self.x11_writer,
        };
        match self.declaration.sync(mask, &mut port) {
            SurfaceOpacitySync::Unchanged => {}
            SurfaceOpacitySync::Declared(request) => {
                tracing::debug!(?request, ?target, "Window surface opacity hint declared");
            }
            SurfaceOpacitySync::Failed { request, error } => {
                self.report_failure(request, target, &error);
            }
        }
    }

    /// Отказ платформы: первый — на warn с контекстом, остальные — на debug.
    ///
    /// Отказ не фатален: окно рисуется как прежде, теряется только экономия композитора.
    fn report_failure(
        &mut self,
        request: SurfaceOpacityRequest,
        target: NativeOpacityTarget,
        error: &NativeOpacityError,
    ) {
        if self.failure_reported {
            tracing::debug!(?request, ?target, %error, "Window surface opacity hint failed again");
            return;
        }
        self.failure_reported = true;
        tracing::warn!(
            ?request,
            ?target,
            %error,
            "Window surface opacity hint failed; compositor keeps repainting behind the window"
        );
    }
}

/// Адаптер настоящего окна к [`SurfaceOpacityPort`] на время одной синхронизации.
struct WinitSurfaceOpacityPort<'window> {
    /// Окно winit.
    window: &'window Window,
    /// Платформенный путь этого окна.
    target: NativeOpacityTarget,
    /// Писатель свойства X11.
    #[cfg(target_os = "linux")]
    x11_writer: &'window mut X11OpaqueRegionWriter,
}

impl SurfaceOpacityPort for WinitSurfaceOpacityPort<'_> {
    type Error = NativeOpacityError;

    fn opaque_region_sizing(&self) -> OpaqueRegionSizing {
        match self.target {
            NativeOpacityTarget::WinitTransparency => OpaqueRegionSizing::FollowsWindow,
            #[cfg(target_os = "linux")]
            NativeOpacityTarget::X11Window(_) => OpaqueRegionSizing::FixedRectangle,
        }
    }

    fn surface_pixel_size(&self) -> SurfacePixelSize {
        self.window.inner_size().into()
    }

    fn declare_surface_opacity(
        &mut self,
        request: SurfaceOpacityRequest,
    ) -> Result<(), Self::Error> {
        match self.target {
            NativeOpacityTarget::WinitTransparency => {
                // Регион применяется с ближайшим commit поверхности, то есть вместе с кадром,
                // который уже нарисован в новой форме.
                self.window
                    .set_transparent(request == SurfaceOpacityRequest::Translucent);
                Ok(())
            }
            #[cfg(target_os = "linux")]
            NativeOpacityTarget::X11Window(xid) => match request {
                SurfaceOpacityRequest::Translucent => self.x11_writer.clear_opaque_region(xid),
                SurfaceOpacityRequest::OpaqueRectangle(size) => {
                    self.x11_writer.declare_whole_window_opaque(xid, size)
                }
                // Для X11 политика просит прямоугольник (`FixedRectangle`); если запрос всё же
                // пришёл без размера, берём текущий — смысл «непрозрачно всё окно» тот же.
                SurfaceOpacityRequest::OpaqueWholeSurface => {
                    let size = self.surface_pixel_size();
                    self.x11_writer.declare_whole_window_opaque(xid, size)
                }
            },
        }
    }
}

// Тесты различают X11-путь, которого вне Linux нет.
#[cfg(all(test, target_os = "linux"))]
mod tests;
