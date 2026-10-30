//! Атрибуты главного окна Fastiplayer: сборка отдельно от создания окна ради теста.
//!
//! Окно обычное непрозрачное. Скругление углов удалено из обоих рендереров (решение 57 плана
//! gles2-renderer): на Intel Ironlake ARGB-поверхность не идёт на плоскость дисплея, и KWin
//! копирует каждый кадр. Под X11 непрозрачность задаётся только при создании окна, поэтому
//! именно здесь, а не позже, фиксируется `with_transparent(false)`.

use winit::window::{Window, WindowAttributes};

/// Собирает атрибуты главного окна: рамки нет (titlebar рисует egui), поверхность непрозрачная.
pub(crate) fn main_window_attributes() -> WindowAttributes {
    // App ID и иконка: по ним панель задач показывает лого Fastiplayer, см. `window_identity`.
    super::window_identity::with_window_identity(Window::default_attributes())
        .with_title("Fastiplayer")
        .with_inner_size(winit::dpi::PhysicalSize::new(1280, 720))
        // Минимум выведен из реальных размеров titlebar и нижней панели, см. `window_limits`.
        .with_min_inner_size(super::window_limits::min_inner_size())
        .with_decorations(false)
        .with_transparent(false)
        .with_visible(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Главное окно создаётся непрозрачным и без системной рамки: без альфа-поверхности KWin
    /// не копирует кадры, а титульную строку рисует сам плеер.
    #[test]
    fn main_window_is_opaque_and_frameless() {
        let attributes = main_window_attributes();

        assert!(!attributes.transparent);
        assert!(!attributes.decorations);
        assert!(attributes.visible);
    }

    /// Минимум окна берётся из `window_limits` (реальные размеры titlebar и нижней панели),
    /// а не из «магического» литерала при создании окна.
    #[test]
    fn main_window_minimum_size_comes_from_window_limits() {
        let attributes = main_window_attributes();

        assert_eq!(
            attributes.min_inner_size,
            Some(super::super::window_limits::min_inner_size().into())
        );
    }
}
