//! Аппаратный путь через ПУБЛИЧНЫЙ контракт `RenderShell`: кадр NV12 из настоящего буфера ядра
//! (`/dev/udmabuf`) → `prepare_video_import` (обёртка по fence) → `install_video_import` →
//! `materialize` → `acquire` → `render_frame` → пиксели; кэш импорта, отказ драйвера, возврат
//! кадра провайдеру после fence ровно один раз.
//!
//! Без EGL, без `/dev/udmabuf` или на драйвере без аппаратного zero-copy тест пропускается с
//! причиной в stderr (`GLES2_EGUI_REQUIRE_EGL=1` делает отсутствие EGL падением).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use render_core::RenderViewport;
use render_gles2_shell::Gles2Renderer;
use render_shell_api::{
    DecoderSurfacePoolSlots, EguiFrameContent, FrameImportUnsupportedReason,
    FrameMaterializationOutcome, HardwareFrameRefusal, RenderFrameInput, RenderFrameOutcome,
    RenderScreenDescriptor, RenderShell, RenderSurfaceSize, RenderableFrameKey,
    SurfaceFrameAcquire, SurfacePresentSettings, VideoImportPath, VideoImportPlan,
};
use support::nv12_buffers::{Nv12Layout, PALETTE, TestNv12};
use support::provider::{
    DescriptorReply, IdentityReply, ProviderOnlyDecoder, ProviderState, ScriptedProvider,
};
use support::{
    TARGET_HEIGHT, TARGET_WIDTH, assert_close, block_centers, contract_for, decoded_frame,
    picture_rect, pixel, reference_rgba_bt709_limited,
};
use video_backend_api::{
    PresentFrameResourceProviderHandle, StartedVideoBackend, VideoBackendDecoderThreadHandle,
};
use video_core::DecodedFrame;
use video_present_core::{
    VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease, VideoFrameReleaseOutcome,
    VideoFrameReleaseSink,
};

const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

const HARDWARE_PLAN: VideoImportPlan = VideoImportPlan {
    path: VideoImportPath::HardwareZeroCopy,
    decoder_surface_pool: DecoderSurfacePoolSlots::new(8),
};

/// Возврат кадра из аренды идёт в провайдер аренды (обёртку рендера, ждущую fence).
struct ProviderReleaseSink;

impl VideoFrameReleaseSink for ProviderReleaseSink {
    fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
        match release.resource_provider() {
            Some(provider) => {
                provider.release_frame(release.resource_handle());
                VideoFrameReleaseOutcome::Accepted
            }
            None => VideoFrameReleaseOutcome::NoOp,
        }
    }
}

/// Рендер с подключённым аппаратным декодером или пропуск теста (причина — в stderr).
struct Stand {
    renderer: Gles2Renderer,
    state: Arc<ProviderState>,
    player_thread: Box<VideoBackendDecoderThreadHandle>,
    layout: Nv12Layout,
}

impl Stand {
    fn new(test_name: &str, layout: Nv12Layout) -> Option<Self> {
        let mut renderer = match Gles2Renderer::new_offscreen(
            RenderSurfaceSize {
                width: TARGET_WIDTH,
                height: TARGET_HEIGHT,
            },
            SurfacePresentSettings::default(),
        ) {
            Ok(renderer) => renderer,
            Err(error) if error.stage == render_shell_api::RendererCreateStage::ApiConnection => {
                assert!(
                    std::env::var_os(REQUIRE_EGL_VARIABLE).is_none(),
                    "{test_name}: EGL is required but unavailable: {error}"
                );
                eprintln!("skip {test_name}: no EGL: {error}");
                return None;
            }
            Err(error) => panic!("{test_name}: renderer creation failed: {error}"),
        };
        if !renderer
            .capabilities()
            .supports_frame_contract(contract_for(layout))
        {
            eprintln!("skip {test_name}: the driver does not declare hardware zero-copy");
            return None;
        }
        let nv12 = TestNv12::create(test_name, layout, &PALETTE)?;
        let state = ProviderState::new(nv12, 500);
        let inner = PresentFrameResourceProviderHandle::new(ScriptedProvider(Arc::clone(&state)));
        let prepared = renderer
            .video_frames()
            .prepare_video_import(
                StartedVideoBackend::from_decoder_thread(
                    "dma-buf-test",
                    ProviderOnlyDecoder { provider: inner },
                ),
                HARDWARE_PLAN,
            )
            .expect("hardware import is prepared");
        renderer
            .video_frames()
            .install_video_import(prepared.pending)
            .expect("hardware import is installed");
        Some(Self {
            renderer,
            state,
            player_thread: prepared.backend.into_decoder_thread(),
            layout,
        })
    }

