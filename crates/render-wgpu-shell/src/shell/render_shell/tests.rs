//! GPU-тесты контракта `RenderShell` на НАСТОЯЩЕМ wgpu-рендере без окна.
//!
//! Работает production-путь кадра: Vulkan device с video features, склад кадров
//! (`wrap_video_backend_for_wgpu_submission` + `HostPlanarWgpuFrameMaterializer`), video pass,
//! egui, один submit. Отличие от приложения только в цели показа: offscreen-текстура вместо
//! swapchain (решение владельца в сессии 02). Без Vulkan-адаптера тест печатает причину
//! пропуска и выходит.

use render_shell_api::conformance::{
    ConformanceVideoSource, RenderShellConformanceHarness, run_render_shell_conformance,
};
use render_shell_api::{
    PresentationDetachOutcome, PresentationReattachOutcome, PresentationReconfigured,
    RenderFrameDropReason, RenderFrameOutcome, RenderScreenDescriptor, RenderShell,
    RendererCreateError, RendererCreateStage, ShellPresentMode, SurfaceFrameAcquire,
    SurfacePresentSettings,
};
use video_core::{DecodedFrame, FrameResourceHandle};
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath};

use crate::Renderer;
use crate::test_support::{
    BLACK_PIXEL, HEADLESS_SIZE, SOFTWARE_PLAN, SolidFrameDecoder, WHITE_FRAME, WHITE_PIXEL,
    decoded_frame, expect_ready, headless_renderer_or_skip, headless_screen, host_planar_frame,
    lease_for, pixel_matches, present,
};

/// Кадр, контракт которого wgpu-рендер не объявляет: зарезервированный RGBA8.
fn undeclared_frame(resource_handle: FrameResourceHandle) -> DecodedFrame {
    decoded_frame(
        resource_handle,
        VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Rgba8,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        },
    )
}

/// Стенд conformance-набора поверх настоящего wgpu-рендера.
struct WgpuHeadlessHarness;

impl RenderShellConformanceHarness for WgpuHeadlessHarness {
    fn create_renderer(&mut self) -> Result<Box<dyn RenderShell>, RendererCreateError> {
        match Renderer::new_headless(HEADLESS_SIZE) {
            Ok(Some(renderer)) => Ok(Box::new(renderer)),
            Ok(None) => Err(RendererCreateError::new(
                RendererCreateStage::AdapterSelection,
                "no Vulkan adapter",
            )),
            Err(error) => Err(RendererCreateError::new(
                RendererCreateStage::DeviceCreation,
                format!("{error:#}"),
            )),
        }
    }

    fn screen(&self) -> RenderScreenDescriptor {
        headless_screen()
    }

    fn initial_presentation(&self) -> SurfacePresentSettings {
        // `Renderer::new_headless` создаёт offscreen-цель с умолчаниями показа.
        SurfacePresentSettings::default()
    }

    fn unsupported_frame(&mut self) -> DecodedFrame {
        undeclared_frame(FrameResourceHandle(9))
    }

    fn supported_video_source(&mut self) -> ConformanceVideoSource {
        let decoder = SolidFrameDecoder::start();
        ConformanceVideoSource {
            backend: decoder.backend,
            plan: SOFTWARE_PLAN,
            frame: host_planar_frame(WHITE_FRAME),
            ledger: decoder.ledger,
        }
    }
}

/// Настоящий wgpu-рендер проходит весь conformance-набор контракта.
#[test]
fn wgpu_renderer_passes_render_shell_conformance() {
    if headless_renderer_or_skip("wgpu_renderer_passes_render_shell_conformance").is_none() {
        return;
    }

    let violations = run_render_shell_conformance(&mut WgpuHeadlessHarness);

    assert!(
        violations.is_empty(),
        "wgpu renderer violates the render shell contract: {violations:#?}"
    );
}

