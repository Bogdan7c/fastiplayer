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
        let plan = plan_frame_color(layout(chroma, SampleDepth::Bits12), sdr(YuvRange::Full))
            .expect("12-bit SDR");
        assert_eq!(plan.variant, ProgramVariant::HighBitSdr);
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
        hdr(HdrTransfer::Hlg),
    )
    .expect("10-bit HDR");
    assert_eq!(plan.variant, ProgramVariant::HighBitHdrTable);
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
            hdr(HdrTransfer::Pq)
        ),
        Err(VideoDrawError::HdrToSdrNeedsHighBitFrame {
            depth: SampleDepth::Bits8
        })
    );
}
