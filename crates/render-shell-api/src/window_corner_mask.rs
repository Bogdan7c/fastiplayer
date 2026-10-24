//! Намерение приложения о форме desktop-окна (скругление углов).
//!
//! Рисует маску конкретный рендер (wgpu — отдельный fullscreen-pass); здесь только то,
//! что решило приложение.

/// Намерение app-слоя о форме текущего кадра в логических UI points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowCornerMask {
    radius_points: f32,
}

impl WindowCornerMask {
    /// Создаёт полностью прямоугольный контур без дополнительного GPU-pass-а.
    #[must_use]
    pub const fn square() -> Self {
        Self { radius_points: 0.0 }
    }

    /// Создаёт скруглённый контур; отрицательные и нечисловые значения безопасно отключаются.
    #[must_use]
    pub fn rounded_in_points(radius_points: f32) -> Self {
        let radius_points = if radius_points.is_finite() {
            radius_points.max(0.0)
        } else {
            0.0
        };
        Self { radius_points }
    }

    /// Возвращает радиус в той же логической системе координат, что и egui.
    #[must_use]
    pub const fn radius_points(self) -> f32 {
        self.radius_points
    }
}

#[cfg(test)]
#[path = "tests/window_corner_mask.rs"]
mod tests;
