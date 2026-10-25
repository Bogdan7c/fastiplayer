//! Аппаратный путь на настоящем EGL: NV12 из DMA-BUF (буфер ядра `udmabuf`) показывается без
//! копии. Тест проверяет пиксели цели, а не вызовы: цвета блоков против CPU-формулы wgpu NV12,
//! перезапись памяти между кадрами, счётчики кэша, отказы и переключение источников.
//!
//! Без `/dev/udmabuf` тесты пропускаются с причиной в stderr (EGL при этом обязателен, если
//! задан `GLES2_EGUI_REQUIRE_EGL`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use gles2_egui::{DmaBufTextureError, FrameStart, Gles2Context, OffscreenPixels};
use gles2_video::{
    BusyReason, CachedDmaBufFrame, ChromaSubsampling, ColorAdjustment, DmaBufImportCacheCapacity,
    DmaBufImportOutcome, PictureMapping, UploadOutcome, VideoColorConversion, VideoDrawError,
    VideoDrawOutcome, VideoDrawRequest, VideoImportError, VideoLayerOptions, VideoLayerRenderer,
    YuvColorEncoding,
};
use support::nv12_dma_buf::{BlockColors, Nv12Layout, PALETTE_A, PALETTE_B, TestNv12};
use support::{
    OwnedFrame, assert_close, draw_frame, extent, fill_request, offscreen_or_skip, pixel,
    reference_rgba, renderer, size,
};

const ENCODING: YuvColorEncoding = YuvColorEncoding::SDR_BT709_LIMITED;
/// Допуск на округление `f32` в шейдере против `f32` эталона.
const TOLERANCE: u8 = 1;

/// Кадр 32×16 растянут на цель 64×32: блок 8×8 занимает 16×16 пикселей цели.
const TARGET_WIDTH: u32 = 64;
const TARGET_HEIGHT: u32 = 32;

fn reference(yuv: [u8; 3]) -> [u8; 4] {
    reference_rgba(ENCODING, ColorAdjustment::IDENTITY, yuv)
}

/// Центр блока `index` на цели, сдвинутый на `top_bar` пикселей вниз.
fn block_center(index: usize, top_bar: u32) -> (u32, u32) {
    let (block_x, block_y) = (index as u32 % 4, index as u32 / 4);
    (block_x * 16 + 8, block_y * 16 + 8 + top_bar)
}

fn assert_palette_drawn(pixels: &OffscreenPixels, colors: &BlockColors, top_bar: u32, what: &str) {
    for (index, yuv) in colors.iter().enumerate() {
        let (x, y) = block_center(index, top_bar);
        assert_close(
            pixel(pixels, x, y),
            reference(*yuv),
            TOLERANCE,
            &format!("{what}: block {index} yuv {yuv:?} at ({x},{y})"),
        );
    }
}

fn drawn(outcome: VideoDrawOutcome) {
    assert!(
        matches!(outcome, VideoDrawOutcome::Drawn { .. }),
        "expected a drawn frame, got {outcome:?}"
    );
}

fn imported(outcome: DmaBufImportOutcome) {
    assert!(
        matches!(outcome, DmaBufImportOutcome::Imported(_)),
        "expected a fresh import, got {outcome:?}"
    );
}

fn full_draw(
    context: &mut Gles2Context,
    video: &mut VideoLayerRenderer,
) -> (VideoDrawOutcome, OffscreenPixels) {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let rects = [support::full_rect(target)];
    let request = fill_request(target, &rects, ENCODING, ColorAdjustment::IDENTITY);
    draw_frame(context, video, &request)
}

/// Один и тот же рисунок из composed и separate буферов даёт цвета блоков эталона.
fn renders_block_colors(layout: Nv12Layout, name: &str) {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let Some(nv12) = TestNv12::create(name, layout, 1, &PALETTE_A) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    imported(
        video
            .import_dma_buf_frame(&context, &nv12.frame(1))
            .unwrap(),
    );
    let (outcome, pixels) = full_draw(&mut context, &mut video);
    drawn(outcome);
    assert_palette_drawn(&pixels, &PALETTE_A, 0, name);
    video.destroy(&context).unwrap();
}

