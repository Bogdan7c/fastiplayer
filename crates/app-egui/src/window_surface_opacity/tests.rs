//! Подсказка непрозрачности следует форме кадра (и на X11 — размеру окна) и не дёргает окно
//! на каждом кадре.

use std::cell::Cell;

use super::*;
use crate::window_corner_policy::resolve_window_corner_mask;

/// Отказ поддельной платформы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RejectedByFakePlatform;

/// Окно без winit: записывает каждый запрос и считает обращения за размером.
#[derive(Debug)]
struct RecordingWindow {
    /// Как платформа держит регион: Wayland-подобно или X11-подобно.
    sizing: OpaqueRegionSizing,
    /// Текущий размер клиентской области; тест меняет его, как resize окна.
    size: SurfacePixelSize,
    /// Сколько раз политика спросила размер.
    size_queries: Cell<usize>,
    /// Платформа отвергает каждый запрос.
    rejects_requests: bool,
    /// Все запросы, дошедшие до окна, по порядку.
    declared: Vec<SurfaceOpacityRequest>,
}

impl RecordingWindow {
    /// Окно Wayland: winit сам держит opaque region в размере окна.
    fn wayland_like() -> Self {
        Self::with_sizing(OpaqueRegionSizing::FollowsWindow)
    }

    /// Окно X11: регион — свойство с размером, которое надо обновлять самим.
    fn x11_like() -> Self {
        Self::with_sizing(OpaqueRegionSizing::FixedRectangle)
    }

    fn with_sizing(sizing: OpaqueRegionSizing) -> Self {
        Self {
            sizing,
            size: pixels(1280, 720),
            size_queries: Cell::new(0),
            rejects_requests: false,
            declared: Vec::new(),
        }
    }
}

impl SurfaceOpacityPort for RecordingWindow {
    type Error = RejectedByFakePlatform;

    fn opaque_region_sizing(&self) -> OpaqueRegionSizing {
        self.sizing
    }

    fn surface_pixel_size(&self) -> SurfacePixelSize {
        self.size_queries.set(self.size_queries.get() + 1);
        self.size
    }

    fn declare_surface_opacity(
        &mut self,
        request: SurfaceOpacityRequest,
    ) -> Result<(), Self::Error> {
        self.declared.push(request);
        if self.rejects_requests {
            Err(RejectedByFakePlatform)
        } else {
            Ok(())
        }
    }
}

/// Размер окна в пикселях.
const fn pixels(width: u32, height: u32) -> SurfacePixelSize {
    SurfacePixelSize { width, height }
}

/// Маска кадра так, как её решает приложение: радиус из конфига 12 pt.
fn frame_mask(is_maximized: bool, is_fullscreen: bool) -> WindowCornerMask {
    resolve_window_corner_mask(12.0, is_maximized, is_fullscreen)
}

/// Маска обычного окна со скруглением.
fn normal_window() -> WindowCornerMask {
    frame_mask(false, false)
}

/// Маска фуллскрина.
fn fullscreen() -> WindowCornerMask {
    frame_mask(false, true)
}

/// Маска развёрнутого окна.
fn maximized() -> WindowCornerMask {
    frame_mask(true, false)
}

#[test]
fn fullscreen_playback_declares_the_surface_opaque_exactly_once() {
    let mut window = RecordingWindow::wayland_like();
    let mut declaration = SurfaceOpacityDeclaration::for_transparent_window();

    // Обычное окно со скруглением: совпадает с атрибутом создания, окно не трогаем.
    assert_eq!(
        declaration.sync(normal_window(), &mut window),
        SurfaceOpacitySync::Unchanged
    );
    // Вход в фуллскрин: квадратный кадр — композитор узнаёт, что под нами рисовать не нужно.
    assert_eq!(
        declaration.sync(fullscreen(), &mut window),
        SurfaceOpacitySync::Declared(SurfaceOpacityRequest::OpaqueWholeSurface)
    );
    // Сотни кадров воспроизведения в фуллскрине — ни одного повторного вызова окна.
    for _ in 0..300 {
        assert_eq!(
            declaration.sync(fullscreen(), &mut window),
            SurfaceOpacitySync::Unchanged
        );
    }
    assert_eq!(
        window.declared,
        vec![SurfaceOpacityRequest::OpaqueWholeSurface]
    );
}

#[test]
fn leaving_fullscreen_restores_translucency_for_rounded_corners() {
    let mut window = RecordingWindow::wayland_like();
    let mut declaration = SurfaceOpacityDeclaration::for_transparent_window();
    declaration.sync(fullscreen(), &mut window);
    assert_eq!(
        declaration.sync(normal_window(), &mut window),
        SurfaceOpacitySync::Declared(SurfaceOpacityRequest::Translucent)
    );
    // Развёрнутое окно тоже квадратное — снова непрозрачно.
    assert_eq!(
        declaration.sync(maximized(), &mut window),
        SurfaceOpacitySync::Declared(SurfaceOpacityRequest::OpaqueWholeSurface)
    );
    assert_eq!(
        window.declared,
        vec![
            SurfaceOpacityRequest::OpaqueWholeSurface,
            SurfaceOpacityRequest::Translucent,
            SurfaceOpacityRequest::OpaqueWholeSurface
        ]
    );
}

