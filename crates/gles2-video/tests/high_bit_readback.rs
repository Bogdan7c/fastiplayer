//! 10/12 бит SDR на настоящем EGL: пары байтов доходят до шейдера точными кодами, выборка не
//! смешивает байты, цветность выбирается как у wgpu, хвост строки не виден, цвет — формула wgpu.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use gles2_video::{
    ChromaSubsampling, ColorAdjustment, PlaneTextureFormat, RowLengthPreference, SampleDepth,
    UploadOutcome, VideoDrawOutcome, VideoLayerOptions, YuvColorEncoding, YuvMatrix, YuvRange,
};
use support::high_bit::{HighBitFrame, HighBitPattern, reference_high_bit_sdr};
use support::{
    OwnedFrame, assert_close, assert_rect, draw_frame, extent, fill_request, full_rect,
    offscreen_or_skip, pixel, reference_rgba, renderer, size,
};

/// Перевод float → unorm8 у реализаций может отличаться на 1.
const COLOR_TOLERANCE: u8 = 1;

const LAYOUTS: [ChromaSubsampling; 3] = [
    ChromaSubsampling::Yuv420,
    ChromaSubsampling::Yuv422,
    ChromaSubsampling::Yuv444,
];

const ROW_LENGTH_MODES: [RowLengthPreference; 2] = [
    RowLengthPreference::BestAvailable,
    RowLengthPreference::StrideWideTextures,
];

fn options(row_length: RowLengthPreference) -> VideoLayerOptions {
    VideoLayerOptions {
        row_length,
        ..VideoLayerOptions::default()
    }
}

fn encoding(matrix: YuvMatrix, range: YuvRange) -> YuvColorEncoding {
    YuvColorEncoding { matrix, range }
}

/// Коды 8-битных отсчётов, сдвинутые в разрядность `depth`, плюс младшие биты.
fn probe_codes(depth: SampleDepth) -> Vec<[u16; 3]> {
    let shift = depth.bits() - 8;
    let max = u16::try_from(depth.max_code()).unwrap();
    let scaled = |code: u16| code << shift;
    vec![
        [scaled(16), scaled(128), scaled(128)],
        [scaled(235), scaled(128), scaled(128)],
        [0, 0, 0],
        [max, max, max],
        [scaled(63) + 1, scaled(102) + 2, scaled(240) + 3],
        [scaled(173) + 3, scaled(42), scaled(26) + 1],
        [scaled(90) + 2, scaled(150) + 1, scaled(110)],
    ]
}

/// Однотонные кадры каждой раскладки, разрядности, диапазона и матрицы — ровно формула wgpu.
#[test]
fn solid_frames_match_wgpu_formula_for_every_layout_depth_and_range() {
    for row_length in ROW_LENGTH_MODES {
        let target = size(12, 6);
        let Some(mut context) = offscreen_or_skip("high_bit_solid_frames", target) else {
            return;
        };
        let mut video = renderer(&context, options(row_length));
        let rects = [full_rect(target)];
        let mut key = 0;
        for depth in [SampleDepth::Bits10, SampleDepth::Bits12] {
            for layout in LAYOUTS {
                for (matrix, range) in [
                    (YuvMatrix::Bt709, YuvRange::Limited),
                    (YuvMatrix::Bt709, YuvRange::Full),
                    (YuvMatrix::Bt601, YuvRange::Limited),
                ] {
                    for codes in probe_codes(depth) {
                        key += 1;
                        let frame = HighBitFrame::solid(layout, depth, extent(5, 3), 3, codes);
                        let outcome = video.upload(&context, &frame.upload(key)).unwrap();
                        assert!(matches!(outcome, UploadOutcome::Uploaded(_)), "{outcome:?}");
                        let color = encoding(matrix, range);
                        let request =
                            fill_request(target, &rects, color, ColorAdjustment::IDENTITY);
                        let (drawn, pixels) = draw_frame(&mut context, &mut video, &request);
                        assert_eq!(drawn, VideoDrawOutcome::Drawn { draw_calls: 1 });
                        assert_rect(
                            &pixels,
                            full_rect(target),
                            reference_high_bit_sdr(depth, color, ColorAdjustment::IDENTITY, codes),
                            COLOR_TOLERANCE,
                            &format!("{row_length:?} {depth:?} {layout:?} {color:?} {codes:?}"),
                        );
                    }
                }
            }
        }
        video.destroy(&context).unwrap();
    }
}

