//! Данные инстансов (по одному на картинку) и геометрия размещения на экране.
//!
//! Чистая логика без GPU: превращает `SubtitleBitmap` + слот атласа в 32 байта для
//! vertex buffer-а. Угол холста на экране считает контракт
//! (`render_shell_api::SubtitleCanvasOrigins`) — одна формула для wgpu и GLES2.

use subtitle_raster::{OverlayPlacementGroup, PixelPoint, SubtitleBitmap};

use super::atlas::AtlasSlot;

/// Размер одного инстанса в байтах: rect (4 x f32) + атлас (2 x u32) + цвет (4 x u8) +
/// индекс группы размещения (u32).
pub(super) const INSTANCE_STRIDE_BYTES: usize = 32;

/// Индекс группы в uniform-е шейдера: 0 — основная, 1 — фиксированная.
/// Должен совпадать с выбором origin-а в `subtitle_overlay.wgsl`.
fn group_shader_index(group: OverlayPlacementGroup) -> u32 {
    match group {
        OverlayPlacementGroup::Main => 0,
        OverlayPlacementGroup::Fixed => 1,
    }
}

/// Один инстанс: где картинка на холсте, где её маска в атласе и каким цветом заливать.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct BitmapInstance {
    /// `[x, y, width, height]` в пикселях холста субтитров (не экрана).
    pub(super) canvas_rect: [f32; 4],
    /// Левый верхний угол маски в атласе, пиксели.
    pub(super) atlas_origin: [u32; 2],
    /// Прямой RGBA цвет слоя, как отдал растеризатор (sRGB-значения, без линеаризации).
    pub(super) color_rgba: [u8; 4],
    /// Индекс группы размещения (см. [`group_shader_index`]): выбирает сдвиг в шейдере.
    pub(super) group_index: u32,
}

impl BitmapInstance {
    /// Собирает инстанс из геометрии холста, слота в атласе и цвета.
    pub(super) fn from_parts(
        origin: PixelPoint,
        width: u32,
        height: u32,
        slot: AtlasSlot,
        color_rgba: [u8; 4],
        group: OverlayPlacementGroup,
    ) -> Self {
        Self {
            // Размеры холста ограничены `max_canvas_side` растеризатора, поэтому f32
            // представляет их точно (до 2^24).
            canvas_rect: [
                origin.x as f32,
                origin.y as f32,
                width as f32,
                height as f32,
            ],
            atlas_origin: [slot.x, slot.y],
            color_rgba,
            group_index: group_shader_index(group),
        }
    }

    /// Инстанс настоящей картинки растеризатора.
    pub(super) fn from_bitmap(bitmap: &SubtitleBitmap, slot: AtlasSlot) -> Self {
        let color = bitmap.color();
        Self::from_parts(
            bitmap.origin(),
            bitmap.width(),
            bitmap.height(),
            slot,
            [color.red, color.green, color.blue, color.alpha],
            bitmap.placement_group(),
        )
    }

    /// Дописывает инстанс в байтовый буфер в раскладке vertex buffer-а (little/native endian).
    pub(super) fn append_bytes(&self, bytes: &mut Vec<u8>) {
        for value in self.canvas_rect {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        for value in self.atlas_origin {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        bytes.extend_from_slice(&self.color_rgba);
        bytes.extend_from_slice(&self.group_index.to_ne_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_keeps_canvas_geometry_atlas_slot_and_color() {
        let instance = BitmapInstance::from_parts(
            PixelPoint { x: 12, y: 80 },
            40,
            16,
            AtlasSlot { x: 64, y: 32 },
            [1, 2, 3, 200],
            OverlayPlacementGroup::Fixed,
        );
        assert_eq!(instance.canvas_rect, [12.0, 80.0, 40.0, 16.0]);
        assert_eq!(instance.atlas_origin, [64, 32]);
        assert_eq!(instance.color_rgba, [1, 2, 3, 200]);
        assert_eq!(instance.group_index, 1);
    }

    #[test]
    fn instance_bytes_follow_vertex_layout_in_order() {
        let instance = BitmapInstance::from_parts(
            PixelPoint { x: 3, y: 4 },
            5,
            6,
            AtlasSlot { x: 7, y: 8 },
            [9, 10, 11, 12],
            OverlayPlacementGroup::Main,
        );
        let mut bytes = vec![0xAA];
        instance.append_bytes(&mut bytes);
        assert_eq!(
            bytes.len(),
            1 + INSTANCE_STRIDE_BYTES,
            "appends, not overwrites"
        );
        let body = &bytes[1..];
        let float_at =
            |offset: usize| f32::from_ne_bytes(body[offset..offset + 4].try_into().unwrap());
        let uint_at =
            |offset: usize| u32::from_ne_bytes(body[offset..offset + 4].try_into().unwrap());
        assert_eq!(
            [float_at(0), float_at(4), float_at(8), float_at(12)],
            [3.0, 4.0, 5.0, 6.0]
        );
        assert_eq!([uint_at(16), uint_at(20)], [7, 8]);
        assert_eq!(&body[24..28], &[9, 10, 11, 12]);
        assert_eq!(uint_at(28), 0, "индекс группы Main");
    }
}
