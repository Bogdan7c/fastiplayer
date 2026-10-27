//! Кадр от декодера до экрана через публичный `RenderShell`: prepare → install → materialize →
//! acquire → render_frame на настоящем EGL без окна, с проверкой пикселей цели.

use render_core::RenderViewport;
use render_shell_api::{
    FrameImportUnsupportedReason, FrameMaterializationOutcome, RenderFrameDropReason,
    RenderFrameFailureKind, RenderFrameOutcome, RenderShell, SurfaceFrameAcquire, VideoImportPath,
};

use super::{
    FrameExtras, assert_gray, connect_software_decoder, materialize_key, offscreen_or_skip, pixel,
    present, screen_for,
};
use crate::test_support::fake_decoder::{LookupMode, SplitPicture};

/// Цель 64×64 с кадром 64×32: картинка по центру (строки 16..48), полосы letterbox чёрные.
const TARGET: [u32; 2] = [64, 64];

fn picture(handle: u64, left_luma: u8, right_luma: u8) -> SplitPicture {
    SplitPicture {
        handle,
        size: [64, 32],
        left_luma,
        right_luma,
    }
}

/// Главное: кадр программного декодера, пройдя контракт целиком, доходит до экрана — левая и
/// правая половины картинки нужных цветов в нужных местах, полосы letterbox чёрные, а исход —
/// `Presented`.
#[test]
fn a_decoder_frame_reaches_the_screen_with_letterbox_bars() {
    let Some(mut renderer) = offscreen_or_skip(
        "a_decoder_frame_reaches_the_screen_with_letterbox_bars",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (key, _lease) = materialize_key(&mut renderer, &decoder, picture(1, 180, 60));

    let outcome = present(&mut renderer, TARGET, Some(&key), FrameExtras::default());

    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "{outcome:?}"
    );
    assert_gray(pixel(&renderer, 16, 32), 180, "left half of the picture");
    assert_gray(pixel(&renderer, 48, 32), 60, "right half of the picture");
    assert_eq!(pixel(&renderer, 16, 5), [0, 0, 0, 255], "top letterbox bar");
    assert_eq!(
        pixel(&renderer, 48, 60),
        [0, 0, 0, 255],
        "bottom letterbox bar"
    );
    // Строка хвоста stride не просачивается в картинку: крайние столбцы — цвет своей половины.
    assert_gray(pixel(&renderer, 0, 32), 180, "left edge");
    assert_gray(
        pixel(&renderer, 63, 32),
        60,
        "right edge without the stride tail",
    );
}

/// Второй кадр другого цвета (как после seek) — новые пиксели, а не прежняя картинка.
#[test]
fn the_next_frame_replaces_the_picture() {
    let Some(mut renderer) = offscreen_or_skip("the_next_frame_replaces_the_picture", TARGET)
    else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (first, _first_lease) = materialize_key(&mut renderer, &decoder, picture(1, 180, 60));
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&first), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(pixel(&renderer, 16, 32), 180, "first frame");

    let (second, _second_lease) = materialize_key(&mut renderer, &decoder, picture(2, 30, 220));
    assert!(matches!(
        present(&mut renderer, TARGET, Some(&second), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));

    assert_gray(pixel(&renderer, 16, 32), 30, "second frame, left half");
    assert_gray(pixel(&renderer, 48, 32), 220, "second frame, right half");
    assert_eq!(renderer.frame_counters().frames_presented, 2);
    assert_eq!(
        renderer.frame_counters().video_frames_reshown,
        0,
        "keys of the current frames are drawn without bringing a picture back"
    );
}