    fn lease(&self, handle: u64) -> (VideoFrameLease, DecodedFrame) {
        let frame = decoded_frame(contract_for(self.layout), handle);
        let lease = VideoFrameLease::new(
            VideoFrameLeaseConfig::new(1, frame.clone(), Arc::new(ProviderReleaseSink))
                .with_resource_provider(self.player_thread.resource_provider()),
        );
        (lease, frame)
    }

    fn materialize(&mut self, lease: &VideoFrameLease) -> FrameMaterializationOutcome {
        self.renderer.video_frames().materialize(lease).outcome
    }

    fn present(&mut self, key: &RenderableFrameKey) -> RenderFrameOutcome {
        let screen = RenderScreenDescriptor {
            size_in_pixels: [TARGET_WIDTH, TARGET_HEIGHT],
            pixels_per_point: 1.0,
        };
        let SurfaceFrameAcquire::Acquired(surface) = self.renderer.acquire_surface_frame(screen)
        else {
            panic!("the surface is acquired");
        };
        self.renderer.render_frame(RenderFrameInput {
            surface,
            video_frame: Some(key),
            egui: EguiFrameContent::empty_fresh(),
            screen,
            video_viewport: RenderViewport::full_surface(TARGET_WIDTH, TARGET_HEIGHT),
            video_exclusion_rects: Vec::new(),
            subtitle_overlay: None,
        })
    }

    /// Цвета палитры в центрах блоков и чёрные полосы letterbox.
    fn assert_palette_on_screen(&self, frame: &DecodedFrame, what: &str) {
        let pixels = self.renderer.read_offscreen_pixels().expect("readback");
        for (index, x, y) in block_centers(picture_rect(frame)) {
            assert_close(
                pixel(&pixels, x, y),
                reference_rgba_bt709_limited(PALETTE[index]),
                &format!("{what}: block {index} at ({x},{y})"),
            );
        }
        for (x, y) in [(2, 1), (TARGET_WIDTH - 3, 1), (2, TARGET_HEIGHT - 2)] {
            assert_close(
                pixel(&pixels, x, y),
                [0, 0, 0, 255],
                &format!("{what}: letterbox bar at ({x},{y})"),
            );
        }
    }
}

fn ready_key(outcome: FrameMaterializationOutcome) -> RenderableFrameKey {
    match outcome {
        FrameMaterializationOutcome::Ready(key) => key,
        other => panic!("the hardware frame must be Ready, got {other:?}"),
    }
}

/// Кадр NV12 (в обеих раскладках) доходит до экрана без копии: цвета блоков на месте, полосы
/// letterbox чёрные, кэш импорта показывает один импорт; второй кадр той же поверхности —
/// попадание в кэш без нового дескриптора.
fn hardware_frames_reach_the_screen(layout: Nv12Layout, name: &str) {
    let Some(mut stand) = Stand::new(name, layout) else {
        return;
    };
    let (first_lease, first_frame) = stand.lease(1);
    let first_key = ready_key(stand.materialize(&first_lease));
    let outcome = stand.present(&first_key);
    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "{outcome:?}"
    );
    stand.assert_palette_on_screen(&first_frame, name);
    let stats = stand.renderer.video_frames().import_stats().expect("stats");
    assert_eq!(
        (stats.imports_performed, stats.cache.entries),
        (1, 1),
        "{stats:?}"
    );
    assert_eq!(
        stand.state.descriptor_lookups(),
        1,
        "one dup fd for the import"
    );

    let (second_lease, second_frame) = stand.lease(2);
    let second_key = ready_key(stand.materialize(&second_lease));
    assert!(matches!(
        stand.present(&second_key),
        RenderFrameOutcome::Presented(_)
    ));
    stand.assert_palette_on_screen(&second_frame, name);
    let stats = stand.renderer.video_frames().import_stats().expect("stats");
    assert_eq!(
        (stats.imports_performed, stats.cache.hits),
        (1, 1),
        "{stats:?}"
    );
    assert_eq!(
        stand.state.descriptor_lookups(),
        1,
        "a cache hit must not duplicate descriptors"
    );
}

