//! Настоящие кадры FFmpeg доходят до экрана GLES2-рендера (сессия 05 плана gles2-renderer).
//!
//! Production FFmpeg decoder thread (VP9, кадры в блоках нашего `get_buffer2`, stride больше
//! видимой ширины) → дескриптор кадра → адаптер `render-gles2-shell` → видеослой `gles2-video`
//! (одна копия драйвером) → offscreen-цель EGL → readback. Каждый пиксель каждого кадра
//! сверяется с CPU-формулой цвета по байтам того же дескриптора. Кадр возвращается декодеру
//! только после fence кадра показа (решение владельца сессии 05). Путь проверяется дважды: с
//! `GL_EXT_unpack_subimage` (если есть) и с текстурами шириной в stride.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]
#![cfg(feature = "vendored-ffmpeg")]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::{VideoCodec, VideoDecodeRequirement};
use gles2_egui::{
    CreateError, FenceStatus, FrameStart, Gles2Context, GpuFence, OffscreenAlpha, SurfaceSize,
};
use gles2_video::{
    RowLengthPreference, UploadOutcome, VideoLayerOptions, VideoLayerRenderer, YuvColorEncoding,
    YuvMatrix, YuvRange,
};
use media_core::TrackId;
use render_core::{ColorPipelineSettings, RenderViewport};
use render_gles2_shell::{
    VideoDrawPlan, VideoPlacement, host_planar_frame_upload, renderable_frame,
};
use support::wgpu_acceptance::send_decode_packet_and_collect_frames;
use video_backend_api::{PresentFrameResourceDescriptorLookup, VideoBackendDecoderThreadHandle};
use video_core::{
    DecodePacket, DecodedFrame, FrameResourceDescriptor, FrameResourceHandle,
    VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState, VideoStreamConfigResult,
    VideoStreamDecodeConfig,
};
use video_ffmpeg::FfmpegSoftwareVideoBackendFactory;
use video_frame_contract::VideoFrameContract;

const VP9_FIXTURE_RELATIVE_PATH: &str =
    "tests/fixtures/direct-frame-buffers/vp9-176x100-12frames.ivf";
const FIXTURE_FRAME_COUNT: usize = 12;
const FIXTURE_FRAME_INTERVAL: Duration = Duration::from_millis(200);
const VP9_TRACK_ID: TrackId = TrackId::new(1);
const FRAME_WIDTH: u32 = 176;
const FRAME_HEIGHT: u32 = 100;
const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";

/// Режет IVF на пакеты-кадры (32-байтный заголовок файла, 12-байтный заголовок кадра).
fn read_vp9_ivf_packets() -> Vec<Bytes> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(VP9_FIXTURE_RELATIVE_PATH);
    let file_bytes = std::fs::read(&path).expect("read VP9 IVF fixture");
    assert_eq!(&file_bytes[8..12], b"VP90", "IVF fourcc must be VP9");
    let mut packets = Vec::new();
    let mut offset = 32;
    while offset + 12 <= file_bytes.len() {
        let frame_len =
            u32::from_le_bytes(file_bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let payload_start = offset + 12;
        packets.push(Bytes::copy_from_slice(
            &file_bytes[payload_start..payload_start + frame_len],
        ));
        offset = payload_start + frame_len;
    }
    packets
}

