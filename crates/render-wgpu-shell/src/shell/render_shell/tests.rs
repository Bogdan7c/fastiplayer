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
    RenderFrameOutcome, RenderScreenDescriptor, RenderShell, RendererCreateError,
    RendererCreateStage,
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