#[test]
fn composed_nv12_frames_reach_the_screen_through_the_shell() {
    hardware_frames_reach_the_screen(
        Nv12Layout::Composed,
        "composed_nv12_frames_reach_the_screen_through_the_shell",
    );
}

#[test]
fn separate_nv12_frames_reach_the_screen_through_the_shell() {
    hardware_frames_reach_the_screen(
        Nv12Layout::Separate,
        "separate_nv12_frames_reach_the_screen_through_the_shell",
    );
}

/// Кадр возвращается провайдеру ровно один раз и только после того, как приложение отпустило
/// ключ и аренду; возврат шёл по fence кадра показа.
#[test]
fn the_hardware_frame_goes_back_to_the_provider_once_after_the_app_lets_go() {
    let name = "the_hardware_frame_goes_back_to_the_provider_once_after_the_app_lets_go";
    let Some(mut stand) = Stand::new(name, Nv12Layout::Composed) else {
        return;
    };
    let (lease, _frame) = stand.lease(1);
    let key = ready_key(stand.materialize(&lease));
    assert!(matches!(
        stand.present(&key),
        RenderFrameOutcome::Presented(_)
    ));
    stand
        .renderer
        .wait_for_gpu_idle(IDLE_TIMEOUT)
        .expect("idle");
    assert_eq!(
        stand.state.released_handles(),
        Vec::<u64>::new(),
        "the app still holds it"
    );

    drop(key);
    drop(lease);
    stand
        .renderer
        .wait_for_gpu_idle(IDLE_TIMEOUT)
        .expect("idle");

    assert_eq!(
        stand.state.released_handles(),
        vec![1],
        "returned exactly once"
    );
    let stats = stand.renderer.frame_release_stats();
    assert_eq!(stats.released_without_fence, 0, "{stats:?}");
}

/// Провайдер отдал память процесса вместо DMA-BUF: ответ рендера запускает программный фолбэк
/// приложения (`Unsupported(RendererCannotShowHardwareFrame)`), а не молчаливый чёрный экран.
#[test]
fn a_host_descriptor_on_the_hardware_path_asks_for_the_software_fallback() {
    let name = "a_host_descriptor_on_the_hardware_path_asks_for_the_software_fallback";
    let Some(mut stand) = Stand::new(name, Nv12Layout::Composed) else {
        return;
    };
    stand.state.reply_to_identity(IdentityReply::Unsupported);
    stand.state.reply_to_descriptor(DescriptorReply::HostPlanar);
    let (lease, _frame) = stand.lease(1);

    let outcome = stand.materialize(&lease);

    match outcome {
        FrameMaterializationOutcome::Unsupported(
            FrameImportUnsupportedReason::RendererCannotShowHardwareFrame(
                HardwareFrameRefusal::HostMemoryDescriptor,
            ),
        ) => {}
        other => panic!("expected the fallback rejection, got {other:?}"),
    }
}

/// Занятость, отсутствие и фатальное состояние провайдера на аппаратном пути различимы.
#[test]
fn hardware_provider_answers_stay_distinct() {
    let name = "hardware_provider_answers_stay_distinct";
    let Some(mut stand) = Stand::new(name, Nv12Layout::Composed) else {
        return;
    };
    let (lease, _frame) = stand.lease(1);
    for (reply, check) in [
        (
            IdentityReply::Busy,
            (|outcome: &FrameMaterializationOutcome| {
                matches!(outcome, FrameMaterializationOutcome::Busy)
            }) as fn(&FrameMaterializationOutcome) -> bool,
        ),
        (IdentityReply::Missing, |outcome| {
            matches!(outcome, FrameMaterializationOutcome::Missing)
        }),
        (IdentityReply::Fatal, |outcome| {
            matches!(
                outcome,
                FrameMaterializationOutcome::Error(
                    render_shell_api::FrameImportError::ProviderFatal
                )
            )
        }),
    ] {
        stand.state.reply_to_identity(reply);
        let outcome = stand.materialize(&lease);
        assert!(check(&outcome), "{reply:?}: {outcome:?}");
    }
}