fn offscreen_or_skip(test_name: &str) -> Option<Gles2Context> {
    let size = SurfaceSize::new(FRAME_WIDTH, FRAME_HEIGHT).unwrap();
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

fn open_decoder() -> Box<VideoBackendDecoderThreadHandle> {
    let started = FfmpegSoftwareVideoBackendFactory::new()
        .start_for_composition()
        .expect("software FFmpeg backend starts");
    let decoder = started.into_decoder_thread();
    let config = VideoStreamDecodeConfig::from_requirement(
        VP9_TRACK_ID,
        &VideoDecodeRequirement::new(VideoCodec::Vp9),
        VideoFrameContract::host_yuv420_planar8(),
    );
    assert_eq!(
        decoder.configure_stream(config),
        VideoStreamConfigResult::Configured
    );
    decoder
}

/// Декодирует фикстуру и отдаёт каждый кадр `on_frame` сразу, как его выдал декодер: кадры
/// возвращаются декодеру по ходу (пул декодера ограничен, хвост DPB ждёт освобождений).
fn decode_streaming(
    decoder: &VideoBackendDecoderThreadHandle,
    mut on_frame: impl FnMut(DecodedFrame),
) {
    for (index, payload) in read_vp9_ivf_packets().into_iter().enumerate() {
        let packet = DecodePacket {
            track_id: VP9_TRACK_ID,
            pts: FIXTURE_FRAME_INTERVAL * u32::try_from(index).unwrap(),
            dts: None,
            track_pts: None,
            track_dts: None,
            generation: 1,
            encoded_bytes: payload,
            keyframe: index == 0,
            resolved_color: None,
        };
        send_decode_packet_and_collect_frames(decoder, packet)
            .into_iter()
            .for_each(&mut on_frame);
    }
    assert!(matches!(
        decoder.begin_end_of_stream_drain(1),
        VideoDecoderEndOfStreamDrainResult::Started(_)
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let drained = matches!(
            decoder.end_of_stream_drain_state(),
            VideoDecoderEndOfStreamDrainState::Drained { .. }
        );
        while let Some(frame) = decoder.try_recv_frame() {
            on_frame(frame);
        }
        if drained {
            break;
        }
        assert!(Instant::now() < deadline, "VP9 EOF drain timeout");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Эталонный RGBA8 одного пикселя (транскрипция формулы wgpu, без цветокоррекции).
fn reference_rgba(encoding: YuvColorEncoding, [y, u, v]: [u8; 3]) -> [u8; 4] {
    let [y, u, v] = [y, u, v].map(|sample| f32::from(sample) / 255.0);
    let (offset, scale) = match encoding.range {
        YuvRange::Limited => (0.0625_f32, 1.164_383_6_f32),
        YuvRange::Full => (0.0, 1.0),
    };
    let rows: [[f32; 3]; 3] = match (encoding.matrix, encoding.range) {
        (YuvMatrix::Bt709, YuvRange::Limited) => [
            [1.0, 0.0, 1.792_741_1],
            [1.0, -0.213_248_61, -0.532_909_33],
            [1.0, 2.112_401_7, 0.0],
        ],
        (YuvMatrix::Bt709, YuvRange::Full) => [
            [1.0, 0.0, 1.574_8],
            [1.0, -0.187_324_27, -0.468_124_27],
            [1.0, 1.855_6, 0.0],
        ],
        (YuvMatrix::Bt601, YuvRange::Limited) => [
            [1.0, 0.0, 1.596_027_1],
            [1.0, -0.391_762_3, -0.812_968],
            [1.0, 2.017_232_2, 0.0],
        ],
        (YuvMatrix::Bt601, YuvRange::Full) => [
            [1.0, 0.0, 1.402],
            [1.0, -0.344_136_3, -0.714_136_3],
            [1.0, 1.772, 0.0],
        ],
    };
    let yuv = [(y - offset) * scale, u - 0.5, v - 0.5];
    let rgb = rows.map(|row| row[0] * yuv[0] + row[1] * yuv[1] + row[2] * yuv[2]);
    let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    [byte(rgb[0]), byte(rgb[1]), byte(rgb[2]), 255]
}

/// Отсчёты Y/U/V под пикселем `(x, y)` при рисовании 1:1 (центр текселя, цветность co-sited).
fn source_samples(descriptor: &FrameResourceDescriptor, x: u32, y: u32) -> [u8; 3] {
    let FrameResourceDescriptor::HostPlanar(host_planar) = descriptor else {
        panic!("FFmpeg frames are host-planar");
    };
    let sample = |plane_index: usize, plane_x: u32, plane_y: u32| {
        let block = host_planar.visible_plane_block(plane_index).unwrap();
        block.bytes[plane_y as usize * block.stride + plane_x as usize]
    };
    [
        sample(0, x, y),
        sample(1, x / 2, y / 2),
        sample(2, x / 2, y / 2),
    ]
}

/// Кадр декодера, ожидающий fence своего кадра показа.
struct InFlightDecoderFrame {
    fence: Arc<GpuFence>,
    handle: FrameResourceHandle,
    descriptor: FrameResourceDescriptor,
}

/// Вернуть декодеру кадры, чей fence сработал (ровно один раз каждый).
fn release_completed(
    decoder: &VideoBackendDecoderThreadHandle,
    in_flight: &mut Vec<InFlightDecoderFrame>,
    released: &mut usize,
) {
    let (completed, pending): (Vec<_>, Vec<_>) = in_flight
        .drain(..)
        .partition(|frame| frame.fence.poll().unwrap() == FenceStatus::Signaled);
    *in_flight = pending;
    for frame in completed {
        // Сначала отпускаем свой взгляд на память кадра, потом возвращаем кадр декодеру.
        drop(frame.descriptor);
        decoder.release_frame(frame.handle);
        *released += 1;
    }
}

fn render_fixture_through_gles2(row_length: RowLengthPreference) {
    let test_name = "vp9_frames_reach_gles2_screen";
    let Some(mut context) = offscreen_or_skip(test_name) else {
        return;
    };
    let decoder = open_decoder();
    let provider = decoder.resource_provider();
    let mut video = VideoLayerRenderer::new(
        &context,
        VideoLayerOptions {
            row_length,
            ..VideoLayerOptions::default()
        },
    )
    .unwrap();
    let mut in_flight = Vec::new();
    let mut released = 0;
    let mut copied_bytes = 0;
    let mut distinct_frames = std::collections::HashSet::new();
    let mut drawn_frames = 0;
    decode_streaming(decoder.as_ref(), |frame| {
        let frame = &frame;
        drawn_frames += 1;
        let PresentFrameResourceDescriptorLookup::Ready { descriptor, .. } =
            provider.resource_descriptor_lookup(frame.resource_handle)
        else {
            panic!("decoded frame {:?} has a descriptor", frame.resource_handle);
        };
        let FrameResourceDescriptor::HostPlanar(host_planar) = &descriptor else {
            panic!("software decode yields host-planar frames");
        };
        assert!(
            host_planar.planes[0].stride > FRAME_WIDTH as usize,
            "decoder rows carry a tail (stride {})",
            host_planar.planes[0].stride
        );
        let upload = host_planar_frame_upload(frame, &descriptor).unwrap();
        let UploadOutcome::Uploaded(report) = video.upload(&context, &upload).unwrap() else {
            panic!("frame {:?} must upload", frame.pts);
        };
        // Каждый видимый байт кадра — ровно один раз (с `GL_UNPACK_ROW_LENGTH` хвосты строк
        // не копируются, без него копируются вместе со строками).
        let visible_bytes =
            (FRAME_WIDTH * FRAME_HEIGHT + 2 * (FRAME_WIDTH / 2) * (FRAME_HEIGHT / 2)) as usize;
        if report.upload_calls == 3 {
            assert_eq!(
                report.copied_bytes, visible_bytes,
                "row-length upload copies visible bytes"
            );
        } else {
            assert!(
                report.copied_bytes > visible_bytes,
                "stride-wide upload carries row tails"
            );
            assert_eq!(
                report.upload_calls, 6,
                "two calls per plane without row length"
            );
        }
        copied_bytes += report.copied_bytes;
        let plan = VideoDrawPlan::for_frame(
            &renderable_frame(frame).unwrap(),
            VideoPlacement {
                surface_size: (FRAME_WIDTH, FRAME_HEIGHT),
                video_viewport: RenderViewport::full_surface(FRAME_WIDTH, FRAME_HEIGHT),
                exclusions: &[],
            },
            &ColorPipelineSettings::identity(),
        )
        .unwrap();
        match context.begin_frame().unwrap() {
            FrameStart::Ready(active) => {
                video.draw(&active, &plan.request()).unwrap();
                active.present().unwrap();
            }
            FrameStart::Skipped(reason) => panic!("skipped: {reason:?}"),
        }
        // Один fence кадра показа: им же охраняется и слот кольца, и кадр декодера.
        let fence = Arc::new(context.insert_fence().unwrap());
        video.retire_submitted_work(Arc::clone(&fence));

        let pixels = context.read_offscreen_pixels().unwrap();
        let encoding = plan.request().color;
        for y in 0..FRAME_HEIGHT {
            for x in 0..FRAME_WIDTH {
                let expected = reference_rgba(encoding, source_samples(&descriptor, x, y));
                let actual = pixels.pixel(x, y).unwrap();
                for channel in 0..4 {
                    assert!(
                        actual[channel].abs_diff(expected[channel]) <= 1,
                        "frame {:?} pixel ({x},{y}): {actual:?} vs reference {expected:?}",
                        frame.pts
                    );
                }
            }
        }
        distinct_frames.insert(pixels.as_rgba().to_vec());
        in_flight.push(InFlightDecoderFrame {
            fence,
            handle: frame.resource_handle,
            descriptor,
        });
        release_completed(decoder.as_ref(), &mut in_flight, &mut released);
    });
    assert_eq!(
        drawn_frames, FIXTURE_FRAME_COUNT,
        "every fixture frame reached the screen"
    );
    // Readback дождался GPU, поэтому все fence-ы сработали: каждый кадр вернётся декодеру.
    for frame in &in_flight {
        assert_eq!(
            frame.fence.wait(Duration::from_secs(5)).unwrap(),
            FenceStatus::Signaled
        );
    }
    release_completed(decoder.as_ref(), &mut in_flight, &mut released);
    assert_eq!(
        released, FIXTURE_FRAME_COUNT,
        "every decoder frame released exactly once"
    );
    assert!(
        distinct_frames.len() > FIXTURE_FRAME_COUNT / 2,
        "moving picture, not a still"
    );
    let stats = video.stats();
    assert_eq!(stats.frames_uploaded, FIXTURE_FRAME_COUNT as u64);
    assert_eq!(stats.copied_bytes, copied_bytes as u64);
    assert_eq!(
        stats.texture_allocations, 9,
        "3 slots × 3 planes, no per-frame allocation"
    );
    video.destroy(&context).unwrap();
}

#[test]
fn vp9_frames_reach_gles2_screen_with_best_row_length_path() {
    render_fixture_through_gles2(RowLengthPreference::BestAvailable);
}

#[test]
fn vp9_frames_reach_gles2_screen_with_stride_wide_textures() {
    render_fixture_through_gles2(RowLengthPreference::StrideWideTextures);
}