#[test]
fn composed_nv12_renders_known_block_colors() {
    renders_block_colors(
        Nv12Layout::Composed,
        "composed_nv12_renders_known_block_colors",
    );
}

#[test]
fn separate_nv12_buffers_render_the_same_colors() {
    renders_block_colors(
        Nv12Layout::Separate,
        "separate_nv12_buffers_render_the_same_colors",
    );
}

/// Letterbox: картинка 2:1 в области 64×48 — чёрные полосы сверху и снизу по 8 пикселей.
#[test]
fn letterbox_bars_are_black_and_picture_is_shifted() {
    let name = "letterbox_bars_are_black_and_picture_is_shifted";
    let target = size(64, 48);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let Some(nv12) = TestNv12::create(name, Nv12Layout::Composed, 1, &PALETTE_A) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    imported(
        video
            .import_dma_buf_frame(&context, &nv12.frame(1))
            .unwrap(),
    );
    let rects = [support::full_rect(target)];
    let request = VideoDrawRequest {
        picture: PictureMapping {
            uv_scale: [1.0, 1.5],
            uv_offset: [0.0, -0.25],
        },
        ..fill_request(target, &rects, ENCODING, ColorAdjustment::IDENTITY)
    };
    let (outcome, pixels) = draw_frame(&mut context, &mut video, &request);
    drawn(outcome);
    for (x, y) in [(2, 2), (60, 5), (2, 42), (60, 46)] {
        assert_close(pixel(&pixels, x, y), [0, 0, 0, 255], 0, "letterbox bar");
    }
    assert_palette_drawn(&pixels, &PALETTE_A, 8, name);
    video.destroy(&context).unwrap();
}

/// Тот же паспорт: память перезаписана, импорта нет — второй кадр видит новые цвета.
#[test]
fn cached_frame_shows_memory_rewritten_by_the_producer() {
    let name = "cached_frame_shows_memory_rewritten_by_the_producer";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let Some(mut nv12) = TestNv12::create(name, Nv12Layout::Composed, 1, &PALETTE_A) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    imported(
        video
            .import_dma_buf_frame(&context, &nv12.frame(1))
            .unwrap(),
    );
    let (_, first) = full_draw(&mut context, &mut video);
    assert_palette_drawn(&first, &PALETTE_A, 0, "first frame");

    nv12.rewrite(&PALETTE_B);
    let images_before = context.dma_buf_texture_stats().images_created;
    assert_eq!(
        video.show_cached_dma_buf_frame(&nv12.identity, gles2_video::VideoFrameKey(2)),
        CachedDmaBufFrame::Shown
    );
    let (_, second) = full_draw(&mut context, &mut video);
    assert_palette_drawn(&second, &PALETTE_B, 0, "rewritten frame");
    assert_eq!(
        context.dma_buf_texture_stats().images_created,
        images_before,
        "showing from the cache must not import again"
    );
    // Повтор того же ключа — без работы.
    assert_eq!(
        video.show_cached_dma_buf_frame(&nv12.identity, gles2_video::VideoFrameKey(2)),
        CachedDmaBufFrame::AlreadyCurrent
    );
    video.destroy(&context).unwrap();
}

/// После импорта все fd закрыты (буферы освобождены владельцем): текстура держит память сама.
#[test]
fn frame_keeps_drawing_after_every_fd_is_closed() {
    let name = "frame_keeps_drawing_after_every_fd_is_closed";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let Some(nv12) = TestNv12::create(name, Nv12Layout::Separate, 1, &PALETTE_A) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    imported(
        video
            .import_dma_buf_frame(&context, &nv12.frame(1))
            .unwrap(),
    );
    drop(nv12);
    let (outcome, pixels) = full_draw(&mut context, &mut video);
    drawn(outcome);
    assert_palette_drawn(&pixels, &PALETTE_A, 0, name);
    video.destroy(&context).unwrap();
}

