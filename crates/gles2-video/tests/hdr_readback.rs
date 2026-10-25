//! HDR → SDR на настоящем EGL: таблица на GPU против точной формулы `f64`, перестройка таблицы
//! только при смене настроек, отказы.
//!
//! Допуск — документированный в модуле `hdr` crate-а: худший случай 3/255, отличие больше 1/255
//! у долей процента отсчётов. Здесь к нему добавляется округление float → unorm8 GPU (±1), но
//! худший случай проверяется тем же числом: превышение — повод разбираться, а не расширять.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use gles2_video::{
    ChromaSubsampling, ColorAdjustment, HdrReferenceLevels, HdrToSdrConversion, HdrTransfer,
    SampleDepth, VideoColorConversion, VideoDrawError, VideoLayerOptions, YuvColorEncoding,
    YuvRange,
};
use support::high_bit::{HighBitFrame, HighBitPattern, reference_hdr};
use support::{
    OwnedFrame, assert_rect, draw_frame, extent, fill_request_with, full_rect, offscreen_or_skip,
    pixel, renderer, size,
};

/// Худшее отличие от точной формулы, шагов 1/255.
const MAX_DIFFERENCE: u8 = 3;
/// Доля пикселей с отличием больше 2/255, не больше (CPU-модель таблицы: < 0,01 %).
///
/// Долю «больше 1/255» тест только печатает: она зависит от фильтра драйвера. На 780M — 0,2–0,4 %;
/// llvmpipe фильтрует unorm8-текстуры в 8-битной целой арифметике и округляет смешанное значение
/// узлов до 1/255 ещё до декодирования — там 1–2 % при том же худшем случае 2/255.
const MAX_SHARE_ABOVE_TWO: f64 = 0.005;

fn hdr(range: YuvRange, transfer: HdrTransfer, levels: HdrReferenceLevels) -> VideoColorConversion {
    VideoColorConversion::HdrToSdr(HdrToSdrConversion {
        range,
        transfer,
        levels,
    })
}

