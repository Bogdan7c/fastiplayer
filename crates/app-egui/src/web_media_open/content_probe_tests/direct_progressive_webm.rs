//! N06 vertical: direct HTTP WebM -> Symphonia -> FFmpeg VP9 -> WGPU submit.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use codec_core::{VideoCodec, VideoDecodeRequirement};
use media_core::{DemuxReadEvent, Demuxer, Packet, TrackKind};
use render_core::RenderViewport;
use render_wgpu_video::{
    HostPlanarWgpuFrameMaterializer, HostPlanarWgpuTextureViewLookup, WgpuRenderableFrame,
    WgpuVideoRenderInput, WgpuVideoRenderer, wrap_video_backend_for_wgpu_submission,
};
use source_core::CancellationToken;
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProviderHandle,
    VideoBackendDecoderThreadHandle,
};
use video_core::{
    DecodePacket, DecodeSendError, DecodedFrame, VideoDecoderEndOfStreamDrainResult,
    VideoDecoderEndOfStreamDrainState, VideoStreamConfigResult, VideoStreamDecodeConfig,
};
use video_ffmpeg::FfmpegSoftwareVideoBackendFactory;
use video_frame_contract::VideoFrameContract;

use super::{FixtureOriginResponse, RangeFixtureOrigin};

/// Bounded deadline для decoder ACK, WGPU completion и submitted release.
const ACCEPTANCE_TIMEOUT: Duration = Duration::from_secs(10);
/// Маленькая offscreen target шире generated 16x16 VP9 fixture-а.
const TARGET_WIDTH: u32 = 64;
/// Маленькая offscreen target выше generated 16x16 VP9 fixture-а.
const TARGET_HEIGHT: u32 = 64;
/// Production renderer поддерживает этот обычный SDR surface format.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;
/// Единственная decoder generation первого direct open-а.
const DECODE_GENERATION: u64 = 1;
/// HTTP owner делает два overlapping bounded reads exact tiny WebM fixture-а.
const N14A_HTTP_WEBM_INITIAL_BODY_BYTES: usize = 1_578;

/// Tiny muxed VP9+Opus WebM, generated once by FFmpeg 6.2 for hermetic tests.
pub(crate) const MUXED_WEBM_BASE64: &str = "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";

/// Headless renderer state не требует window или compositor-а.
pub(crate) struct OffscreenWgpuHarness {
    /// Device владеет render/upload/readback resources.
    device: wgpu::Device,
    /// Queue является production submit boundary и release fence owner-ом.
    queue: wgpu::Queue,
    /// Настоящий WGPU video renderer.
    renderer: WgpuVideoRenderer,
    /// Offscreen render attachment.
    target_texture: wgpu::Texture,
    /// View offscreen attachment-а.
    target_view: wgpu::TextureView,
    /// Buffer доказывает выполненный draw, а не clear-only path.
    readback_buffer: wgpu::Buffer,
    /// WGPU-required aligned copy stride.
    padded_bytes_per_row: u32,
}

