//! Функциональный тест адаптера на настоящем EGL: кадр программного декодера (`DecodedFrame` +
//! `HostPlanarFrameDescriptor`) через адаптер и видеослой доходит до экрана — letterbox,
//! сайдбар и цвет из `render-core`; кадр декодера отпускается только после fence. То же для
//! HDR-кадра 10 бит (PQ, BT.2020): HDR → SDR по живым настройкам.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly on broken invariants"
)]

use std::sync::{Arc, Weak};
use std::time::Duration;

use codec_core::{
    ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoColorMetadata,
    VideoDisplayOrientation,
};
use gles2_egui::{CreateError, FenceStatus, FrameStart, Gles2Context, SurfaceSize};
use gles2_video::{
    HdrReferenceLevels, HdrTransfer, ShaderPrecision, UploadOutcome, VideoDrawOutcome,
    VideoLayerOptions, VideoLayerRenderer, bt2446c_to_sdr,
};
use render_core::{PictureLayout, RenderLiveSettings, RenderViewport};
use render_gles2_shell::{
    GLES2_HIGH_BIT_SOFTWARE_FRAME_CONTRACTS, GLES2_SOFTWARE_FRAME_CONTRACTS, VideoDrawPlan,
    VideoPlacement, gles2_render_capabilities, host_planar_frame_upload, renderable_frame,
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
    match Gles2Context::offscreen(size) {
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

/// Как разложить кадр 4:2:0 64×32: две половины яркости и нейтральная цветность.
struct FrameSamples {
    pixel_layout: VideoFramePixelLayout,
    /// Байт на отсчёт (1 — 8 бит, 2 — 10/12 бит LE).
    bytes_per_sample: usize,
    left_luma: u16,
    right_luma: u16,
    neutral_chroma: u16,
    color: VideoColorMetadata,
}

/// 8 бит, полный диапазон: серый Y даёт тот же байт на экране.
fn eight_bit_samples() -> FrameSamples {
    FrameSamples {
        pixel_layout: VideoFramePixelLayout::Yuv420Planar8,
        bytes_per_sample: 1,
        left_luma: 180,
        right_luma: 60,
        neutral_chroma: 128,
        color: VideoColorMetadata {
            range: ColorRange::Full,
            ..VideoColorMetadata::sdr_bt709_limited()
        },
    }
}

/// 10 бит HDR PQ BT.2020 limited: светлая (~200 нит) и тёмная половины.
fn hdr_pq_samples() -> FrameSamples {
    FrameSamples {
        pixel_layout: VideoFramePixelLayout::Yuv420Planar10Le,
        bytes_per_sample: 2,
        left_luma: 575,
        right_luma: 300,
        neutral_chroma: 512,
        color: VideoColorMetadata {
            range: ColorRange::Limited,
            matrix: MatrixCoefficients::Bt2020,
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Pq,
            ..VideoColorMetadata::sdr_bt709_limited()
        },
    }
}

/// Кадр 4:2:0 64×32: левая половина — `left_luma`, правая — `right_luma`; stride с хвостом мусора.
fn decoded_frame(
    orientation: VideoDisplayOrientation,
    samples: &FrameSamples,
) -> (DecodedFrame, FrameResourceDescriptor, Weak<TrackedOwner>) {
    let (width, height, tail_samples) = (64_u32, 32_u32, 16_usize);
    let sample_bytes = samples.bytes_per_sample;
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
                let code = match role {
                    HostPlaneRole::Luma if x < plane_width / 2 => samples.left_luma,
                    HostPlaneRole::Luma => samples.right_luma,
                    HostPlaneRole::ChromaU | HostPlaneRole::ChromaV => samples.neutral_chroma,
                };
                bytes.extend_from_slice(&code.to_le_bytes()[..sample_bytes]);
            }
            if row + 1 < plane_height {
                bytes.extend(std::iter::repeat_n(0xFF, tail_samples * sample_bytes));
            }
        }
        planes.push(HostPlaneDescriptor {
            role,
            offset,
            stride: (plane_width as usize + tail_samples) * sample_bytes,
            visible_width: plane_width,
            visible_height: plane_height,
            bytes_per_sample: sample_bytes,
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
            pixel_layout: samples.pixel_layout,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        },
        width,
        height,
        render_width: width,
        render_height: height,
        display_orientation: orientation,
        color: samples.color.clone(),
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
    // 10/12 бит объявлены ровно тогда, когда у драйвера есть highp.
    let highp = ShaderPrecision::for_capabilities(context.capabilities()).supports_high_bit_depth();
    for contract in GLES2_HIGH_BIT_SOFTWARE_FRAME_CONTRACTS {
        assert_eq!(
            capabilities.supports_frame_contract(contract),
            highp,
            "{contract:?}"
        );
    }

    let mut video = VideoLayerRenderer::new(&context, VideoLayerOptions::default()).unwrap();
    let (frame, descriptor, owner) =
        decoded_frame(VideoDisplayOrientation::Identity, &eight_bit_samples());
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
        &RenderLiveSettings::default(),
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

/// HDR-кадр 10 бит (PQ, BT.2020) доходит до экрана через адаптер: HDR → SDR по живым
/// настройкам по умолчанию, обе половины — цвет точной формулы с допуском таблицы (3/255);
/// кадр декодера отпускается после fence.
#[test]
fn hdr_decoder_frame_reaches_the_screen_through_the_adapter() {
    const TABLE_TOLERANCE: u8 = 3;
    let Some(mut context) = offscreen_or_skip("hdr_decoder_frame_reaches_the_screen", 64, 32)
    else {
        return;
    };
    if !ShaderPrecision::for_capabilities(context.capabilities()).supports_high_bit_depth() {
        eprintln!("skip hdr_decoder_frame_reaches_the_screen: driver has no highp");
        return;
    }
    let samples = hdr_pq_samples();
    let mut video = VideoLayerRenderer::new(&context, VideoLayerOptions::default()).unwrap();
    let (frame, descriptor, owner) = decoded_frame(VideoDisplayOrientation::Identity, &samples);
    let upload = host_planar_frame_upload(&frame, &descriptor).unwrap();
    assert!(matches!(
        video.upload(&context, &upload).unwrap(),
        UploadOutcome::Uploaded(_)
    ));
    let renderable = renderable_frame(&frame).unwrap();
    let plan = VideoDrawPlan::for_frame(
        &renderable,
        VideoPlacement {
            surface_size: (64, 32),
            video_viewport: RenderViewport::full_surface(64, 32),
            exclusions: &[],
        },
        &RenderLiveSettings::default(),
    )
    .unwrap();
    let FrameStart::Ready(active) = context.begin_frame().unwrap() else {
        panic!("offscreen frame skipped");
    };
    assert_eq!(
        video.draw(&active, &plan.request()).unwrap(),
        VideoDrawOutcome::Drawn { draw_calls: 1 }
    );
    active.present().unwrap();
    let fence = Arc::new(context.insert_fence().unwrap());
    video.retire_submitted_work(Arc::clone(&fence));
    assert!(
        owner.upgrade().is_some(),
        "decoder frame is held until its fence"
    );
    assert_eq!(
        fence.wait(Duration::from_secs(5)).unwrap(),
        FenceStatus::Signaled
    );
    drop(descriptor);
    assert!(
        owner.upgrade().is_none(),
        "decoder frame released after the fence"
    );

    let pixels = context.read_offscreen_pixels().unwrap();
    // Нейтраль: U = V = центр, R' = G' = B' = нормализованный Y.
    let expected = |luma_code: u16| {
        let normalized = (f64::from(luma_code) - 64.0) / 876.0;
        bt2446c_to_sdr(
            [normalized; 3],
            HdrTransfer::Pq,
            HdrReferenceLevels::REFERENCE_DEFAULT,
        )
        .map(|value| (value * 255.0).round() as u8)
    };
    for (x, luma_code) in [(10, samples.left_luma), (50, samples.right_luma)] {
        let want = expected(luma_code);
        let got = pixels.pixel(x, 16).unwrap();
        for channel in 0..3 {
            assert!(
                got[channel].abs_diff(want[channel]) <= TABLE_TOLERANCE,
                "x={x} code {luma_code}: {got:?} vs {want:?}"
            );
        }
    }
    assert!(
        expected(samples.left_luma)[1] > expected(samples.right_luma)[1] + 50,
        "the two halves are clearly different tones"
    );
    video.destroy(&context).unwrap();
}