/// Градиент 64×48 4:4:4, нарисованный 1:1: каждый пиксель — свои коды. Y по горизонтали, U и V
/// по вертикали и по диагонали — покрывает тёмное, светлое, нейтральное и насыщенное.
#[test]
fn hdr_gradient_matches_exact_formula_within_table_tolerance() {
    let target = size(64, 48);
    let Some(mut context) = offscreen_or_skip("hdr_gradient_matches_exact_formula", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let rects = [full_rect(target)];
    let mut key = 0;
    for (depth, range) in [
        (SampleDepth::Bits10, YuvRange::Limited),
        (SampleDepth::Bits12, YuvRange::Full),
    ] {
        let max = depth.max_code();
        let shift = depth.bits() - 8;
        let luma = move |x: u32, _y: u32| ((16 << shift) + x * (219 << shift) / 63) as u16;
        let chroma_u = move |x: u32, y: u32| {
            ((16 << shift) + (y * 5 + x) * (224 << shift) / 378).min(max) as u16
        };
        let chroma_v = move |x: u32, y: u32| {
            ((240 << shift) - (y * 3 + x * 2) * (224 << shift) / 267).min(max) as u16
        };
        for transfer in [HdrTransfer::Pq, HdrTransfer::Hlg] {
            key += 1;
            let frame = HighBitFrame::from_pattern(
                ChromaSubsampling::Yuv444,
                depth,
                extent(64, 48),
                2,
                &HighBitPattern {
                    luma: &luma,
                    chroma_u: &chroma_u,
                    chroma_v: &chroma_v,
                },
            );
            video.upload(&context, &frame.upload(key)).unwrap();
            let levels = HdrReferenceLevels::REFERENCE_DEFAULT;
            let request = fill_request_with(target, &rects, hdr(range, transfer, levels));
            // Смена передаточной функции — новая таблица в фоне: ждём её готовности.
            let pixels = draw_until_table_settled(&mut context, &mut video, &request);
            let mut worst = 0;
            let mut above_one = 0;
            let mut above_two = 0;
            for y in 0..48 {
                for x in 0..64 {
                    let codes = [luma(x, y), chroma_u(x, y), chroma_v(x, y)];
                    let expected = reference_hdr(depth, range, transfer, levels, codes);
                    let actual = pixel(&pixels, x, y);
                    let difference = (0..3)
                        .map(|channel| actual[channel].abs_diff(expected[channel]))
                        .max()
                        .unwrap_or(0);
                    assert!(
                        difference <= MAX_DIFFERENCE,
                        "{depth:?} {range:?} {transfer:?} ({x},{y}) codes {codes:?}: \
                         {actual:?} vs {expected:?}"
                    );
                    worst = worst.max(difference);
                    above_one += u32::from(difference > 1);
                    above_two += u32::from(difference > 2);
                }
            }
            let pixel_count = f64::from(64 * 48);
            let share_above_two = f64::from(above_two) / pixel_count;
            eprintln!(
                "{depth:?} {range:?} {transfer:?}: worst {worst}/255, {:.2} % above 1/255, \
                 {:.2} % above 2/255",
                f64::from(above_one) / pixel_count * 100.0,
                share_above_two * 100.0
            );
            assert!(
                share_above_two <= MAX_SHARE_ABOVE_TWO,
                "{transfer:?}: {share_above_two}"
            );
        }
    }
    video.destroy(&context).unwrap();
}

/// Однотонные HDR-кадры всех раскладок: тот же результат, что формула (цветность 4:2:0/4:2:2 не
/// ломает HDR-путь).
#[test]
fn solid_hdr_frames_of_every_layout_match_the_formula() {
    let target = size(10, 6);
    let Some(mut context) = offscreen_or_skip("solid_hdr_frames", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let rects = [full_rect(target)];
    let levels = HdrReferenceLevels::REFERENCE_DEFAULT;
    let mut key = 0;
    for layout in [
        ChromaSubsampling::Yuv420,
        ChromaSubsampling::Yuv422,
        ChromaSubsampling::Yuv444,
    ] {
        // Нейтраль около 203 нит PQ, тёмная нейтраль, насыщенный красный и смешанный цвет.
        for codes in [
            [575_u16, 512, 512],
            [200, 512, 512],
            [300, 400, 900],
            [700, 300, 600],
        ] {
            for transfer in [HdrTransfer::Pq, HdrTransfer::Hlg] {
                key += 1;
                let frame =
                    HighBitFrame::solid(layout, SampleDepth::Bits10, extent(5, 3), 1, codes);
                video.upload(&context, &frame.upload(key)).unwrap();
                let request =
                    fill_request_with(target, &rects, hdr(YuvRange::Limited, transfer, levels));
                // Смена передаточной функции — новая таблица в фоне: ждём её готовности.
                let pixels = draw_until_table_settled(&mut context, &mut video, &request);
                assert_rect(
                    &pixels,
                    full_rect(target),
                    reference_hdr(
                        SampleDepth::Bits10,
                        YuvRange::Limited,
                        transfer,
                        levels,
                        codes,
                    ),
                    MAX_DIFFERENCE,
                    &format!("{layout:?} {transfer:?} {codes:?}"),
                );
            }
        }
    }
    video.destroy(&context).unwrap();
}

/// Таблица считается один раз на набор «передаточная функция + уровни», а не на кадр. Без
/// кэша по ключу счётчик рос бы с каждым кадром.
#[test]
fn hdr_table_is_rebuilt_only_when_settings_change() {
    let target = size(4, 4);
    let Some(mut context) = offscreen_or_skip("hdr_table_is_rebuilt_only", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let rects = [full_rect(target)];
    let frame = HighBitFrame::solid(
        ChromaSubsampling::Yuv420,
        SampleDepth::Bits10,
        extent(4, 4),
        0,
        [600, 500, 540],
    );
    let base = HdrReferenceLevels::REFERENCE_DEFAULT;
    let brighter = HdrReferenceLevels::new(160.0, 1_000.0).unwrap();
    let steps = [
        (HdrTransfer::Pq, base, 1),
        (HdrTransfer::Pq, base, 1),
        (HdrTransfer::Pq, base, 1),
        (HdrTransfer::Pq, brighter, 2),
        (HdrTransfer::Pq, brighter, 2),
        (HdrTransfer::Hlg, brighter, 3),
        (HdrTransfer::Hlg, brighter, 3),
    ];
    video.upload(&context, &frame.upload(1)).unwrap();
    let mut previous_pixels = None;
    for (index, (transfer, levels, builds)) in steps.into_iter().enumerate() {
        let request = fill_request_with(target, &rects, hdr(YuvRange::Limited, transfer, levels));
        // Смена параметров считается в фоне: рисуем, пока новая таблица не встанет на GPU.
        let pixels = draw_until_table_settled(&mut context, &mut video, &request);
        assert_eq!(video.stats().hdr_table_builds, builds, "step {index}");
        // Новые уровни видны после готовности таблицы: белый 160 нит темнее белого 100.
        let center = pixel(&pixels, 2, 2);
        if index == 3 {
            let before: [u8; 4] = previous_pixels.expect("earlier frame");
            assert!(
                center[1] < before[1],
                "brighter reference white darkens: {before:?} → {center:?}"
            );
        }
        previous_pixels = Some(center);
    }
    video.destroy(&context).unwrap();
}

/// HDR → SDR для 8-битного кадра — типизированный отказ до записи в цель; SDR того же кадра
/// после него рисуется как обычно.
#[test]
fn hdr_for_an_eight_bit_frame_is_refused_before_drawing() {
    let target = size(4, 4);
    let Some(mut context) = offscreen_or_skip("hdr_for_eight_bit_refused", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let frame = OwnedFrame::solid(ChromaSubsampling::Yuv420, extent(4, 4), 0, [120, 128, 128]);
    video.upload(&context, &frame.upload(1)).unwrap();
    let rects = [full_rect(target)];
    let gles2_egui::FrameStart::Ready(active) = context.begin_frame().unwrap() else {
        panic!("offscreen frame skipped");
    };
    let refused = video.draw(
        &active,
        &fill_request_with(
            target,
            &rects,
            hdr(
                YuvRange::Limited,
                HdrTransfer::Pq,
                HdrReferenceLevels::REFERENCE_DEFAULT,
            ),
        ),
    );
    assert_eq!(
        refused,
        Err(VideoDrawError::HdrToSdrNeedsHighBitFrame {
            depth: SampleDepth::Bits8
        })
    );
    active.present().unwrap();
    assert_eq!(video.stats().draw_calls, 0, "nothing drawn");
    assert_eq!(
        video.stats().hdr_table_builds,
        0,
        "no table for a refused frame"
    );
    let sdr = VideoColorConversion::Sdr {
        encoding: YuvColorEncoding::SDR_BT709_LIMITED,
        adjustment: ColorAdjustment::IDENTITY,
    };
    let (_, pixels) = draw_frame(
        &mut context,
        &mut video,
        &fill_request_with(target, &rects, sdr),
    );
    assert_rect(
        &pixels,
        full_rect(target),
        support::reference_rgba(
            YuvColorEncoding::SDR_BT709_LIMITED,
            ColorAdjustment::IDENTITY,
            [120, 128, 128],
        ),
        1,
        "SDR after refusal",
    );
    video.destroy(&context).unwrap();
}

/// Рисует, пока фон не доложил, что новая таблица встала на GPU; возвращает пиксели последнего
/// кадра. Потолок по времени — защита от зависания теста, а не часть проверяемого поведения.
fn draw_until_table_settled(
    context: &mut gles2_egui::Gles2Context,
    video: &mut gles2_video::VideoLayerRenderer,
    request: &gles2_video::VideoDrawRequest<'_>,
) -> gles2_egui::OffscreenPixels {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let (_, pixels) = draw_frame(context, video, request);
        if !video.has_pending_hdr_table() {
            return pixels;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "HDR table never became ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// Кадр HDR 4×4 однотонный: центр кадра — эталон формулы для заданных уровней.
fn solid_hdr_setup() -> ([u16; 3], HighBitFrame) {
    let codes = [600_u16, 500, 540];
    let frame = HighBitFrame::solid(
        ChromaSubsampling::Yuv420,
        SampleDepth::Bits10,
        extent(4, 4),
        0,
        codes,
    );
    (codes, frame)
}

fn expected_center(codes: [u16; 3], levels: HdrReferenceLevels) -> [u8; 4] {
    reference_hdr(
        SampleDepth::Bits10,
        YuvRange::Limited,
        HdrTransfer::Pq,
        levels,
        codes,
    )
}

fn assert_center_near(actual: [u8; 4], expected: [u8; 4], what: &str) {
    let difference = (0..3)
        .map(|channel| actual[channel].abs_diff(expected[channel]))
        .max()
        .unwrap_or(0);
    assert!(
        difference <= MAX_DIFFERENCE,
        "{what}: {actual:?} vs {expected:?}"
    );
}

/// Самая первая таблица считается синхронно: первый же HDR-кадр правильный, фона нет.
#[test]
fn first_hdr_table_is_built_synchronously_and_the_first_frame_is_right() {
    let target = size(4, 4);
    let Some(mut context) = offscreen_or_skip("first_hdr_table_sync", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let rects = [full_rect(target)];
    let (codes, frame) = solid_hdr_setup();
    video.upload(&context, &frame.upload(1)).unwrap();
    let levels = HdrReferenceLevels::REFERENCE_DEFAULT;
    let request = fill_request_with(
        target,
        &rects,
        hdr(YuvRange::Limited, HdrTransfer::Pq, levels),
    );
    let (_, pixels) = draw_frame(&mut context, &mut video, &request);
    assert_center_near(
        pixel(&pixels, 2, 2),
        expected_center(codes, levels),
        "first frame",
    );
    let stats = video.stats();
    assert_eq!(stats.hdr_table_sync_builds, 1);
    assert_eq!(stats.hdr_table_background_builds, 0);
    assert!(!video.has_pending_hdr_table());
    video.destroy(&context).unwrap();
}

/// Смена уровней при уже имеющейся таблице: `draw` не строит таблицу сам. Пока фон не готов,
/// кадр со старой таблицей; когда готов — с новой. Счётчик синхронных сборок не растёт.
#[test]
fn level_change_is_built_in_background_old_table_until_ready() {
    let target = size(4, 4);
    let Some(mut context) = offscreen_or_skip("level_change_background", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let rects = [full_rect(target)];
    let (codes, frame) = solid_hdr_setup();
    video.upload(&context, &frame.upload(1)).unwrap();
    let old_levels = HdrReferenceLevels::REFERENCE_DEFAULT;
    let new_levels = HdrReferenceLevels::new(160.0, 1_000.0).unwrap();
    let old_expected = expected_center(codes, old_levels);
    let new_expected = expected_center(codes, new_levels);
    assert!(
        old_expected[1].abs_diff(new_expected[1]) > 2 * MAX_DIFFERENCE,
        "scenario must tell the tables apart: {old_expected:?} vs {new_expected:?}"
    );
    let old_request = fill_request_with(
        target,
        &rects,
        hdr(YuvRange::Limited, HdrTransfer::Pq, old_levels),
    );
    draw_frame(&mut context, &mut video, &old_request);
    assert_eq!(video.stats().hdr_table_sync_builds, 1);

    let new_request = fill_request_with(
        target,
        &rects,
        hdr(YuvRange::Limited, HdrTransfer::Pq, new_levels),
    );
    let (_, first_pixels) = draw_frame(&mut context, &mut video, &new_request);
    // Поток рисования таблицу не строил в любом случае.
    assert_eq!(video.stats().hdr_table_sync_builds, 1, "no inline build");
    // Фон мог успеть (быстрая машина): тогда кадр уже новый. Не успел — обязан быть старый, а не
    // чёрный и не полуготовый.
    let first_center = pixel(&first_pixels, 2, 2);
    if video.has_pending_hdr_table() {
        assert_center_near(first_center, old_expected, "old table while building");
    } else {
        assert_center_near(first_center, new_expected, "background finished at once");
    }
    let settled = draw_until_table_settled(&mut context, &mut video, &new_request);
    assert_center_near(pixel(&settled, 2, 2), new_expected, "new table after ready");
    let stats = video.stats();
    assert_eq!(stats.hdr_table_sync_builds, 1);
    assert_eq!(stats.hdr_table_background_builds, 1);
    assert_eq!(stats.hdr_table_builds, 2);
    video.destroy(&context).unwrap();
}

/// Быстрая серия смен (ползунок): в фоне строятся не все значения; итог — последнее.
#[test]
fn rapid_level_changes_skip_intermediate_tables() {
    let target = size(4, 4);
    let Some(mut context) = offscreen_or_skip("rapid_level_changes", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let rects = [full_rect(target)];
    let (codes, frame) = solid_hdr_setup();
    video.upload(&context, &frame.upload(1)).unwrap();
    let first = fill_request_with(
        target,
        &rects,
        hdr(
            YuvRange::Limited,
            HdrTransfer::Pq,
            HdrReferenceLevels::REFERENCE_DEFAULT,
        ),
    );
    draw_frame(&mut context, &mut video, &first);
    let whites = [110.0_f32, 120.0, 130.0, 140.0, 150.0, 160.0];
    let mut last_request = None;
    for white in whites {
        let levels = HdrReferenceLevels::new(white, 1_000.0).unwrap();
        let request = fill_request_with(
            target,
            &rects,
            hdr(YuvRange::Limited, HdrTransfer::Pq, levels),
        );
        draw_frame(&mut context, &mut video, &request);
        last_request = Some(request);
    }
    let last_request = last_request.expect("changes were made");
    let settled = draw_until_table_settled(&mut context, &mut video, &last_request);
    let last_levels = HdrReferenceLevels::new(160.0, 1_000.0).unwrap();
    assert_center_near(
        pixel(&settled, 2, 2),
        expected_center(codes, last_levels),
        "last table wins",
    );
    let stats = video.stats();
    assert_eq!(
        stats.hdr_table_sync_builds, 1,
        "only the very first table is inline"
    );
    assert!(
        stats.hdr_table_background_builds < whites.len() as u64,
        "intermediate tables are skipped: {stats:?}"
    );
    assert!(stats.hdr_table_superseded >= 1, "{stats:?}");
    video.destroy(&context).unwrap();
}

/// Уничтожение слоя посреди сборки не зависает.
#[test]
fn destroy_during_background_build_returns() {
    let target = size(4, 4);
    let Some(mut context) = offscreen_or_skip("destroy_during_build", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let rects = [full_rect(target)];
    let (_, frame) = solid_hdr_setup();
    video.upload(&context, &frame.upload(1)).unwrap();
    for white in [100.0_f32, 170.0] {
        let levels = HdrReferenceLevels::new(white, 1_000.0).unwrap();
        let request = fill_request_with(
            target,
            &rects,
            hdr(YuvRange::Limited, HdrTransfer::Pq, levels),
        );
        draw_frame(&mut context, &mut video, &request);
    }
    let started = std::time::Instant::now();
    video.destroy(&context).unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "destroy must not wait for the build"
    );
}
