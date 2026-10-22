//! GPU-тесты: копия плоскостей уходит в очередь вместе с кадром, который её рисует.
//!
//! Работает настоящая цепочка: `HostPlanarWgpuFrameMaterializer` (memcpy в staging и
//! запись копии) → `WgpuVideoRenderer::render_or_clear` → submit → чтение пикселя
//! цели обратно с GPU. Проверяется то, что видно на экране: белый кадр должен
//! оказаться белым только тогда, когда его копия реально отправлена.
//!
//! Нужен любой Vulkan/GL адаптер. Если его нет, тест сообщает об этом и пропускается,
//! как остальные GPU-тесты crate-а.

use std::sync::mpsc;
use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use render_core::RenderViewport;
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup,
};
use video_core::{
    DecodedFrame, FrameResourceDescriptor, FrameResourceHandle, HostPlanarFrameDescriptor,
    HostPlaneDescriptor, HostPlaneRole, VideoFrameDiagnostics,
};
use video_frame_contract::VideoFrameContract;

use crate::video::{
    HostPlanarWgpuFrameMaterializer, HostPlanarWgpuTextureViewLookup, HostPlanarWgpuTextureViews,
    WgpuRenderableFrame, WgpuVideoRenderInput, WgpuVideoRenderOutcome, WgpuVideoRenderer,
};

const FRAME_WIDTH: u32 = 4;
const FRAME_HEIGHT: u32 = 2;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Ограниченный диапазон BT.709: Y = 235 — белый, Y = 16 — чёрный, U = V = 128 — без цвета.
const WHITE_LUMA: u8 = 235;
const BLACK_LUMA: u8 = 16;
const NEUTRAL_CHROMA: u8 = 128;

/// Handle белого кадра (провайдер отдаёт белые плоскости).
const WHITE_FRAME: FrameResourceHandle = FrameResourceHandle(1);
/// Handle чёрного кадра.
const BLACK_FRAME: FrameResourceHandle = FrameResourceHandle(2);

const WHITE_PIXEL: [u8; 4] = [255, 255, 255, 255];
const BLACK_PIXEL: [u8; 4] = [0, 0, 0, 255];
/// Допуск округления цветового преобразования шейдера.
const PIXEL_TOLERANCE: u8 = 4;

/// Device/queue для теста либо `None`, если адаптера в окружении нет.
fn try_create_gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

/// Провайдер host-кадров: белые плоскости для `WHITE_FRAME`, чёрные для остальных.
#[derive(Clone)]
struct SolidColorFrameProvider;

impl SolidColorFrameProvider {
    fn descriptor(handle: FrameResourceHandle) -> HostPlanarFrameDescriptor {
        let luma = if handle == WHITE_FRAME {
            WHITE_LUMA
        } else {
            BLACK_LUMA
        };
        let luma_len = (FRAME_WIDTH * FRAME_HEIGHT) as usize;
        let chroma_len = luma_len / 4;
        let mut bytes = vec![luma; luma_len];
        bytes.extend(std::iter::repeat_n(NEUTRAL_CHROMA, chroma_len * 2));
        let plane = |role, offset, width: u32, height: u32| HostPlaneDescriptor {
            role,
            offset,
            stride: width as usize,
            visible_width: width,
            visible_height: height,
            bytes_per_sample: 1,
        };
        HostPlanarFrameDescriptor::from_owned_buffer(
            bytes,
            vec![
                plane(HostPlaneRole::Luma, 0, FRAME_WIDTH, FRAME_HEIGHT),
                plane(
                    HostPlaneRole::ChromaU,
                    luma_len,
                    FRAME_WIDTH / 2,
                    FRAME_HEIGHT / 2,
                ),
                plane(
                    HostPlaneRole::ChromaV,
                    luma_len + chroma_len,
                    FRAME_WIDTH / 2,
                    FRAME_HEIGHT / 2,
                ),
            ],
        )
    }
}

impl PresentFrameResourceProvider for SolidColorFrameProvider {
    fn resource_lookup(&self, _handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        PresentFrameResourceProviderLookup::Ready {
            resource_pool_lock_wait: Duration::ZERO,
        }
    }

    fn try_resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        PresentFrameResourceDescriptorLookup::Ready {
            descriptor: FrameResourceDescriptor::HostPlanar(Self::descriptor(handle)),
            resource_pool_lock_wait: Duration::ZERO,
        }
    }

    fn release_frame(&self, _handle: FrameResourceHandle) {}
}