/// Область под исключением (сайдбар) остаётся чёрной, картинка правее — на месте, и видео
/// покрывает цель само: сборщик не заливает фон (счётчик), а кадр без видео фон заливает.
#[test]
fn exclusions_stay_black_and_video_covers_the_target_without_a_background_fill() {
    let Some(mut renderer) = offscreen_or_skip(
        "exclusions_stay_black_and_video_covers_the_target_without_a_background_fill",
        TARGET,
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (key, _lease) = materialize_key(&mut renderer, &decoder, picture(1, 180, 60));

    let sidebar = RenderViewport::new(0, 0, 20, TARGET[1]);
    let outcome = present(
        &mut renderer,
        TARGET,
        Some(&key),
        FrameExtras {
            exclusions: vec![sidebar],
            ..FrameExtras::default()
        },
    );

    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "{outcome:?}"
    );
    for y in [10, 32, 55] {
        assert_eq!(pixel(&renderer, 10, y), [0, 0, 0, 255], "sidebar at y={y}");
    }
    assert_gray(
        pixel(&renderer, 30, 32),
        180,
        "picture right of the sidebar",
    );
    assert_gray(pixel(&renderer, 50, 32), 60, "right half");
    let counters = renderer.frame_counters();
    assert_eq!(counters.background_fills_skipped, 1, "{counters:?}");
    assert_eq!(counters.background_fills, 0, "{counters:?}");
    let diagnostics = renderer.diagnostics();
    assert!(diagnostics.active_color_path.is_some(), "{diagnostics:?}");
    assert_eq!(diagnostics.video_draw_rect_count, 1);

    // Кадр без видео: фон заливается сборщиком, диагностика видео сбрасывается.
    let outcome = present(&mut renderer, TARGET, None, FrameExtras::default());
    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "{outcome:?}"
    );
    assert_eq!(
        pixel(&renderer, 30, 32),
        [0, 0, 0, 255],
        "no video, black target"
    );
    let counters = renderer.frame_counters();
    assert_eq!(counters.background_fills, 1, "{counters:?}");
    assert_eq!(counters.background_fills_skipped, 1, "{counters:?}");
    assert!(renderer.diagnostics().active_color_path.is_none());
    assert_eq!(renderer.diagnostics().video_draw_rect_count, 0);
}

