//! Видео из НАСТОЯЩЕГО FFmpeg-декодера доходит до показа в GLES2-рендере (сессия 09 плана
//! gles2-renderer): production-цепочка плеера целиком, кроме окна и приложения.
//!
//! FFmpeg decoder thread (VP9, кадры в блоках нашего `get_buffer2`, stride больше видимой
//! ширины) → `prepare_video_import` (обёртка освобождения по fence) → `install_video_import` →
//! аренда кадра → `materialize` → `acquire_surface_frame` → `render_frame` → `Presented` с
//! ожидаемым pts и пикселями. Каждый пиксель каждого кадра сверяется с CPU-формулой цвета по
//! байтам того же дескриптора; все кадры возвращаются декодеру ровно по одному разу по fence.
//!
//! Фикстура — закоммиченный маленький синтетический IVF из `crates/video-ffmpeg` (генерация —
//! `tests/fixtures/direct-frame-buffers/generate.sh` там же, источник `testsrc2`).
//!
//! Без EGL тест пропускается с причиной; `GLES2_EGUI_REQUIRE_EGL=1` превращает пропуск в
//! падение. Аппаратный VA-API путь здесь не гоняется (нужна видеокарта и файл владельца): его
//! покрывают `dma_buf_frame_to_screen` (настоящие буферы ядра `udmabuf`) и стенд
//! `video-vaapi/examples/gles2_dmabuf_bench.rs`, а живую приёмку делает главный агент.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "integration test: panics are test assertions; pixel math on tiny frames"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use codec_core::{VideoCodec, VideoDecodeRequirement};
use gles2_video::{VideoColorConversion, YuvColorEncoding, YuvMatrix, YuvRange};
use media_core::{TimeBase, TrackId, TrackTimestamp};
use render_core::{RenderLiveSettings, RenderViewport};
use render_gles2_shell::{Gles2Renderer, VideoDrawPlan, VideoPlacement, renderable_frame};
use render_shell_api::{
    DecoderSurfacePoolSlots, EguiFrameContent, FrameMaterializationOutcome, RenderFrameInput,
    RenderFrameOutcome, RenderScreenDescriptor, RenderShell, RenderSurfaceSize, RenderableFrameKey,
    SurfaceFrameAcquire, SurfacePresentSettings, VideoImportPath, VideoImportPlan,
};
use video_backend_api::{PresentFrameResourceDescriptorLookup, VideoBackendDecoderThreadHandle};
use video_core::{
    DecodePacket, DecodeSendError, DecodedFrame, FrameResourceDescriptor,
    VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState, VideoStreamConfigResult,
    VideoStreamDecodeConfig,
};
use video_ffmpeg::FfmpegSoftwareVideoBackendFactory;
use video_frame_contract::VideoFrameContract;
use video_present_core::{
    VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease, VideoFrameReleaseOutcome,
    VideoFrameReleaseSink,
};

const VP9_FIXTURE: &str =
    "../video-ffmpeg/tests/fixtures/direct-frame-buffers/vp9-176x100-12frames.ivf";
const FIXTURE_FRAME_COUNT: usize = 12;
const FIXTURE_FRAME_INTERVAL: Duration = Duration::from_millis(200);
const VP9_TRACK_ID: TrackId = TrackId::new(1);
const FRAME_SIZE: [u32; 2] = [176, 100];
const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";
const DECODER_TIMEOUT: Duration = Duration::from_secs(10);

/// Режет IVF на пакеты-кадры (32-байтный заголовок файла, 12-байтный заголовок кадра).
fn read_vp9_ivf_packets() -> Vec<Bytes> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(VP9_FIXTURE);
    let file_bytes = std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "VP9 IVF fixture {} is not readable: {error}",
            path.display()
        )
    });
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

/// Эталонный RGBA8 одного пикселя (формула цвета без цветокоррекции, как у wgpu).
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

