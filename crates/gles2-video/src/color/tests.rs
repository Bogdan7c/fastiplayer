use super::*;

/// Пошаговая формула wgpu в `f32` (транскрипция `host_yuv420_8bit_to_rgba.wgsl`) до `clamp`.
fn wgpu_stepwise(
    encoding: YuvColorEncoding,
    adjustment: ColorAdjustment,
    yuv: [f32; 3],
) -> [f32; 3] {
    let (luma_offset, luma_scale) = match encoding.range {
        YuvRange::Limited => (0.0625_f32, 1.164_383_6_f32),
        YuvRange::Full => (0.0, 1.0),
    };
    let rows = matrix_rows(encoding).map(|row| row.map(|value| value as f32));
    let weights = match encoding.matrix {
        YuvMatrix::Bt601 => [0.299_f32, 0.587, 0.114],
        YuvMatrix::Bt709 => [0.212_6_f32, 0.715_2, 0.072_2],
    };
    let normalized = [
        (yuv[0] - luma_offset) * luma_scale,
        yuv[1] - 0.5,
        yuv[2] - 0.5,
    ];
    let rgb =
        rows.map(|row| row[0] * normalized[0] + row[1] * normalized[1] + row[2] * normalized[2]);
    let luma = rgb[0] * weights[0] + rgb[1] * weights[1] + rgb[2] * weights[2];
    let exposure = adjustment.exposure.exp2();
    [0, 1, 2].map(|channel| {
        let saturated = luma + (rgb[channel] - luma) * adjustment.saturation;
        let contrasted = (saturated - 0.5) * adjustment.contrast + 0.5;
        (contrasted * exposure + adjustment.brightness) * adjustment.rgb_gain[channel]
            + adjustment.rgb_offset[channel]
    })
}

fn apply(transform: &ColorTransform, yuv: [f32; 3]) -> [f32; 3] {
    transform
        .rows
        .map(|row| row[0] * yuv[0] + row[1] * yuv[1] + row[2] * yuv[2] + row[3])
}

const ENCODINGS: [YuvColorEncoding; 4] = [
    YuvColorEncoding {
        matrix: YuvMatrix::Bt709,
        range: YuvRange::Limited,
    },
    YuvColorEncoding {
        matrix: YuvMatrix::Bt709,
        range: YuvRange::Full,
    },
    YuvColorEncoding {
        matrix: YuvMatrix::Bt601,
        range: YuvRange::Limited,
    },
    YuvColorEncoding {
        matrix: YuvMatrix::Bt601,
        range: YuvRange::Full,
    },
];

/// Одна матрица на кадр даёт то же, что пошаговая формула wgpu, с ошибкой на три порядка меньше
/// шага 1/255 — для всех кодировок, ручек цветокоррекции и отсчётов по всему кубу YUV.
#[test]
fn folded_transform_matches_wgpu_stepwise_formula() {
    let identity = ColorAdjustment::IDENTITY;
    let adjustments = [
        identity,
        ColorAdjustment {
            brightness: 0.2,
            ..identity
        },
        ColorAdjustment {
            contrast: 1.7,
            ..identity
        },
        ColorAdjustment {
            saturation: 0.0,
            ..identity
        },
        ColorAdjustment {
            saturation: 2.0,
            ..identity
        },
        ColorAdjustment {
            exposure: 1.5,
            ..identity
        },
        ColorAdjustment {
            exposure: -2.0,
            ..identity
        },
        ColorAdjustment {
            rgb_gain: [1.4, 0.5, 0.9],
            rgb_offset: [0.1, -0.05, 0.0],
            ..identity
        },
        ColorAdjustment {
            brightness: -0.05,
            contrast: 1.3,
            saturation: 1.2,
            exposure: 0.4,
            rgb_gain: [0.9, 1.1, 1.0],
            rgb_offset: [0.02, 0.0, -0.03],
        },
    ];
    let mut worst = 0.0_f32;
    for encoding in ENCODINGS {
        for adjustment in adjustments {
            let transform = ColorTransform::new(encoding, adjustment);
            for y in (0..=255).step_by(15) {
                for u in (0..=255).step_by(17) {
                    for v in (0..=255).step_by(17) {
                        let yuv = [y, u, v].map(|sample: i32| sample as f32 / 255.0);
                        let expected = wgpu_stepwise(encoding, adjustment, yuv);
                        let actual = apply(&transform, yuv);
                        for channel in 0..3 {
                            worst = worst.max((expected[channel] - actual[channel]).abs());
                        }
                    }
                }
            }
        }
    }
    assert!(
        worst < 1e-5,
        "folded transform deviates by {worst}, must stay far below 1/255"
    );
}

#[test]
fn yuv_to_rgb_alone_maps_studio_white_and_black() {
    let limited = yuv_to_rgb(ENCODINGS[0]);
    let black = limited.apply([16.0 / 255.0, 0.5, 0.5]);
    let white = limited.apply([235.0 / 255.0, 0.5, 0.5]);
    // 16/256 вместо 16/255 (историческая формула): чёрный чуть выше нуля, белый чуть ниже 1.
    assert!(black.iter().all(|value| value.abs() < 0.005), "{black:?}");
    assert!(
        white.iter().all(|value| (value - 1.0).abs() < 0.005),
        "{white:?}"
    );
}

#[test]
fn identity_adjustment_is_the_identity_affine() {
    let identity = sdr_adjustment(ENCODINGS[0], ColorAdjustment::IDENTITY);
    let sample = [0.3, 0.6, 0.9];
    let result = identity.apply(sample);
    for channel in 0..3 {
        assert!(
            (result[channel] - sample[channel]).abs() < 1e-12,
            "{result:?}"
        );
    }
}
