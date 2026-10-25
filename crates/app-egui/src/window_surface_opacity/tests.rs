//! Подсказка непрозрачности следует форме кадра и не дёргает окно на каждом кадре.

use std::cell::RefCell;

use super::*;
use crate::window_corner_policy::resolve_window_corner_mask;

/// Окно без winit: записывает каждую подсказку, которую получило.
#[derive(Debug, Default)]
struct RecordingWindow {
    declared: RefCell<Vec<SurfaceOpacityHint>>,
}

impl SurfaceOpacityPort for RecordingWindow {
    fn declare_surface_opacity(&self, hint: SurfaceOpacityHint) {
        self.declared.borrow_mut().push(hint);
    }
}

/// Маска кадра так, как её решает приложение: радиус из конфига 12 pt.
fn frame_mask(is_maximized: bool, is_fullscreen: bool) -> WindowCornerMask {
    resolve_window_corner_mask(12.0, is_maximized, is_fullscreen)
}

#[test]
fn fullscreen_playback_declares_the_surface_opaque_exactly_once() {
    let window = RecordingWindow::default();
    let mut declaration = SurfaceOpacityDeclaration::for_transparent_window();

    // Обычное окно со скруглением: совпадает с атрибутом создания, окно не трогаем.
    assert_eq!(
        declaration.sync(frame_mask(false, false), &window),
        SurfaceOpacitySync::Unchanged
    );
    // Вход в фуллскрин: квадратный кадр — композитор узнаёт, что под нами рисовать не нужно.
    assert_eq!(
        declaration.sync(frame_mask(false, true), &window),
        SurfaceOpacitySync::Declared(SurfaceOpacityHint::Opaque)
    );
    // Сотни кадров воспроизведения в фуллскрине — ни одного повторного вызова окна.
    for _ in 0..300 {
        assert_eq!(
            declaration.sync(frame_mask(false, true), &window),
            SurfaceOpacitySync::Unchanged
        );
    }
    assert_eq!(*window.declared.borrow(), vec![SurfaceOpacityHint::Opaque]);
}

#[test]
fn leaving_fullscreen_restores_translucency_for_rounded_corners() {
    let window = RecordingWindow::default();
    let mut declaration = SurfaceOpacityDeclaration::for_transparent_window();
    declaration.sync(frame_mask(false, true), &window);
    assert_eq!(
        declaration.sync(frame_mask(false, false), &window),
        SurfaceOpacitySync::Declared(SurfaceOpacityHint::Translucent)
    );
    // Развёрнутое окно тоже квадратное — снова непрозрачно.
    assert_eq!(
        declaration.sync(frame_mask(true, false), &window),
        SurfaceOpacitySync::Declared(SurfaceOpacityHint::Opaque)
    );
    assert_eq!(
        *window.declared.borrow(),
        vec![
            SurfaceOpacityHint::Opaque,
            SurfaceOpacityHint::Translucent,
            SurfaceOpacityHint::Opaque
        ]
    );
}

#[test]
fn zero_configured_radius_is_opaque_even_in_a_normal_window() {
    // Пользователь отключил скругление: прозрачных пикселей нет и в обычном окне.
    assert_eq!(
        SurfaceOpacityHint::for_corner_mask(resolve_window_corner_mask(0.0, false, false)),
        SurfaceOpacityHint::Opaque
    );
    assert_eq!(
        SurfaceOpacityHint::for_corner_mask(WindowCornerMask::rounded_in_points(0.5)),
        SurfaceOpacityHint::Translucent
    );
}
