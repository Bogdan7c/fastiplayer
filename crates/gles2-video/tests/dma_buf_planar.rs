//! Программный кадр без копии на настоящем EGL: планарный 8 бит из DMA-BUF (буфер ядра
//! `udmabuf`) показывается той же программой, что кадр после загрузки. Тесты проверяют пиксели
//! цели и состояние кэша, а не вызовы: паритет с путём `upload` тех же байтов, перезапись
//! памяти, счётчики импортов, вытеснение и удаление текстур, переключение источников, отказы.
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

use gles2_egui::{Gles2Context, OffscreenPixels};
use gles2_video::{
    CachedDmaBufFrame, ChromaSubsampling, ColorAdjustment, PlanarDmaBufImportOutcome,
    UploadOutcome, VideoFrameKey, VideoLayerOptions, VideoLayerRenderer,
};
use support::nv12_dma_buf::{Nv12Layout, PALETTE_A, TestNv12};
use support::planar_dma_buf::{
    ENCODING, TARGET_HEIGHT, TARGET_WIDTH, TestPlanar, drawn, full_draw, imported, patterned_frame,
};
use support::{
    OwnedFrame, assert_close, extent, offscreen_or_skip, pixel, reference_rgba, renderer, size,
};

/// Допуск на округление `f32` между двумя вычислениями координат (загрузка и импорт).
const TOLERANCE: u8 = 1;

/// Все пиксели двух картинок равны с допуском; возвращает число пикселей, не равных чёрному.
fn assert_images_match(left: &OffscreenPixels, right: &OffscreenPixels, what: &str) -> usize {
    let mut non_black = 0;
    for y in 0..TARGET_HEIGHT {
        for x in 0..TARGET_WIDTH {
            let (a, b) = (pixel(left, x, y), pixel(right, x, y));
            assert_close(a, b, TOLERANCE, &format!("{what} at ({x},{y})"));
            if a[..3] != [0, 0, 0] {
                non_black += 1;
            }
        }
    }
    non_black
}

/// Кадр, показанный из DMA-BUF, и тот же кадр после `upload` дают одну картинку.
fn import_matches_upload(chroma: ChromaSubsampling, width: u32, height: u32, name: &str) {
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let host = patterned_frame(chroma, width, height, 1);
    let Some(planar) = TestPlanar::create(name, 1, &host) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    imported(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(1))
            .unwrap(),
    );
    let (outcome, from_import) = full_draw(&mut context, &mut video);
    drawn(outcome);

    assert!(matches!(
        video.upload(&context, &host.upload(2)).unwrap(),
        UploadOutcome::Uploaded(_)
    ));
    let (outcome, from_upload) = full_draw(&mut context, &mut video);
    drawn(outcome);
    let non_black = assert_images_match(&from_import, &from_upload, name);
    assert!(
        non_black > 1000,
        "the pattern must be visible, only {non_black} non-black pixels"
    );
    video.destroy(&context).unwrap();
}

#[test]
fn imported_420_matches_the_upload_path() {
    import_matches_upload(
        ChromaSubsampling::Yuv420,
        32,
        16,
        "imported_420_matches_the_upload_path",
    );
}

#[test]
fn imported_422_matches_the_upload_path() {
    import_matches_upload(
        ChromaSubsampling::Yuv422,
        32,
        16,
        "imported_422_matches_the_upload_path",
    );
}

#[test]
fn imported_444_matches_the_upload_path() {
    import_matches_upload(
        ChromaSubsampling::Yuv444,
        32,
        16,
        "imported_444_matches_the_upload_path",
    );
}

/// Нечётный размер: цветность округляется вверх, как на пути загрузки.
#[test]
fn imported_odd_sized_420_matches_the_upload_path() {
    import_matches_upload(
        ChromaSubsampling::Yuv420,
        31,
        15,
        "imported_odd_sized_420_matches_the_upload_path",
    );
}

