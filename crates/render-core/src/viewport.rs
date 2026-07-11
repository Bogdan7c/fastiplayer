use serde::{Deserialize, Serialize};

/// Renderer-neutral область видео в физических пикселях surface target-а.
///
/// App layer вычисляет эту область из layout-а, а concrete renderer сам решает,
/// как применить её к своему backend-у. Поэтому тип не содержит `egui`, `wgpu`
/// или windowing-объекты.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderViewport {
    /// Левый край области видео в physical pixels.
    pub x: u32,

    /// Верхний край области видео в physical pixels.
    pub y: u32,

    /// Ширина области видео в physical pixels.
    pub width: u32,

    /// Высота области видео в physical pixels.
    pub height: u32,
}

impl RenderViewport {
    /// Создаёт viewport без clamp-а; владелец surface должен зажать его перед render pass.
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Возвращает viewport на всю surface.
    #[must_use]
    pub const fn full_surface(surface_width: u32, surface_height: u32) -> Self {
        Self::new(0, 0, surface_width, surface_height)
    }

    /// Размер viewport-а как `(width, height)` для letterbox расчётов.
    #[must_use]
    pub const fn size(self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Возвращает `true`, если viewport не может безопасно принять draw.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Правый край viewport-а с защитой от переполнения.
    #[must_use]
    pub const fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    /// Нижний край viewport-а с защитой от переполнения.
    #[must_use]
    pub const fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }

    /// Возвращает пересечение двух viewport-ов или `None`, если они не пересекаются.
    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        if self.is_empty() || other.is_empty() {
            return None;
        }

        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());

        if right <= x || bottom <= y {
            return None;
        }

        Some(Self::new(x, y, right - x, bottom - y))
    }

    /// Разбивает viewport на видимые прямоугольники после вычитания `excluded`.
    ///
    /// Метод нужен для UI overlay-ов: video shader сохраняет пропорции по исходному
    /// `self`, а concrete renderer рисует только в возвращённых scissor-областях.
    #[must_use]
    pub fn subtract(self, excluded: Self) -> Vec<Self> {
        let Some(excluded) = self.intersection(excluded) else {
            return vec![self];
        };

        let mut visible_rects = Vec::with_capacity(4);
        let self_right = self.right();
        let self_bottom = self.bottom();
        let excluded_right = excluded.right();
        let excluded_bottom = excluded.bottom();

        if excluded.y > self.y {
            visible_rects.push(Self::new(self.x, self.y, self.width, excluded.y - self.y));
        }

        if excluded_bottom < self_bottom {
            visible_rects.push(Self::new(
                self.x,
                excluded_bottom,
                self.width,
                self_bottom - excluded_bottom,
            ));
        }

        if excluded.x > self.x {
            visible_rects.push(Self::new(
                self.x,
                excluded.y,
                excluded.x - self.x,
                excluded.height,
            ));
        }

        if excluded_right < self_right {
            visible_rects.push(Self::new(
                excluded_right,
                excluded.y,
                self_right - excluded_right,
                excluded.height,
            ));
        }

        visible_rects
    }

    /// Зажимает viewport к surface; некорректный запрос возвращает full-surface fallback.
    ///
    /// Fallback нужен, чтобы отсутствие/сбой layout rect-а не создавали нулевой scissor
    /// и не меняли старое поведение рендера полного окна.
    #[must_use]
    pub fn clamp_to_surface(self, surface_width: u32, surface_height: u32) -> Self {
        let full_surface = Self::full_surface(surface_width, surface_height);
        if self.is_empty() || self.x >= surface_width || self.y >= surface_height {
            return full_surface;
        }

        let clamped_width = self.width.min(surface_width - self.x);
        let clamped_height = self.height.min(surface_height - self.y);
        if clamped_width == 0 || clamped_height == 0 {
            return full_surface;
        }

        Self::new(self.x, self.y, clamped_width, clamped_height)
    }
}
#[cfg(test)]
#[path = "tests/viewport.rs"]
mod tests;
