//! Значения uniform-ов кадра (чистая логика, без GL): что считает CPU один раз на кадр, чтобы
//! шейдер не делил и не ветвился на каждом пикселе.

use crate::frame_color::ColorUniformValues;
use crate::geometry::{PictureMapping, QuarterTurn};
use crate::planes::{ChromaSubsampling, PlaneExtent};
use crate::program::chroma_subsampled_axes;
use crate::upload_plan::TextureExtent;

/// Как шейдер берёт билинейную выборку плоскости: `clamp(source_uv × scale, min, max)` в
/// координатах текстуры, плюс `1 / размер текстуры` для центров текселей.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlaneSampling {
    /// Видимый размер / размер текстуры (текстура может быть шире: stride).
    pub(crate) scale: [f32; 2],
    /// Центр первого текселя.
    pub(crate) min: [f32; 2],
    /// Центр последнего видимого текселя (дальше — хвост строки или край).
    pub(crate) max: [f32; 2],
    /// `1 / размер текстуры`.
    pub(crate) texel_size: [f32; 2],
    /// Видимый размер в текселях.
    pub(crate) visible: [f32; 2],
}

impl PlaneSampling {
    /// Выборка плоскости с видимым размером `visible` в текстуре `allocated`.
    pub(crate) fn new(visible: PlaneExtent, allocated: TextureExtent) -> Self {
        let visible = [visible.width, visible.height].map(texel_count);
        let texture = [allocated.width, allocated.height].map(texel_count);
        let per_axis = |value: fn(f32, f32) -> f32| {
            [value(visible[0], texture[0]), value(visible[1], texture[1])]
        };
        Self {
            scale: per_axis(|visible, texture| visible / texture),
            min: per_axis(|_, texture| 0.5 / texture),
            max: per_axis(|visible, texture| (visible - 0.5) / texture),
            texel_size: per_axis(|_, texture| 1.0 / texture),
            visible,
        }
    }

    /// Nearest-выборка цветности 10/12 бит: по прореженной оси координата масштабируется так,
    /// что `floor(u × размер_текстуры) = floor(uv × яркость / 2)` — это `floor(floor(uv × яркость)
    /// / 2)` wgpu (для целого делителя `floor(floor(a) / 2) = floor(a / 2)`), в том числе при
    /// нечётной ширине, где `ceil(яркость / 2) ≠ яркость / 2`. Границы — центры первого и
    /// последнего видимого текселя.
    pub(crate) fn nearest_chroma(
        chroma: Self,
        luma_visible: PlaneExtent,
        subsampling: ChromaSubsampling,
    ) -> Self {
        let (subsampled_x, subsampled_y) = chroma_subsampled_axes(subsampling);
        let axis_scale = |subsampled: bool, luma_side: u32, axis: usize| {
            if subsampled {
                // Половина яркости в текселях текстуры цветности.
                texel_count(luma_side) * 0.5 * chroma.texel_size[axis]
            } else {
                chroma.scale[axis]
            }
        };
        Self {
            scale: [
                axis_scale(subsampled_x, luma_visible.width, 0),
                axis_scale(subsampled_y, luma_visible.height, 1),
            ],
            ..chroma
        }
    }
}

/// Число текселей как `f32`: стороны текстур GLES2 не больше `GL_MAX_TEXTURE_SIZE` (единицы
/// тысяч), в `f32` они точны.
fn texel_count(side: u32) -> f32 {
    // Нулевая сторона невозможна (кадр без строк отклоняет загрузка); 1 не даёт делить на 0.
    side.max(1) as f32
}

/// Все uniform-ы кадра в порядке упаковки шейдера.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DrawUniformValues {
    /// `xy` — uv_scale, `zw` — uv_offset.
    pub(crate) display_mapping: [f32; 4],
    /// Строки `display_uv → source_uv`.
    pub(crate) orientation_rows: [[f32; 3]; 2],
    /// `xy` — scale, `zw` — min яркости.
    pub(crate) luma_coord_scale_min: [f32; 4],
    /// `xy` — max, `zw` — видимый размер яркости.
    pub(crate) luma_coord_max_size: [f32; 4],
    /// `xy` — scale, `zw` — min цветности.
    pub(crate) chroma_coord_scale_min: [f32; 4],
    /// `xy` — max, `zw` — номер последнего видимого текселя цветности.
    pub(crate) chroma_coord_max_last: [f32; 4],
    /// `1 / размер текстуры цветности`.
    pub(crate) chroma_texel_size: [f32; 2],
    /// Цвет.
    pub(crate) color: ColorUniformValues,
}

impl DrawUniformValues {
    /// Uniform-ы для плоскостей яркости `luma` и цветности `chroma` (U и V одного размера).
    pub(crate) fn new(
        luma: PlaneSampling,
        chroma: PlaneSampling,
        picture: PictureMapping,
        orientation: QuarterTurn,
        color: ColorUniformValues,
    ) -> Self {
        let [uv_scale_x, uv_scale_y] = picture.uv_scale;
        let [uv_offset_x, uv_offset_y] = picture.uv_offset;
        Self {
            display_mapping: [uv_scale_x, uv_scale_y, uv_offset_x, uv_offset_y],
            orientation_rows: orientation.display_to_source_rows(),
            luma_coord_scale_min: [luma.scale[0], luma.scale[1], luma.min[0], luma.min[1]],
            luma_coord_max_size: [luma.max[0], luma.max[1], luma.visible[0], luma.visible[1]],
            chroma_coord_scale_min: [
                chroma.scale[0],
                chroma.scale[1],
                chroma.min[0],
                chroma.min[1],
            ],
            chroma_coord_max_last: [
                chroma.max[0],
                chroma.max[1],
                chroma.visible[0] - 1.0,
                chroma.visible[1] - 1.0,
            ],
            chroma_texel_size: chroma.texel_size,
            color,
        }
    }
}

#[cfg(test)]
mod tests;
