//! Функциональный тест адаптера на настоящем EGL: кадр программного декодера (`DecodedFrame` +
//! `HostPlanarFrameDescriptor`) через адаптер и видеослой доходит до экрана — letterbox,
//! сайдбар и цвет из `render-core`; кадр декодера отпускается только после fence.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

use std::sync::{Arc, Weak};
use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use gles2_egui::{CreateError, FenceStatus, FrameStart, Gles2Context, OffscreenAlpha, SurfaceSize};
use gles2_video::{UploadOutcome, VideoDrawOutcome, VideoLayerOptions, VideoLayerRenderer};
use render_core::{ColorPipelineSettings, PictureLayout, RenderViewport};
use render_gles2_shell::{
    GLES2_SOFTWARE_FRAME_CONTRACTS, VideoDrawPlan, VideoPlacement, gles2_render_capabilities,
    host_planar_frame_upload, renderable_frame,
};
use video_core::{
    DecodedFrame, FrameResourceDescriptor, FrameResourceHandle, HostPlanarFrameDescriptor,
    HostPlanarFrameOwner, HostPlanarOwnedBuffer, HostPlaneDescriptor, HostPlaneRole,
    VideoFrameDiagnostics,
};
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout, VideoFrameTransferPath};

const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";

fn offscreen_or_skip(test_name: &str, width: u32, height: u32) -> Option<Gles2Context> {
    let size = SurfaceSize::new(width, height).unwrap();
    match Gles2Context::offscreen_with_alpha(size, OffscreenAlpha::Opaque) {
        Ok(context) => Some(context),
        Err(
            error @ (CreateError::EglUnavailable { .. }
            | CreateError::PlatformExtensionMissing { .. }
            | CreateError::DisplayOpen(_)),
        ) => {
            assert!(
                std::env::var_os(REQUIRE_EGL_VARIABLE).is_none(),
                "{test_name}: EGL is required but unavailable: {error}"
            );
            eprintln!("skip {test_name}: no EGL surfaceless GLES2 context: {error}");
            None
        }
        Err(error) => panic!("{test_name}: offscreen context failed: {error}"),
    }
}

/// Владелец памяти кадра, за жизнью которого можно следить (`Weak`): как кадр программного декодера.
#[derive(Debug)]
struct TrackedOwner(HostPlanarOwnedBuffer);

impl HostPlanarFrameOwner for TrackedOwner {
    fn visible_row_bytes(
        &self,
        plane_index: usize,
        plane: &HostPlaneDescriptor,
        row_index: u32,
        visible_row_bytes: usize,
    ) -> anyhow::Result<&[u8]> {
        self.0
            .visible_row_bytes(plane_index, plane, row_index, visible_row_bytes)
    }

    fn visible_plane_block(
        &self,
        plane_index: usize,
        plane: &HostPlaneDescriptor,
        block_len: usize,
    ) -> anyhow::Result<&[u8]> {
        self.0.visible_plane_block(plane_index, plane, block_len)
    }
}

/// Кадр 4:2:0 64×32: левая половина серая (Y 180), правая тёмная (Y 60); stride с хвостом мусора.
fn decoded_frame(
    orientation: VideoDisplayOrientation,
) -> (DecodedFrame, FrameResourceDescriptor, Weak<TrackedOwner>) {
    let (width, height, tail) = (64_u32, 32_u32, 16_usize);
    let mut bytes = Vec::new();
    let mut planes = Vec::new();
    for (role, plane_width, plane_height) in [
        (HostPlaneRole::Luma, width, height),
        (HostPlaneRole::ChromaU, width / 2, height / 2),
        (HostPlaneRole::ChromaV, width / 2, height / 2),
    ] {
        let offset = bytes.len();
        for row in 0..plane_height {
            for x in 0..plane_width {
                bytes.push(match role {
                    HostPlaneRole::Luma if x < plane_width / 2 => 180,
                    HostPlaneRole::Luma => 60,
                    HostPlaneRole::ChromaU | HostPlaneRole::ChromaV => 128,
                });
            }
            if row + 1 < plane_height {
                bytes.extend(std::iter::repeat_n(0xFF, tail));
            }
        }
        planes.push(HostPlaneDescriptor {
            role,
            offset,
            stride: plane_width as usize + tail,
            visible_width: plane_width,
            visible_height: plane_height,
            bytes_per_sample: 1,
        });
    }
    let owner = Arc::new(TrackedOwner(HostPlanarOwnedBuffer::new(bytes)));
    let weak = Arc::downgrade(&owner);
    let descriptor =
        FrameResourceDescriptor::HostPlanar(HostPlanarFrameDescriptor::new(owner, planes));
    let frame = DecodedFrame {
        generation: 1,
        pts: Duration::ZERO,
        frame_contract: VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Yuv420Planar8,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        },
        width,
        height,
        render_width: width,
        render_height: height,
        display_orientation: orientation,
        // Полный диапазон: серый Y даёт тот же байт на экране.
        color: VideoColorMetadata {
            range: codec_core::ColorRange::Full,
            ..VideoColorMetadata::sdr_bt709_limited()
        },
        resource_handle: FrameResourceHandle(7),
        diagnostics: VideoFrameDiagnostics::default(),
    };
    (frame, descriptor, weak)
}