/// Рендер без окна под размер кадра или пропуск теста.
fn renderer_or_skip(test_name: &str) -> Option<Gles2Renderer> {
    match Gles2Renderer::new_offscreen(
        RenderSurfaceSize {
            width: FRAME_SIZE[0],
            height: FRAME_SIZE[1],
        },
        SurfacePresentSettings::default(),
    ) {
        Ok(renderer) => Some(renderer),
        Err(error) if error.stage == render_shell_api::RendererCreateStage::ApiConnection => {
            assert!(
                std::env::var_os(REQUIRE_EGL_VARIABLE).is_none(),
                "{test_name}: EGL is required but unavailable: {error}"
            );
            eprintln!("skip {test_name}: no EGL: {error}");
            None
        }
        Err(error) => panic!("{test_name}: renderer creation failed: {error}"),
    }
}

/// Отправляет пакет и забирает кадры, которые декодер выдал к подтверждению пакета.
fn send_packet_and_collect(
    decoder: &VideoBackendDecoderThreadHandle,
    packet: DecodePacket,
) -> Vec<DecodedFrame> {
    match decoder.send_packet(packet) {
        Ok(()) => {}
        Err(DecodeSendError::Backpressure(reason)) => panic!("decode backpressure: {reason:?}"),
        Err(DecodeSendError::Fatal(error)) => panic!("fatal decoder send: {error}"),
    }
    let deadline = Instant::now() + DECODER_TIMEOUT;
    let mut frames = Vec::new();
    loop {
        while let Some(frame) = decoder.try_recv_frame() {
            frames.push(frame);
        }
        if decoder.drain_completed_packet_count() > 0 {
            break;
        }
        if let Some(error) = decoder.try_recv_error() {
            panic!("software FFmpeg decoder ended in error: {error}");
        }
        assert!(Instant::now() < deadline, "decoder packet ACK timeout");
        std::thread::sleep(Duration::from_millis(1));
    }
    while let Some(frame) = decoder.try_recv_frame() {
        frames.push(frame);
    }
    frames
}

