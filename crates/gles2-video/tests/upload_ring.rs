//! Загрузка и кольцо на настоящем EGL: ровно одна копия на плоскость кадра, 0 на повтор,
//! `Busy` без новых текстур при занятых слотах, переиспользование слота после fence, отказы
//! (DMA-BUF, чужая раскладка, чужой контекст), подложка под egui в сборщике кадра.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use std::os::fd::AsFd;

use egui::{Color32, Context, Rect, pos2};
use gles2_egui::{
    CornerRadius, EguiPainterOptions, FrameCompositor, FrameStart, UiContent, UiFrameOutcome,
    UiFrameRequest, UiScreen,
};
use gles2_video::{
    BusyReason, ChromaSubsampling, ColorAdjustment, DmaBufPlane, FrameLayoutError, PlaneRole,
    PlaneSource, PlaneSourceKind, UploadOutcome, UploadSlotCount, VideoDrawOutcome,
    VideoLayerOptions, VideoUnderlay, VideoUploadError, YuvColorEncoding,
};
use support::{
    OwnedFrame, assert_close, draw_frame, extent, fill_request, full_rect, offscreen_or_skip,
    pixel, reference_rgba, renderer, size,
};

const GRAY: [u8; 3] = [128, 128, 128];

fn uploaded(outcome: UploadOutcome) -> gles2_video::UploadReport {
    match outcome {
        UploadOutcome::Uploaded(report) => report,
        other => panic!("expected an upload, got {other:?}"),
    }
}