fn decoded_frame(resource_handle: FrameResourceHandle) -> DecodedFrame {
    DecodedFrame {
        generation: 0,
        pts: Duration::ZERO,
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
        width: FRAME_WIDTH,
        height: FRAME_HEIGHT,
        render_width: FRAME_WIDTH,
        render_height: FRAME_HEIGHT,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle,
        diagnostics: VideoFrameDiagnostics::default(),
    }
}

/// Настоящий материализатор, рендерер и цель с буфером обратного чтения.
struct UploadToScreenChain {
    device: wgpu::Device,
    queue: wgpu::Queue,
    materializer: HostPlanarWgpuFrameMaterializer,
    renderer: WgpuVideoRenderer,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    readback: wgpu::Buffer,
}

impl UploadToScreenChain {
    fn new((device, queue): (wgpu::Device, wgpu::Queue)) -> Self {
        let materializer = HostPlanarWgpuFrameMaterializer::new(
            &device,
            &queue,
            PresentFrameResourceProviderHandle::new(SolidColorFrameProvider),
        );
        let mut renderer = WgpuVideoRenderer::new(&device, TARGET_FORMAT);
        renderer.resize(FRAME_WIDTH, FRAME_HEIGHT);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pending upload test target"),
            size: wgpu::Extent3d {
                width: FRAME_WIDTH,
                height: FRAME_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pending upload test readback"),
            size: u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT * FRAME_HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self {
            device,
            queue,
            materializer,
            renderer,
            target,
            target_view,
            readback,
        }
    }

    /// Загрузка кадра через материализатор (memcpy в staging + запись GPU-копии).
    fn upload(&self, handle: FrameResourceHandle) -> HostPlanarWgpuTextureViews {
        match self
            .materializer
            .try_host_planar_texture_view_lookup(&decoded_frame(handle))
        {
            HostPlanarWgpuTextureViewLookup::Ready { views, .. } => *views,
            _ => panic!("материализатор должен загрузить кадр {handle:?}"),
        }
    }

    /// Рисует кадр из views слота; encoder возвращается неотправленным.
    fn draw(
        &mut self,
        handle: FrameResourceHandle,
        views: &HostPlanarWgpuTextureViews,
    ) -> (WgpuVideoRenderOutcome, wgpu::CommandEncoder) {
        let decoded = decoded_frame(handle);
        let frame = WgpuRenderableFrame::from_decoded_host_planar(&decoded, views.plane_views())
            .expect("host-planar кадр из views материализатора");
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let outcome = self
            .renderer
            .render_or_clear(WgpuVideoRenderInput {
                frame: Some(&frame),
                video_viewport: RenderViewport {
                    x: 0,
                    y: 0,
                    width: FRAME_WIDTH,
                    height: FRAME_HEIGHT,
                },
                video_exclusion_rects: &[],
                target: &self.target_view,
                encoder: &mut encoder,
                device: &self.device,
                queue: &self.queue,
            })
            .expect("рендерер принимает host-planar кадр");
        assert!(outcome.frame_drawn, "кадр должен дойти до render pass-а");
        (outcome, encoder)
    }

    /// Отправляет кадр одним submit-ом (копия загрузки, если есть, — первой) и
    /// возвращает левый верхний пиксель цели.
    fn submit_and_read_pixel(
        &self,
        host_upload: Option<wgpu::CommandBuffer>,
        mut encoder: wgpu::CommandEncoder,
    ) -> [u8; 4] {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
                    rows_per_image: Some(FRAME_HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: FRAME_WIDTH,
                height: FRAME_HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        let submission = self
            .queue
            .submit(host_upload.into_iter().chain([encoder.finish()]));
        let readback_slice = self.readback.slice(..);
        let (mapped_sender, mapped_receiver) = mpsc::sync_channel(1);
        readback_slice.map_async(wgpu::MapMode::Read, move |mapping| {
            let _ = mapped_sender.send(mapping);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(GPU_WAIT_TIMEOUT),
            })
            .expect("GPU завершает кадр");
        mapped_receiver
            .recv_timeout(GPU_WAIT_TIMEOUT)
            .expect("callback маппинга")
            .expect("маппинг буфера чтения");
        let pixel = {
            let mapped = readback_slice
                .get_mapped_range()
                .expect("буфер чтения отображён");
            [mapped[0], mapped[1], mapped[2], mapped[3]]
        };
        self.readback.unmap();
        pixel
    }
}

fn assert_pixel_near(actual: [u8; 4], expected: [u8; 4], what: &str) {
    let close = actual
        .iter()
        .zip(expected)
        .all(|(&channel, expected_channel)| channel.abs_diff(expected_channel) <= PIXEL_TOLERANCE);
    assert!(close, "{what}: пиксель {actual:?}, ожидался ~{expected:?}");
}

#[test]
fn drawn_frame_hands_its_upload_to_the_frame_submit() {
    let Some(gpu) = try_create_gpu() else {
        eprintln!("пропуск: в окружении нет wgpu-адаптера");
        return;
    };
    let mut chain = UploadToScreenChain::new(gpu);

    let views = chain.upload(WHITE_FRAME);
    let (outcome, encoder) = chain.draw(WHITE_FRAME, &views);
    let host_upload = outcome
        .host_upload
        .expect("первый показ свежезагруженного слота несёт его GPU-копию");
    let pixel = chain.submit_and_read_pixel(Some(host_upload.into_command_buffer()), encoder);

    assert_pixel_near(pixel, WHITE_PIXEL, "кадр с копией в том же submit-е");
}

/// Материализатор больше не отправляет копию сам: без копии из исхода рендера кадр
/// рисует незагруженную текстуру. На старом коде (свой submit на кадр) кадр был бы белым.
#[test]
fn upload_is_not_submitted_apart_from_the_frame() {
    let Some(gpu) = try_create_gpu() else {
        eprintln!("пропуск: в окружении нет wgpu-адаптера");
        return;
    };
    let mut chain = UploadToScreenChain::new(gpu);

    let views = chain.upload(WHITE_FRAME);
    let (outcome, encoder) = chain.draw(WHITE_FRAME, &views);
    assert!(outcome.host_upload.is_some());
    drop(outcome);
    let pixel = chain.submit_and_read_pixel(None, encoder);

    assert!(
        pixel[0] < WHITE_PIXEL[0] / 2,
        "без копии загрузки кадр не может быть белым: {pixel:?}"
    );
}

/// Кадр загрузили, но не нарисовали: его копия уходит в очередь перед следующей
/// загрузкой, и позже нарисованный слот показывает свои pixels без повторной копии.
#[test]
fn undrawn_upload_is_submitted_before_the_next_upload() {
    let Some(gpu) = try_create_gpu() else {
        eprintln!("пропуск: в окружении нет wgpu-адаптера");
        return;
    };
    let mut chain = UploadToScreenChain::new(gpu);

    let white_views = chain.upload(WHITE_FRAME);
    let black_views = chain.upload(BLACK_FRAME);

    let (white_outcome, white_encoder) = chain.draw(WHITE_FRAME, &white_views);
    assert!(
        white_outcome.host_upload.is_none(),
        "копия белого кадра уже ушла в очередь перед загрузкой чёрного"
    );
    let white_pixel = chain.submit_and_read_pixel(None, white_encoder);
    assert_pixel_near(
        white_pixel,
        WHITE_PIXEL,
        "слот, загруженный до чужой загрузки",
    );

    let (black_outcome, black_encoder) = chain.draw(BLACK_FRAME, &black_views);
    let black_upload = black_outcome
        .host_upload
        .expect("копия последней загрузки ждёт submit-а своего кадра");
    let black_pixel =
        chain.submit_and_read_pixel(Some(black_upload.into_command_buffer()), black_encoder);
    assert_pixel_near(black_pixel, BLACK_PIXEL, "последняя загрузка с кадром");
}

#[test]
fn repeated_draw_of_a_slot_carries_no_second_upload() {
    let Some(gpu) = try_create_gpu() else {
        eprintln!("пропуск: в окружении нет wgpu-адаптера");
        return;
    };
    let mut chain = UploadToScreenChain::new(gpu);
    let views = chain.upload(WHITE_FRAME);
    let (first_outcome, first_encoder) = chain.draw(WHITE_FRAME, &views);
    let first_upload = first_outcome
        .host_upload
        .map(|upload| upload.into_command_buffer());
    let _ = chain.submit_and_read_pixel(first_upload, first_encoder);

    let (repeat_outcome, repeat_encoder) = chain.draw(WHITE_FRAME, &views);
    assert!(
        repeat_outcome.host_upload.is_none(),
        "повторный показ слота не должен повторять копию"
    );
    let pixel = chain.submit_and_read_pixel(None, repeat_encoder);

    assert_pixel_near(pixel, WHITE_PIXEL, "повторный показ того же слота");
}