/// Независимая проверка цвета (а не только паритет двух путей нашего кода): одноцветный кадр
/// равен CPU-эталону формулы wgpu.
#[test]
fn solid_imported_frame_has_the_reference_color() {
    let name = "solid_imported_frame_has_the_reference_color";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let yuv = [145, 54, 34];
    let host = OwnedFrame::solid(ChromaSubsampling::Yuv420, extent(32, 16), 3, yuv);
    let Some(planar) = TestPlanar::create(name, 1, &host) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    imported(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(1))
            .unwrap(),
    );
    let (outcome, pixels) = full_draw(&mut context, &mut video);
    drawn(outcome);
    let expected = reference_rgba(ENCODING, ColorAdjustment::IDENTITY, yuv);
    for (x, y) in [(2, 2), (32, 16), (61, 29)] {
        assert_close(pixel(&pixels, x, y), expected, TOLERANCE, "solid color");
    }
    video.destroy(&context).unwrap();
}

/// Тот же паспорт: импорта нет (ни одной новой привязки), кадр показан из кэша.
#[test]
fn second_show_of_the_same_identity_does_not_import_again() {
    let name = "second_show_of_the_same_identity_does_not_import_again";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let host = patterned_frame(ChromaSubsampling::Yuv420, 32, 16, 1);
    let Some(planar) = TestPlanar::create(name, 1, &host) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    assert_eq!(
        video.show_cached_planar_dma_buf_frame(&planar.identity, VideoFrameKey(1)),
        CachedDmaBufFrame::NotCached,
        "nothing is cached before the first import"
    );
    imported(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(1))
            .unwrap(),
    );
    assert_eq!(context.dma_buf_texture_stats().attaches_succeeded, 3);
    let (_, first) = full_draw(&mut context, &mut video);

    assert_eq!(
        video.show_cached_planar_dma_buf_frame(&planar.identity, VideoFrameKey(2)),
        CachedDmaBufFrame::Shown
    );
    let (outcome, second) = full_draw(&mut context, &mut video);
    drawn(outcome);
    assert_images_match(&first, &second, "same buffer, same content");
    assert_eq!(
        context.dma_buf_texture_stats().attaches_succeeded,
        3,
        "showing from the cache must not import again"
    );
    // Повтор того же ключа — без работы; импорт уже лежащего паспорта — без новых привязок.
    assert_eq!(
        video.show_cached_planar_dma_buf_frame(&planar.identity, VideoFrameKey(2)),
        CachedDmaBufFrame::AlreadyCurrent
    );
    assert_eq!(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(2))
            .unwrap(),
        PlanarDmaBufImportOutcome::AlreadyCurrent
    );
    assert_eq!(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(3))
            .unwrap(),
        PlanarDmaBufImportOutcome::ReusedCachedImport
    );
    assert_eq!(context.dma_buf_texture_stats().attaches_succeeded, 3);
    let snapshot = video.planar_dma_buf_import_snapshot();
    assert_eq!(
        (snapshot.imports, snapshot.hits, snapshot.entries),
        (1, 1, 1)
    );
    video.destroy(&context).unwrap();
}

/// Производитель перезаписал тот же блок: следующий кадр без повторного импорта видит новое.
#[test]
fn rewriting_the_buffer_is_visible_on_the_next_frame() {
    let name = "rewriting_the_buffer_is_visible_on_the_next_frame";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let first_host = patterned_frame(ChromaSubsampling::Yuv420, 32, 16, 1);
    let second_host = patterned_frame(ChromaSubsampling::Yuv420, 32, 16, 4);
    let Some(mut planar) = TestPlanar::create(name, 1, &first_host) else {
        return;
    };
    let mut video = renderer(&context, VideoLayerOptions::default());
    imported(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(1))
            .unwrap(),
    );
    let (_, first) = full_draw(&mut context, &mut video);

    planar.rewrite(&second_host);
    assert_eq!(
        video.show_cached_planar_dma_buf_frame(&planar.identity, VideoFrameKey(2)),
        CachedDmaBufFrame::Shown
    );
    let (_, rewritten) = full_draw(&mut context, &mut video);
    assert_eq!(context.dma_buf_texture_stats().attaches_succeeded, 3);

    // Ожидаемая картинка второго кадра — путь загрузки тех же байтов.
    video.upload(&context, &second_host.upload(3)).unwrap();
    let (_, expected) = full_draw(&mut context, &mut video);
    assert_images_match(&rewritten, &expected, "rewritten content");
    let differing = (0..TARGET_HEIGHT)
        .flat_map(|y| (0..TARGET_WIDTH).map(move |x| (x, y)))
        .filter(|&(x, y)| pixel(&first, x, y) != pixel(&rewritten, x, y))
        .count();
    assert!(
        differing > 1000,
        "the rewrite must change the picture ({differing} pixels)"
    );
    video.destroy(&context).unwrap();
}

