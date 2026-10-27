//! Отсечение примитива egui (`ClippedPrimitive::clip_rect`) → scissor GL.
//!
//! Правило округления — ровно как `egui_wgpu::renderer::ScissorRect::new` (0.36): точки ×
//! `pixels_per_point`, `round`, насыщающее приведение к `u32` (отрицательное → 0), прижатие к
//! цели. Отличие одно — ось Y: у wgpu начало сверху, у GL `glScissor` — снизу, поэтому
//! `y_gl = высота − (y + высота_прямоугольника)`.

use epaint::Rect;

use crate::damage::TargetArea;
use crate::draw_gl::GlScissor;

/// Прямоугольник в пикселях цели с началом в верхнем левом углу (как у egui и wgpu).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TopLeftPixelRect {
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) width: u32,
    pub(super) height: u32,
}

/// Отсечение egui в пикселях (как у egui-wgpu). `None` — пустая площадь: меш не рисуется.
pub(super) fn clip_rect_in_pixels(
    clip_rect: Rect,
    pixels_per_point: f32,
    target_size: [u32; 2],
) -> Option<TopLeftPixelRect> {
    // `as u32` у f32 насыщающее (NaN → 0, отрицательное → 0) — так же, как в egui-wgpu.
    let to_pixel = |points: f32| (pixels_per_point * points).round() as u32;
    let min_x = to_pixel(clip_rect.min.x).clamp(0, target_size[0]);
    let min_y = to_pixel(clip_rect.min.y).clamp(0, target_size[1]);
    let max_x = to_pixel(clip_rect.max.x).clamp(min_x, target_size[0]);
    let max_y = to_pixel(clip_rect.max.y).clamp(min_y, target_size[1]);
    let rect = TopLeftPixelRect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    };
    (rect.width > 0 && rect.height > 0).then_some(rect)
}

/// Перевод в координаты `glScissor` (начало снизу). Значения уже прижаты к цели, а размер цели
/// ограничен `GL_MAX_VIEWPORT_DIMS`/размером окна, поэтому помещаются в `GLint`.
pub(super) fn gl_scissor(rect: TopLeftPixelRect, target_height: u32) -> GlScissor {
    let bottom = target_height - (rect.y + rect.height);
    GlScissor {
        x: saturating_gl_int(rect.x),
        y: saturating_gl_int(bottom),
        width: saturating_gl_int(rect.width),
        height: saturating_gl_int(rect.height),
    }
}

/// Разрешённая слою область в координатах `glScissor` (начало снизу). `None` — рисовать нечего
/// (`TargetArea::Nothing`); `Some(None)`-случая нет: вся цель выражается отдельным вариантом.
pub(super) enum PaintScissorLimit {
    /// Ограничения нет.
    Unrestricted,
    /// Только этот прямоугольник.
    Region(GlScissor),
    /// Рисовать нечего.
    Nothing,
}

/// Область перерисовки кадра → ограничение scissor-а художника.
pub(super) fn paint_scissor_limit(area: TargetArea, target_height: u32) -> PaintScissorLimit {
    match area {
        TargetArea::Whole => PaintScissorLimit::Unrestricted,
        TargetArea::Nothing => PaintScissorLimit::Nothing,
        TargetArea::Region(rect) => PaintScissorLimit::Region(gl_scissor(
            TopLeftPixelRect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            },
            target_height,
        )),
    }
}

/// Пересечение двух scissor-ов GL; `None` — пересечения нет.
pub(super) fn intersect_scissors(first: GlScissor, second: GlScissor) -> Option<GlScissor> {
    let left = first.x.max(second.x);
    let bottom = first.y.max(second.y);
    let right = first
        .x
        .saturating_add(first.width)
        .min(second.x.saturating_add(second.width));
    let top = first
        .y
        .saturating_add(first.height)
        .min(second.y.saturating_add(second.height));
    (right > left && top > bottom).then_some(GlScissor {
        x: left,
        y: bottom,
        width: right - left,
        height: top - bottom,
    })
}

fn saturating_gl_int(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use epaint::{Rect, pos2};

    use super::{TopLeftPixelRect, clip_rect_in_pixels, gl_scissor, intersect_scissors};
    use crate::draw_gl::GlScissor;

    #[test]
    fn clip_rect_rounds_scales_and_flips_y_for_gl() {
        let clip = Rect::from_min_max(pos2(10.2, 5.0), pos2(30.6, 20.4));
        let pixels = clip_rect_in_pixels(clip, 2.0, [200, 100]);
        assert_eq!(
            pixels,
            Some(TopLeftPixelRect {
                x: 20,
                y: 10,
                width: 41,
                height: 31
            })
        );
        // Сверху 10 строк, высота 31 → снизу остаётся 100 − 41 = 59.
        assert_eq!(
            pixels.map(|rect| gl_scissor(rect, 100)),
            Some(GlScissor {
                x: 20,
                y: 59,
                width: 41,
                height: 31
            })
        );
    }

    #[test]
    fn clip_rect_is_clamped_to_target_and_empty_area_is_skipped() {
        let everything = Rect::EVERYTHING;
        assert_eq!(
            clip_rect_in_pixels(everything, 1.0, [64, 32]),
            Some(TopLeftPixelRect {
                x: 0,
                y: 0,
                width: 64,
                height: 32
            })
        );
        let outside = Rect::from_min_max(pos2(100.0, 0.0), pos2(120.0, 10.0));
        assert_eq!(clip_rect_in_pixels(outside, 1.0, [64, 32]), None);
        let negative = Rect::from_min_max(pos2(-20.0, -20.0), pos2(-1.0, -1.0));
        assert_eq!(clip_rect_in_pixels(negative, 1.0, [64, 32]), None);
    }

    #[test]
    fn scissor_intersection_is_the_common_part_or_nothing() {
        let a = GlScissor {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        let b = GlScissor {
            x: 6,
            y: 4,
            width: 10,
            height: 10,
        };
        assert_eq!(
            intersect_scissors(a, b),
            Some(GlScissor {
                x: 6,
                y: 4,
                width: 4,
                height: 6
            })
        );
        let touching = GlScissor {
            x: 10,
            y: 0,
            width: 5,
            height: 5,
        };
        assert_eq!(intersect_scissors(a, touching), None);
    }
}
