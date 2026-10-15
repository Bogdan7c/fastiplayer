//! Нейтральный расчёт «картинки без чёрных полос» внутри video viewport-а.
//!
//! Модуль владеет единственной формулой fit (contain + центрирование). Её используют
//! и video shader path (через uv scale/offset), и любые оверлеи (например, субтитры),
//! которым нужна реальная область картинки в координатах surface.

use crate::{RenderViewport, RenderableFrame};

/// Размер картинки после container display orientation (в пикселях).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PictureSize {
    /// Ширина картинки после quarter-turn orientation.
    pub width: u32,
    /// Высота картинки после quarter-turn orientation.
    pub height: u32,
}

impl PictureSize {
    /// Создаёт размер картинки без проверок; вырожденность решает [`PictureLayout::fit`].
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Берёт ориентированный display size кадра.
    #[must_use]
    pub const fn from_frame(frame: &RenderableFrame) -> Self {
        Self::new(
            frame.oriented_display_width(),
            frame.oriented_display_height(),
        )
    }
}

/// Прямоугольник картинки в float-пикселях surface (без чёрных полос).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PictureRect {
    /// Левый край в пикселях surface.
    pub x: f32,
    /// Верхний край в пикселях surface.
    pub y: f32,
    /// Ширина в пикселях.
    pub width: f32,
    /// Высота в пикселях.
    pub height: f32,
}

/// Результат fit-а: прямоугольник картинки и uv-параметры для shader-а.
///
/// Существует только для невырожденных входов: нулевой viewport или нулевая картинка
/// дают `None` из [`PictureLayout::fit`], поэтому внутри нет NaN/inf.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PictureLayout {
    rect: PictureRect,
    uv_scale: [f32; 2],
    uv_offset: [f32; 2],
}

impl PictureLayout {
    /// Вписывает картинку в viewport (contain) и центрирует.
    ///
    /// Возвращает `None`, если viewport или картинка имеют нулевую сторону.
    #[must_use]
    pub fn fit(picture: PictureSize, viewport: RenderViewport) -> Option<Self> {
        if viewport.is_empty() || picture.width == 0 || picture.height == 0 {
            return None;
        }
        // Порядок float-операций намеренно совпадает со старой формулой shader path:
        // результат uv должен быть бит-в-бит прежним.
        let video_aspect = picture.width as f32 / picture.height.max(1) as f32;
        let viewport_aspect = viewport.width as f32 / viewport.height.max(1) as f32;
        let viewport_width = viewport.width as f32;
        let viewport_height = viewport.height as f32;
        let viewport_x = viewport.x as f32;
        let viewport_y = viewport.y as f32;

        if video_aspect > viewport_aspect {
            // Картинка шире viewport-а: полосы сверху и снизу.
            let scale_y = video_aspect / viewport_aspect;
            let height = viewport_height / scale_y;
            Some(Self {
                rect: PictureRect {
                    x: viewport_x,
                    y: viewport_y + (viewport_height - height) * 0.5,
                    width: viewport_width,
                    height,
                },
                uv_scale: [1.0, scale_y],
                uv_offset: [0.0, (1.0 - scale_y) * 0.5],
            })
        } else {
            // Картинка уже (или равна): полосы слева и справа.
            let scale_x = viewport_aspect / video_aspect;
            let width = viewport_width / scale_x;
            Some(Self {
                rect: PictureRect {
                    x: viewport_x + (viewport_width - width) * 0.5,
                    y: viewport_y,
                    width,
                    height: viewport_height,
                },
                uv_scale: [scale_x, 1.0],
                uv_offset: [(1.0 - scale_x) * 0.5, 0.0],
            })
        }
    }

    /// Вариант [`Self::fit`] для кадра с учётом его display orientation.
    #[must_use]
    pub fn fit_frame(frame: &RenderableFrame, viewport: RenderViewport) -> Option<Self> {
        Self::fit(PictureSize::from_frame(frame), viewport)
    }

    /// Область картинки без полос в координатах surface.
    #[must_use]
    pub const fn rect(&self) -> PictureRect {
        self.rect
    }

    /// Пара `(uv_scale, uv_offset)` для shader-а: `display_uv = uv * scale + offset`.
    #[must_use]
    pub const fn uv_scale_and_offset(&self) -> ([f32; 2], [f32; 2]) {
        (self.uv_scale, self.uv_offset)
    }
}

#[cfg(test)]
#[path = "tests/picture_rect.rs"]
mod tests;
