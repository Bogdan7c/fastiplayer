//! Геометрия на настоящем EGL: поворот 0/90/180/270, letterbox (полосы чёрные, картинка в своём
//! прямоугольнике пиксель в пиксель), исключения (под сайдбаром — только чёрный), выбор отсчёта
//! цветности, хвост строки без `GL_EXT_unpack_subimage`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use gles2_video::{
    ChromaSubsampling, ColorAdjustment, PictureMapping, PixelRect, QuarterTurn,
    RowLengthPreference, VideoDrawOutcome, VideoDrawRequest, VideoLayerOptions, YuvColorEncoding,
    YuvMatrix, YuvRange,
};
use support::{
    FramePattern, OwnedFrame, assert_close, assert_rect, draw_frame, extent, fill_request,
    full_rect, offscreen_or_skip, pixel, reference_rgba, renderer, size,
};

/// Полный диапазон BT.709: яркость `Y` при нейтральной цветности даёт серый `Y` без растяжения.
const FULL_BT709: YuvColorEncoding = YuvColorEncoding {
    matrix: YuvMatrix::Bt709,
    range: YuvRange::Full,
};

const BLACK: [u8; 4] = [0, 0, 0, 255];

fn gray(value: u8) -> [u8; 4] {
    [value, value, value, 255]
}

/// Серый кадр 4:4:4, яркость задаёт `luma(x, y)`.
fn gray_frame(width: u32, height: u32, luma: &dyn Fn(u32, u32) -> u8) -> OwnedFrame {
    OwnedFrame::from_pattern(
        ChromaSubsampling::Yuv444,
        extent(width, height),
        0,
        &FramePattern {
            luma,
            chroma_u: &|_, _| 128,
            chroma_v: &|_, _| 128,
        },
    )
}

/// Квадранты: левый верх 40, правый верх 100, левый низ 160, правый низ 220.
fn quadrant_luma(width: u32, height: u32) -> impl Fn(u32, u32) -> u8 {
    move |x, y| match (x < width / 2, y < height / 2) {
        (true, true) => 40,
        (false, true) => 100,
        (true, false) => 160,
        (false, false) => 220,
    }
}

/// Поворот: угол экрана показывает правильный угол кадра декодера (как
/// `display_orientation_uv_transform` у wgpu). Цель — в повёрнутых пропорциях, кадр рисуется
/// 1:1, поэтому каждый пиксель — центр своего текселя.
#[test]
fn quarter_turns_put_source_quadrants_in_rotated_corners() {
    // Кадр 8×4; после 90/270 цель 4×8.
    let source_width = 8;
    let source_height = 4;
    let frame = gray_frame(
        source_width,
        source_height,
        &quadrant_luma(source_width, source_height),
    );
    // (поворот, размер цели, цвета углов экрана: левый верх, правый верх, левый низ, правый низ)
    let cases = [
        (QuarterTurn::Identity, [8, 4], [40, 100, 160, 220]),
        // 90° по часовой: левый верх экрана = левый низ кадра.
        (QuarterTurn::Clockwise90, [4, 8], [160, 40, 220, 100]),
        (QuarterTurn::Half, [8, 4], [220, 160, 100, 40]),
        (QuarterTurn::Clockwise270, [4, 8], [100, 220, 40, 160]),
    ];
    for (orientation, [width, height], corners) in cases {
        let target = size(width, height);
        let Some(mut context) = offscreen_or_skip("quarter_turns", target) else {
            return;
        };
        let mut video = renderer(&context, VideoLayerOptions::default());
        video.upload(&context, &frame.upload(1)).unwrap();
        let rects = [full_rect(target)];
        let request = VideoDrawRequest {
            orientation,
            ..fill_request(target, &rects, FULL_BT709, ColorAdjustment::IDENTITY)
        };
        let (_, pixels) = draw_frame(&mut context, &mut video, &request);
        let [top_left, top_right, bottom_left, bottom_right] = corners;
        for (x, y, expected) in [
            (0, 0, top_left),
            (width - 1, 0, top_right),
            (0, height - 1, bottom_left),
            (width - 1, height - 1, bottom_right),
        ] {
            assert_close(
                pixel(&pixels, x, y),
                gray(expected),
                1,
                &format!("{orientation:?} corner ({x},{y})"),
            );
        }
        video.destroy(&context).unwrap();
    }
}