/// Сборка кода из пары байтов точна до единицы кода: усиление делает шаг кода в 64 шага
/// выхода, поэтому коды `c − 1`, `c`, `c + 1` дают 64, 128 и 192. Перепутанные байты,
/// смешивание или потеря младшего бита уводят выход далеко.
#[test]
fn byte_pairs_reach_the_shader_as_exact_codes() {
    const STEP_IN_OUTPUT_BYTES: f32 = 64.0;
    const CENTER_OUTPUT_BYTE: f32 = 128.0;
    for row_length in ROW_LENGTH_MODES {
        let target = size(3, 1);
        let Some(mut context) = offscreen_or_skip("byte_pairs_reach_the_shader", target) else {
            return;
        };
        let mut video = renderer(&context, options(row_length));
        let rects = [full_rect(target)];
        let mut key = 0;
        for (depth, code) in [
            (SampleDepth::Bits12, 0_u16),
            (SampleDepth::Bits12, 1),
            (SampleDepth::Bits12, 255),
            (SampleDepth::Bits12, 256),
            (SampleDepth::Bits10, 1023),
            (SampleDepth::Bits12, 1023),
            (SampleDepth::Bits12, 4095),
        ] {
            key += 1;
            let max = depth.max_code() as f32;
            let neutral = 128_u16 << (depth.bits() - 8);
            // Три столбца: код − 1, код, код + 1 (за краями диапазона — сам код).
            let column_code = move |x: u32| match x {
                0 => code.saturating_sub(1),
                1 => code,
                _ => code
                    .saturating_add(1)
                    .min(u16::try_from(depth.max_code()).unwrap()),
            };
            let frame = HighBitFrame::from_pattern(
                ChromaSubsampling::Yuv444,
                depth,
                extent(3, 1),
                2,
                &HighBitPattern {
                    luma: &|x, _| column_code(x),
                    chroma_u: &|_, _| neutral,
                    chroma_v: &|_, _| neutral,
                },
            );
            video.upload(&context, &frame.upload(key)).unwrap();
            // Full: y = code / max. (y + brightness) × gain: код `code` → 128/255, шаг кода → 64/255.
            let gain = STEP_IN_OUTPUT_BYTES / 255.0 * max;
            let brightness = CENTER_OUTPUT_BYTE / 255.0 / gain - f32::from(code) / max;
            let adjustment = ColorAdjustment {
                brightness,
                rgb_gain: [gain; 3],
                ..ColorAdjustment::IDENTITY
            };
            let request = fill_request(
                target,
                &rects,
                encoding(YuvMatrix::Bt709, YuvRange::Full),
                adjustment,
            );
            let (_, pixels) = draw_frame(&mut context, &mut video, &request);
            for x in 0..3 {
                let offset = f32::from(column_code(x)) - f32::from(code);
                let expected = (CENTER_OUTPUT_BYTE + offset * STEP_IN_OUTPUT_BYTES) as u8;
                assert_close(
                    pixel(&pixels, x, 0),
                    [expected, expected, expected, 255],
                    COLOR_TOLERANCE,
                    &format!("{row_length:?} {depth:?} code {code} column {x}"),
                );
            }
        }
        video.destroy(&context).unwrap();
    }
}

