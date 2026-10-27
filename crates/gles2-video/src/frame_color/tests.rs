use super::*;
use crate::color::{ColorAdjustment, YuvColorEncoding, YuvMatrix, YuvRange};
use crate::hdr::HdrToSdrConversion;
use crate::planes::{ChromaSubsampling, SampleDepth};

fn layout(chroma: ChromaSubsampling, depth: SampleDepth) -> PlanarLayout {
    PlanarLayout { chroma, depth }
}

fn sdr(range: YuvRange) -> VideoColorConversion {
    VideoColorConversion::Sdr {
        encoding: YuvColorEncoding {
            matrix: YuvMatrix::Bt709,
            range,
        },
        adjustment: ColorAdjustment::IDENTITY,
    }
}

fn hdr(transfer: HdrTransfer) -> VideoColorConversion {
    VideoColorConversion::HdrToSdr(HdrToSdrConversion {
        range: YuvRange::Limited,
        transfer,
        levels: HdrReferenceLevels::REFERENCE_DEFAULT,
    })
}

#[test]
fn eight_bit_sdr_keeps_the_per_layout_program_and_one_matrix() {
    let plan = plan_frame_color(
        layout(ChromaSubsampling::Yuv422, SampleDepth::Bits8),
        PlaneTextureFormat::RedExt,
        sdr(YuvRange::Limited),
    )
    .expect("8-bit SDR");
    assert_eq!(
        plan.variant,
        ProgramVariant::EightBit(ChromaSubsampling::Yuv422)
    );
    assert!(matches!(plan.uniforms, ColorUniformValues::EightBit(_)));
    assert_eq!(plan.hdr_table, None);
}

#[test]
fn high_bit_sdr_uses_one_program_for_every_layout_and_its_depth() {
    for chroma in [
        ChromaSubsampling::Yuv420,
        ChromaSubsampling::Yuv422,
        ChromaSubsampling::Yuv444,
    ] {
        let plan = plan_frame_color(
            layout(chroma, SampleDepth::Bits12),
            PlaneTextureFormat::LuminanceAlpha,
            sdr(YuvRange::Full),
        )
        .expect("12-bit SDR");
        assert_eq!(
            plan.variant,
            ProgramVariant::HighBitSdr(BytePairChannels::LuminanceAlpha)
        );
        let ColorUniformValues::HighBit {
            normalization,
            hdr_table,
            ..
        } = plan.uniforms
        else {
            panic!("high-bit uniforms expected, got {:?}", plan.uniforms);
        };
        assert_eq!(
            normalization,
            CodeNormalization::new(SampleDepth::Bits12, YuvRange::Full)
        );
        assert_eq!(hdr_table, None);
    }
}

#[test]
fn hdr_requires_the_table_of_its_transfer_and_levels() {
    let plan = plan_frame_color(
        layout(ChromaSubsampling::Yuv420, SampleDepth::Bits10),
        PlaneTextureFormat::LuminanceAlpha,
        hdr(HdrTransfer::Hlg),
    )
    .expect("10-bit HDR");
    assert_eq!(
        plan.variant,
        ProgramVariant::HighBitHdrTable(BytePairChannels::LuminanceAlpha)
    );
    let table = plan.hdr_table.expect("HDR needs a table");
    assert_eq!(
        table.key,
        HdrLutKey::new(HdrTransfer::Hlg, HdrReferenceLevels::REFERENCE_DEFAULT)
    );
    assert!(matches!(
        plan.uniforms,
        ColorUniformValues::HighBit {
            hdr_table: Some(_),
            ..
        }
    ));
}

#[test]
fn hdr_on_an_eight_bit_frame_is_a_typed_refusal() {
    assert_eq!(
        plan_frame_color(
            layout(ChromaSubsampling::Yuv420, SampleDepth::Bits8),
            PlaneTextureFormat::RedExt,
            hdr(HdrTransfer::Pq)
        ),
        Err(VideoDrawError::HdrToSdrNeedsHighBitFrame {
            depth: SampleDepth::Bits8
        })
    );
}

#[test]
fn nv12_sdr_uses_the_same_matrix_as_planar_eight_bit() {
    let nv12 = plan_nv12_color(sdr(YuvRange::Limited)).expect("NV12 SDR");
    let planar = plan_frame_color(
        layout(ChromaSubsampling::Yuv420, SampleDepth::Bits8),
        PlaneTextureFormat::Luminance,
        sdr(YuvRange::Limited),
    )
    .expect("planar SDR");
    assert_eq!(ColorUniformValues::EightBit(nv12), planar.uniforms);
}

#[test]
fn nv12_full_range_differs_from_limited() {
    let limited = plan_nv12_color(sdr(YuvRange::Limited)).expect("limited");
    let full = plan_nv12_color(sdr(YuvRange::Full)).expect("full");
    assert_ne!(limited, full);
}

#[test]
fn nv12_rejects_hdr_to_sdr_as_an_eight_bit_frame() {
    assert_eq!(
        plan_nv12_color(hdr(HdrTransfer::Pq)),
        Err(VideoDrawError::HdrToSdrNeedsHighBitFrame {
            depth: SampleDepth::Bits8
        })
    );
}

#[test]
fn high_bit_program_reads_byte_pairs_where_the_textures_keep_them() {
    // Загрузка — `LUMINANCE_ALPHA` (`.ra`), импорт без копии — `GR88` (`.rg`): та же разрядность
    // и цвет, другая программа.
    let ten_bit = layout(ChromaSubsampling::Yuv420, SampleDepth::Bits10);
    let uploaded = plan_frame_color(
        ten_bit,
        PlaneTextureFormat::LuminanceAlpha,
        sdr(YuvRange::Limited),
    )
    .expect("uploaded 10-bit");
    let imported = plan_frame_color(ten_bit, PlaneTextureFormat::RgExt, hdr(HdrTransfer::Pq))
        .expect("imported 10-bit");
    assert_eq!(
        uploaded.variant,
        ProgramVariant::HighBitSdr(BytePairChannels::LuminanceAlpha)
    );
    assert_eq!(
        imported.variant,
        ProgramVariant::HighBitHdrTable(BytePairChannels::RedGreen)
    );
}

#[test]
fn high_bit_frame_in_eight_bit_textures_is_a_typed_refusal() {
    let twelve_bit = layout(ChromaSubsampling::Yuv444, SampleDepth::Bits12);
    assert_eq!(
        plan_frame_color(twelve_bit, PlaneTextureFormat::RedExt, sdr(YuvRange::Full)),
        Err(VideoDrawError::HighBitFrameWithoutBytePairTextures {
            depth: SampleDepth::Bits12,
            format: PlaneTextureFormat::RedExt,
        })
    );
}