/// Три буфера по кругу, девять кадров: три импорта, шесть попаданий, EGLImage не текут.
#[test]
fn three_buffers_in_a_ring_import_once_each() {
    let name = "three_buffers_in_a_ring_import_once_each";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let mut buffers = Vec::new();
    for resource_id in 0..3 {
        match TestNv12::create(name, Nv12Layout::Composed, resource_id, &PALETTE_A) {
            Some(buffer) => buffers.push(buffer),
            None => return,
        }
    }
    let mut video = renderer(&context, VideoLayerOptions::default());
    video
        .set_dma_buf_import_capacity(
            &context,
            DmaBufImportCacheCapacity::matching_decoder_surface_pool(3),
        )
        .unwrap();
    for frame_number in 0..9_u64 {
        let buffer = &buffers[(frame_number % 3) as usize];
        let key = gles2_video::VideoFrameKey(frame_number + 1);
        if video.show_cached_dma_buf_frame(&buffer.identity, key) == CachedDmaBufFrame::NotCached {
            imported(
                video
                    .import_dma_buf_frame(&context, &buffer.frame(frame_number + 1))
                    .unwrap(),
            );
        }
        let (outcome, pixels) = full_draw(&mut context, &mut video);
        drawn(outcome);
        assert_palette_drawn(&pixels, &PALETTE_A, 0, &format!("frame {frame_number}"));
    }
    let snapshot = video.dma_buf_import_snapshot();
    assert_eq!(
        (snapshot.imports, snapshot.hits, snapshot.entries),
        (3, 6, 3)
    );
    let stats = context.dma_buf_texture_stats();
    assert_eq!(stats.images_created, 6, "two planes per import");
    assert_eq!(
        stats.images_created, stats.images_destroyed,
        "no EGLImage leaks"
    );
    video.destroy(&context).unwrap();
}

/// Отказ драйвера (fd, не являющийся dma-buf): типизированная ошибка, кэш пуст, прежний
/// кадр остаётся на экране, повторное рисование не падает.
#[test]
fn driver_rejection_keeps_the_previous_frame_and_leaves_the_cache_clean() {
    let name = "driver_rejection_keeps_the_previous_frame_and_leaves_the_cache_clean";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    // Программные драйверы Mesa (llvmpipe/softpipe) не проверяют fd при `eglCreateImage` и
    // «импортируют» даже `/dev/null` (проверено в сессии 07): отказ драйвера там не
    // воспроизвести. На аппаратном драйвере принятый не-dma-buf — падение теста.
    let gl_renderer = context.capabilities().gl_renderer();
    if ["llvmpipe", "softpipe"]
        .iter()
        .any(|driver| gl_renderer.contains(driver))
    {
        eprintln!(
            "skip {name}: software driver ({gl_renderer}) does not validate dma-buf fds at import"
        );
        return;
    }
    let mut video = renderer(&context, VideoLayerOptions::default());
    // Прежний кадр — обычная загрузка из памяти.
    let host = OwnedFrame::solid(
        ChromaSubsampling::Yuv420,
        extent(32, 16),
        0,
        [128, 128, 128],
    );
    video.upload(&context, &host.upload(1)).unwrap();

    let Some(good) = TestNv12::create(name, Nv12Layout::Composed, 9, &PALETTE_A) else {
        return;
    };
    // Паспорт настоящий, дескрипторы — /dev/null.
    let null = std::fs::File::open("/dev/null").unwrap();
    let good_frame = good.frame(2);
    let plane = |source: &gles2_video::DmaBufPlane<'_>| gles2_video::DmaBufPlane {
        fd: std::os::fd::AsFd::as_fd(&null),
        ..*source
    };
    let broken = gles2_video::DmaBufNv12Frame::new(
        gles2_video::VideoFrameKey(2),
        &good.identity,
        good_frame.luma_extent(),
        plane(good_frame.luma()),
        plane(good_frame.chroma()),
    )
    .unwrap();
    let error = video.import_dma_buf_frame(&context, &broken).unwrap_err();
    assert!(
        matches!(
            error,
            VideoImportError::Attach {
                source: DmaBufTextureError::ImageCreation(_)
                    | DmaBufTextureError::TextureTarget { .. },
                ..
            }
        ),
        "driver rejection keeps its own reason: {error:?}"
    );
    let snapshot = video.dma_buf_import_snapshot();
    assert_eq!(
        (snapshot.entries, snapshot.imports, snapshot.import_failures),
        (0, 0, 1)
    );
    let stats = context.dma_buf_texture_stats();
    assert_eq!(stats.images_created, stats.images_destroyed);
    // Прежний (загруженный) кадр цел: серый, как был.
    let (outcome, pixels) = full_draw(&mut context, &mut video);
    drawn(outcome);
    assert_close(
        pixel(&pixels, 32, 16),
        reference_rgba(ENCODING, ColorAdjustment::IDENTITY, [128, 128, 128]),
        TOLERANCE,
        "previous host frame survives a failed import",
    );
    // И импорт настоящих дескрипторов после отказа работает.
    imported(
        video
            .import_dma_buf_frame(&context, &good.frame(3))
            .unwrap(),
    );
    let (_, pixels) = full_draw(&mut context, &mut video);
    assert_palette_drawn(&pixels, &PALETTE_A, 0, "import after a failure");
    video.destroy(&context).unwrap();
}