/// Функциональный тест: кадр программного декода через контракт доходит до цели показа.
///
/// Белый кадр декодера → `prepare/install_video_import` → `materialize` → `render_frame` →
/// белый пиксель; кадр без видео — чёрный. Повтор того же ключа берёт привязку ресурсов из
/// кэша (счётчики горячего пути в нейтральной форме).
#[test]
fn software_frame_reaches_target_through_render_shell_contract() {
    let Some(mut renderer) =
        headless_renderer_or_skip("software_frame_reaches_target_through_render_shell_contract")
    else {
        return;
    };
    let decoder = SolidFrameDecoder::start();
    let prepared = renderer
        .video_frames()
        .prepare_video_import(decoder.backend, SOFTWARE_PLAN)
        .expect("prepare software import");
    renderer
        .video_frames()
        .install_video_import(prepared.pending)
        .expect("install software import");
    let decoder_thread = prepared.backend.into_decoder_thread();
    let lease = lease_for(
        host_planar_frame(WHITE_FRAME),
        decoder_thread.resource_provider(),
    );
    let key = expect_ready(renderer.video_frames().materialize(&lease).outcome);

    let outcome = present(&mut renderer, Some(&key));
    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "frame with video must be presented, got {outcome:?}"
    );
    let center = [HEADLESS_SIZE[0] / 2, HEADLESS_SIZE[1] / 2];
    let pixel = renderer
        .read_offscreen_pixel(center[0], center[1])
        .expect("headless renderer has an offscreen target");
    assert!(
        pixel_matches(pixel, WHITE_PIXEL),
        "white decoder frame must reach the target, got {pixel:?}"
    );
    let after_first = renderer.hot_path_stats();
    assert!(
        after_first.resource_bindings_created > 0,
        "first frame must create its resource binding: {after_first:?}"
    );

    let outcome = present(&mut renderer, Some(&key));
    assert!(matches!(outcome, RenderFrameOutcome::Presented(_)));
    let after_repeat = renderer.hot_path_stats();
    assert_eq!(
        after_repeat.resource_bindings_created, after_first.resource_bindings_created,
        "repeating the same key must not create a new binding"
    );
    assert!(
        after_repeat.resource_bindings_reused > after_first.resource_bindings_reused,
        "repeating the same key must reuse the cached binding: {after_repeat:?}"
    );

    let outcome = present(&mut renderer, None);
    assert!(matches!(outcome, RenderFrameOutcome::Presented(_)));
    let pixel = renderer
        .read_offscreen_pixel(center[0], center[1])
        .expect("headless renderer has an offscreen target");
    assert!(
        pixel_matches(pixel, BLACK_PIXEL),
        "frame without video must clear the target to black, got {pixel:?}"
    );
}

/// Подключает программный декодер стенда и готовит ключ белого кадра.
fn install_white_software_frame(
    renderer: &mut Renderer,
) -> (
    render_shell_api::RenderableFrameKey,
    video_present_core::VideoFrameLease,
    Box<video_backend_api::VideoBackendDecoderThreadHandle>,
) {
    let decoder = SolidFrameDecoder::start();
    let prepared = renderer
        .video_frames()
        .prepare_video_import(decoder.backend, SOFTWARE_PLAN)
        .expect("prepare software import");
    renderer
        .video_frames()
        .install_video_import(prepared.pending)
        .expect("install software import");
    let decoder_thread = prepared.backend.into_decoder_thread();
    let lease = lease_for(
        host_planar_frame(WHITE_FRAME),
        decoder_thread.resource_provider(),
    );
    let key = expect_ready(renderer.video_frames().materialize(&lease).outcome);
    (key, lease, decoder_thread)
}

/// Центральный пиксель offscreen-цели.
fn center_pixel(renderer: &Renderer) -> [u8; 4] {
    renderer
        .read_offscreen_pixel(HEADLESS_SIZE[0] / 2, HEADLESS_SIZE[1] / 2)
        .expect("headless renderer has an offscreen target")
}