impl OffscreenWgpuHarness {
    /// Создаёт Vulkan device/queue; lavapipe подходит как hermetic software adapter.
    pub(crate) fn new() -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .expect("получить Vulkan adapter для direct WebM acceptance");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("N06 direct WebM offscreen device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .expect("создать WGPU device для direct WebM acceptance");
        let target_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("N06 direct WebM target"),
            size: wgpu::Extent3d {
                width: TARGET_WIDTH,
                height: TARGET_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let visible_bytes_per_row = TARGET_WIDTH * 4;
        let padded_bytes_per_row = visible_bytes_per_row
            .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("N06 direct WebM readback"),
            size: u64::from(padded_bytes_per_row) * u64::from(TARGET_HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut renderer = WgpuVideoRenderer::new(&device, TARGET_FORMAT);
        renderer.resize(TARGET_WIDTH, TARGET_HEIGHT);

        Self {
            device,
            queue,
            renderer,
            target_texture,
            target_view,
            readback_buffer,
            padded_bytes_per_row,
        }
    }

    /// Даёт vertical test-у device только для production materializer-а.
    pub(crate) const fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Даёт vertical test-у queue для backend submission contract-а.
    pub(crate) const fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Материализует, рисует, submit-ит и освобождает один decoded frame.
    pub(crate) fn submit_and_release(
        &mut self,
        materializer: &HostPlanarWgpuFrameMaterializer,
        renderer_provider: &PresentFrameResourceProviderHandle,
        frame: DecodedFrame,
    ) -> bool {
        let resource_handle = frame.resource_handle;
        let uploaded_views = match materializer.try_host_planar_texture_view_lookup(&frame) {
            HostPlanarWgpuTextureViewLookup::Ready { views, .. } => views,
            HostPlanarWgpuTextureViewLookup::Busy { .. } => {
                panic!("HostPlanar materializer неожиданно занят")
            }
            HostPlanarWgpuTextureViewLookup::Missing { .. } => {
                panic!("decoded VP9 resource отсутствует")
            }
            HostPlanarWgpuTextureViewLookup::Unsupported { reason, .. } => {
                panic!("HostPlanar materializer отклонил VP9 frame: {reason:?}")
            }
            HostPlanarWgpuTextureViewLookup::Error { .. } => {
                panic!("HostPlanar materializer завершился ошибкой")
            }
        };
        let renderable_frame = WgpuRenderableFrame::from_decoded_host_yuv(
            &frame,
            &uploaded_views.y_view,
            &uploaded_views.u_view,
            &uploaded_views.v_view,
        )
        .expect("собрать WGPU renderable direct WebM frame");
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("N06 direct WebM encoder"),
            });
        let drew_video = self
            .renderer
            .render_or_clear(WgpuVideoRenderInput {
                frame: Some(&renderable_frame),
                video_viewport: RenderViewport {
                    x: 0,
                    y: 0,
                    width: TARGET_WIDTH,
                    height: TARGET_HEIGHT,
                },
                video_exclusion_rects: &[],
                target: &self.target_view,
                encoder: &mut encoder,
                device: &self.device,
                queue: &self.queue,
            })
            .expect("WGPU renderer должен принять direct WebM frame");
        assert!(
            drew_video,
            "direct WebM frame не должен стать clear-only pass-ом"
        );
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(TARGET_HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: TARGET_WIDTH,
                height: TARGET_HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        let submission_index = self.queue.submit([encoder.finish()]);
        renderer_provider.release_frame(resource_handle);
        let readback_slice = self.readback_buffer.slice(..);
        let (mapping_sender, mapping_receiver) = mpsc::sync_channel(1);
        readback_slice.map_async(wgpu::MapMode::Read, move |mapping_result| {
            mapping_sender
                .send(mapping_result)
                .expect("передать direct WebM readback result");
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission_index),
                timeout: Some(ACCEPTANCE_TIMEOUT),
            })
            .expect("дождаться direct WebM WGPU submit");
        mapping_receiver
            .recv_timeout(ACCEPTANCE_TIMEOUT)
            .expect("получить direct WebM map callback")
            .expect("map direct WebM readback buffer");
        let mapped_bytes = readback_slice.get_mapped_range();
        let visible_row_bytes = usize::try_from(TARGET_WIDTH * 4).expect("visible row bytes");
        let padded_row_bytes =
            usize::try_from(self.padded_bytes_per_row).expect("padded row bytes");
        let contains_visible_video = mapped_bytes
            .chunks_exact(padded_row_bytes)
            .take(usize::try_from(TARGET_HEIGHT).expect("target height"))
            .any(|row| {
                row[..visible_row_bytes]
                    .chunks_exact(4)
                    .any(|pixel| pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0)
            });
        drop(mapped_bytes);
        self.readback_buffer.unmap();
        assert_resource_released(renderer_provider, resource_handle, &self.device);
        contains_visible_video
    }
}

/// Запускает production FFmpeg VP9 backend с host-planar output contract-ом.
pub(crate) fn open_decoder(
    video_track: &media_core::TrackInfo,
    queue: &wgpu::Queue,
    codec: VideoCodec,
) -> (
    Box<VideoBackendDecoderThreadHandle>,
    PresentFrameResourceProviderHandle,
) {
    let started_backend = FfmpegSoftwareVideoBackendFactory::new()
        .start_for_composition()
        .expect("запустить software FFmpeg backend");
    let (wrapped_backend, renderer_provider, _submission_queue_binding) =
        wrap_video_backend_for_wgpu_submission(started_backend, queue);
    let decoder = wrapped_backend.into_decoder_thread();
    let stream_config = VideoStreamDecodeConfig::from_requirement(
        video_track.id,
        &VideoDecodeRequirement::new(codec),
        VideoFrameContract::host_yuv420_planar8(),
    )
    .with_codec_private(video_track.codec_private.clone());
    assert_eq!(
        decoder.configure_stream(stream_config),
        VideoStreamConfigResult::Configured
    );
    (decoder, renderer_provider)
}

