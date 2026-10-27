//! Кэш планарных импортов на настоящем EGL: вытеснение и удаление текстур, общая ёмкость и
//! очистка двух кэшей, отказы (10 бит, чужой контекст, драйвер) и то, что отказ не оставляет
//! следов. Паритет пикселей и переключение источников — в `dma_buf_planar.rs`.
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

use gles2_egui::DmaBufTextureError;
use gles2_video::{
    CachedDmaBufFrame, ChromaSubsampling, ColorAdjustment, DmaBufImportCacheCapacity,
    DmaBufPlanarFrame, DmaBufPlanarFrameError, DmaBufPlanarIdentity, DmaBufPlaneLocation,
    PlanarLayout, SampleDepth, VideoDrawOutcome, VideoFrameKey, VideoImportError,
    VideoLayerOptions,
};
use support::nv12_dma_buf::{Nv12Layout, PALETTE_A, TestNv12};
use support::planar_dma_buf::{
    ENCODING, TARGET_HEIGHT, TARGET_WIDTH, TestPlanar, drawn, full_draw, imported, patterned_frame,
};
use support::state::snapshot;
use support::{
    OwnedFrame, assert_close, extent, offscreen_or_skip, pixel, reference_rgba, renderer, size,
};

/// Допуск на округление `f32` между двумя вычислениями координат (загрузка и импорт).
const TOLERANCE: u8 = 1;

/// Три блока по кругу при ёмкости 3: по одному импорту на блок, остальное — попадания.
/// Другой паспорт — новый импорт; вытесненные записи удаляют свои текстуры.
#[test]
fn new_identities_import_and_eviction_deletes_their_textures() {
    let name = "new_identities_import_and_eviction_deletes_their_textures";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(context) = offscreen_or_skip(name, target) else {
        return;
    };
    let host = patterned_frame(ChromaSubsampling::Yuv420, 32, 16, 1);
    let mut blocks = Vec::new();
    for resource_id in 0..3 {
        match TestPlanar::create(name, resource_id, &host) {
            Some(block) => blocks.push(block),
            None => return,
        }
    }
    let mut video = renderer(&context, VideoLayerOptions::default());
    let before = snapshot(&context);
    video
        .set_dma_buf_import_capacity(
            &context,
            DmaBufImportCacheCapacity::matching_decoder_surface_pool(2),
        )
        .unwrap();
    imported(
        video
            .import_planar_dma_buf_frame(&context, &blocks[0].frame(1))
            .unwrap(),
    );
    imported(
        video
            .import_planar_dma_buf_frame(&context, &blocks[1].frame(2))
            .unwrap(),
    );
    assert_eq!(
        snapshot(&context).live_textures,
        before.live_textures + 6,
        "two blocks, three textures each"
    );
    // Третий блок вытесняет самый давний (нулевой): живых текстур по-прежнему шесть.
    imported(
        video
            .import_planar_dma_buf_frame(&context, &blocks[2].frame(3))
            .unwrap(),
    );
    assert_eq!(
        snapshot(&context).live_textures,
        before.live_textures + 6,
        "the evicted block's textures are deleted"
    );
    let planar = video.planar_dma_buf_import_snapshot();
    assert_eq!(
        (planar.imports, planar.entries, planar.evicted_capacity),
        (3, 2, 1)
    );
    assert_eq!(
        video.show_cached_planar_dma_buf_frame(&blocks[0].identity, VideoFrameKey(4)),
        CachedDmaBufFrame::NotCached,
        "the evicted block needs a new import"
    );
    assert_eq!(
        video.show_cached_planar_dma_buf_frame(&blocks[1].identity, VideoFrameKey(5)),
        CachedDmaBufFrame::Shown
    );
    let stats = context.dma_buf_texture_stats();
    assert_eq!(stats.images_created, 9);
    assert_eq!(
        stats.images_created, stats.images_destroyed,
        "no EGLImage leaks"
    );

    // Уменьшение ёмкости до одной записи удаляет ещё три текстуры, текущий кадр остаётся.
    video
        .set_dma_buf_import_capacity(
            &context,
            DmaBufImportCacheCapacity::matching_decoder_surface_pool(1),
        )
        .unwrap();
    assert_eq!(snapshot(&context).live_textures, before.live_textures + 3);
    video.destroy(&context).unwrap();
    assert_eq!(
        snapshot(&context).live_textures,
        before.live_textures,
        "destroy leaves no textures"
    );
}

/// Вытеснение текущего кадра (его запись больше не лежит в кэше): рисовать нечем.
#[test]
fn evicting_the_current_frame_leaves_nothing_to_draw() {
    let name = "evicting_the_current_frame_leaves_nothing_to_draw";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let host = patterned_frame(ChromaSubsampling::Yuv420, 32, 16, 1);
    let Some(planar) = TestPlanar::create(name, 1, &host) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    imported(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(1))
            .unwrap(),
    );
    video.purge_dma_buf_imports(&context).unwrap();
    assert_eq!(video.planar_dma_buf_import_snapshot().entries, 0);
    let (outcome, pixels) = full_draw(&mut context, &mut video);
    assert_eq!(outcome, VideoDrawOutcome::ClearedWithoutFrame);
    assert_close(pixel(&pixels, 32, 16), [0, 0, 0, 255], 0, "cleared target");
    assert_eq!(
        video.show_cached_planar_dma_buf_frame(&planar.identity, VideoFrameKey(2)),
        CachedDmaBufFrame::NotCached
    );
    video.destroy(&context).unwrap();
}

