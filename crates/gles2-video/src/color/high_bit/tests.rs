use super::*;

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= 1e-6,
        "{what}: {actual} vs {expected}"
    );
}

#[test]
fn ten_bit_limited_codes_follow_wgpu_ranges_and_clamps() {
    let normalization = CodeNormalization::new(SampleDepth::Bits10, YuvRange::Limited);
    let cases = [
        // (коды Y, U, V) → нормализованные (Y, U, V)
        ([64, 512, 512], [0.0, 0.0, 0.0]),
        ([940, 960, 64], [1.0, 0.5, -0.5]),
        // Y за пределами студийного диапазона ограничивается, U/V — только 0..max.
        ([0, 0, 1023], [0.0, -512.0 / 896.0, 511.0 / 896.0]),
        ([1023, 1023, 0], [1.0, 511.0 / 896.0, -512.0 / 896.0]),
    ];
    for (codes, expected) in cases {
        let actual = normalization.apply(codes);
        for channel in 0..3 {
            assert_close(
                actual[channel],
                expected[channel],
                &format!("{codes:?}[{channel}]"),
            );
        }
    }
}

#[test]
fn twelve_bit_full_codes_use_max_code_as_divisor() {
    let normalization = CodeNormalization::new(SampleDepth::Bits12, YuvRange::Full);
    let actual = normalization.apply([4095, 2048, 0]);
    assert_close(actual[0], 1.0, "Y max");
    assert_close(actual[1], 0.0, "U centre");
    assert_close(actual[2], -2048.0 / 4095.0, "V zero");
    // Лимитный 12 бит: 16·16 и 235·16.
    let limited = CodeNormalization::new(SampleDepth::Bits12, YuvRange::Limited);
    assert_close(limited.apply([256, 2048, 2048])[0], 0.0, "12-bit black");
    assert_close(limited.apply([3760, 2048, 2048])[0], 1.0, "12-bit white");
}

#[test]
fn bt709_normalized_rows_are_the_wgpu_16bit_coefficients() {
    let rows = normalized_yuv_to_rgb(YuvMatrix::Bt709).linear;
    assert_eq!(rows[0], [1.0, 0.0, 1.574_8]);
    assert_eq!(rows[1], [1.0, -0.187_324_27, -0.468_124_27]);
    assert_eq!(rows[2], [1.0, 1.855_6, 0.0]);
}

#[test]
fn bt2020_matrix_inverts_the_bt2020_encoding_of_primaries() {
    let matrix = bt2020_normalized_yuv_to_rgb();
    for rgb in [
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.5, 0.5, 0.5],
    ] {
        let luma = BT2020_KR * rgb[0] + BT2020_KG * rgb[1] + BT2020_KB * rgb[2];
        let cb = (rgb[2] - luma) / (2.0 * (1.0 - BT2020_KB));
        let cr = (rgb[0] - luma) / (2.0 * (1.0 - BT2020_KR));
        let decoded = matrix.apply([luma, cb, cr]);
        for channel in 0..3 {
            assert_close(
                decoded[channel],
                rgb[channel],
                &format!("{rgb:?}[{channel}]"),
            );
        }
    }
}