/// Отправляет real compressed packet и ждёт durable completion ACK.
pub(crate) fn decode_packet(
    decoder: &VideoBackendDecoderThreadHandle,
    packet: Packet,
) -> Vec<DecodedFrame> {
    let decode_packet = DecodePacket {
        track_id: packet.track_id,
        pts: packet.pts,
        dts: packet.dts,
        track_pts: packet.track_pts,
        track_dts: packet.track_dts,
        generation: DECODE_GENERATION,
        encoded_bytes: packet.data,
        keyframe: packet.keyframe.is_known_keyframe(),
        resolved_color: None,
    };
    match decoder.send_packet(decode_packet) {
        Ok(()) => {}
        Err(DecodeSendError::Backpressure(reason)) => {
            panic!("unexpected serial VP9 decode backpressure: {reason:?}")
        }
        Err(DecodeSendError::Fatal(error)) => panic!("fatal VP9 decoder send: {error}"),
    }
    let deadline = Instant::now() + ACCEPTANCE_TIMEOUT;
    let mut frames = Vec::new();
    loop {
        while let Some(frame) = decoder.try_recv_frame() {
            frames.push(frame);
        }
        if decoder.drain_completed_packet_count() > 0 {
            return frames;
        }
        if let Some(error) = decoder.try_recv_error() {
            panic!("software VP9 decoder failed: {error}");
        }
        assert!(Instant::now() < deadline, "VP9 packet ACK timeout");
        thread::sleep(Duration::from_millis(1));
    }
}

/// Читает direct WebM до первого реально decoded VP9 frame-а.
fn decode_first_frame(
    demuxer: &mut dyn Demuxer,
    decoder: &VideoBackendDecoderThreadHandle,
) -> DecodedFrame {
    loop {
        match demuxer.next_event().expect("read direct WebM event") {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                let mut frames = decode_packet(decoder, packet);
                if let Some(frame) = frames.pop() {
                    for unused in frames {
                        decoder.release_frame(unused.resource_handle);
                    }
                    return frame;
                }
            }
            DemuxReadEvent::EndOfStream => return drain_decoder(decoder),
            _ => {}
        }
    }
}

