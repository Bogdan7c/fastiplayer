//! Общий стенд сквозных приёмок программного FFmpeg до WGPU submit/release.
//!
//! Стенд не знает, откуда берутся packets (MPEG-TS демуксер, IVF-фикстура):
//! тест сам строит `DecodePacket`, а стенд проводит кадр через production
//! FFmpeg backend, HostPlanar materializer, `WgpuVideoRenderer`, настоящий
//! `Queue::submit` и submitted release.

// Каждый интеграционный тест подключает стенд целиком, но использует свою часть.
#![allow(
    dead_code,
    reason = "shared acceptance harness; each test binary uses a subset"
)]

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use render_core::RenderViewport;
use render_wgpu_video::{
    HostPlanarWgpuFrameMaterializer, HostPlanarWgpuTextureViewLookup, WgpuRenderableFrame,
    WgpuVideoRenderInput, WgpuVideoRenderer, wrap_video_backend_for_wgpu_submission,
};
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProviderHandle,
    VideoBackendDecoderThreadHandle,
};
use video_core::{
    DecodePacket, DecodeSendError, DecodedFrame, VideoDecoderEndOfStreamDrainResult,
    VideoDecoderEndOfStreamDrainState, VideoStreamConfigResult, VideoStreamDecodeConfig,
};
use video_ffmpeg::FfmpegSoftwareVideoBackendFactory;

/// Максимальное ожидание decoder ACK, WGPU completion или release callback-а.
pub const ACCEPTANCE_TIMEOUT: Duration = Duration::from_secs(10);

/// Ширина synthetic corpus и offscreen render target-а.
const TARGET_WIDTH: u32 = 160;

/// Высота synthetic corpus и offscreen render target-а.
const TARGET_HEIGHT: u32 = 90;

/// Формат offscreen target-а совпадает с обычным 8-bit SDR surface path-ом.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// Две поколения, которые обязана доказать сквозная приёмка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptanceGeneration {
    /// Кадр обычного воспроизведения до seek.
    BeforeSeek,

    /// Кадр нового поколения после ненулевого seek.
    AfterSeek,
}

impl AcceptanceGeneration {
    /// Возвращает wire generation, передаваемую production decoder protocol-у.
    pub const fn value(self) -> u64 {
        match self {
            Self::BeforeSeek => 1,
            Self::AfterSeek => 2,
        }
    }
}

/// Факты, полученные только после materialize, render, submit, completion и release.
#[derive(Debug)]
pub struct SubmittedFrameEvidence {
    /// Поколение реально отправленного кадра.
    pub generation: u64,

    /// PTS реально отправленного кадра.
    pub pts: Duration,

    /// Decoder-owned handle, прошедший submitted release lifecycle.
    pub resource_handle: video_core::FrameResourceHandle,

    /// Readback обнаружил хотя бы один ненулевой RGB-компонент video draw-а.
    pub contains_visible_video: bool,
}

/// Headless WGPU boundary без window/surface/compositor ownership.
pub struct OffscreenWgpuHarness {
    /// Device выполняет production materializer и renderer commands.
    pub device: wgpu::Device,

    /// Queue принимает upload, render submit и completion callback-и.
    pub queue: wgpu::Queue,

    /// Production video renderer, используемый приложением перед shell submit.
    renderer: WgpuVideoRenderer,

    /// Offscreen texture заменяет swapchain image, не меняя video render boundary.
    target_texture: wgpu::Texture,

    /// View передаётся production `render_or_clear`.
    target_view: wgpu::TextureView,

    /// MAP_READ buffer доказывает, что submit выполнил video draw, а не только clear.
    readback_buffer: wgpu::Buffer,

    /// WGPU требует 256-byte alignment для `bytes_per_row` texture copy.
    padded_bytes_per_row: u32,
}

