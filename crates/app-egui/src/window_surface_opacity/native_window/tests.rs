//! Выбор платформенного пути подсказки по native handle окна.

use std::num::NonZeroU32;
use std::ptr::NonNull;

use winit::raw_window_handle::{WaylandWindowHandle, XcbWindowHandle, XlibWindowHandle};

use super::*;

#[test]
fn xlib_window_goes_through_the_x11_opaque_region() {
    assert_eq!(
        NativeOpacityTarget::for_raw_handle(RawWindowHandle::Xlib(XlibWindowHandle::new(
            0x0480_0003
        ))),
        NativeOpacityTarget::X11Window(X11WindowId::new(0x0480_0003))
    );
}

#[test]
fn xcb_window_goes_through_the_x11_opaque_region() {
    let xid = NonZeroU32::new(0x0480_0003).unwrap_or(NonZeroU32::MIN);
    assert_eq!(
        NativeOpacityTarget::for_raw_handle(RawWindowHandle::Xcb(XcbWindowHandle::new(xid))),
        NativeOpacityTarget::X11Window(X11WindowId::new(0x0480_0003))
    );
}

#[test]
fn wayland_window_keeps_the_winit_transparency_path() {
    // Указатель на surface не разыменовывается: проверяется только вид handle-а.
    let surface = NonNull::<std::ffi::c_void>::dangling();
    assert_eq!(
        NativeOpacityTarget::for_raw_handle(RawWindowHandle::Wayland(WaylandWindowHandle::new(
            surface
        ))),
        NativeOpacityTarget::WinitTransparency
    );
}

#[test]
fn xlib_id_outside_the_xid_range_falls_back_to_winit() {
    let broken_xid = std::ffi::c_ulong::from(u32::MAX) + 1;
    assert_eq!(
        NativeOpacityTarget::for_raw_handle(RawWindowHandle::Xlib(XlibWindowHandle::new(
            broken_xid
        ))),
        NativeOpacityTarget::WinitTransparency
    );
}
