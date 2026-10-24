//! Цвет на настоящем EGL: известные цвета YUV для 4:2:0/4:2:2/4:4:4, limited/full, BT.601/709,
//! каждая ручка цветокоррекции и все вместе — против CPU-формулы wgpu.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use gles2_video::{
    ChromaSubsampling, ColorAdjustment, PlaneFormatPreference, UploadOutcome, VideoDrawOutcome,
    VideoLayerOptions, YuvColorEncoding, YuvMatrix, YuvRange,
};
use support::{
    OwnedFrame, assert_rect, draw_frame, extent, fill_request, full_rect, offscreen_or_skip,
    reference_rgba, renderer, size,
};

/// Отсчёты YUV, которые проверяются во всех кодировках: чёрный и белый studio-range, тёмный и
/// светлый край полного диапазона, насыщенные цвета (красный, синий, зелёный BT.709 limited) и
/// произвольные середины.
const PROBE_SAMPLES: [[u8; 3]; 9] = [
    [16, 128, 128],
    [235, 128, 128],
    [0, 0, 0],
    [255, 255, 255],
    [63, 102, 240],
    [32, 240, 118],
    [173, 42, 26],
    [128, 64, 192],
    [90, 150, 110],
];

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

const LAYOUTS: [ChromaSubsampling; 3] = [
    ChromaSubsampling::Yuv420,
    ChromaSubsampling::Yuv422,
    ChromaSubsampling::Yuv444,
];

/// Допуск: перевод float → unorm8 у реализаций может отличаться на 1.
const COLOR_TOLERANCE: u8 = 1;

/// Однотонный кадр каждого цвета, каждой кодировки и раскладки даёт ровно цвет CPU-формулы.
#[test]
fn solid_frames_match_cpu_formula_for_every_layout_range_and_matrix() {
    for plane_format in [
        PlaneFormatPreference::BestAvailable,
        PlaneFormatPreference::LuminanceOnly,
    ] {
        let target = size(12, 6);
        let Some(mut context) = offscreen_or_skip("solid_frames_match_cpu_formula", target) else {
            return;
        };
        let mut video = renderer(
            &context,
            VideoLayerOptions {
                plane_format,
                ..VideoLayerOptions::default()
            },
        );
        let rects = [full_rect(target)];
        let mut key = 0;
        for layout in LAYOUTS {
            for encoding in ENCODINGS {
                for yuv in PROBE_SAMPLES {
                    key += 1;
                    // Кадр меньше цели (нечётный, со хвостом строки) — растяжение и обрезка
                    // хвоста не должны менять однотонный цвет.
                    let frame = OwnedFrame::solid(layout, extent(5, 3), 3, yuv);
                    let outcome = video.upload(&context, &frame.upload(key)).unwrap();
                    assert!(matches!(outcome, UploadOutcome::Uploaded(_)), "{outcome:?}");
                    let request = fill_request(target, &rects, encoding, ColorAdjustment::IDENTITY);
                    let (drawn, pixels) = draw_frame(&mut context, &mut video, &request);
                    assert_eq!(drawn, VideoDrawOutcome::Drawn { draw_calls: 1 });
                    assert_rect(
                        &pixels,
                        full_rect(target),
                        reference_rgba(encoding, ColorAdjustment::IDENTITY, yuv),
                        COLOR_TOLERANCE,
                        &format!("{plane_format:?} {layout:?} {encoding:?} yuv {yuv:?}"),
                    );
                }
            }
        }
        video.destroy(&context).unwrap();
    }
}

/// Каждая ручка цветокоррекции по отдельности и все вместе — против CPU-формулы.
#[test]
fn each_color_adjustment_knob_matches_cpu_formula() {
    let identity = ColorAdjustment::IDENTITY;
    let knobs = [
        (
            "brightness",
            ColorAdjustment {
                brightness: 0.12,
                ..identity
            },
        ),
        (
            "brightness down",
            ColorAdjustment {
                brightness: -0.2,
                ..identity
            },
        ),
        (
            "contrast",
            ColorAdjustment {
                contrast: 1.6,
                ..identity
            },
        ),
        (
            "contrast down",
            ColorAdjustment {
                contrast: 0.4,
                ..identity
            },
        ),
        (
            "saturation",
            ColorAdjustment {
                saturation: 1.8,
                ..identity
            },
        ),
        (
            "desaturation",
            ColorAdjustment {
                saturation: 0.0,
                ..identity
            },
        ),
        (
            "exposure",
            ColorAdjustment {
                exposure: 0.7,
                ..identity
            },
        ),
        (
            "exposure down",
            ColorAdjustment {
                exposure: -1.3,
                ..identity
            },
        ),
        (
            "gain",
            ColorAdjustment {
                rgb_gain: [1.3, 0.6, 0.9],
                ..identity
            },
        ),
        (
            "offset",
            ColorAdjustment {
                rgb_offset: [0.05, -0.1, 0.2],
                ..identity
            },
        ),
        (
            "all together",
            ColorAdjustment {
                brightness: 0.05,
                contrast: 1.25,
                saturation: 1.4,
                exposure: -0.35,
                rgb_gain: [1.1, 0.95, 1.2],
                rgb_offset: [-0.02, 0.03, 0.0],
            },
        ),
    ];
    let target = size(8, 4);
    let Some(mut context) =
        offscreen_or_skip("each_color_adjustment_knob_matches_cpu_formula", target)
    else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let rects = [full_rect(target)];
    // Тёплый средний цвет: у каждой ручки виден свой сдвиг всех трёх каналов.
    let yuv = [120, 100, 160];
    let frame = OwnedFrame::solid(ChromaSubsampling::Yuv420, extent(8, 4), 0, yuv);
    video.upload(&context, &frame.upload(1)).unwrap();
    for encoding in [ENCODINGS[0], ENCODINGS[3]] {
        let plain = reference_rgba(encoding, identity, yuv);
        for (name, adjustment) in knobs {
            let expected = reference_rgba(encoding, adjustment, yuv);
            assert_ne!(
                expected, plain,
                "{name}: knob must change the reference colour"
            );
            let request = fill_request(target, &rects, encoding, adjustment);
            let (_, pixels) = draw_frame(&mut context, &mut video, &request);
            assert_rect(
                &pixels,
                full_rect(target),
                expected,
                COLOR_TOLERANCE,
                &format!("{name} {encoding:?}"),
            );
        }
    }
    video.destroy(&context).unwrap();
}