impl OffscreenWgpuHarness {
    /// Создаёт настоящий Vulkan device/queue и renderer без display server-а.
    pub fn new() -> Self {
        // Production shell тоже создаёт Vulkan-only instance без заранее захваченного display.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        // Offscreen acceptance не требует surface compatibility и работает на lavapipe CI.
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("Get Vulkan adapter for headless WGPU acceptance");

        // Software host-planar path не требует NV12/P010 device features.
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("AUD-013 offscreen acceptance device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .expect("WGPU device for software host-upload acceptance");

        // Target поддерживает production render attachment и последующий acceptance readback.
        let target_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("AUD-013 offscreen video target"),
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

        // Copy stride округляется вверх до официального WGPU alignment contract-а.
        let unpadded_bytes_per_row = TARGET_WIDTH * 4;
        let copy_alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(copy_alignment) * copy_alignment;
        let readback_size = u64::from(padded_bytes_per_row) * u64::from(TARGET_HEIGHT);
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("AUD-013 offscreen video readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        // Renderer получает тот же target format, с которым создан offscreen attachment.
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

    /// Материализует decoded frame, рисует его, submit-ит и ждёт submitted release.
    pub fn submit_and_release(
        &mut self,
        materializer: &HostPlanarWgpuFrameMaterializer,
        renderer_provider: &PresentFrameResourceProviderHandle,
        frame: DecodedFrame,
    ) -> SubmittedFrameEvidence {
        // Сохраняем scheduler evidence до передачи handle release boundary-у.
        let generation = frame.generation;
        let pts = frame.pts;
        let resource_handle = frame.resource_handle;

        // Настоящий materializer читает AVFrame-backed descriptor и загружает Y/U/V planes.
        let uploaded_views = match materializer.try_host_planar_texture_view_lookup(&frame) {
            HostPlanarWgpuTextureViewLookup::Ready { views, .. } => views,
            HostPlanarWgpuTextureViewLookup::Busy { .. } => {
                panic!("production HostPlanar materializer flashback Busy")
            }
            HostPlanarWgpuTextureViewLookup::Missing { .. } => {
                panic!("decoded AVFrame resource absent materializer boundary")
            }
            HostPlanarWgpuTextureViewLookup::Unsupported { reason, .. } => {
                panic!("production HostPlanar materializer flattened frame: {reason:?}")
            }
            HostPlanarWgpuTextureViewLookup::Error { .. } => {
                panic!("production HostPlanar materializer ended in error")
            }
        };

        // Renderable wrapper проверяет decoded metadata/contract перед shader boundary.
        // Views слота несут ожидающую GPU-копию плоскостей: рендерер отдаст её в submit кадра.
        let renderable_frame =
            WgpuRenderableFrame::from_decoded_host_planar(&frame, uploaded_views.plane_views())
                .expect("gather up production WGPU renderable frame");

        // Один encoder содержит настоящий video render pass и texture-to-buffer evidence copy.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("AUD-013 offscreen video encoder"),
            });

        // `true` запрещает принять clear-only path за успешный video submit.
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
            .expect("production WGPU renderer must accept materialized frame");
        assert!(
            drew_video.frame_drawn,
            "renderer should not replace video draw clear-only pass"
        );
        assert!(
            drew_video.external_sync.is_none(),
            "host-upload frames have no external writer to synchronize with"
        );

        // Copy записывается после render pass в тот же command buffer.
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

        // Настоящий Queue::submit является обязательной границей AUD-013: копия
        // загрузки кадра идёт первой в том же submit-е, что и его render pass.
        let host_upload = drew_video
            .host_upload
            .map(|upload| upload.into_command_buffer());
        let submission_index = self
            .queue
            .submit(host_upload.into_iter().chain([encoder.finish()]));

        // Renderer-owned provider регистрирует release только после уже выполненного submit.
        renderer_provider.release_frame(resource_handle);

        // Map callback дополнительно доказывает завершение submitted copy.
        let readback_slice = self.readback_buffer.slice(..);
        let (mapping_sender, mapping_receiver) = mpsc::sync_channel(1);
        readback_slice.map_async(wgpu::MapMode::Read, move |mapping_result| {
            mapping_sender
                .send(mapping_result)
                .expect("Transmit the result of WGPU readback mapping");
        });

        // Ждём конкретный submit; в это же время WGPU вызывает submitted release callback.
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission_index),
                timeout: Some(ACCEPTANCE_TIMEOUT),
            })
            .expect("wait offscreen WGPU submit");
        mapping_receiver
            .recv_timeout(ACCEPTANCE_TIMEOUT)
            .expect("obtain callback WGPU readback mapping")
            .expect("Successfully display WGPU readback buffer");

        // Проверяем только видимые bytes каждой строки, исключая alignment padding.
        let mapped_bytes = readback_slice
            .get_mapped_range()
            .expect("Get mapped range WGPU readback");
        let visible_row_bytes = usize::try_from(TARGET_WIDTH * 4).expect("row bytes are placed");
        let padded_row_bytes = usize::try_from(self.padded_bytes_per_row).expect("stride fits");
        let contains_visible_video = mapped_bytes
            .chunks_exact(padded_row_bytes)
            .take(usize::try_from(TARGET_HEIGHT).expect("height-place"))
            .any(|row| {
                row[..visible_row_bytes]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0)
            });
        drop(mapped_bytes);
        self.readback_buffer.unmap();

        // Callback обязан вернуть decoder-owned resource точно в этом completed lifecycle.
        assert_resource_released(renderer_provider, resource_handle, &self.device);

        SubmittedFrameEvidence {
            generation,
            pts,
            resource_handle,
            contains_visible_video,
        }
    }
}

