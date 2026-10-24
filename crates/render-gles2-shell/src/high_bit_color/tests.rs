use codec_core::HdrMetadata;

use super::*;

fn color(
    primaries: ColorPrimaries,
    matrix: MatrixCoefficients,
    transfer: TransferFunction,
    range: ColorRange,
) -> VideoColorMetadata {
    VideoColorMetadata {
        range,
        matrix,
        primaries,
        transfer,
        ..VideoColorMetadata::sdr_bt709_limited()
    }
}

fn hdr_color(transfer: TransferFunction, range: ColorRange) -> VideoColorMetadata {
    color(
        ColorPrimaries::Bt2020,
        MatrixCoefficients::Bt2020,
        transfer,
        range,
    )
}

fn mastering(primaries: ColorPrimaries, transfer: TransferFunction) -> HdrMetadata {
    HdrMetadata {
        color_primaries: primaries,
        transfer_function: transfer,
        max_luminance_nits: Some(1_000.0),
        min_luminance_nits: Some(0.005),
        max_content_light_level_nits: Some(1_000),
        max_frame_average_light_level_nits: Some(400),
    }
}

#[test]
fn sdr_bt709_with_explicit_range_draws_without_color_adjustment() {
    for (range, expected) in [
        (ColorRange::Limited, YuvRange::Limited),
        (ColorRange::Full, YuvRange::Full),
    ] {
        let conversion = high_bit_color_conversion(
            &color(
                ColorPrimaries::Bt709,
                MatrixCoefficients::Bt709,
                TransferFunction::Bt709,
                range,
            ),
            &HdrToSdrSettings::default(),
        );
        assert_eq!(
            conversion,
            Ok(VideoColorConversion::Sdr {
                encoding: YuvColorEncoding {
                    matrix: YuvMatrix::Bt709,
                    range: expected
                },
                adjustment: ColorAdjustment::IDENTITY,
            })
        );
    }
}

#[test]
fn hdr_pq_and_hlg_take_levels_from_settings() {
    let settings = HdrToSdrSettings {
        sdr_reference_white_nits: 120.0,
        hdr_reference_peak_nits: 2_000.0,
        ..HdrToSdrSettings::default()
    };
    for (transfer, expected) in [
        (TransferFunction::Pq, HdrTransfer::Pq),
        (TransferFunction::Hlg, HdrTransfer::Hlg),
    ] {
        let mut frame_color = hdr_color(transfer, ColorRange::Limited);
        frame_color.hdr_metadata = Some(mastering(ColorPrimaries::Bt2020, transfer));
        assert_eq!(
            high_bit_color_conversion(&frame_color, &settings),
            Ok(VideoColorConversion::HdrToSdr(HdrToSdrConversion {
                range: YuvRange::Limited,
                transfer: expected,
                levels: HdrReferenceLevels::new(120.0, 2_000.0).expect("valid levels"),
            }))
        );
    }
}

#[test]
fn hdr_with_disabled_or_invalid_tone_mapping_is_refused() {
    let frame_color = hdr_color(TransferFunction::Pq, ColorRange::Full);
    let disabled = HdrToSdrSettings {
        enabled: false,
        ..HdrToSdrSettings::default()
    };
    let inverted = HdrToSdrSettings {
        sdr_reference_white_nits: 500.0,
        hdr_reference_peak_nits: 400.0,
        ..HdrToSdrSettings::default()
    };
    for settings in [disabled, inverted] {
        assert_eq!(
            high_bit_color_conversion(&frame_color, &settings),
            Err(HighBitColorError::HdrToSdrUnavailable),
            "{settings:?}"
        );
    }
}

#[test]
fn hdr_metadata_outside_bt2446c_path_is_refused_with_its_reason() {
    let settings = HdrToSdrSettings::default();
    let mut wrong_primaries = hdr_color(TransferFunction::Pq, ColorRange::Limited);
    wrong_primaries.primaries = ColorPrimaries::Bt709;
    let mut wrong_matrix = hdr_color(TransferFunction::Hlg, ColorRange::Limited);
    wrong_matrix.matrix = MatrixCoefficients::Bt709;
    let mut mismatched = hdr_color(TransferFunction::Pq, ColorRange::Limited);
    mismatched.hdr_metadata = Some(mastering(ColorPrimaries::Bt2020, TransferFunction::Hlg));
    // HDR только в метаданных мастеринга, у самого кадра — SDR-передаточная функция.
    let mut transfer_only_in_metadata = hdr_color(TransferFunction::Bt709, ColorRange::Limited);
    transfer_only_in_metadata.hdr_metadata =
        Some(mastering(ColorPrimaries::Bt2020, TransferFunction::Pq));
    for (frame_color, reason) in [
        (
            wrong_primaries,
            HdrColorRejection::Primaries(ColorPrimaries::Bt709),
        ),
        (
            wrong_matrix,
            HdrColorRejection::Matrix(MatrixCoefficients::Bt709),
        ),
        (mismatched, HdrColorRejection::MasteringMismatch),
        (
            transfer_only_in_metadata,
            HdrColorRejection::Transfer(TransferFunction::Bt709),
        ),
    ] {
        assert_eq!(
            high_bit_color_conversion(&frame_color, &settings),
            Err(HighBitColorError::UnsupportedHdr(reason))
        );
    }
}

#[test]
fn wide_gamut_sdr_bt601_and_unknown_range_are_refused_like_wgpu() {
    let settings = HdrToSdrSettings::default();
    let bt2020_sdr = hdr_color(TransferFunction::Bt709, ColorRange::Limited);
    let bt601 = color(
        ColorPrimaries::Smpte170m,
        MatrixCoefficients::Bt601,
        TransferFunction::Bt709,
        ColorRange::Limited,
    );
    for frame_color in [bt2020_sdr, bt601] {
        assert!(matches!(
            high_bit_color_conversion(&frame_color, &settings),
            Err(HighBitColorError::UnsupportedColor { .. })
        ));
    }
    let unknown_range = color(
        ColorPrimaries::Bt709,
        MatrixCoefficients::Bt709,
        TransferFunction::Bt709,
        ColorRange::Unknown,
    );
    assert_eq!(
        high_bit_color_conversion(&unknown_range, &settings),
        Err(HighBitColorError::RangeUnknown)
    );
    assert_eq!(
        high_bit_color_conversion(
            &hdr_color(TransferFunction::Hlg, ColorRange::Unknown),
            &settings
        ),
        Err(HighBitColorError::RangeUnknown)
    );
}