#[test]
fn wayland_like_window_ignores_resizes_and_never_asks_for_the_size() {
    let mut window = RecordingWindow::wayland_like();
    let mut declaration = SurfaceOpacityDeclaration::for_transparent_window();
    declaration.sync(maximized(), &mut window);

    // winit сам растягивает регион при resize: политика не должна звать окно повторно.
    window.size = pixels(2560, 1440);
    assert_eq!(
        declaration.sync(maximized(), &mut window),
        SurfaceOpacitySync::Unchanged
    );
    assert_eq!(
        window.declared,
        vec![SurfaceOpacityRequest::OpaqueWholeSurface]
    );
    assert_eq!(window.size_queries.get(), 0);
}

#[test]
fn x11_fullscreen_declares_the_window_rectangle_exactly_once() {
    let mut window = RecordingWindow::x11_like();
    let mut declaration = SurfaceOpacityDeclaration::for_transparent_window();

    // Обычное окно: свойства у нового окна нет, X11 не трогаем и размер не спрашиваем.
    assert_eq!(
        declaration.sync(normal_window(), &mut window),
        SurfaceOpacitySync::Unchanged
    );
    assert_eq!(window.size_queries.get(), 0);

    // Фуллскрин пришёл вместе с размером экрана.
    window.size = pixels(1920, 1080);
    assert_eq!(
        declaration.sync(fullscreen(), &mut window),
        SurfaceOpacitySync::Declared(SurfaceOpacityRequest::OpaqueRectangle(pixels(1920, 1080)))
    );
    for _ in 0..300 {
        assert_eq!(
            declaration.sync(fullscreen(), &mut window),
            SurfaceOpacitySync::Unchanged
        );
    }
    assert_eq!(
        window.declared,
        vec![SurfaceOpacityRequest::OpaqueRectangle(pixels(1920, 1080))]
    );
}

#[test]
fn x11_resize_while_opaque_redeclares_the_new_rectangle_once() {
    let mut window = RecordingWindow::x11_like();
    let mut declaration = SurfaceOpacityDeclaration::for_transparent_window();

    // Размер фуллскрина приходит асинхронно: первый квадратный кадр ещё в старом размере.
    declaration.sync(fullscreen(), &mut window);
    window.size = pixels(1920, 1080);
    assert_eq!(
        declaration.sync(fullscreen(), &mut window),
        SurfaceOpacitySync::Declared(SurfaceOpacityRequest::OpaqueRectangle(pixels(1920, 1080)))
    );
    for _ in 0..10 {
        assert_eq!(
            declaration.sync(fullscreen(), &mut window),
            SurfaceOpacitySync::Unchanged
        );
    }

    // Выход в развёрнутое окно другого размера — снова один вызов с новым размером.
    window.size = pixels(1920, 1040);
    assert_eq!(
        declaration.sync(maximized(), &mut window),
        SurfaceOpacitySync::Declared(SurfaceOpacityRequest::OpaqueRectangle(pixels(1920, 1040)))
    );
    assert_eq!(
        declaration.sync(maximized(), &mut window),
        SurfaceOpacitySync::Unchanged
    );
    assert_eq!(
        window.declared,
        vec![
            SurfaceOpacityRequest::OpaqueRectangle(pixels(1280, 720)),
            SurfaceOpacityRequest::OpaqueRectangle(pixels(1920, 1080)),
            SurfaceOpacityRequest::OpaqueRectangle(pixels(1920, 1040)),
        ]
    );
}

#[test]
fn x11_leaving_fullscreen_clears_the_region_and_ignores_later_resizes() {
    let mut window = RecordingWindow::x11_like();
    let mut declaration = SurfaceOpacityDeclaration::for_transparent_window();
    window.size = pixels(1920, 1080);
    declaration.sync(fullscreen(), &mut window);

    window.size = pixels(1280, 720);
    assert_eq!(
        declaration.sync(normal_window(), &mut window),
        SurfaceOpacitySync::Declared(SurfaceOpacityRequest::Translucent)
    );
    // Обычное окно тянут мышью: прозрачному окну регион не нужен, X11 молчит.
    let size_queries_after_leaving = window.size_queries.get();
    window.size = pixels(1000, 600);
    assert_eq!(
        declaration.sync(normal_window(), &mut window),
        SurfaceOpacitySync::Unchanged
    );
    assert_eq!(window.size_queries.get(), size_queries_after_leaving);
    assert_eq!(
        window.declared,
        vec![
            SurfaceOpacityRequest::OpaqueRectangle(pixels(1920, 1080)),
            SurfaceOpacityRequest::Translucent,
        ]
    );
}

#[test]
fn rejected_request_is_not_retried_every_frame_but_next_change_tries_again() {
    let mut window = RecordingWindow::x11_like();
    window.rejects_requests = true;
    let mut declaration = SurfaceOpacityDeclaration::for_transparent_window();

    assert_eq!(
        declaration.sync(fullscreen(), &mut window),
        SurfaceOpacitySync::Failed {
            request: SurfaceOpacityRequest::OpaqueRectangle(pixels(1280, 720)),
            error: RejectedByFakePlatform,
        }
    );
    // Тот же кадр — без повторной попытки (иначе отказ стоил бы запроса X11 на каждом кадре).
    for _ in 0..10 {
        assert_eq!(
            declaration.sync(fullscreen(), &mut window),
            SurfaceOpacitySync::Unchanged
        );
    }
    // Следующее изменение (выход из фуллскрина) снова доходит до платформы.
    window.rejects_requests = false;
    assert_eq!(
        declaration.sync(normal_window(), &mut window),
        SurfaceOpacitySync::Declared(SurfaceOpacityRequest::Translucent)
    );
    assert_eq!(
        window.declared,
        vec![
            SurfaceOpacityRequest::OpaqueRectangle(pixels(1280, 720)),
            SurfaceOpacityRequest::Translucent,
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