/// Стартует FFmpeg backend, оборачивает submitted releases production WGPU binding-ом
/// и конфигурирует поток переданной конфигурацией.
pub fn open_wrapped_decoder(
    stream_config: VideoStreamDecodeConfig,
    queue: &wgpu::Queue,
) -> (
    Box<VideoBackendDecoderThreadHandle>,
    PresentFrameResourceProviderHandle,
) {
    let started_backend = FfmpegSoftwareVideoBackendFactory::new()
        .start_for_composition()
        .expect("Launch the software FFmpeg backend");
    assert_eq!(started_backend.backend_id(), "ffmpeg-sw");

    // Wrapper разделяет unsubmitted decoder release и submitted renderer release semantics.
    let (wrapped_backend, renderer_provider, _submission_queue_binding) =
        wrap_video_backend_for_wgpu_submission(started_backend, queue);
    let decoder = wrapped_backend.into_decoder_thread();

    assert_eq!(
        decoder.configure_stream(stream_config),
        VideoStreamConfigResult::Configured
    );

    (decoder, renderer_provider)
}

/// Передаёт один real compressed packet и возвращает все опубликованные decoded frames.
pub fn send_decode_packet_and_collect_frames(
    decoder: &VideoBackendDecoderThreadHandle,
    decode_packet: DecodePacket,
) -> Vec<DecodedFrame> {
    match decoder.send_packet(decode_packet) {
        Ok(()) => {}
        Err(DecodeSendError::Backpressure(reason)) => {
            panic!("Unexpected serial decode backpressure: {reason:?}")
        }
        Err(DecodeSendError::Fatal(error)) => panic!("fatal decoder send: {error}"),
    }

    // ACK предотвращает скрытую очередь packets между start и seek поколениями.
    let deadline = Instant::now() + ACCEPTANCE_TIMEOUT;
    let mut decoded_frames = Vec::new();
    loop {
        while let Some(frame) = decoder.try_recv_frame() {
            decoded_frames.push(frame);
        }
        if decoder.drain_completed_packet_count() > 0 {
            break;
        }
        if let Some(error) = decoder.try_recv_error() {
            panic!("software FFmpeg decoder ended in error: {error}");
        }
        assert!(Instant::now() < deadline, "decoder packet ACK timeout");
        thread::sleep(Duration::from_millis(1));
    }

    while let Some(frame) = decoder.try_recv_frame() {
        decoded_frames.push(frame);
    }
    decoded_frames
}