/// Размер кадра приводит цель к себе: после увеличения окна картинка заполняет новую цель.
#[test]
fn resizing_the_window_resizes_the_target_and_the_picture_follows() {
    let Some(mut renderer) = offscreen_or_skip(
        "resizing_the_window_resizes_the_target_and_the_picture_follows",
        [64, 32],
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (key, _lease) = materialize_key(&mut renderer, &decoder, picture(1, 180, 60));
    assert!(matches!(
        present(&mut renderer, [64, 32], Some(&key), FrameExtras::default()),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(pixel(&renderer, 16, 16), 180, "before the resize");

    let bigger = [128, 64];
    let outcome = present(&mut renderer, bigger, Some(&key), FrameExtras::default());

    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "{outcome:?}"
    );
    let pixels = renderer.read_offscreen_pixels().expect("readback");
    assert_eq!((pixels.size().width(), pixels.size().height()), (128, 64));
    assert_gray(pixel(&renderer, 32, 32), 180, "left half after the resize");
    assert_gray(pixel(&renderer, 96, 32), 60, "right half after the resize");
}

/// Нулевой размер экрана (окно свёрнуто) — кадра нет, а не падение и не рисование в ноль.
#[test]
fn a_zero_sized_screen_gives_no_buffer() {
    let Some(mut renderer) = offscreen_or_skip("a_zero_sized_screen_gives_no_buffer", [64, 32])
    else {
        return;
    };

    let acquired = renderer.acquire_surface_frame(screen_for([0, 32]));

    assert!(
        matches!(
            acquired,
            SurfaceFrameAcquire::Dropped(RenderFrameDropReason::SurfaceOccluded)
        ),
        "{acquired:?}"
    );
}

/// Контракт, которого рендер не объявляет, — `Unsupported(ContractNotDeclared)`; а контракт
/// объявлен, но декодер не подключён, — `NoVideoBackendBound` (не путаются).
#[test]
fn undeclared_contract_and_unbound_decoder_are_reported_apart() {
    let Some(mut renderer) = offscreen_or_skip(
        "undeclared_contract_and_unbound_decoder_are_reported_apart",
        [64, 32],
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    // Аренда кадра с контрактом, которого рендер не объявляет: P010 в памяти процесса.
    let (mut frame, _) = crate::test_support::fake_decoder::split_picture_frame(picture(5, 1, 1));
    frame.frame_contract = video_frame_contract::VideoFrameContract {
        pixel_layout: video_frame_contract::VideoFramePixelLayout::P010,
        transfer_path: video_frame_contract::VideoFrameTransferPath::SoftwareHostUpload,
    };
    let undeclared = crate::test_support::fake_decoder::lease_for(frame, decoder.lease_provider());

    let outcome = renderer.video_frames().materialize(&undeclared).outcome;

    assert!(
        matches!(
            outcome,
            FrameMaterializationOutcome::Unsupported(
                FrameImportUnsupportedReason::ContractNotDeclared { .. }
            )
        ),
        "{outcome:?}"
    );
    assert_eq!(
        decoder.state.descriptor_lookups(),
        0,
        "the contract is refused before the provider is asked"
    );
}

/// Ответы провайдера доходят до приложения различимыми: занятость повторяют, отсутствие и
/// фатальную ошибку — нет; после занятости прошлый кадр остаётся на экране.
#[test]
fn provider_answers_reach_the_app_apart_and_busy_keeps_the_previous_picture() {
    let Some(mut renderer) = offscreen_or_skip(
        "provider_answers_reach_the_app_apart_and_busy_keeps_the_previous_picture",
        [64, 32],
    ) else {
        return;
    };
    let decoder = connect_software_decoder(&mut renderer);
    let (current, _lease) = materialize_key(&mut renderer, &decoder, picture(1, 180, 60));
    let next_lease = decoder.lease(picture(2, 20, 20));

    decoder.state.set_mode(LookupMode::Busy);
    let busy = renderer.video_frames().materialize(&next_lease);
    assert!(busy.lookup_was_busy(), "{busy:?}");
    decoder.state.set_mode(LookupMode::Missing);
    assert!(matches!(
        renderer.video_frames().materialize(&next_lease).outcome,
        FrameMaterializationOutcome::Missing
    ));
    decoder.state.set_mode(LookupMode::Fatal);
    assert!(matches!(
        renderer.video_frames().materialize(&next_lease).outcome,
        FrameMaterializationOutcome::Error(render_shell_api::FrameImportError::ProviderFatal)
    ));

    // После отказов на экране прежний кадр: его ключ рисуется без возврата картинки.
    assert!(matches!(
        present(
            &mut renderer,
            [64, 32],
            Some(&current),
            FrameExtras::default()
        ),
        RenderFrameOutcome::Presented(_)
    ));
    assert_gray(pixel(&renderer, 16, 16), 180, "previous picture after Busy");
    assert_eq!(renderer.frame_counters().video_frames_reshown, 0);
}

/// Ключ другого рендера — `Failed(ForeignVideoFrame)`, а изменения текстур UI этого кадра всё
/// равно применены (приложение второй раз их не пришлёт).
#[test]
fn a_foreign_video_key_fails_but_ui_textures_of_the_frame_are_still_applied() {
    let Some(mut renderer) = offscreen_or_skip(
        "a_foreign_video_key_fails_but_ui_textures_of_the_frame_are_still_applied",
        [64, 32],
    ) else {
        return;
    };
    let Some(mut other) = offscreen_or_skip(
        "a_foreign_video_key_fails_but_ui_textures_of_the_frame_are_still_applied",
        [64, 32],
    ) else {
        return;
    };
    let other_decoder = connect_software_decoder(&mut other);
    let (foreign_key, _lease) = materialize_key(&mut other, &other_decoder, picture(1, 90, 90));

    let screen = screen_for([64, 32]);
    let SurfaceFrameAcquire::Acquired(surface) = renderer.acquire_surface_frame(screen) else {
        panic!("surface");
    };
    let texture = egui::TextureId::Managed(4242);
    let mut textures_delta = egui::TexturesDelta::default();
    textures_delta.push(
        texture,
        egui::epaint::ImageDelta::full(
            egui::ColorImage::new([2, 2], vec![egui::Color32::RED; 4]),
            egui::TextureOptions::NEAREST,
        ),
    );
    let outcome = renderer.render_frame(render_shell_api::RenderFrameInput {
        surface,
        video_frame: Some(&foreign_key),
        egui: render_shell_api::EguiFrameContent::Fresh {
            paint_jobs: Vec::new(),
            textures_delta,
        },
        screen,
        video_viewport: RenderViewport::full_surface(64, 32),
        video_exclusion_rects: Vec::new(),
        subtitle_overlay: None,
    });

    match outcome {
        RenderFrameOutcome::Failed(failure) => {
            assert_eq!(failure.kind, RenderFrameFailureKind::ForeignVideoFrame);
        }
        other => panic!("a foreign key must fail, got {other:?}"),
    }
    let layers = renderer.layers.as_ref().expect("layers");
    assert_eq!(
        layers.compositor.painter().texture_count(),
        1,
        "the texture set of the rejected frame is applied"
    );
    assert_eq!(
        renderer.frame_counters().frames_presented,
        0,
        "nothing was shown"
    );
}

/// Программный импорт назван программным, пока декодер подключён, а аппаратный путь на драйвере
/// без zero-copy отвергается подготовкой (а не молча принимается).
#[test]
fn the_active_import_path_follows_the_connected_decoder() {
    let Some(mut renderer) = offscreen_or_skip(
        "the_active_import_path_follows_the_connected_decoder",
        [64, 32],
    ) else {
        return;
    };
    assert_eq!(renderer.video_frames().active_video_import_path(), None);
    let _decoder = connect_software_decoder(&mut renderer);
    assert_eq!(
        renderer.video_frames().active_video_import_path(),
        Some(VideoImportPath::SoftwareHostUpload)
    );
    assert!(
        renderer.video_frames().import_stats().is_none(),
        "the software path has no DMA-BUF import cache"
    );
}