/// Функциональный тест «откат пересоздания сохраняет показ» на настоящем wgpu (сессия 02a).
///
/// Пока рендер отпустил окно, кадр не выдаётся (типизированный `PresentationDetached`, а не
/// поломка surface). После возврата окна тот же SW-кадр через тот же импорт снова доходит до
/// цели: белый пиксель поверх предварительно очищенного в чёрный кадра.
#[test]
fn reattached_presentation_shows_software_frame_again() {
    let Some(mut renderer) =
        headless_renderer_or_skip("reattached_presentation_shows_software_frame_again")
    else {
        return;
    };
    let (key, _lease, _decoder_thread) = install_white_software_frame(&mut renderer);
    assert!(matches!(
        present(&mut renderer, Some(&key)),
        RenderFrameOutcome::Presented(_)
    ));
    assert!(pixel_matches(center_pixel(&renderer), WHITE_PIXEL));
    // Чёрный кадр перед detach: белый после reattach докажет, что кадр нарисован заново.
    assert!(matches!(
        present(&mut renderer, None),
        RenderFrameOutcome::Presented(_)
    ));
    assert!(pixel_matches(center_pixel(&renderer), BLACK_PIXEL));

    assert_eq!(
        renderer.detach_presentation(),
        PresentationDetachOutcome::Detached
    );
    let acquire = renderer.acquire_surface_frame(headless_screen());
    assert!(
        matches!(
            acquire,
            SurfaceFrameAcquire::Dropped(RenderFrameDropReason::PresentationDetached)
        ),
        "detached renderer must not give a buffer, got {acquire:?}"
    );

    assert_eq!(
        renderer.reattach_presentation(),
        Ok(PresentationReattachOutcome::Reattached)
    );
    let outcome = present(&mut renderer, Some(&key));
    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "frame after reattach must be presented, got {outcome:?}"
    );
    let pixel = center_pixel(&renderer);
    assert!(
        pixel_matches(pixel, WHITE_PIXEL),
        "software frame must reach the target again after reattach, got {pixel:?}"
    );
}

/// Перенастройка показа на месте не трогает импорт кадров и его кэш привязок (сессия 02a).
///
/// После перенастройки тот же ключ рисуется без новой привязки ресурсов (кэш импорта жив),
/// а белый кадр доходит до цели. Повтор тех же настроек — явный no-op.
#[test]
fn presentation_reconfigure_keeps_video_import_and_its_binding_cache() {
    let Some(mut renderer) = headless_renderer_or_skip(
        "presentation_reconfigure_keeps_video_import_and_its_binding_cache",
    ) else {
        return;
    };
    let (key, _lease, _decoder_thread) = install_white_software_frame(&mut renderer);
    assert!(matches!(
        present(&mut renderer, Some(&key)),
        RenderFrameOutcome::Presented(_)
    ));
    let before = renderer.hot_path_stats();
    let requested = SurfacePresentSettings {
        present_mode: ShellPresentMode::Mailbox,
        max_frame_latency: 3,
        ..SurfacePresentSettings::default()
    };

    assert_eq!(
        renderer.reconfigure_presentation(requested),
        Ok(PresentationReconfigured::AsRequested)
    );
    assert_eq!(
        renderer.reconfigure_presentation(requested),
        Ok(PresentationReconfigured::AlreadyActive)
    );
    assert!(renderer.video_frames().active_video_import_path().is_some());
    assert!(matches!(
        present(&mut renderer, Some(&key)),
        RenderFrameOutcome::Presented(_)
    ));
    let after = renderer.hot_path_stats();
    assert_eq!(
        after.resource_bindings_created, before.resource_bindings_created,
        "in-place reconfigure must not rebuild the frame binding"
    );
    assert!(after.resource_bindings_reused > before.resource_bindings_reused);
    assert!(pixel_matches(center_pixel(&renderer), WHITE_PIXEL));
}