/// Кадр декодера доходит до экрана: картинка 2:1 в цели 120×90 с сайдбаром 20 px слева. Полосы
/// letterbox и сайдбар чёрные, картинка — в `PictureLayout::rect()`, левая половина серая, правая
/// тёмная. Кадр декодера держится очередью до fence и отпускается после него ровно один раз.
#[test]
fn decoder_frame_reaches_the_screen_through_the_adapter() {
    let (surface_width, surface_height) = (120_u32, 90_u32);
    let Some(mut context) = offscreen_or_skip(
        "decoder_frame_reaches_the_screen",
        surface_width,
        surface_height,
    ) else {
        return;
    };
    let capabilities = gles2_render_capabilities(context.capabilities());
    for contract in GLES2_SOFTWARE_FRAME_CONTRACTS {
        assert!(
            capabilities.supports_frame_contract(contract),
            "{contract:?}"
        );
    }
    assert!(!capabilities.supports_frame_contract(VideoFrameContract::host_yuv420_planar10le()));

    let mut video = VideoLayerRenderer::new(&context, VideoLayerOptions::default()).unwrap();
    let (frame, descriptor, owner) = decoded_frame(VideoDisplayOrientation::Identity);
    let upload = host_planar_frame_upload(&frame, &descriptor).unwrap();
    assert!(matches!(
        video.upload(&context, &upload).unwrap(),
        UploadOutcome::Uploaded(_)
    ));

    let sidebar = RenderViewport::new(0, 0, 20, surface_height);
    let renderable = renderable_frame(&frame).unwrap();
    let plan = VideoDrawPlan::for_frame(
        &renderable,
        VideoPlacement {
            surface_size: (surface_width, surface_height),
            video_viewport: RenderViewport::full_surface(surface_width, surface_height),
            exclusions: &[sidebar],
        },
        &ColorPipelineSettings::identity(),
    )
    .unwrap();
    let outcome = match context.begin_frame().unwrap() {
        FrameStart::Ready(active) => {
            let outcome = video.draw(&active, &plan.request()).unwrap();
            active.present().unwrap();
            outcome
        }
        FrameStart::Skipped(reason) => panic!("skipped: {reason:?}"),
    };
    assert_eq!(outcome, VideoDrawOutcome::Drawn { draw_calls: 1 });

    // Кадр декодера отпускается по fence кадра показа (решение владельца сессии 05).
    // Один fence кадра показа на видеослой и на кадр декодера.
    let fence = Arc::new(context.insert_fence().unwrap());
    video.retire_submitted_work(Arc::clone(&fence));
    let mut decoder_frame = Some(descriptor);
    assert!(
        owner.upgrade().is_some(),
        "decoder frame is held until its fence"
    );
    assert_eq!(
        fence.wait(Duration::from_secs(5)).unwrap(),
        FenceStatus::Signaled
    );
    // Fence сработал — кадр возвращается декодеру ровно один раз.
    drop(decoder_frame.take());
    assert!(decoder_frame.is_none());
    assert!(
        owner.upgrade().is_none(),
        "decoder frame memory is gone after release"
    );

    let pixels = context.read_offscreen_pixels().unwrap();
    let picture = PictureLayout::fit_frame(
        &renderable,
        RenderViewport::new(0, 0, surface_width, surface_height),
    )
    .unwrap()
    .rect();
    // Картинка 2:1 в 120×90: 120×60, сверху и снизу полосы по 15.
    assert_eq!(
        (picture.x, picture.y, picture.width, picture.height),
        (0.0, 15.0, 120.0, 60.0)
    );
    let pixel = |x: u32, y: u32| pixels.pixel(x, y).unwrap();
    for x in 0..surface_width {
        assert_eq!(pixel(x, 5), [0, 0, 0, 255], "top bar at x={x}");
        assert_eq!(pixel(x, 84), [0, 0, 0, 255], "bottom bar at x={x}");
    }
    for y in 15..75 {
        assert_eq!(pixel(10, y), [0, 0, 0, 255], "sidebar at y={y}");
        assert!(
            pixel(40, y)[0].abs_diff(180) <= 1,
            "left half at y={y}: {:?}",
            pixel(40, y)
        );
        assert!(
            pixel(100, y)[0].abs_diff(60) <= 1,
            "right half at y={y}: {:?}",
            pixel(100, y)
        );
        assert!(
            pixel(119, y)[0].abs_diff(60) <= 1,
            "right edge without row tail at y={y}"
        );
    }
    video.destroy(&context).unwrap();
}