/// Ровно одна копия на плоскость: 3 плоскости на кадр, байт скопировано столько, сколько
/// видимых байт у кадра; повтор того же кадра — 0 загрузок и 0 байт.
#[test]
fn each_plane_is_copied_once_and_repeats_copy_nothing() {
    let target = size(16, 8);
    let Some(mut context) = offscreen_or_skip("each_plane_is_copied_once", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let frame = OwnedFrame::solid(ChromaSubsampling::Yuv420, extent(13, 7), 5, GRAY);
    // С `GL_UNPACK_ROW_LENGTH` в текстуры уходят ровно видимые байты, без расширения — блоки
    // плоскостей вместе с хвостами строк (текстура шириной в stride). В обоих случаях — по
    // одному разу.
    let (expected_copied, expected_calls) =
        if context.capabilities().tiers().conveniences.unpack_subimage {
            (frame.visible_bytes(), 3)
        } else {
            (frame.block_bytes(), 6)
        };
    let report = uploaded(video.upload(&context, &frame.upload(1)).unwrap());
    assert_eq!(report.copied_bytes, expected_copied);
    assert_eq!(report.upload_calls, expected_calls);
    assert_eq!(report.allocated_textures, 3);

    let rects = [full_rect(target)];
    let request = fill_request(
        target,
        &rects,
        YuvColorEncoding::SDR_BT709_LIMITED,
        ColorAdjustment::IDENTITY,
    );
    for _ in 0..3 {
        // Повтор кадра: тот же ключ — без загрузки, но кадр рисуется.
        assert_eq!(
            video.upload(&context, &frame.upload(1)).unwrap(),
            UploadOutcome::AlreadyCurrent
        );
        let (outcome, _) = draw_frame(&mut context, &mut video, &request);
        assert_eq!(outcome, VideoDrawOutcome::Drawn { draw_calls: 1 });
    }
    let stats = video.stats();
    assert_eq!(stats.frames_uploaded, 1);
    assert_eq!(stats.planes_uploaded, 3);
    assert_eq!(stats.copied_bytes, expected_copied as u64);
    assert_eq!(stats.repeated_frames, 3);
    assert_eq!(stats.draw_calls, 3);

    // Второй кадр того же размера пишется в уже выделенные текстуры другого слота: 3 новые
    // текстуры (слот новый), а третьему кадру — тоже; дальше кольцо не выделяет ничего.
    for key in 2..=6 {
        let report = uploaded(video.upload(&context, &frame.upload(key)).unwrap());
        assert_eq!(report.copied_bytes, expected_copied);
        draw_frame(&mut context, &mut video, &request);
    }
    assert_eq!(video.allocated_slots(), UploadSlotCount::DEFAULT.get());
    assert_eq!(
        video.stats().texture_allocations,
        9,
        "3 slots × 3 planes, then reuse"
    );
    video.destroy(&context).unwrap();
}

/// N+1 кадр при занятых слотах → `Busy`: текущий кадр прежний, новых текстур нет, байты кадра
/// не прочитаны (вызывающий не отпускает кадр декодера). После fence слот переиспользуется.
#[test]
fn busy_ring_does_not_grow_and_reuses_slot_after_fence() {
    let target = size(8, 8);
    let Some(mut context) = offscreen_or_skip("busy_ring_does_not_grow", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let rects = [full_rect(target)];
    let request = fill_request(
        target,
        &rects,
        YuvColorEncoding::SDR_BT709_LIMITED,
        ColorAdjustment::IDENTITY,
    );
    let frames: Vec<OwnedFrame> = (0..4)
        .map(|index| {
            OwnedFrame::solid(
                ChromaSubsampling::Yuv444,
                extent(8, 8),
                0,
                [40 + index * 50, 128, 128],
            )
        })
        .collect();

    // Три кадра загружены и нарисованы, но fence их работы ещё не отдан.
    for (key, frame) in (1..).zip(&frames[..3]) {
        uploaded(video.upload(&context, &frame.upload(key)).unwrap());
        match context.begin_frame().unwrap() {
            FrameStart::Ready(active) => {
                video.draw(&active, &request).unwrap();
                active.present().unwrap();
            }
            FrameStart::Skipped(reason) => panic!("skipped: {reason:?}"),
        }
    }
    let allocations_before = video.stats().texture_allocations;
    assert_eq!(
        video.upload(&context, &frames[3].upload(4)).unwrap(),
        UploadOutcome::Busy(BusyReason::AwaitingSubmittedWorkFence)
    );
    assert_eq!(
        video.stats().texture_allocations,
        allocations_before,
        "Busy allocates nothing"
    );
    assert_eq!(video.allocated_slots(), 3, "ring does not grow");
    assert_eq!(video.stats().busy_rejections, 1);
    // Текущий кадр — третий: его и рисует повтор.
    let (_, pixels) = draw_frame(&mut context, &mut video, &request);
    let third = reference_rgba(
        YuvColorEncoding::SDR_BT709_LIMITED,
        ColorAdjustment::IDENTITY,
        [140, 128, 128],
    );
    assert_close(
        pixel(&pixels, 4, 4),
        third,
        1,
        "Busy keeps the current frame",
    );

    // `draw_frame` отдал fence; после его срабатывания слот снова доступен.
    context
        .insert_fence()
        .unwrap()
        .wait(std::time::Duration::from_secs(5))
        .unwrap();
    let report = uploaded(video.upload(&context, &frames[3].upload(4)).unwrap());
    assert_eq!(
        report.allocated_textures, 0,
        "slot is reused, not reallocated"
    );
    let (_, pixels) = draw_frame(&mut context, &mut video, &request);
    let fourth = reference_rgba(
        YuvColorEncoding::SDR_BT709_LIMITED,
        ColorAdjustment::IDENTITY,
        [190, 128, 128],
    );
    assert_close(
        pixel(&pixels, 4, 4),
        fourth,
        1,
        "reused slot shows the new frame",
    );
    assert_eq!(video.allocated_slots(), 3);
    video.destroy(&context).unwrap();
}

/// Отказы загрузки — типизированные, кольцо и текущий кадр не тронуты.
#[test]
fn rejected_uploads_leave_the_current_frame_untouched() {
    let target = size(8, 8);
    let Some(mut context) = offscreen_or_skip("rejected_uploads", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let frame = OwnedFrame::solid(ChromaSubsampling::Yuv420, extent(8, 8), 0, [200, 128, 128]);
    uploaded(video.upload(&context, &frame.upload(1)).unwrap());

    // DMA-BUF: тип есть, загрузка — сессии 07/12.
    let memfd = std::fs::File::open("/dev/null").unwrap();
    let mut dma_buf_frame = frame.upload(2);
    dma_buf_frame.chroma_v = PlaneSource::DmaBuf(DmaBufPlane {
        fd: memfd.as_fd(),
        offset: 0,
        pitch: 4,
        modifier: 0,
        drm_format: 0x2020_3852,
    });
    assert_eq!(
        video.upload(&context, &dma_buf_frame),
        Err(VideoUploadError::PlaneSourceUnsupported {
            role: PlaneRole::ChromaV,
            kind: PlaneSourceKind::DmaBuf
        })
    );

    // Цветность 4:4:4 под видом 4:2:0: строки не той ширины.
    let wrong = OwnedFrame::solid(ChromaSubsampling::Yuv444, extent(8, 8), 0, GRAY);
    let mut mislabeled = wrong.upload(3);
    mislabeled.layout.chroma = ChromaSubsampling::Yuv420;
    assert_eq!(
        video.upload(&context, &mislabeled),
        Err(VideoUploadError::Layout(
            FrameLayoutError::RowBytesMismatch {
                role: PlaneRole::ChromaU,
                expected: 4,
                actual: 8
            }
        ))
    );

    // Чужой контекст.
    let Some(other) = offscreen_or_skip("rejected_uploads other context", target) else {
        return;
    };
    assert_eq!(
        video.upload(&other, &frame.upload(4)),
        Err(VideoUploadError::ForeignContext)
    );

    assert_eq!(
        video.stats().frames_uploaded,
        1,
        "no rejected frame counted as uploaded"
    );
    let rects = [full_rect(target)];
    let request = fill_request(
        target,
        &rects,
        YuvColorEncoding::SDR_BT709_LIMITED,
        ColorAdjustment::IDENTITY,
    );
    let (_, pixels) = draw_frame(&mut context, &mut video, &request);
    let first = reference_rgba(
        YuvColorEncoding::SDR_BT709_LIMITED,
        ColorAdjustment::IDENTITY,
        [200, 128, 128],
    );
    assert_close(
        pixel(&pixels, 2, 2),
        first,
        1,
        "first frame is still current",
    );
    video.destroy(&context).unwrap();
}

/// Без загруженного кадра рисование заливает цель чёрным; после `forget_uploaded_frames` тот
/// же ключ загружается снова.
#[test]
fn draw_without_frame_clears_black_and_forgotten_keys_reload() {
    let target = size(8, 8);
    let Some(mut context) = offscreen_or_skip("draw_without_frame", target) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let rects = [full_rect(target)];
    let request = fill_request(
        target,
        &rects,
        YuvColorEncoding::SDR_BT709_LIMITED,
        ColorAdjustment::IDENTITY,
    );
    let (outcome, pixels) = draw_frame(&mut context, &mut video, &request);
    assert_eq!(outcome, VideoDrawOutcome::ClearedWithoutFrame);
    assert_eq!(pixel(&pixels, 3, 3), [0, 0, 0, 255]);

    let frame = OwnedFrame::solid(ChromaSubsampling::Yuv420, extent(8, 8), 0, GRAY);
    uploaded(video.upload(&context, &frame.upload(1)).unwrap());
    video.forget_uploaded_frames();
    let (outcome, _) = draw_frame(&mut context, &mut video, &request);
    assert_eq!(outcome, VideoDrawOutcome::ClearedWithoutFrame);
    uploaded(video.upload(&context, &frame.upload(1)).unwrap());
    assert_eq!(
        video.stats().frames_uploaded,
        2,
        "same key after forget is uploaded again"
    );
    video.destroy(&context).unwrap();
}

/// Подложка в сборщике кадра: видео под полупрозрачной панелью egui смешивается как в wgpu
/// (egui поверх видео, `ONE, ONE_MINUS_SRC_ALPHA`), вне панели — чистое видео.
#[test]
fn video_underlay_sits_under_egui_in_the_frame_compositor() {
    let target = size(32, 16);
    let Some(mut context) = offscreen_or_skip("video_underlay_under_egui", target) else {
        return;
    };
    let mut compositor = FrameCompositor::new(&context, EguiPainterOptions::default()).unwrap();
    let mut video = renderer(&context, VideoLayerOptions::default());
    let full_bt709 = YuvColorEncoding {
        matrix: gles2_video::YuvMatrix::Bt709,
        range: gles2_video::YuvRange::Full,
    };
    let frame = OwnedFrame::solid(
        ChromaSubsampling::Yuv420,
        extent(32, 16),
        0,
        [200, 128, 128],
    );
    uploaded(video.upload(&context, &frame.upload(1)).unwrap());

    let egui_context = Context::default();
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_max(pos2(0.0, 0.0), pos2(32.0, 16.0))),
        max_texture_side: Some(4096),
        ..egui::RawInput::default()
    };
    let output = egui_context.run_ui(input, |ui| {
        // Полупрозрачный чёрный: premultiplied (0,0,0,128).
        ui.painter().rect_filled(
            Rect::from_min_max(pos2(0.0, 0.0), pos2(16.0, 16.0)),
            0.0,
            Color32::from_black_alpha(128),
        );
    });
    let primitives = egui_context.tessellate(output.shapes, output.pixels_per_point);
    let textures_delta = output.textures_delta;

    let rects = [full_rect(target)];
    let mut underlay = VideoUnderlay::new(
        &mut video,
        fill_request(target, &rects, full_bt709, ColorAdjustment::IDENTITY),
    );
    let outcome = compositor
        .render(
            &mut context,
            UiFrameRequest {
                screen: UiScreen {
                    size_in_pixels: [32, 16],
                    pixels_per_point: 1.0,
                },
                content: UiContent::Fresh {
                    primitives,
                    textures_delta,
                },
                background: [1.0, 0.0, 0.0, 1.0],
                layers: &mut underlay,
                corner_radius: CornerRadius::square(),
            },
        )
        .unwrap();
    assert!(
        matches!(outcome, UiFrameOutcome::Presented(_)),
        "{outcome:?}"
    );
    assert_eq!(
        underlay.outcome(),
        Some(VideoDrawOutcome::Drawn { draw_calls: 1 })
    );
    video.retire_submitted_work(std::sync::Arc::new(context.insert_fence().unwrap()));

    let pixels = context.read_offscreen_pixels().unwrap();
    // Видео заливает красный фон: справа чистый серый 200.
    assert_close(
        pixel(&pixels, 24, 8),
        [200, 200, 200, 255],
        1,
        "video replaces background",
    );
    // Слева egui: 200 × (1 − 128/255) ≈ 100.
    assert_close(
        pixel(&pixels, 8, 8),
        [100, 100, 100, 255],
        2,
        "egui blends over video",
    );
    compositor.destroy(&context).unwrap();
    video.destroy(&context).unwrap();
}
