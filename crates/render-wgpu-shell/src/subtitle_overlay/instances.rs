//! Данные инстансов (по одному на картинку) и геометрия размещения на экране.
//!
//! Чистая логика без GPU: превращает `SubtitleBitmap` + слот атласа в 28 байт для
//! vertex buffer-а и переводит прямоугольник картинки видео в целый пиксельный origin.

use render_core::PictureRect;
use render_shell_api::{SubtitleLayerOffset, SubtitleLayerOffsets};
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

/// Левый верхний угол холста субтитров на экране: origin картинки видео плюс сдвиг слоя,
/// округлённый до целого пикселя surface.
///
/// Холст растеризуется 1:1 к пикселям экрана, поэтому дробный origin дал бы «мыло» от
/// интерполяции; округление гарантирует, что маска ложится ровно на пиксельную сетку.
/// Округляется сумма (а не слагаемые): иначе сдвиг `0.4 + 0.4` потерялся бы дважды.
/// `None` для нечисловых координат или сдвига (защита от NaN/inf, а не штатный случай).
pub(super) fn placement_origin(
    picture_rect: PictureRect,
    layer_offset: SubtitleLayerOffset,
) -> Option<[f32; 2]> {
    let x = picture_rect.x + layer_offset.x;
    let y = picture_rect.y + layer_offset.y;
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    Some([x.round(), y.round()])
}

/// Углы холста для обеих групп: `[основная, фиксированная]`; `None`, если любой сдвиг
/// нечисловой (см. [`placement_origin`]).
pub(super) fn placement_origins(
    picture_rect: PictureRect,
    offsets: SubtitleLayerOffsets,
) -> Option<[[f32; 2]; 2]> {
    Some([
        placement_origin(picture_rect, offsets.main)?,
        placement_origin(picture_rect, offsets.fixed)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32) -> PictureRect {
        PictureRect {
            x,
            y,
            width: 100.0,
            height: 50.0,
        }
    }

    #[test]
    fn placement_origin_rounds_to_whole_surface_pixels() {
        let none = SubtitleLayerOffset::NONE;
        assert_eq!(placement_origin(rect(10.4, 18.5), none), Some([10.0, 19.0]));
        assert_eq!(placement_origin(rect(0.0, 17.6), none), Some([0.0, 18.0]));
    }

    #[test]
    fn placement_origin_rejects_non_finite_coordinates() {
        let none = SubtitleLayerOffset::NONE;
        assert_eq!(placement_origin(rect(f32::NAN, 0.0), none), None);
        assert_eq!(placement_origin(rect(0.0, f32::INFINITY), none), None);
        let nan_offset = SubtitleLayerOffset {
            x: f32::NAN,
            y: 0.0,
        };
        assert_eq!(placement_origin(rect(0.0, 0.0), nan_offset), None);
        let inf_offset = SubtitleLayerOffset {
            x: 0.0,
            y: f32::NEG_INFINITY,
        };
        assert_eq!(placement_origin(rect(0.0, 0.0), inf_offset), None);
    }

    #[test]
    fn group_origins_follow_their_own_offsets() {
        let offsets = SubtitleLayerOffsets {
            main: SubtitleLayerOffset { x: 0.0, y: -30.0 },
            fixed: SubtitleLayerOffset::NONE,
        };
        assert_eq!(
            placement_origins(rect(10.0, 20.0), offsets),
            Some([[10.0, -10.0], [10.0, 20.0]])
        );
        let broken = SubtitleLayerOffsets {
            main: SubtitleLayerOffset::NONE,
            fixed: SubtitleLayerOffset {
                x: f32::NAN,
                y: 0.0,
            },
        };
        assert_eq!(placement_origins(rect(0.0, 0.0), broken), None);
        assert_eq!(
            SubtitleLayerOffsets::uniform(SubtitleLayerOffset { x: 1.0, y: 2.0 }).fixed,
            SubtitleLayerOffset { x: 1.0, y: 2.0 }
        );
    }

    #[test]
    fn placement_origin_adds_the_layer_offset_before_rounding() {
        let offset = SubtitleLayerOffset { x: 0.4, y: -30.4 };
        // 10.4 + 0.4 = 10.8 -> 11; 18.5 - 30.4 = -11.9 -> -12 (слой может уйти выше картинки).
        assert_eq!(
            placement_origin(rect(10.4, 18.5), offset),
            Some([11.0, -12.0])
        );
    }

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
