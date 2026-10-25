//! Вершины квадов оверлея — чистый CPU.
//!
//! GLES 2.0 без инстансинга: каждая картинка — два треугольника (6 вершин), её атрибуты
//! повторены на всех вершинах (интерполяция одинаковых значений безвредна — замена
//! `@interpolate(flat)` wgpu). Вершина (24 байта):
//!
//! | Смещение | Атрибут | Тип | Смысл |
//! |---|---|---|---|
//! | 0 | `a_canvas_position` | 2 × f32 | угол квада в пикселях холста |
//! | 8 | `a_atlas_uv` | 2 × f32 | тот же угол в нормированных координатах атласа |
//! | 16 | `a_tint` | 4 × u8 (норм.) | прямой цвет картинки |
//! | 20 | `a_group` | f32 | 0 — основная группа, 1 — фиксированная |
//!
//! Координаты атласа в углах квада интерполируются линейно, поэтому в центре пикселя холста
//! `n + 0.5` получается центр текселя `(slot + n + 0.5) / side` — выборке `NEAREST` не нужна
//! адресная арифметика (и зажим `clamp` шейдера wgpu: центры пикселей внутри квада всегда внутри
//! картинки, ведь квад лежит на целых пикселях).

use crate::gl_calls::overlay::OverlayVertexLayout;

use super::OverlayPlacementGroup;
use super::atlas::AtlasSlot;
use super::{OverlayBitmapRect, OverlayTint};

/// Вершин на картинку: два треугольника.
pub(super) const VERTICES_PER_BITMAP: usize = 6;

/// Раскладка вершины (см. таблицу модуля).
pub(super) const VERTEX_LAYOUT: OverlayVertexLayout = OverlayVertexLayout {
    stride: 24,
    canvas_position_offset: 0,
    atlas_uv_offset: 8,
    tint_offset: 16,
    group_offset: 20,
};

/// Углы квада `(доля ширины, доля высоты)` для двух треугольников: (0,0)(1,0)(0,1) и
/// (1,0)(1,1)(0,1). Направление обхода не важно: отбраковка выключена.
const QUAD_CORNERS: [(u32, u32); VERTICES_PER_BITMAP] =
    [(0, 0), (1, 0), (0, 1), (1, 0), (1, 1), (0, 1)];

/// Значение `a_group` шейдера: 0 — основная группа, 1 — фиксированная (`mix` углов холста).
const fn group_selector(group: OverlayPlacementGroup) -> f32 {
    match group {
        OverlayPlacementGroup::Main => 0.0,
        OverlayPlacementGroup::Fixed => 1.0,
    }
}

/// Картинка, готовая к записи в буфер вершин.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct BitmapQuad {
    /// Где картинка на холсте.
    pub(super) rect: OverlayBitmapRect,
    /// Где её пиксели в атласе.
    pub(super) slot: AtlasSlot,
    /// Цвет.
    pub(super) tint: OverlayTint,
    /// Группа размещения.
    pub(super) group: OverlayPlacementGroup,
}