/// Letterbox: кадр 2:1 в квадратной цели. `uv_scale`/`uv_offset` — та же формула, что у
/// `render_core::PictureLayout::fit` (видео шире области → полосы сверху и снизу). Картинка
/// занимает строки 8..24 пиксель в пиксель, полосы — непрозрачный чёрный.
#[test]
fn letterbox_bars_are_black_and_picture_fills_its_rect() {
    let target = size(32, 32);
    let Some(mut context) = offscreen_or_skip("letterbox_bars_are_black", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let frame = gray_frame(16, 8, &|_, _| 200);
    video.upload(&context, &frame.upload(1)).unwrap();
    let rects = [full_rect(target)];
    let video_aspect = 16.0_f32 / 8.0;
    let viewport_aspect = 1.0_f32;
    let scale_y = video_aspect / viewport_aspect;
    let request = VideoDrawRequest {
        picture: PictureMapping {
            uv_scale: [1.0, scale_y],
            uv_offset: [0.0, (1.0 - scale_y) * 0.5],
        },
        ..fill_request(target, &rects, FULL_BT709, ColorAdjustment::IDENTITY)
    };
    let (_, pixels) = draw_frame(&mut context, &mut video, &request);
    let bar = |y, height| PixelRect {
        x: 0,
        y,
        width: 32,
        height,
    };
    assert_rect(&pixels, bar(0, 8), BLACK, 0, "top bar");
    assert_rect(&pixels, bar(8, 16), gray(200), 1, "picture");
    assert_rect(&pixels, bar(24, 8), BLACK, 0, "bottom bar");
    // Шейдер видео прошёл только по картинке: полосы залил `glClear` (сессия 12a). С ветвлением
    // в шейдере по всей области было бы 32 × 32.
    assert_eq!(video.stats().shaded_video_pixels, 32 * 16);
    video.destroy(&context).unwrap();
}

/// Исключения: видимые части области без левого сайдбара. Под сайдбаром только чёрная заливка
/// цели, справа — кадр; draw-вызов на каждый видимый прямоугольник.
#[test]
fn nothing_but_black_is_drawn_under_the_sidebar() {
    let target = size(40, 20);
    let Some(mut context) = offscreen_or_skip("nothing_but_black_under_sidebar", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let frame = gray_frame(40, 20, &|_, _| 180);
    video.upload(&context, &frame.upload(1)).unwrap();
    // Сайдбар — x 0..12; оставшееся разбито на два прямоугольника (как `RenderViewport::subtract`
    // может отдать несколько частей).
    let visible = [
        PixelRect {
            x: 12,
            y: 0,
            width: 28,
            height: 8,
        },
        PixelRect {
            x: 12,
            y: 8,
            width: 28,
            height: 12,
        },
    ];
    let request = fill_request(target, &visible, FULL_BT709, ColorAdjustment::IDENTITY);
    let (outcome, pixels) = draw_frame(&mut context, &mut video, &request);
    assert_eq!(outcome, VideoDrawOutcome::Drawn { draw_calls: 2 });
    assert_rect(
        &pixels,
        PixelRect {
            x: 0,
            y: 0,
            width: 12,
            height: 20,
        },
        BLACK,
        0,
        "sidebar",
    );
    for rect in visible {
        assert_rect(&pixels, rect, gray(180), 1, "visible video");
    }
    video.destroy(&context).unwrap();
}

/// Цветность 4:2:0 и 4:2:2 берётся из текселя `floor(тексель_яркости / 2)` (nearest co-sited,
/// как wgpu-хост), без смешивания соседних отсчётов цветности при рисовании 1:1.
#[test]
fn subsampled_chroma_is_taken_from_the_co_sited_texel() {
    for layout in [ChromaSubsampling::Yuv420, ChromaSubsampling::Yuv422] {
        let target = size(8, 4);
        let Some(mut context) = offscreen_or_skip("subsampled_chroma_co_sited", target) else {
            return;
        };
        let mut video = renderer(&context, VideoLayerOptions::default());
        // Цветность меняется по столбцам: чётный столбец цветности — красный, нечётный — синий.
        let red = [81, 90, 240];
        let blue = [41, 240, 110];
        let frame = OwnedFrame::from_pattern(
            layout,
            extent(8, 4),
            0,
            &FramePattern {
                luma: &|x, _| if (x / 2) % 2 == 0 { red[0] } else { blue[0] },
                chroma_u: &|x, _| if x % 2 == 0 { red[1] } else { blue[1] },
                chroma_v: &|x, _| if x % 2 == 0 { red[2] } else { blue[2] },
            },
        );
        video.upload(&context, &frame.upload(1)).unwrap();
        let encoding = YuvColorEncoding {
            matrix: YuvMatrix::Bt709,
            range: YuvRange::Limited,
        };
        let rects = [full_rect(target)];
        let request = fill_request(target, &rects, encoding, ColorAdjustment::IDENTITY);
        let (_, pixels) = draw_frame(&mut context, &mut video, &request);
        for x in 0..8 {
            let expected = if (x / 2) % 2 == 0 { red } else { blue };
            assert_close(
                pixel(&pixels, x, 1),
                reference_rgba(encoding, ColorAdjustment::IDENTITY, expected),
                1,
                &format!("{layout:?} column {x}"),
            );
        }
        video.destroy(&context).unwrap();
    }
}

/// Уменьшение 9 → 6 столбцов с нечётной шириной (цветность 5 столбцов): цветность 4:2:0 —
/// nearest-выборка по координате из вершин — обязана дать тексель формулы wgpu
/// `floor(floor(u × w) / 2)` на каждом столбце.
///
/// Соотношение выбрано так, что центр пикселя цели не попадает ровно на границу текселя
/// яркости (`(i + 0.5) × 9 / 6` не бывает целым). На точной границе выбор решает округление
/// интерполяции `f32` — и у прежнего шейдера с `floor` на пикселе, и у wgpu (проверено в
/// сессии 12a: на 8 → 6 прежний шейдер на radeonsi брал тот же «левый» тексель, что и новый).
#[test]
fn downscaled_chroma_follows_the_exact_co_sited_formula() {
    let target = size(6, 4);
    let Some(mut context) = offscreen_or_skip("downscaled_chroma_co_sited", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    // Яркость ровная (билинейное смешивание её не меняет), цветность — своя на каждый столбец.
    let luma = 128;
    let chroma_by_column: [[u8; 2]; 5] = [[90, 240], [240, 110], [60, 60], [200, 200], [128, 30]];
    let frame = OwnedFrame::from_pattern(
        ChromaSubsampling::Yuv420,
        extent(9, 4),
        0,
        &FramePattern {
            luma: &|_, _| luma,
            chroma_u: &|x, _| chroma_by_column[x as usize][0],
            chroma_v: &|x, _| chroma_by_column[x as usize][1],
        },
    );
    video.upload(&context, &frame.upload(1)).unwrap();
    let encoding = YuvColorEncoding {
        matrix: YuvMatrix::Bt709,
        range: YuvRange::Limited,
    };
    let rects = [full_rect(target)];
    let request = fill_request(target, &rects, encoding, ColorAdjustment::IDENTITY);
    let (_, pixels) = draw_frame(&mut context, &mut video, &request);
    for column in 0..6_u32 {
        let luma_texel = ((f64::from(column) + 0.5) * 9.0 / 6.0).floor() as usize;
        let [u, v] = chroma_by_column[luma_texel / 2];
        assert_close(
            pixel(&pixels, column, 1),
            reference_rgba(encoding, ColorAdjustment::IDENTITY, [luma, u, v]),
            1,
            &format!("column {column} (luma texel {luma_texel})"),
        );
    }
    video.destroy(&context).unwrap();
}

/// Нечётная ширина, stride больше ширины, текстура шириной в stride (путь без
/// `GL_EXT_unpack_subimage`): хвост строки — мусор `0xFF`, но справа нет светлой полосы, а
/// картинка совпадает с той же картинкой без хвоста строки побайтно.
#[test]
fn row_tail_never_leaks_into_the_picture_without_unpack_subimage() {
    // Растяжение 5×3 → 50×30: билинейная выборка у правого края.
    let target = size(50, 30);
    let gradient = |x: u32, y: u32| (20 + x * 30 + y * 10) as u8;
    let padded = gray_frame_with_tail(5, 3, 11, &gradient);
    let tight = gray_frame_with_tail(5, 3, 0, &gradient);
    let mut images = Vec::new();
    for (frame, row_length) in [
        (&padded, RowLengthPreference::StrideWideTextures),
        (&padded, RowLengthPreference::BestAvailable),
        (&tight, RowLengthPreference::StrideWideTextures),
    ] {
        let Some(mut context) = offscreen_or_skip("row_tail_never_leaks", target) else {
            return;
        };
        let mut video = renderer(
            &context,
            VideoLayerOptions {
                row_length,
                ..VideoLayerOptions::default()
            },
        );
        video.upload(&context, &frame.upload(1)).unwrap();
        let rects = [full_rect(target)];
        let request = fill_request(target, &rects, FULL_BT709, ColorAdjustment::IDENTITY);
        let (_, pixels) = draw_frame(&mut context, &mut video, &request);
        // Правый столбец — последний видимый отсчёт строки: по вертикали он смешан между
        // строками 0..2 (140..160), но мусор хвоста (255) поднял бы его выше.
        let lowest = gradient(4, 0);
        let highest = gradient(4, 2);
        for y in 0..30 {
            let actual = pixel(&pixels, 49, y)[0];
            assert!(
                (lowest - 1..=highest + 1).contains(&actual),
                "{row_length:?}: right edge row {y} is {actual}, expected {lowest}..={highest}"
            );
        }
        images.push(pixels.as_rgba().to_vec());
        video.destroy(&context).unwrap();
    }
    assert_eq!(
        images[0], images[2],
        "stride-wide texture equals tight rows pixel for pixel"
    );
    assert_eq!(
        images[1], images[2],
        "row-length upload equals tight rows pixel for pixel"
    );
}

fn gray_frame_with_tail(
    width: u32,
    height: u32,
    row_tail: usize,
    luma: &dyn Fn(u32, u32) -> u8,
) -> OwnedFrame {
    OwnedFrame::from_pattern(
        ChromaSubsampling::Yuv420,
        extent(width, height),
        row_tail,
        &FramePattern {
            luma,
            chroma_u: &|_, _| 128,
            chroma_v: &|_, _| 128,
        },
    )
}

/// Заливка — только там, куда видео не рисует, но результат как у полной заливки wgpu: после
/// кадра на всю цель кадр в левой половине оставляет правую половину чёрной, а не прошлой.
#[test]
fn everything_outside_the_video_viewport_is_black_even_over_a_previous_frame() {
    let target = size(32, 16);
    let Some(mut context) = offscreen_or_skip("outside_viewport_is_black", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let frame = gray_frame(16, 16, &|_, _| 220);
    video.upload(&context, &frame.upload(1)).unwrap();
    let full = [full_rect(target)];
    let (_, pixels) = draw_frame(
        &mut context,
        &mut video,
        &fill_request(target, &full, FULL_BT709, ColorAdjustment::IDENTITY),
    );
    assert_close(
        pixel(&pixels, 28, 8),
        gray(220),
        1,
        "first frame fills the target",
    );

    let left_half = PixelRect {
        x: 0,
        y: 0,
        width: 16,
        height: 16,
    };
    let visible = [left_half];
    let request = VideoDrawRequest {
        viewport: left_half,
        ..fill_request(target, &visible, FULL_BT709, ColorAdjustment::IDENTITY)
    };
    let (outcome, pixels) = draw_frame(&mut context, &mut video, &request);
    assert_eq!(outcome, VideoDrawOutcome::Drawn { draw_calls: 1 });
    assert_rect(&pixels, left_half, gray(220), 1, "video viewport");
    assert_rect(
        &pixels,
        PixelRect {
            x: 16,
            y: 0,
            width: 16,
            height: 16,
        },
        BLACK,
        0,
        "outside the viewport",
    );
    video.destroy(&context).unwrap();
}
