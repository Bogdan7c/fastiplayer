use super::*;
use crate::color::{ColorAdjustment, ColorTransform, YuvColorEncoding};

#[test]
fn exact_texture_samples_like_clamp_to_edge() {
    let sampling = PlaneSampling::new(
        PlaneExtent {
            width: 4,
            height: 2,
        },
        TextureExtent {
            width: 4,
            height: 2,
        },
    );
    assert_eq!(sampling.scale, [1.0, 1.0]);
    // Центры крайних текселей: CLAMP_TO_EDGE даёт за ними то же значение.
    assert_eq!(sampling.min, [0.125, 0.25]);
    assert_eq!(sampling.max, [0.875, 0.75]);
    assert_eq!(sampling.texel_size, [0.25, 0.5]);
}

#[test]
fn stride_wide_texture_stops_before_the_row_tail() {
    // Видно 5 текселей из 8: координата не заходит дальше центра пятого.
    let sampling = PlaneSampling::new(
        PlaneExtent {
            width: 5,
            height: 3,
        },
        TextureExtent {
            width: 8,
            height: 3,
        },
    );
    assert_eq!(sampling.scale, [5.0 / 8.0, 1.0]);
    assert_eq!(sampling.max[0], 4.5 / 8.0);
    assert!(
        sampling.max[0] * 8.0 < 5.0,
        "never samples texel 5 (row tail)"
    );
}

#[test]
fn uniforms_pack_letterbox_orientation_vertex_scales_and_clamps() {
    let luma = PlaneSampling::new(
        PlaneExtent {
            width: 6,
            height: 4,
        },
        TextureExtent {
            width: 6,
            height: 4,
        },
    );
    let chroma = PlaneSampling::new(
        PlaneExtent {
            width: 3,
            height: 2,
        },
        TextureExtent {
            width: 4,
            height: 2,
        },
    );
    let values = DrawUniformValues::new(
        luma,
        chroma,
        PictureMapping {
            uv_scale: [1.0, 2.0],
            uv_offset: [0.0, -0.5],
        },
        QuarterTurn::Clockwise90,
        ColorUniformValues::EightBit(ColorTransform::new(
            YuvColorEncoding::SDR_BT709_LIMITED,
            ColorAdjustment::IDENTITY,
        )),
    );
    assert_eq!(values.display_mapping, [1.0, 2.0, 0.0, -0.5]);
    assert_eq!(
        values.orientation_rows,
        QuarterTurn::Clockwise90.display_to_source_rows()
    );
    // Координаты выборок — в вершинах: видимая часть / размер текстуры.
    assert_eq!(values.luma_uv_scale, [1.0, 1.0]);
    assert_eq!(values.chroma_uv_scale, [0.75, 1.0]);
    // Зажим (только для текстур шире видимой части): центры первого и последнего видимого.
    assert_eq!(
        values.luma_coord_clamp,
        [0.5 / 6.0, 0.5 / 4.0, 5.5 / 6.0, 3.5 / 4.0]
    );
    assert_eq!(values.chroma_coord_clamp, [0.125, 0.25, 0.625, 0.75]);
    // Co-sited 4:2:2 по X: ширина яркости, последний видимый тексель цветности, 1 / ширина.
    assert_eq!(values.chroma_co_sited_x, [6.0, 2.0, 0.25]);
}

/// Тексель, который выберет `GL_NEAREST` для координаты `coord` в текстуре ширины `texture`.
fn nearest_texel(coord: f32, texture: u32) -> u32 {
    (coord * texture as f32).floor() as u32
}

#[test]
fn nearest_chroma_picks_the_wgpu_texel_for_odd_and_stride_wide_planes() {
    // Яркость 5×3, цветность 4:2:0 3×2 в текстуре шириной 4 (stride с хвостом).
    let luma_visible = PlaneExtent {
        width: 5,
        height: 3,
    };
    let chroma = PlaneSampling::new(
        PlaneExtent {
            width: 3,
            height: 2,
        },
        TextureExtent {
            width: 4,
            height: 2,
        },
    );
    let sampling = PlaneSampling::nearest_chroma(chroma, luma_visible, ChromaSubsampling::Yuv420);
    // Пиксели цели 0..50 по ширине: source_uv в центрах пикселей.
    for pixel in 0..50_u32 {
        let uv = (pixel as f32 + 0.5) / 50.0;
        let coord = (uv * sampling.scale[0]).clamp(sampling.min[0], sampling.max[0]);
        // wgpu: min(floor(floor(uv × 5) / 2), 3 − 1).
        let luma_texel = (uv * 5.0).floor() as u32;
        let expected = (luma_texel / 2).min(2);
        assert_eq!(nearest_texel(coord, 4), expected, "pixel {pixel}, uv {uv}");
    }
}

#[test]
fn nearest_chroma_keeps_full_resolution_axes() {
    let luma_visible = PlaneExtent {
        width: 6,
        height: 4,
    };
    let chroma = PlaneSampling::new(
        PlaneExtent {
            width: 3,
            height: 4,
        },
        TextureExtent {
            width: 3,
            height: 4,
        },
    );
    let sampling = PlaneSampling::nearest_chroma(chroma, luma_visible, ChromaSubsampling::Yuv422);
    assert_eq!(
        sampling.scale,
        [1.0, 1.0],
        "4:2:2: x by half luma (3/3), y as is"
    );
    let full = PlaneSampling::nearest_chroma(luma_like(), luma_visible, ChromaSubsampling::Yuv444);
    assert_eq!(full.scale, luma_like().scale, "4:4:4 is the luma sampling");
}

fn luma_like() -> PlaneSampling {
    PlaneSampling::new(
        PlaneExtent {
            width: 6,
            height: 4,
        },
        TextureExtent {
            width: 8,
            height: 4,
        },
    )
}