/// Дописывает 6 вершин картинки в `bytes` (атлас стороной `atlas_side`).
///
/// Числа холста и атласа — целые меньше 2²⁴, в `f32` они точны; деление на сторону атласа степени
/// двойки тоже точно, на другую сторону ошибается меньше 10⁻⁷ — много меньше половины текселя.
pub(super) fn append_bitmap_quad(bytes: &mut Vec<u8>, quad: &BitmapQuad, atlas_side: u32) {
    #[expect(
        clippy::cast_precision_loss,
        reason = "координаты холста и атласа меньше 2^24 (предел текстуры GL), в f32 они точны"
    )]
    let to_float = |value: i64| value as f32;
    let side = to_float(i64::from(atlas_side));
    let tint = [
        quad.tint.red,
        quad.tint.green,
        quad.tint.blue,
        quad.tint.alpha,
    ];
    let group = group_selector(quad.group);
    for (width_share, height_share) in QUAD_CORNERS {
        let dx = i64::from(quad.rect.width * width_share);
        let dy = i64::from(quad.rect.height * height_share);
        let canvas = [
            to_float(i64::from(quad.rect.x) + dx),
            to_float(i64::from(quad.rect.y) + dy),
        ];
        let atlas = [
            to_float(i64::from(quad.slot.x) + dx) / side,
            to_float(i64::from(quad.slot.y) + dy) / side,
        ];
        for value in canvas.into_iter().chain(atlas) {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        bytes.extend_from_slice(&tint);
        bytes.extend_from_slice(&group.to_ne_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn float_at(bytes: &[u8], offset: usize) -> f32 {
        let mut word = [0_u8; 4];
        word.copy_from_slice(&bytes[offset..offset + 4]);
        f32::from_ne_bytes(word)
    }

    fn quad(group: OverlayPlacementGroup) -> BitmapQuad {
        BitmapQuad {
            rect: OverlayBitmapRect {
                x: -3,
                y: 80,
                width: 40,
                height: 16,
            },
            slot: AtlasSlot { x: 64, y: 32 },
            tint: OverlayTint {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 200,
            },
            group,
        }
    }

    #[test]
    fn quad_writes_six_vertices_with_canvas_and_atlas_corners() {
        let mut bytes = vec![0xAA];
        append_bitmap_quad(&mut bytes, &quad(OverlayPlacementGroup::Fixed), 256);
        assert_eq!(
            bytes.len(),
            1 + VERTICES_PER_BITMAP * VERTEX_LAYOUT.stride,
            "дописывает, а не перезаписывает"
        );
        let body = &bytes[1..];
        let vertex = |index: usize| &body[index * VERTEX_LAYOUT.stride..];
        // Вершина 0 — левый верх, вершина 4 — правый низ.
        let top_left = vertex(0);
        assert_eq!([float_at(top_left, 0), float_at(top_left, 4)], [-3.0, 80.0]);
        assert_eq!(
            [float_at(top_left, 8), float_at(top_left, 12)],
            [64.0 / 256.0, 32.0 / 256.0]
        );
        assert_eq!(&top_left[16..20], &[1, 2, 3, 200]);
        assert_eq!(float_at(top_left, 20), 1.0, "фиксированная группа");
        let bottom_right = vertex(4);
        assert_eq!(
            [float_at(bottom_right, 0), float_at(bottom_right, 4)],
            [37.0, 96.0]
        );
        assert_eq!(
            [float_at(bottom_right, 8), float_at(bottom_right, 12)],
            [104.0 / 256.0, 48.0 / 256.0]
        );
    }

    #[test]
    fn main_group_selects_the_first_origin() {
        let mut bytes = Vec::new();
        append_bitmap_quad(&mut bytes, &quad(OverlayPlacementGroup::Main), 512);
        for vertex in bytes.as_chunks::<{ VERTEX_LAYOUT.stride }>().0 {
            assert_eq!(float_at(vertex, 20), 0.0);
        }
    }

    #[test]
    fn atlas_corners_hit_texel_centres_at_pixel_centres() {
        // Линейная интерполяция между углами: в центре пикселя n холста координата атласа равна
        // центру текселя slot + n + 0.5 — то, что выбирает NEAREST.
        let mut bytes = Vec::new();
        append_bitmap_quad(&mut bytes, &quad(OverlayPlacementGroup::Main), 256);
        let left = float_at(&bytes, 8);
        let right = float_at(&bytes[VERTEX_LAYOUT.stride..], 8);
        let width = 40.0_f32;
        for pixel in 0..40_u8 {
            let share = (f32::from(pixel) + 0.5) / width;
            let atlas_texel = (left + (right - left) * share) * 256.0;
            assert!(
                (atlas_texel - (64.0 + f32::from(pixel) + 0.5)).abs() < 1e-3,
                "pixel {pixel}: {atlas_texel}"
            );
        }
    }
}