/// Завершает FFmpeg DPB drain, удерживая максимум один renderer candidate frame.
pub fn drain_decoder_to_eof_presentable(
    decoder: &VideoBackendDecoderThreadHandle,
    generation: AcceptanceGeneration,
    minimum_pts: Duration,
) -> Option<DecodedFrame> {
    // Begin boundary должен принять именно generation, которая сейчас декодируется.
    let begin_result = decoder.begin_end_of_stream_drain(generation.value());
    assert!(
        matches!(
            begin_result,
            VideoDecoderEndOfStreamDrainResult::Started(
                VideoDecoderEndOfStreamDrainState::Draining {
                    generation: started_generation
                } | VideoDecoderEndOfStreamDrainState::Drained {
                    generation: started_generation
                }
            ) if started_generation == generation.value()
        ),
        "decoder must accept EOF drain current generation, got {begin_result:?}"
    );

    // Worker может публиковать DPB tail до того, как сменит state на Drained.
    let deadline = Instant::now() + ACCEPTANCE_TIMEOUT;
    let mut presentable_frame = None;
    loop {
        while let Some(frame) = decoder.try_recv_frame() {
            retain_one_presentable_frame(
                decoder,
                generation,
                minimum_pts,
                &mut presentable_frame,
                frame,
            );
        }
        match decoder.end_of_stream_drain_state() {
            VideoDecoderEndOfStreamDrainState::Drained {
                generation: drained_generation,
            } if drained_generation == generation.value() => break,
            VideoDecoderEndOfStreamDrainState::Fatal { error, .. } => {
                panic!("software FFmpeg EOF drain ended in error: {error}")
            }
            _ => {}
        }
        if let Some(error) = decoder.try_recv_error() {
            panic!("software FFmpeg decoder ended during EOF: {error}");
        }
        assert!(Instant::now() < deadline, "decoder EOF drain timeout");
        thread::sleep(Duration::from_millis(1));
    }

    // После terminal state забираем кадры, опубликованные вместе с последним wake-up.
    while let Some(frame) = decoder.try_recv_frame() {
        retain_one_presentable_frame(
            decoder,
            generation,
            minimum_pts,
            &mut presentable_frame,
            frame,
        );
    }
    presentable_frame
}

/// Удерживает один renderer candidate, не блокируя bounded pool остальным tail-ом.
fn retain_one_presentable_frame(
    decoder: &VideoBackendDecoderThreadHandle,
    generation: AcceptanceGeneration,
    minimum_pts: Duration,
    presentable_frame: &mut Option<DecodedFrame>,
    frame: DecodedFrame,
) {
    assert_eq!(
        frame.generation,
        generation.value(),
        "EOF drain should not publish stale generation"
    );
    if presentable_frame.is_none() && frame.pts >= minimum_pts {
        *presentable_frame = Some(frame);
    } else {
        decoder.release_frame(frame.resource_handle);
    }
}

/// Освобождает возможный decoder tail, который не был передан renderer-у перед seek.
pub fn release_unsubmitted_decoder_tail(decoder: &VideoBackendDecoderThreadHandle) {
    while let Some(frame) = decoder.try_recv_frame() {
        decoder.release_frame(frame.resource_handle);
    }
}

/// Доказывает, что submitted callback действительно удалил AVFrame-backed resource.
fn assert_resource_released(
    renderer_provider: &PresentFrameResourceProviderHandle,
    resource_handle: video_core::FrameResourceHandle,
    device: &wgpu::Device,
) {
    let deadline = Instant::now() + ACCEPTANCE_TIMEOUT;
    loop {
        match renderer_provider.resource_descriptor_lookup(resource_handle) {
            PresentFrameResourceDescriptorLookup::Missing { .. } => return,
            PresentFrameResourceDescriptorLookup::Ready { .. } => {}
            PresentFrameResourceDescriptorLookup::Busy { .. } => {}
            PresentFrameResourceDescriptorLookup::Fatal { .. } => {
                panic!("resource provider finished fatal afterward submitted release")
            }
        }

        assert!(
            Instant::now() < deadline,
            "submitted resource not released after WGPU completion"
        );
        device
            .poll(wgpu::PollType::Poll)
            .expect("poll submitted release callback");
        thread::sleep(Duration::from_millis(1));
    }
}