/// Смена формата потока при неудачном импорте: старые записи уничтожены, текущий кадр пропал.
#[test]
fn failed_import_after_format_change_leaves_no_current_frame() {
    let name = "failed_import_after_format_change_leaves_no_current_frame";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let Some(good) = TestNv12::create(name, Nv12Layout::Composed, 1, &PALETTE_A) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    imported(
        video
            .import_dma_buf_frame(&context, &good.frame(1))
            .unwrap(),
    );

    // Тот же размер, но другой модификатор: сигнатура формата сменилась.
    let other = TestNv12::create(name, Nv12Layout::Composed, 2, &PALETTE_B).unwrap();
    let template = other.frame(2);
    let mut changed_identity = other.identity.clone();
    changed_identity.format.modifier = 0x0200_0000_1040_1b04;
    let null = std::fs::File::open("/dev/null").unwrap();
    let plane = |source: &gles2_video::DmaBufPlane<'_>| gles2_video::DmaBufPlane {
        fd: std::os::fd::AsFd::as_fd(&null),
        modifier: changed_identity.format.modifier,
        ..*source
    };
    let broken = gles2_video::DmaBufNv12Frame::new(
        gles2_video::VideoFrameKey(2),
        &changed_identity,
        template.luma_extent(),
        plane(template.luma()),
        plane(template.chroma()),
    )
    .unwrap();
    assert!(video.import_dma_buf_frame(&context, &broken).is_err());
    let snapshot = video.dma_buf_import_snapshot();
    assert_eq!(
        (
            snapshot.entries,
            snapshot.evicted_format_change,
            snapshot.import_failures
        ),
        (0, 1, 1)
    );
    let (outcome, pixels) = full_draw(&mut context, &mut video);
    assert_eq!(outcome, VideoDrawOutcome::ClearedWithoutFrame);
    assert_close(pixel(&pixels, 32, 16), [0, 0, 0, 255], 0, "cleared target");
    video.destroy(&context).unwrap();
}

