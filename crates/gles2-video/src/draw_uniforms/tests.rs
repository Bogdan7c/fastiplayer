use super::*;

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
fn uniforms_pack_letterbox_orientation_and_last_chroma_texel() {
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
        YuvColorEncoding::SDR_BT709_LIMITED,
        ColorAdjustment::IDENTITY,
    );
    assert_eq!(values.display_mapping, [1.0, 2.0, 0.0, -0.5]);
    assert_eq!(
        values.orientation_rows,
        QuarterTurn::Clockwise90.display_to_source_rows()
    );
    assert_eq!(values.luma_coord_max_size[2..], [6.0, 4.0]);
    assert_eq!(values.chroma_coord_max_last[2..], [2.0, 1.0]);
    assert_eq!(values.chroma_texel_size, [0.25, 0.5]);
}