/// Ёмкость и очистка действуют на оба кэша.
#[test]
fn capacity_and_purge_cover_both_import_caches() {
    let name = "capacity_and_purge_cover_both_import_caches";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(context) = offscreen_or_skip(name, target) else {
        return;
    };
    let host = patterned_frame(ChromaSubsampling::Yuv420, 32, 16, 1);
    let (Some(planar), Some(nv12)) = (
        TestPlanar::create(name, 1, &host),
        TestNv12::create(name, Nv12Layout::Composed, 2, &PALETTE_A),
    ) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    video
        .set_dma_buf_import_capacity(
            &context,
            DmaBufImportCacheCapacity::matching_decoder_surface_pool(5),
        )
        .unwrap();
    assert_eq!(video.dma_buf_import_snapshot().capacity, 5);
    assert_eq!(video.planar_dma_buf_import_snapshot().capacity, 5);
    imported(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(1))
            .unwrap(),
    );
    video
        .import_dma_buf_frame(&context, &nv12.frame(2))
        .unwrap();
    video.purge_dma_buf_imports(&context).unwrap();
    assert_eq!(video.dma_buf_import_snapshot().entries, 0);
    assert_eq!(video.planar_dma_buf_import_snapshot().entries, 0);
    assert_eq!(video.planar_dma_buf_import_snapshot().purged, 1);
    video.destroy(&context).unwrap();
}

/// 10 бит не доходит до драйвера: типизированный отказ при сборке кадра, привязок нет.
#[test]
fn ten_bit_frame_is_refused_before_any_import() {
    let name = "ten_bit_frame_is_refused_before_any_import";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(context) = offscreen_or_skip(name, target) else {
        return;
    };
    let host = patterned_frame(ChromaSubsampling::Yuv420, 32, 16, 1);
    let Some(planar) = TestPlanar::create(name, 1, &host) else {
        return;
    };
    let mut ten_bit: DmaBufPlanarIdentity = planar.identity.clone();
    ten_bit.layout = PlanarLayout {
        chroma: ChromaSubsampling::Yuv420,
        depth: SampleDepth::Bits10,
    };
    let fd = planar.frame(1).dma_buf();
    assert_eq!(
        DmaBufPlanarFrame::new(VideoFrameKey(1), &ten_bit, fd).unwrap_err(),
        DmaBufPlanarFrameError::UnsupportedDepth {
            depth: SampleDepth::Bits10
        }
    );
    assert_eq!(context.dma_buf_texture_stats().attaches_succeeded, 0);
    assert_eq!(context.dma_buf_texture_stats().images_created, 0);
}

/// Чужой контекст: типизированный отказ, кэш не тронут.
#[test]
fn foreign_context_is_refused_without_touching_the_cache() {
    let name = "foreign_context_is_refused_without_touching_the_cache";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(context) = offscreen_or_skip(name, target) else {
        return;
    };
    let Some(other) = offscreen_or_skip(name, target) else {
        return;
    };
    let host = patterned_frame(ChromaSubsampling::Yuv420, 32, 16, 1);
    let Some(planar) = TestPlanar::create(name, 1, &host) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    assert_eq!(
        video
            .import_planar_dma_buf_frame(&other, &planar.frame(1))
            .unwrap_err(),
        VideoImportError::ForeignContext
    );
    let planar_snapshot = video.planar_dma_buf_import_snapshot();
    assert_eq!((planar_snapshot.entries, planar_snapshot.imports), (0, 0));
    assert_eq!(other.dma_buf_texture_stats().images_created, 0);
    video.destroy(&context).unwrap();
}