/// Завершает FFmpeg reorder queue, если tiny fixture удержала кадр до EOS.
pub(crate) fn drain_decoder(decoder: &VideoBackendDecoderThreadHandle) -> DecodedFrame {
    let begin = decoder.begin_end_of_stream_drain(DECODE_GENERATION);
    assert!(matches!(
        begin,
        VideoDecoderEndOfStreamDrainResult::Started(_)
    ));
    let deadline = Instant::now() + ACCEPTANCE_TIMEOUT;
    let mut first_frame = None;
    loop {
        while let Some(frame) = decoder.try_recv_frame() {
            if first_frame.is_none() {
                first_frame = Some(frame);
            } else {
                decoder.release_frame(frame.resource_handle);
            }
        }
        match decoder.end_of_stream_drain_state() {
            VideoDecoderEndOfStreamDrainState::Drained { .. } => {
                return first_frame.expect("VP9 EOF drain должен выдать decoded frame");
            }
            VideoDecoderEndOfStreamDrainState::Fatal { error, .. } => {
                panic!("VP9 EOF drain failed: {error}")
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "VP9 EOF drain timeout");
        thread::sleep(Duration::from_millis(1));
    }
}

/// Ждёт, пока WGPU completion callback вернёт decoder-owned frame resource.
fn assert_resource_released(
    renderer_provider: &PresentFrameResourceProviderHandle,
    resource_handle: video_core::FrameResourceHandle,
    device: &wgpu::Device,
) {
    let deadline = Instant::now() + ACCEPTANCE_TIMEOUT;
    loop {
        match renderer_provider.resource_descriptor_lookup(resource_handle) {
            PresentFrameResourceDescriptorLookup::Missing { .. } => return,
            PresentFrameResourceDescriptorLookup::Ready { .. }
            | PresentFrameResourceDescriptorLookup::Busy { .. } => {}
            PresentFrameResourceDescriptorLookup::Fatal { .. } => {
                panic!("direct WebM resource provider failed after submit")
            }
        }
        assert!(
            Instant::now() < deadline,
            "submitted VP9 resource release timeout"
        );
        device
            .poll(wgpu::PollType::Poll)
            .expect("poll WGPU release");
        thread::sleep(Duration::from_millis(1));
    }
}

/// HTTP WebM достигает decoded, drawn, submitted, completed и released frame boundary.
#[test]
fn n14a_consumer_http_webm_reaches_submitted_readback_with_exact_accounting() {
    let webm_bytes = base64::engine::general_purpose::STANDARD
        .decode(MUXED_WEBM_BASE64)
        .expect("decode tiny muxed WebM fixture");
    let origin = RangeFixtureOrigin::spawn_with_response(FixtureOriginResponse::Ogg(webm_bytes));
    let locator = origin.media_url_with_extension("webm");
    let classified = crate::direct_progressive_open::classify_direct_media_url(&locator)
        .expect("WebM должен классифицироваться direct");
    assert_eq!(
        origin.request_count(),
        0,
        "syntactic classifier не имеет права загружать root resource"
    );
    let mut app_config = rustiplayer_config::AppConfig::default();
    app_config.yt_dlp.enabled = false;
    let opened = crate::direct_progressive_open::open_direct_media(
        &classified,
        &app_config.network,
        &app_config.player.demux,
        CancellationToken::new(),
    )
    .expect("open direct WebM");
    let (mut demuxer, _endpoint_recovery) = opened.into_runtime_parts();
    let video_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .cloned()
        .expect("direct WebM должен иметь video track");

    let mut wgpu_harness = OffscreenWgpuHarness::new();
    let (decoder, renderer_provider) =
        open_decoder(&video_track, &wgpu_harness.queue, VideoCodec::Vp9);
    let materializer = HostPlanarWgpuFrameMaterializer::new(
        &wgpu_harness.device,
        &wgpu_harness.queue,
        renderer_provider.clone(),
    );
    let decoded_frame = decode_first_frame(demuxer.as_mut(), decoder.as_ref());
    assert_eq!(decoded_frame.generation, DECODE_GENERATION);
    assert!(wgpu_harness.submit_and_release(&materializer, &renderer_provider, decoded_frame,));
    assert_eq!(
        origin.request_count(),
        2,
        "direct WebM open использует exact probe/read cohort без classifier fetch-а"
    );
    assert_eq!(
        origin.response_body_bytes(),
        N14A_HTTP_WEBM_INITIAL_BODY_BYTES,
        "две bounded Range responses обязаны вернуть exact WebM fixture bytes"
    );
}

/// N14B: graceful close/restart повторно доводит direct WebM до WGPU без extractor.
#[test]
fn n14b_lifecycle_http_webm_close_restart_reaches_submitted_readback_without_extractor() {
    let webm_bytes = base64::engine::general_purpose::STANDARD
        .decode(MUXED_WEBM_BASE64)
        .expect("decode tiny muxed WebM fixture");
    let origin = RangeFixtureOrigin::spawn_with_response(FixtureOriginResponse::Ogg(webm_bytes));
    let locator = origin.media_url_with_extension("webm");
    let classified = crate::direct_progressive_open::classify_direct_media_url(&locator)
        .expect("WebM должен классифицироваться direct");
    let mut app_config = rustiplayer_config::AppConfig::default();
    app_config.yt_dlp.enabled = false;
    let mut wgpu_harness = OffscreenWgpuHarness::new();

    let mut open_render_and_close = || {
        let opened = crate::direct_progressive_open::open_direct_media(
            &classified,
            &app_config.network,
            &app_config.player.demux,
            CancellationToken::new(),
        )
        .expect("open direct WebM lifecycle attempt");
        let (mut demuxer, endpoint_recovery) = opened.into_runtime_parts();
        let video_track = demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .cloned()
            .expect("direct WebM должен иметь video track");
        let (decoder, renderer_provider) =
            open_decoder(&video_track, &wgpu_harness.queue, VideoCodec::Vp9);
        let materializer = HostPlanarWgpuFrameMaterializer::new(
            &wgpu_harness.device,
            &wgpu_harness.queue,
            renderer_provider.clone(),
        );
        let decoded_frame = decode_first_frame(demuxer.as_mut(), decoder.as_ref());
        assert!(wgpu_harness.submit_and_release(&materializer, &renderer_provider, decoded_frame,));
        drop(decoder);
        drop(demuxer);
        drop(endpoint_recovery);
    };

    open_render_and_close();
    open_render_and_close();
    assert_eq!(
        origin.request_count(),
        4,
        "cold open и restart должны выполнить по одному exact probe/read cohort"
    );
    assert_eq!(
        origin.response_body_bytes(),
        N14A_HTTP_WEBM_INITIAL_BODY_BYTES * 2,
        "restart должен повторить только exact WebM body cohort"
    );
}