/// Растяжение кадра из чередующихся кодов 255 (`0x00FF`) и 256 (`0x0100`): каждый пиксель цели —
/// ровно код своего текселя (nearest, как `textureLoad` wgpu). Билинейная выборка смешала бы
/// младшие и старшие байты отдельно и дала бы коды около 128 и 383.
#[test]
fn nearest_sampling_never_blends_byte_pairs() {
    let target = size(24, 4);
    let Some(mut context) = offscreen_or_skip("nearest_sampling_never_blends", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let depth = SampleDepth::Bits10;
    let code_at = |x: u32| if x.is_multiple_of(2) { 255_u16 } else { 256 };
    let frame = HighBitFrame::from_pattern(
        ChromaSubsampling::Yuv444,
        depth,
        extent(6, 1),
        0,
        &HighBitPattern {
            luma: &|x, _| code_at(x),
            chroma_u: &|_, _| 512,
            chroma_v: &|_, _| 512,
        },
    );
    video.upload(&context, &frame.upload(1)).unwrap();
    // Усиление вокруг 255,5: 255 → 64, 256 → 192 — промежуточного кода быть не может.
    let max = depth.max_code() as f32;
    let gain = 128.0 / 255.0 * max;
    let adjustment = ColorAdjustment {
        brightness: 128.0 / 255.0 / gain - 255.5 / max,
        rgb_gain: [gain; 3],
        ..ColorAdjustment::IDENTITY
    };
    let color = encoding(YuvMatrix::Bt709, YuvRange::Full);
    let rects = [full_rect(target)];
    let (_, pixels) = draw_frame(
        &mut context,
        &mut video,
        &fill_request(target, &rects, color, adjustment),
    );
    for x in 0..target.width() {
        // Центр пикселя цели → тексель floor(uv × 6).
        let texel = ((x as f32 + 0.5) / target.width() as f32 * 6.0).floor() as u32;
        let expected = reference_high_bit_sdr(depth, color, adjustment, [code_at(texel), 512, 512]);
        assert_close(
            pixel(&pixels, x, 1),
            expected,
            COLOR_TOLERANCE,
            &format!("x {x}"),
        );
    }
    video.destroy(&context).unwrap();
}

/// Цветность 4:2:0 нечётной ширины: тексель цветности — `floor(floor(uv × ширина) / 2)`, как у
/// wgpu (`ceil(5 / 2) = 3` текселя, а не 2,5). Каждый пиксель растянутой картинки сверяется с
/// эталоном своего текселя; хвост строки с мусором не виден.
#[test]
fn subsampled_chroma_and_row_tails_follow_wgpu_texel_choice() {
    for row_length in ROW_LENGTH_MODES {
        let target = size(50, 30);
        let Some(mut context) = offscreen_or_skip("subsampled_chroma_high_bit", target) else {
            return;
        };
        let mut video = renderer(&context, options(row_length));
        let depth = SampleDepth::Bits10;
        let luma_code = |x: u32, y: u32| 100 + 130 * x as u16 + 37 * y as u16;
        let u_code = |x: u32, y: u32| 200 + 260 * x as u16 + 90 * y as u16;
        let v_code = |x: u32, y: u32| 800 - 210 * x as u16 - 120 * y as u16;
        let frame = HighBitFrame::from_pattern(
            ChromaSubsampling::Yuv420,
            depth,
            extent(5, 3),
            3,
            &HighBitPattern {
                luma: &luma_code,
                chroma_u: &u_code,
                chroma_v: &v_code,
            },
        );
        video.upload(&context, &frame.upload(1)).unwrap();
        let color = encoding(YuvMatrix::Bt709, YuvRange::Limited);
        let rects = [full_rect(target)];
        let (_, pixels) = draw_frame(
            &mut context,
            &mut video,
            &fill_request(target, &rects, color, ColorAdjustment::IDENTITY),
        );
        for y in 0..target.height() {
            for x in 0..target.width() {
                let luma_texel = |position: u32, side: u32, luma_side: f32| {
                    ((position as f32 + 0.5) / side as f32 * luma_side).floor() as u32
                };
                let (luma_x, luma_y) = (luma_texel(x, 50, 5.0), luma_texel(y, 30, 3.0));
                let (chroma_x, chroma_y) = ((luma_x / 2).min(2), (luma_y / 2).min(1));
                let codes = [
                    luma_code(luma_x, luma_y),
                    u_code(chroma_x, chroma_y),
                    v_code(chroma_x, chroma_y),
                ];
                assert_close(
                    pixel(&pixels, x, y),
                    reference_high_bit_sdr(depth, color, ColorAdjustment::IDENTITY, codes),
                    COLOR_TOLERANCE,
                    &format!("{row_length:?} pixel ({x},{y})"),
                );
            }
        }
        video.destroy(&context).unwrap();
    }
}

/// Цветокоррекция видеослоя действует и на 10/12 бит (свойство crate-а; у плеера адаптер
/// передаёт тождественную, как wgpu).
#[test]
fn color_adjustment_applies_to_high_bit_sdr() {
    let target = size(4, 4);
    let Some(mut context) = offscreen_or_skip("high_bit_color_adjustment", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let depth = SampleDepth::Bits12;
    let codes = [2000, 1200, 2900];
    let frame = HighBitFrame::solid(ChromaSubsampling::Yuv420, depth, extent(4, 4), 0, codes);
    video.upload(&context, &frame.upload(1)).unwrap();
    let adjustment = ColorAdjustment {
        brightness: 0.05,
        contrast: 1.2,
        saturation: 0.6,
        exposure: -0.3,
        rgb_gain: [1.1, 0.9, 1.0],
        rgb_offset: [0.0, 0.02, -0.03],
    };
    let color = encoding(YuvMatrix::Bt709, YuvRange::Limited);
    let rects = [full_rect(target)];
    let (_, pixels) = draw_frame(
        &mut context,
        &mut video,
        &fill_request(target, &rects, color, adjustment),
    );
    assert_rect(
        &pixels,
        full_rect(target),
        reference_high_bit_sdr(depth, color, adjustment, codes),
        COLOR_TOLERANCE,
        "all knobs",
    );
    video.destroy(&context).unwrap();
}

/// Слот кольца, в котором был 8-битный кадр, перевыделяет текстуры под пары байтов и обратно;
/// оба кадра рисуются своим цветом, а 10/12 бит пишут в текстуры вдвое больше байт на отсчёт.
#[test]
fn switching_depth_reallocates_textures_and_keeps_colors() {
    let target = size(8, 4);
    let Some(mut context) = offscreen_or_skip("switching_depth", target) else {
        return;
    };
    // Кольцо по умолчанию — 3 слота по кругу: кадр 4 (10 бит) попадает в слот кадра 1 (8 бит).
    let mut video = renderer(&context, VideoLayerOptions::default());
    assert_eq!(
        video.plane_texture_format_for(SampleDepth::Bits10),
        PlaneTextureFormat::LuminanceAlpha
    );
    let color = encoding(YuvMatrix::Bt709, YuvRange::Limited);
    let rects = [full_rect(target)];
    let request = fill_request(target, &rects, color, ColorAdjustment::IDENTITY);
    let eight = OwnedFrame::solid(ChromaSubsampling::Yuv420, extent(8, 4), 0, [120, 90, 160]);
    let ten_codes = [480, 360, 640];
    let ten = HighBitFrame::solid(
        ChromaSubsampling::Yuv420,
        SampleDepth::Bits10,
        extent(8, 4),
        0,
        ten_codes,
    );
    for key in 1..=4 {
        let (outcome, expected) = if key % 2 == 1 {
            (
                video.upload(&context, &eight.upload(key)).unwrap(),
                reference_rgba(color, ColorAdjustment::IDENTITY, [120, 90, 160]),
            )
        } else {
            (
                video.upload(&context, &ten.upload(key)).unwrap(),
                reference_high_bit_sdr(
                    SampleDepth::Bits10,
                    color,
                    ColorAdjustment::IDENTITY,
                    ten_codes,
                ),
            )
        };
        let UploadOutcome::Uploaded(report) = outcome else {
            panic!("frame {key} not uploaded: {outcome:?}");
        };
        // 8×4 + 2 × 4×2 отсчётов: 48 байт у 8 бит, 96 у 10.
        let expected_bytes = if key % 2 == 1 { 48 } else { 96 };
        assert_eq!(report.copied_bytes, expected_bytes, "frame {key}");
        assert_eq!(report.upload_calls, 3, "one copy per plane, frame {key}");
        let (_, pixels) = draw_frame(&mut context, &mut video, &request);
        assert_rect(
            &pixels,
            full_rect(target),
            expected,
            COLOR_TOLERANCE,
            &format!("frame {key}"),
        );
    }
    // 3 слота × 3 текстуры при первой загрузке + 3 на смену разрядности в слоте 0.
    assert_eq!(video.stats().texture_allocations, 12, "{:?}", video.stats());
    video.destroy(&context).unwrap();
}