/// Отказ драйвера (fd, не являющийся dma-buf): типизированная ошибка с причиной, кэш пуст,
/// текстуры удалены, прежний кадр на экране, после отказа настоящий импорт работает.
#[test]
fn driver_rejection_keeps_the_previous_frame_and_leaves_the_cache_clean() {
    let name = "driver_rejection_keeps_the_previous_frame_and_leaves_the_cache_clean";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    // Программные драйверы Mesa не проверяют fd при `eglCreateImage` (см. тест NV12).
    let gl_renderer = context.capabilities().gl_renderer();
    if ["llvmpipe", "softpipe"]
        .iter()
        .any(|driver| gl_renderer.contains(driver))
    {
        eprintln!("skip {name}: software driver ({gl_renderer}) does not validate dma-buf fds");
        return;
    }
    let host = patterned_frame(ChromaSubsampling::Yuv420, 32, 16, 1);
    let Some(planar) = TestPlanar::create(name, 1, &host) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    let gray = OwnedFrame::solid(
        ChromaSubsampling::Yuv420,
        extent(32, 16),
        0,
        [128, 128, 128],
    );
    video.upload(&context, &gray.upload(1)).unwrap();
    let before = snapshot(&context);

    let null = std::fs::File::open("/dev/null").unwrap();
    let broken = DmaBufPlanarFrame::new(
        VideoFrameKey(2),
        &planar.identity,
        std::os::fd::AsFd::as_fd(&null),
    )
    .unwrap();
    let error = video
        .import_planar_dma_buf_frame(&context, &broken)
        .unwrap_err();
    assert!(
        matches!(
            error,
            VideoImportError::PlanarAttach {
                source: DmaBufTextureError::ImageCreation(_)
                    | DmaBufTextureError::TextureTarget { .. },
                ..
            }
        ),
        "driver rejection keeps its own reason: {error:?}"
    );
    let cache = video.planar_dma_buf_import_snapshot();
    assert_eq!(
        (cache.entries, cache.imports, cache.import_failures),
        (0, 0, 1)
    );
    assert_eq!(
        snapshot(&context).live_textures,
        before.live_textures,
        "the textures created for the failed import are deleted"
    );
    let stats = context.dma_buf_texture_stats();
    assert_eq!(stats.images_created, stats.images_destroyed);
    let (outcome, pixels) = full_draw(&mut context, &mut video);
    drawn(outcome);
    assert_close(
        pixel(&pixels, 32, 16),
        reference_rgba(ENCODING, ColorAdjustment::IDENTITY, [128, 128, 128]),
        TOLERANCE,
        "previous host frame survives a failed import",
    );
    imported(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(3))
            .unwrap(),
    );
    video.destroy(&context).unwrap();
}

/// Смещение и шаг плоскостей входят в паспорт: тот же объект с другой раскладкой — другой кэш-ключ.
#[test]
fn same_object_with_another_layout_is_a_different_identity() {
    let name = "same_object_with_another_layout_is_a_different_identity";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(context) = offscreen_or_skip(name, target) else {
        return;
    };
    let host = patterned_frame(ChromaSubsampling::Yuv420, 32, 16, 1);
    let Some(planar) = TestPlanar::create(name, 1, &host) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    imported(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(1))
            .unwrap(),
    );
    let mut shifted = planar.identity.clone();
    shifted.planes[2] = DmaBufPlaneLocation {
        offset: shifted.planes[2].offset + 256,
        ..shifted.planes[2]
    };
    assert_eq!(
        video.show_cached_planar_dma_buf_frame(&shifted, VideoFrameKey(2)),
        CachedDmaBufFrame::NotCached
    );
    video.destroy(&context).unwrap();
}

/// Планарный кадр, потом NV12, потом неудачный импорт NV12 со сменой формата (кэш NV12 выброшен,
/// текущего NV12 нет): планарный кадр, который NV12 уже вытеснил, НЕ воскресает на экране.
#[test]
fn failed_nv12_import_does_not_resurrect_an_older_planar_frame() {
    let name = "failed_nv12_import_does_not_resurrect_an_older_planar_frame";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    // Отказ драйвера воспроизводится только на аппаратном драйвере (см. тест отказа выше).
    let gl_renderer = context.capabilities().gl_renderer();
    if ["llvmpipe", "softpipe"]
        .iter()
        .any(|driver| gl_renderer.contains(driver))
    {
        eprintln!("skip {name}: software driver ({gl_renderer}) does not validate dma-buf fds");
        return;
    }
    let host = patterned_frame(ChromaSubsampling::Yuv420, 32, 16, 1);
    let (Some(planar), Some(nv12)) = (
        TestPlanar::create(name, 1, &host),
        TestNv12::create(name, Nv12Layout::Composed, 2, &PALETTE_A),
    ) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    imported(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(1))
            .unwrap(),
    );
    video
        .import_dma_buf_frame(&context, &nv12.frame(2))
        .unwrap();

    let template = nv12.frame(3);
    let mut changed_identity = nv12.identity.clone();
    changed_identity.format.modifier = 0x0200_0000_1040_1b04;
    let null = std::fs::File::open("/dev/null").unwrap();
    let plane = |source: &gles2_video::DmaBufPlane<'_>| gles2_video::DmaBufPlane {
        fd: std::os::fd::AsFd::as_fd(&null),
        modifier: changed_identity.format.modifier,
        ..*source
    };
    let broken = gles2_video::DmaBufNv12Frame::new(
        VideoFrameKey(3),
        &changed_identity,
        template.luma_extent(),
        plane(template.luma()),
        plane(template.chroma()),
    )
    .unwrap();
    assert!(video.import_dma_buf_frame(&context, &broken).is_err());
    let (outcome, pixels) = full_draw(&mut context, &mut video);
    assert_eq!(outcome, VideoDrawOutcome::ClearedWithoutFrame);
    assert_close(pixel(&pixels, 32, 16), [0, 0, 0, 255], 0, "cleared target");
    video.destroy(&context).unwrap();
}