/// Хвост DPB после конца потока: каждый кадр отдаётся `on_frame` сразу, как его выдал декодер
/// (пул декодера ограничен: хвост ждёт возврата уже показанных кадров).
fn drain_tail(decoder: &VideoBackendDecoderThreadHandle, mut on_frame: impl FnMut(DecodedFrame)) {
    assert!(matches!(
        decoder.begin_end_of_stream_drain(1),
        VideoDecoderEndOfStreamDrainResult::Started(_)
    ));
    let deadline = Instant::now() + DECODER_TIMEOUT;
    loop {
        let drained = matches!(
            decoder.end_of_stream_drain_state(),
            VideoDecoderEndOfStreamDrainState::Drained { .. }
        );
        while let Some(frame) = decoder.try_recv_frame() {
            on_frame(frame);
        }
        if drained {
            return;
        }
        assert!(Instant::now() < deadline, "VP9 EOF drain timeout");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Сколько тест ждёт освобождения слота кольца загрузки, прежде чем счесть его утёкшим.
const BUSY_RING_DEADLINE: Duration = Duration::from_secs(5);

/// Материализует кадр так же, как приложение: `Busy` — честный исход контракта (все слоты
/// кольца ещё под fence прошлых кадров; на llvmpipe GPU дочитывает асинхронно), и тот же
/// кадр повторяется позже. Слот, который не освобождается до срока, — утечка: тест падает.
fn materialize_like_the_app(
    renderer: &mut Gles2Renderer,
    lease: &VideoFrameLease,
    pts: Duration,
) -> RenderableFrameKey {
    let deadline = Instant::now() + BUSY_RING_DEADLINE;
    loop {
        match renderer.video_frames().materialize(lease).outcome {
            FrameMaterializationOutcome::Ready(key) => return key,
            FrameMaterializationOutcome::Busy => {
                assert!(
                    Instant::now() < deadline,
                    "frame {pts:?}: the upload ring stayed Busy past the deadline (leaked slot?)"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            other => panic!("frame {pts:?} must be Ready, got {other:?}"),
        }
    }
}

/// Показывает один кадр декодера через контракт и сверяет pts и пиксели.
fn show_and_verify(
    renderer: &mut Gles2Renderer,
    decoder: &VideoBackendDecoderThreadHandle,
    frame: DecodedFrame,
    shown_pts: &mut Vec<Duration>,
    distinct_pictures: &mut std::collections::HashSet<Vec<u8>>,
) {
    let pts = frame.pts;
    let provider = decoder.resource_provider();
    let PresentFrameResourceDescriptorLookup::Ready { descriptor, .. } =
        provider.resource_descriptor_lookup(frame.resource_handle)
    else {
        panic!("the decoder frame {pts:?} has a descriptor");
    };
    assert!(
        matches!(descriptor, FrameResourceDescriptor::HostPlanar(_)),
        "software decode yields host-planar frames"
    );
    let renderable = renderable_frame(&frame).expect("renderable frame");
    let plan = VideoDrawPlan::for_frame(
        &renderable,
        VideoPlacement {
            surface_size: (FRAME_SIZE[0], FRAME_SIZE[1]),
            video_viewport: RenderViewport::full_surface(FRAME_SIZE[0], FRAME_SIZE[1]),
            exclusions: &[],
        },
        &RenderLiveSettings::default(),
    )
    .expect("draw plan");
    let VideoColorConversion::Sdr { encoding, .. } = plan.request().color else {
        panic!("8-bit VP9 is drawn through the SDR conversion");
    };

    let lease = VideoFrameLease::new(
        VideoFrameLeaseConfig::new(1, frame, Arc::new(ProviderReleaseSink))
            .with_resource_provider(provider),
    );
    let key = materialize_like_the_app(renderer, &lease, pts);
    let screen = RenderScreenDescriptor {
        size_in_pixels: FRAME_SIZE,
        pixels_per_point: 1.0,
    };
    let SurfaceFrameAcquire::Acquired(surface) = renderer.acquire_surface_frame(screen) else {
        panic!("the surface is acquired");
    };
    let outcome = renderer.render_frame(RenderFrameInput {
        surface,
        video_frame: Some(&key),
        egui: EguiFrameContent::empty_fresh(),
        screen,
        video_viewport: RenderViewport::full_surface(FRAME_SIZE[0], FRAME_SIZE[1]),
        video_exclusion_rects: Vec::new(),
        subtitle_overlay: None,
    });
    assert!(
        matches!(outcome, RenderFrameOutcome::Presented(_)),
        "frame {pts:?} must reach the render stage and be presented, got {outcome:?}"
    );
    assert_eq!(
        key.decoded_frame().pts,
        pts,
        "the presented key is the decoded frame"
    );

    let pixels = renderer.read_offscreen_pixels().expect("readback");
    for y in 0..FRAME_SIZE[1] {
        for x in 0..FRAME_SIZE[0] {
            let expected = reference_rgba(encoding, source_samples(&descriptor, x, y));
            let actual = pixels.pixel(x, y).unwrap();
            for channel in 0..4 {
                assert!(
                    actual[channel].abs_diff(expected[channel]) <= 1,
                    "frame {pts:?} pixel ({x},{y}): {actual:?} vs reference {expected:?}"
                );
            }
        }
    }
    distinct_pictures.insert(pixels.as_rgba().to_vec());
    shown_pts.push(pts);
    // Ключ и аренда отпускаются приложением: кадр вернётся декодеру по fence показа.
    drop(key);
    drop(lease);
}

/// Настоящий декодер → контракт рендера → экран, кадр за кадром, с возвратом кадров декодеру.
#[test]
fn ffmpeg_vp9_frames_are_presented_by_the_gles2_renderer_and_returned_to_the_decoder() {
    let Some(mut renderer) = renderer_or_skip(
        "ffmpeg_vp9_frames_are_presented_by_the_gles2_renderer_and_returned_to_the_decoder",
    ) else {
        return;
    };
    let started = FfmpegSoftwareVideoBackendFactory::new()
        .start_for_composition()
        .expect("software FFmpeg backend starts");
    let prepared = renderer
        .video_frames()
        .prepare_video_import(
            started,
            VideoImportPlan {
                path: VideoImportPath::SoftwareHostUpload,
                decoder_surface_pool: DecoderSurfacePoolSlots::new(8),
            },
        )
        .expect("software import is prepared");
    renderer
        .video_frames()
        .install_video_import(prepared.pending)
        .expect("software import is installed");
    let decoder = prepared.backend.into_decoder_thread();
    assert_eq!(
        decoder.configure_stream(VideoStreamDecodeConfig::from_requirement(
            VP9_TRACK_ID,
            &VideoDecodeRequirement::new(VideoCodec::Vp9),
            VideoFrameContract::host_yuv420_planar8(),
        )),
        VideoStreamConfigResult::Configured
    );

    let mut shown_pts = Vec::new();
    let mut distinct_pictures = std::collections::HashSet::new();
    for (index, payload) in read_vp9_ivf_packets().into_iter().enumerate() {
        // Метки времени в базе дорожки (5 кадров в секунду): декодер выдаёт pts кадров из них.
        let track_time = TrackTimestamp::new(
            VP9_TRACK_ID,
            i64::try_from(index).unwrap(),
            TimeBase::new(1, 5).expect("non-zero denominator"),
        );
        let packet = DecodePacket {
            track_id: VP9_TRACK_ID,
            pts: FIXTURE_FRAME_INTERVAL * u32::try_from(index).unwrap(),
            dts: None,
            track_pts: Some(track_time),
            track_dts: None,
            generation: 1,
            encoded_bytes: payload,
            keyframe: index == 0,
            resolved_color: None,
        };
        for frame in send_packet_and_collect(decoder.as_ref(), packet) {
            show_and_verify(
                &mut renderer,
                decoder.as_ref(),
                frame,
                &mut shown_pts,
                &mut distinct_pictures,
            );
        }
    }
    drain_tail(decoder.as_ref(), |frame| {
        show_and_verify(
            &mut renderer,
            decoder.as_ref(),
            frame,
            &mut shown_pts,
            &mut distinct_pictures,
        );
    });

    assert_eq!(
        shown_pts.len(),
        FIXTURE_FRAME_COUNT,
        "every decoded frame reached the screen"
    );
    let expected_pts: Vec<Duration> = (0..FIXTURE_FRAME_COUNT)
        .map(|index| FIXTURE_FRAME_INTERVAL * u32::try_from(index).unwrap())
        .collect();
    let mut sorted_pts = shown_pts.clone();
    sorted_pts.sort();
    assert_eq!(
        sorted_pts, expected_pts,
        "presented pts are the fixture pts"
    );
    assert!(
        distinct_pictures.len() > FIXTURE_FRAME_COUNT / 2,
        "moving picture, not a still"
    );

    // Все кадры вернулись декодеру ровно по одному разу, и вернулись по fence показа.
    renderer
        .wait_for_gpu_idle(Duration::from_secs(5))
        .expect("the GPU becomes idle");
    let stats = renderer.frame_release_stats();
    let released = stats.released_without_fence
        + stats.released_after_fence_signaled_fast
        + stats.released_by_waiter;
    assert_eq!(
        released, FIXTURE_FRAME_COUNT as u64,
        "every decoder frame is released exactly once: {stats:?}"
    );
    assert_eq!(stats.pending_now, 0, "{stats:?}");
    assert_eq!(
        stats.released_without_fence, 0,
        "every shown frame had its own fence published first: {stats:?}"
    );
    assert_eq!(
        renderer.frame_counters().frames_presented,
        FIXTURE_FRAME_COUNT as u64
    );
}
