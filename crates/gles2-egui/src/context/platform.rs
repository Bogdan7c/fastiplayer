//! Перевод дескрипторов окна (`raw-window-handle`) в платформу EGL.
//!
//! Поддерживаются нативный Wayland и X11 через Xlib (так winit 0.30 отдаёт X11/XWayland-окна).
//! Решение владельца: тестируем оба режима. Остальные оконные системы — типизированная ошибка
//! [`UnsupportedWindowSystem`], а не тихий выбор «какой-нибудь» платформы.

use std::os::raw::c_ulong;

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use super::driver::NativeDisplay;

/// Окно, к которому привязывается EGL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WindowBinding {
    /// Display для `eglGetPlatformDisplay`.
    pub(crate) native_display: NativeDisplay,
    /// Окно для surface.
    pub(crate) window: NativeWindow,
}

/// Нативное окно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeWindow {
    /// Адрес `wl_surface`.
    Wayland { surface_address: usize },
    /// XID окна и его visual (`None`, если оконное API visual не сообщило).
    X11 {
        window_xid: c_ulong,
        visual_id: Option<i64>,
    },
}

/// Пара дескрипторов, которую этот crate не умеет привязать к EGL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unsupported window system: display {display}, window {window}")]
pub struct UnsupportedWindowSystem {
    /// Вид display (`Wayland`, `Xlib`, `Xcb`, …).
    pub display: &'static str,
    /// Вид окна.
    pub window: &'static str,
}

/// Клиентские расширения EGL, нужные платформе (KHR- и EXT-варианты равноценны).
pub(crate) fn required_platform_extensions(native: NativeDisplay) -> &'static [&'static str] {
    match native {
        NativeDisplay::Wayland { .. } => &["EGL_KHR_platform_wayland", "EGL_EXT_platform_wayland"],
        NativeDisplay::X11 { .. } => &["EGL_KHR_platform_x11", "EGL_EXT_platform_x11"],
        NativeDisplay::Surfaceless => &["EGL_MESA_platform_surfaceless"],
    }
}

/// Привязка окна по его дескрипторам.
pub(crate) fn window_binding(
    display: RawDisplayHandle,
    window: RawWindowHandle,
) -> Result<WindowBinding, UnsupportedWindowSystem> {
    match (display, window) {
        (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(window)) => {
            Ok(WindowBinding {
                native_display: NativeDisplay::Wayland {
                    display_address: display.display.as_ptr() as usize,
                },
                window: NativeWindow::Wayland {
                    surface_address: window.surface.as_ptr() as usize,
                },
            })
        }
        (RawDisplayHandle::Xlib(display), RawWindowHandle::Xlib(window)) => Ok(WindowBinding {
            native_display: NativeDisplay::X11 {
                display_address: display.display.map(|pointer| pointer.as_ptr() as usize),
            },
            window: NativeWindow::X11 {
                window_xid: window.window,
                // 0 у raw-window-handle означает «visual неизвестен».
                visual_id: (window.visual_id != 0)
                    .then(|| i64::try_from(window.visual_id).unwrap_or(i64::MAX)),
            },
        }),
        (display, window) => Err(UnsupportedWindowSystem {
            display: display_kind(&display),
            window: window_kind(&window),
        }),
    }
}

fn display_kind(display: &RawDisplayHandle) -> &'static str {
    match display {
        RawDisplayHandle::Wayland(_) => "Wayland",
        RawDisplayHandle::Xlib(_) => "Xlib",
        RawDisplayHandle::Xcb(_) => "Xcb",
        RawDisplayHandle::Drm(_) => "Drm",
        RawDisplayHandle::Gbm(_) => "Gbm",
        RawDisplayHandle::Windows(_) => "Windows",
        RawDisplayHandle::AppKit(_) => "AppKit",
        RawDisplayHandle::Android(_) => "Android",
        _ => "other",
    }
}

fn window_kind(window: &RawWindowHandle) -> &'static str {
    match window {
        RawWindowHandle::Wayland(_) => "Wayland",
        RawWindowHandle::Xlib(_) => "Xlib",
        RawWindowHandle::Xcb(_) => "Xcb",
        RawWindowHandle::Drm(_) => "Drm",
        RawWindowHandle::Gbm(_) => "Gbm",
        RawWindowHandle::Win32(_) => "Win32",
        RawWindowHandle::AppKit(_) => "AppKit",
        RawWindowHandle::AndroidNdk(_) => "AndroidNdk",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use std::ptr::NonNull;

    use raw_window_handle::{
        WaylandDisplayHandle, WaylandWindowHandle, XcbDisplayHandle, XlibDisplayHandle,
        XlibWindowHandle,
    };

    use super::*;

    #[test]
    fn wayland_pair_maps_to_wayland_platform() {
        let mut display_object = 0_u8;
        let mut surface_object = 0_u8;
        let display_pointer = NonNull::from(&mut display_object).cast();
        let surface_pointer = NonNull::from(&mut surface_object).cast();
        let binding = window_binding(
            RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display_pointer)),
            RawWindowHandle::Wayland(WaylandWindowHandle::new(surface_pointer)),
        );
        assert_eq!(
            binding,
            Ok(WindowBinding {
                native_display: NativeDisplay::Wayland {
                    display_address: display_pointer.as_ptr() as usize
                },
                window: NativeWindow::Wayland {
                    surface_address: surface_pointer.as_ptr() as usize
                },
            })
        );
    }

    #[test]
    fn xlib_pair_keeps_window_visual() {
        let mut window = XlibWindowHandle::new(0x0420_0007);
        window.visual_id = 0x21;
        let binding = window_binding(
            RawDisplayHandle::Xlib(XlibDisplayHandle::new(None, 0)),
            RawWindowHandle::Xlib(window),
        );
        assert_eq!(
            binding,
            Ok(WindowBinding {
                native_display: NativeDisplay::X11 {
                    display_address: None
                },
                window: NativeWindow::X11 {
                    window_xid: 0x0420_0007,
                    visual_id: Some(0x21)
                },
            })
        );
    }

    #[test]
    fn unknown_xlib_visual_is_not_a_requirement() {
        let binding = window_binding(
            RawDisplayHandle::Xlib(XlibDisplayHandle::new(None, 0)),
            RawWindowHandle::Xlib(XlibWindowHandle::new(7)),
        );
        assert!(matches!(
            binding.map(|binding| binding.window),
            Ok(NativeWindow::X11 {
                visual_id: None,
                ..
            })
        ));
    }

    #[test]
    fn mismatched_or_unsupported_pairs_are_typed_errors() {
        let mut surface_object = 0_u8;
        let surface_pointer = NonNull::from(&mut surface_object).cast();
        assert_eq!(
            window_binding(
                RawDisplayHandle::Xcb(XcbDisplayHandle::new(None, 0)),
                RawWindowHandle::Wayland(WaylandWindowHandle::new(surface_pointer)),
            ),
            Err(UnsupportedWindowSystem {
                display: "Xcb",
                window: "Wayland"
            })
        );
    }
}