/// Загруженный кадр → импорт → загруженный: каждый раз рисуется правильный источник, а кольцо
/// по-прежнему отвечает `Busy`, когда слотов нет.
#[test]
fn sources_switch_both_ways_and_the_ring_still_reports_busy() {
    let name = "sources_switch_both_ways_and_the_ring_still_reports_busy";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let Some(nv12) = TestNv12::create(name, Nv12Layout::Composed, 1, &PALETTE_A) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let green = [145, 54, 34];
    let blue = [41, 240, 110];
    let host = |yuv| OwnedFrame::solid(ChromaSubsampling::Yuv420, extent(32, 16), 0, yuv);
    let first_host = host(green);
    video.upload(&context, &first_host.upload(1)).unwrap();
    let (_, pixels) = full_draw(&mut context, &mut video);
    assert_close(
        pixel(&pixels, 32, 16),
        reference(green),
        TOLERANCE,
        "host frame",
    );

    imported(
        video
            .import_dma_buf_frame(&context, &nv12.frame(2))
            .unwrap(),
    );
    let (_, pixels) = full_draw(&mut context, &mut video);
    assert_palette_drawn(&pixels, &PALETTE_A, 0, "imported frame");

    // Тот же ключ, что у импортированного, — повтор, а не загрузка.
    let second_host = host(blue);
    assert_eq!(
        video.upload(&context, &second_host.upload(2)).unwrap(),
        UploadOutcome::AlreadyCurrent
    );
    video.upload(&context, &second_host.upload(3)).unwrap();
    let (_, pixels) = full_draw(&mut context, &mut video);
    assert_close(
        pixel(&pixels, 32, 16),
        reference(blue),
        TOLERANCE,
        "host frame again",
    );
    assert_close(
        pixel(&pixels, 5, 5),
        reference(blue),
        TOLERANCE,
        "no imported leftovers",
    );

    // Без fence слоты не освобождаются: после переключений кольцо ведёт себя как раньше.
    let mut results = Vec::new();
    for key in 10..14 {
        results.push(video.upload(&context, &second_host.upload(key)).unwrap());
    }
    assert!(
        results.iter().any(|outcome| matches!(
            outcome,
            UploadOutcome::Busy(BusyReason::AwaitingSubmittedWorkFence)
        )),
        "uploads without a fence must hit Busy: {results:?}"
    );
    video.destroy(&context).unwrap();
}

/// HDR → SDR для NV12 8 бит — типизированный отказ до записи в цель.
#[test]
fn hdr_conversion_is_refused_for_nv12() {
    let name = "hdr_conversion_is_refused_for_nv12";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let Some(nv12) = TestNv12::create(name, Nv12Layout::Composed, 1, &PALETTE_A) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    imported(
        video
            .import_dma_buf_frame(&context, &nv12.frame(1))
            .unwrap(),
    );
    let rects = [support::full_rect(target)];
    let request = support::fill_request_with(
        target,
        &rects,
        VideoColorConversion::HdrToSdr(gles2_video::HdrToSdrConversion {
            range: gles2_video::YuvRange::Limited,
            transfer: gles2_video::HdrTransfer::Pq,
            levels: gles2_video::HdrReferenceLevels::REFERENCE_DEFAULT,
        }),
    );
    let FrameStart::Ready(frame) = context.begin_frame().unwrap() else {
        panic!("frame skipped");
    };
    let error = video.draw(&frame, &request).unwrap_err();
    assert!(matches!(
        error,
        VideoDrawError::HdrToSdrNeedsHighBitFrame { .. }
    ));
    frame.present().unwrap();
    video.destroy(&context).unwrap();
}

/// Чужой контекст и очистка: purge удаляет текстуры, текущий кадр забыт.
#[test]
fn purge_deletes_every_import_and_forgets_the_current_frame() {
    let name = "purge_deletes_every_import_and_forgets_the_current_frame";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let Some(nv12) = TestNv12::create(name, Nv12Layout::Composed, 1, &PALETTE_A) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    imported(
        video
            .import_dma_buf_frame(&context, &nv12.frame(1))
            .unwrap(),
    );
    video.purge_dma_buf_imports(&context).unwrap();
    assert_eq!(video.dma_buf_import_snapshot().entries, 0);
    let (outcome, _) = full_draw(&mut context, &mut video);
    assert_eq!(outcome, VideoDrawOutcome::ClearedWithoutFrame);
    // Кэш пуст: тот же паспорт снова требует импорта.
    assert_eq!(
        video.show_cached_dma_buf_frame(&nv12.identity, gles2_video::VideoFrameKey(5)),
        CachedDmaBufFrame::NotCached
    );
    video.destroy(&context).unwrap();
}