/// Ровно один текущий кадр: кольцо, NV12-импорт и планарный импорт вытесняют друг друга в любом
/// порядке, каждый раз рисуется последний показанный.
#[test]
fn only_the_last_shown_source_is_drawn() {
    let name = "only_the_last_shown_source_is_drawn";
    let target = size(TARGET_WIDTH, TARGET_HEIGHT);
    let Some(mut context) = offscreen_or_skip(name, target) else {
        return;
    };
    let uploaded = OwnedFrame::solid(ChromaSubsampling::Yuv420, extent(32, 16), 0, [145, 54, 34]);
    let blue = OwnedFrame::solid(ChromaSubsampling::Yuv420, extent(32, 16), 0, [41, 240, 110]);
    let (Some(planar), Some(nv12)) = (
        TestPlanar::create(name, 1, &blue),
        TestNv12::create(name, Nv12Layout::Composed, 2, &PALETTE_A),
    ) else {
        return;
    };
    let reference = |yuv| reference_rgba(ENCODING, ColorAdjustment::IDENTITY, yuv);
    let center = |pixels: &OffscreenPixels| pixel(pixels, 5, 5);
    let mut video = renderer(&context, VideoLayerOptions::default());

    let blue_yuv = [41, 240, 110];
    let uploaded_yuv = [145, 54, 34];
    let expect_drawn = |context: &mut Gles2Context, video: &mut VideoLayerRenderer, yuv, what| {
        let (outcome, pixels) = full_draw(context, video);
        drawn(outcome);
        assert_close(center(&pixels), reference(yuv), TOLERANCE, what);
    };

    video.upload(&context, &uploaded.upload(1)).unwrap();
    expect_drawn(&mut context, &mut video, uploaded_yuv, "uploaded");

    imported(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(2))
            .unwrap(),
    );
    expect_drawn(&mut context, &mut video, blue_yuv, "planar import");

    // Ключ 1 больше не текущий (текущий — планарный кадр): загрузка не «повтор», а настоящая.
    let reupload = video.upload(&context, &uploaded.upload(1)).unwrap();
    assert!(
        matches!(reupload, UploadOutcome::Uploaded(_)),
        "{reupload:?}"
    );
    expect_drawn(
        &mut context,
        &mut video,
        uploaded_yuv,
        "upload after planar import",
    );

    // Планарный блок лежит в кэше: повторный импорт — без привязок, и он снова текущий.
    assert_eq!(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(3))
            .unwrap(),
        PlanarDmaBufImportOutcome::ReusedCachedImport
    );
    expect_drawn(&mut context, &mut video, blue_yuv, "planar again");

    video
        .import_dma_buf_frame(&context, &nv12.frame(4))
        .unwrap();
    expect_drawn(&mut context, &mut video, PALETTE_A[0], "nv12 import");

    // Планарный импорт поверх NV12: NV12 перестаёт быть текущим.
    assert_eq!(
        video
            .import_planar_dma_buf_frame(&context, &planar.frame(5))
            .unwrap(),
        PlanarDmaBufImportOutcome::ReusedCachedImport
    );
    expect_drawn(&mut context, &mut video, blue_yuv, "planar over nv12");

    assert_eq!(
        video.show_cached_dma_buf_frame(&nv12.identity, VideoFrameKey(6)),
        CachedDmaBufFrame::Shown
    );
    expect_drawn(&mut context, &mut video, PALETTE_A[0], "nv12 from cache");

    assert_eq!(
        video.show_cached_planar_dma_buf_frame(&planar.identity, VideoFrameKey(7)),
        CachedDmaBufFrame::Shown
    );
    expect_drawn(&mut context, &mut video, blue_yuv, "planar from cache");

    video.upload(&context, &uploaded.upload(8)).unwrap();
    expect_drawn(&mut context, &mut video, uploaded_yuv, "upload over planar");

    // Кэши раздельны: планарный импорт не трогал счётчики NV12.
    let nv12_snapshot = video.dma_buf_import_snapshot();
    assert_eq!((nv12_snapshot.imports, nv12_snapshot.hits), (1, 1));
    let planar_snapshot = video.planar_dma_buf_import_snapshot();
    assert_eq!((planar_snapshot.imports, planar_snapshot.hits), (1, 1));
    video.destroy(&context).unwrap();
}
